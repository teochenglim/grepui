//! Pipelines API (R-N1): GreptimeDB pipelines of the org, created, versioned and deleted through
//! GreptimeDB's HTTP API. Writes need `--admin-writes` and are logged; the dry-run does not write.

use super::{Shared, ok};
use crate::{
    error::{ApiError, ReadError},
    org::Org,
    pipelines::{self as p},
    query::{self, Meta},
};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::Response,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;

fn meta(name: &str) -> Meta {
    Meta {
        query: name.into(),
        sql: String::new(),
        binds: vec![],
        ms: 0,
        rows: 0,
        source: "raw",
        note: None,
    }
}

/// Versions of the org's pipelines: short name -> creation times in ns.
async fn versions(s: &Shared, org: &Org) -> Result<(BTreeMap<String, Vec<i64>>, Meta), ApiError> {
    let (rows, m) = match query::run(s.reader.as_ref(), p::list_q()).await {
        Ok(r) => r,
        // No pipeline was ever created: the private database does not exist yet.
        Err(ReadError::Db(d)) if crate::store::is_missing(&d) => (vec![], meta("pipeline_list")),
        Err(e) => return Err(e.into()),
    };
    let mut out: BTreeMap<String, Vec<i64>> = BTreeMap::new();
    for r in &rows {
        if let (Some(n), Some(t)) = (r["name"].as_str(), r["created_ns"].as_i64())
            && let Some(short) = p::short_of(&org.name, n)
        {
            out.entry(short.to_string()).or_default().push(t);
        }
    }
    Ok((out, m))
}

pub async fn list(State(s): State<Shared>, Path(org): Path<String>) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let (v, m) = versions(&s, org).await?;
    let out: Vec<Value> = v
        .iter()
        .map(|(n, ts)| json!({"name": n, "full_name": p::full_name(&org.name, n), "versions": ts.iter().map(|t| json!({"version": p::version_text(*t), "created_ms": t / 1_000_000})).collect::<Vec<_>>(), "latest_ms": ts.iter().max().map(|t| t / 1_000_000)}))
        .collect();
    Ok(ok(
        json!({"pipelines": out, "admin_writes": s.cfg.admin_writes}),
        m,
    ))
}

pub async fn get(
    State(s): State<Shared>,
    Path((org, name)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let short = p::short_name(&name)?;
    let (v, _) = versions(&s, org).await?;
    if !v.contains_key(short) {
        return Err(ApiError::not_found("no such pipeline"));
    }
    let (code, body) = s
        .pipeline_call(
            org,
            "pipeline_get",
            "GET",
            &p::path_for(&p::full_name(&org.name, short)),
            None,
            false,
        )
        .await?;
    let v: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
    if code != 200 {
        return Err(ApiError(StatusCode::BAD_GATEWAY, p::error_text(&body)));
    }
    let first = &v["pipelines"][0];
    Ok(ok(
        json!({"name": short, "version": first["version"], "yaml": first["pipeline"]}),
        meta("pipeline_get"),
    ))
}

#[derive(Deserialize)]
pub struct Yaml {
    yaml: String,
}

pub async fn put(
    State(s): State<Shared>,
    Path((org, name)): Path<(String, String)>,
    Json(b): Json<Yaml>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let short = p::short_name(&name)?;
    p::check_yaml(&b.yaml)?;
    let (code, body) = s
        .pipeline_call(
            org,
            "pipeline_put",
            "POST",
            &p::path_for(&p::full_name(&org.name, short)),
            Some(("application/x-yaml", b.yaml.into_bytes())),
            true,
        )
        .await?;
    if code != 200 {
        return Err(ApiError::bad(p::error_text(&body)));
    }
    let v: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
    Ok(ok(
        json!({"name": short, "version": v["pipelines"][0]["version"]}),
        meta("pipeline_put"),
    ))
}

#[derive(Deserialize)]
pub struct DeleteParams {
    version: Option<String>,
}

pub async fn delete(
    State(s): State<Shared>,
    Path((org, name)): Path<(String, String)>,
    Query(q): Query<DeleteParams>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let short = p::short_name(&name)?;
    s.admin_enabled()?;
    let (v, _) = versions(&s, org).await?;
    let all = v
        .get(short)
        .ok_or_else(|| ApiError::not_found("no such pipeline"))?;
    let want: Vec<String> = match &q.version {
        Some(x) => {
            let Some(t) = all.iter().find(|t| p::version_text(**t) == *x) else {
                return Err(ApiError::not_found("no such version"));
            };
            vec![p::version_text(*t)]
        }
        None => all.iter().map(|t| p::version_text(*t)).collect(),
    };
    for ver in &want {
        let (code, body) = s
            .pipeline_call(
                org,
                "pipeline_delete",
                "DELETE",
                &p::path_with_version(&p::full_name(&org.name, short), ver),
                None,
                true,
            )
            .await?;
        if code != 200 {
            return Err(ApiError(StatusCode::BAD_GATEWAY, p::error_text(&body)));
        }
    }
    Ok(ok(
        json!({"deleted": want.len(), "name": short}),
        meta("pipeline_delete"),
    ))
}

#[derive(Deserialize)]
pub struct DryRun {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    yaml: Option<String>,
    data: Value,
}

/// Run sample records through a stored or inline pipeline. Nothing is stored.
pub async fn dry_run(
    State(s): State<Shared>,
    Path(org): Path<String>,
    Json(b): Json<DryRun>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let full = match &b.name {
        Some(n) => Some(p::full_name(&org.name, p::short_name(n)?)),
        None => None,
    };
    let body = p::dry_body(full.as_deref(), b.yaml.as_deref(), &b.data)?;
    let (code, text) = s
        .pipeline_call(
            org,
            "pipeline_dry_run",
            "POST",
            p::DRYRUN_PATH,
            Some(("application/json", body.into_bytes())),
            false,
        )
        .await?;
    if code != 200 {
        return Err(ApiError::bad(p::error_text(&text)));
    }
    let r = p::dry_result(&text).map_err(ApiError::bad)?;
    Ok(ok(r, meta("pipeline_dry_run")))
}
