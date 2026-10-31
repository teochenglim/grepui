//! Redaction rules API and the response layer (R-N4, ADR 0011). Rules live in the state store and
//! are mirrored in memory; the layer masks every data response and export of the API, so no
//! handler has to remember to.

use super::{Shared, ok};
use crate::{
    error::ApiError,
    org::Org,
    query::Meta,
    redact::{Rule, Rules},
    store::{LOCAL_USER, build::ItemRow},
};
use axum::{
    Json,
    body::Body,
    extract::{MatchedPath, Path, Request, State},
    http::{HeaderValue, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};
use std::sync::{Arc, atomic::Ordering};

const KIND: &str = "redaction";
const MAX_BODY: usize = 32 << 20;

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

/// Read an org's rules from the store into memory. A store that is not migrated has none.
pub async fn reload(s: &Shared, org: &Org) -> Result<(), ApiError> {
    let rows = match super::alerts::engine_items(s, org, KIND).await {
        Ok(r) => r,
        Err(e) if e.0 == StatusCode::SERVICE_UNAVAILABLE => vec![],
        Err(e) => return Err(e),
    };
    let rules: Vec<Rule> = rows
        .iter()
        .filter_map(|r| serde_json::from_str(&r.spec).ok())
        .collect();
    if let Ok(mut m) = s.redact.write() {
        m.insert(org.name.clone(), Arc::new(Rules(rules)));
    }
    super::llm::load_content_mode(s, &org.name).await;
    Ok(())
}

/// Reload every org; the first success makes data responses available.
pub async fn reload_all(s: &Shared) -> bool {
    let mut all = true;
    for org in s.orgs.list() {
        if let Err(e) = reload(s, org).await {
            tracing::warn!(org = %org.name, error = %e.1, "could not load redaction rules");
            all = false;
        }
    }
    if all {
        s.redact_ready.store(true, Ordering::SeqCst);
    }
    all
}

/// Keep the rules fresh across replicas.
pub fn spawn_refresh(s: Shared) {
    s.redact_ready.store(false, Ordering::SeqCst);
    tokio::spawn(async move {
        loop {
            reload_all(&s).await;
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        }
    });
}

/// The role of the caller. `template` is the matched route (`/api/v1/orgs/{org}/public/{token}/..`),
/// never the raw path, which can be spelled with percent escapes.
fn role_of(s: &Shared, template: &str) -> &'static str {
    if template.contains("/public/") || !s.cfg.admin_writes {
        "viewer"
    } else {
        "admin"
    }
}

const RULE_ROUTES: &[&str] = &[
    "/api/v1/orgs/{org}/redaction",
    "/api/v1/orgs/{org}/redaction/{id}",
];

enum Kind {
    Json,
    Ndjson,
    Csv,
}

/// The layer: mask what leaves the API for the caller's role.
pub async fn layer(State(s): State<Shared>, req: Request, next: Next) -> Response {
    let path = req.uri().path().to_string();
    // The route template, set by the router (this layer is a `route_layer`); unmatched paths
    // (the fallback) never reach it.
    let template = req
        .extensions()
        .get::<MatchedPath>()
        .map(|m| m.as_str().to_string())
        .unwrap_or_default();
    let ident = req.extensions().get::<crate::auth::Identity>().cloned();
    let res = next.run(req).await;
    let data_route =
        template.starts_with("/api/v1/orgs/{org}/") && !RULE_ROUTES.contains(&template.as_str());
    if !res.status().is_success() || !data_route {
        return res;
    }
    let ct = res
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let kind = if ct.starts_with("application/json") {
        Kind::Json
    } else if ct.starts_with("application/x-ndjson") {
        Kind::Ndjson
    } else if ct.starts_with("text/csv") {
        Kind::Csv
    } else {
        return res;
    };
    if !s.redact_ready.load(Ordering::SeqCst) {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "redaction rules are still loading; retry in a moment"})),
        )
            .into_response();
    }
    // The org as the handler resolved it: decoded and looked up, never the raw text. If that is
    // not possible the response is withheld rather than sent unmasked.
    let Some(org) = path
        .split('/')
        .nth(4)
        .and_then(super::authz::decode)
        .and_then(|o| s.orgs.resolve(&o).ok().map(|o| o.name.clone()))
    else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": "internal error"})),
        )
            .into_response();
    };
    let rules = s.redact.read().ok().and_then(|m| m.get(&org).cloned());
    // With sign-in the role is the caller's role in the org (public links and anyone below admin
    // are viewers); without it, the `--admin-writes` stand-in decides.
    let role = match &ident {
        Some(i) if i.kind != "local" => {
            if !template.contains("/public/") && i.role_in(&org) == Some(crate::auth::Role::Admin) {
                "admin"
            } else {
                "viewer"
            }
        }
        _ => role_of(&s, &template),
    };
    let rules = rules.filter(|r| !r.is_empty_for(role));
    // Prompts and completions (R-AI8): the org's mode for anyone below admin; a public link never
    // sees more than masked.
    let public = template.contains("/public/");
    let mode = super::llm::effective_mode(
        role == "admin" && !public,
        super::llm::content_mode(&s, &org),
        public,
    );
    if rules.is_none() && mode == crate::llm::Mode::Visible {
        return res;
    }
    let (mut parts, body) = res.into_parts();
    let Ok(bytes) = axum::body::to_bytes(body, MAX_BODY).await else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": "internal error"})),
        )
            .into_response();
    };
    let text = String::from_utf8_lossy(&bytes);
    let out = match kind {
        Kind::Json => match serde_json::from_str::<Value>(&text) {
            Ok(mut v) => {
                // The envelope's `meta` describes the query; only the data is masked.
                let target = match v.get_mut("data") {
                    Some(d) => d,
                    None => &mut v,
                };
                if let Some(r) = &rules {
                    r.apply_value(target, role);
                }
                crate::llm::protect(target, mode);
                v.to_string()
            }
            Err(_) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error": "internal error"})),
                )
                    .into_response();
            }
        },
        Kind::Ndjson => {
            let masked = match &rules {
                Some(r) => r.apply_ndjson(&text, role),
                None => text.to_string(),
            };
            if mode == crate::llm::Mode::Visible {
                masked
            } else {
                masked
                    .lines()
                    .map(|l| match serde_json::from_str::<Value>(l) {
                        Ok(mut v) => {
                            crate::llm::protect(&mut v, mode);
                            v.to_string()
                        }
                        Err(_) => l.to_string(),
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
                    + "\n"
            }
        }
        Kind::Csv => {
            let masked = match &rules {
                Some(r) => r.apply_csv(&text, role),
                None => text.to_string(),
            };
            crate::llm::protect_csv(&masked, mode)
        }
    };
    parts.headers.remove(header::CONTENT_LENGTH);
    parts
        .headers
        .insert("x-grepui-redacted", HeaderValue::from_static("1"));
    Response::from_parts(parts, Body::from(out))
}

// ---- rules -------------------------------------------------------------------------------

fn parse(v: &Value) -> Result<Rule, ApiError> {
    serde_json::from_value::<Rule>(v.clone())
        .map_err(|e| ApiError::bad(format!("invalid rule: {e}")))?
        .canonical()
}

fn json_of(r: &ItemRow) -> Value {
    let mut v: Value = serde_json::from_str(&r.spec).unwrap_or(Value::Null);
    v["id"] = json!(r.item_id);
    v
}

pub async fn list(State(s): State<Shared>, Path(org): Path<String>) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let rows = super::alerts::engine_items(&s, org, KIND).await?;
    Ok(ok(
        json!({"rules": rows.iter().map(json_of).collect::<Vec<_>>(), "role": role_of(&s, ""), "admin_writes": s.cfg.admin_writes}),
        meta("redaction_list"),
    ))
}

async fn write(
    s: &Shared,
    org: &Org,
    id: &str,
    rule: &Rule,
    created: Option<i64>,
    deleted: bool,
) -> Result<Meta, ApiError> {
    s.admin_enabled()?;
    let now = s.now_ms();
    tracing::info!(target: "audit", org = %org.name, actor = LOCAL_USER, action = "redaction_rule", id, field = %rule.field, deleted, "admin write");
    let row = ItemRow {
        item_id: id.into(),
        name: rule.field.clone(),
        spec: serde_json::to_string(rule).map_err(|_| ApiError::bad("invalid rule"))?,
        owner: LOCAL_USER.into(),
        created_ms: created.unwrap_or(now),
        updated_ms: now,
        deleted,
    };
    let m = super::alerts::engine_save(s, org, KIND, &row).await?;
    reload(s, org).await?;
    Ok(m)
}

pub async fn create(
    State(s): State<Shared>,
    Path(org): Path<String>,
    Json(b): Json<Value>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    s.admin_enabled()?;
    let rule = parse(&b)?;
    if super::alerts::engine_items(&s, org, KIND).await?.len() >= crate::redact::MAX_RULES {
        return Err(ApiError::bad(format!(
            "at most {} rules",
            crate::redact::MAX_RULES
        )));
    }
    let id = format!("{:08x}", rand::random::<u32>());
    let m = write(&s, org, &id, &rule, None, false).await?;
    let (mut parts, body) = ok(json!({"id": id, "rule": rule}), m).into_parts();
    parts.status = StatusCode::CREATED;
    Ok(Response::from_parts(parts, body))
}

pub async fn update(
    State(s): State<Shared>,
    Path((org, id)): Path<(String, String)>,
    Json(b): Json<Value>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    s.admin_enabled()?;
    let old = super::alerts::engine_item(&s, org, KIND, &id)
        .await?
        .ok_or_else(|| ApiError::not_found("no such rule"))?;
    let rule = parse(&b)?;
    let m = write(&s, org, &old.item_id, &rule, Some(old.created_ms), false).await?;
    Ok(ok(json!({"id": id, "rule": rule}), m))
}

pub async fn delete(
    State(s): State<Shared>,
    Path((org, id)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    s.admin_enabled()?;
    let old = super::alerts::engine_item(&s, org, KIND, &id)
        .await?
        .ok_or_else(|| ApiError::not_found("no such rule"))?;
    let rule: Rule = serde_json::from_str(&old.spec).map_err(|_| ApiError::bad("invalid rule"))?;
    let m = write(&s, org, &old.item_id, &rule, Some(old.created_ms), true).await?;
    Ok(ok(json!({"deleted": true, "id": id}), m))
}
