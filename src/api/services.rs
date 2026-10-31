//! Services (R-V1 – R-V4): RED list, per-service series and operations, dependencies and the
//! service map, and the catalog with its annotations.

use super::{Shared, ok};
use crate::{
    error::ApiError,
    layout::{self, EdgeIn},
    query::{Window, logs::bucket_for, services},
    store::{
        LOCAL_USER,
        build::{self, ServiceNote},
    },
};
use axum::{
    Json,
    extract::{Path, Query, State},
    response::Response,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;

#[derive(Deserialize)]
pub struct Params {
    from: Option<i64>,
    to: Option<i64>,
    self_loops: Option<bool>,
}

fn bad_table<T>(r: anyhow::Result<T>) -> Result<T, ApiError> {
    r.map_err(|_| ApiError::bad("unsupported traces table name"))
}

fn num(v: &Value) -> f64 {
    v.as_f64().unwrap_or(0.0)
}

/// Add the derived columns: rate per second and error percentage.
fn with_rates(row: &Value, window_ms: i64) -> Value {
    let mut r = row.clone();
    let requests = num(&row["requests"]);
    let secs = (window_ms as f64 / 1000.0).max(1.0);
    r["rate_per_s"] = json!(requests / secs);
    r["error_pct"] = json!(if requests > 0.0 {
        100.0 * num(&row["errors"]) / requests
    } else {
        0.0
    });
    r
}

pub async fn list(
    State(s): State<Shared>,
    Path(org): Path<String>,
    Query(p): Query<Params>,
) -> Result<Response, ApiError> {
    let w = Window::parse(p.from, p.to, None)?;
    let org = s.org(&org)?;
    let (rows, meta) = s
        .run(bad_table(services::services(
            &org.database,
            &org.traces_table,
            None,
            w,
        ))?)
        .await?;
    let data: Vec<Value> = rows
        .iter()
        .map(|r| with_rates(r, w.to_ms - w.from_ms))
        .collect();
    Ok(ok(data, meta))
}

/// One service: its summary, series and operations in one response (the service page).
pub async fn detail(
    State(s): State<Shared>,
    Path((org, service)): Path<(String, String)>,
    Query(p): Query<Params>,
) -> Result<Response, ApiError> {
    let service = services::service_name(&service)?;
    let w = Window::parse(p.from, p.to, None)?;
    let org = s.org(&org)?;
    let (summary, meta) = s
        .run(bad_table(services::services(
            &org.database,
            &org.traces_table,
            Some(&service),
            w,
        ))?)
        .await?;
    let row = summary.first().ok_or_else(|| {
        ApiError::not_found("no spans for that service in this time range; widen the range")
    })?;
    let (series, _) = s
        .run(bad_table(services::series(
            &org.database,
            &org.traces_table,
            &service,
            w,
        ))?)
        .await?;
    let (ops, _) = s
        .run(bad_table(services::operations(
            &org.database,
            &org.traces_table,
            &service,
            w,
        ))?)
        .await?;
    let (bucket_ms, _) = bucket_for(w.to_ms - w.from_ms);
    // The notes are optional: a store that is not migrated yet must not hide the service.
    let note = note_json(
        s.read_store(
            build::service_note_get(s.meta_db(), &org.name, &service)
                .map_err(|_| ApiError::bad("invalid request"))?,
        )
        .await
        .ok()
        .and_then(|(r, _)| r.into_iter().next()),
    );
    Ok(ok(
        json!({
            "service": service, "summary": with_rates(row, w.to_ms - w.from_ms),
            "bucket_ms": bucket_ms, "series": series, "operations": ops, "annotation": note,
        }),
        meta,
    ))
}

fn note_json(row: Option<Value>) -> Value {
    match row {
        Some(r) => json!({
            "description": r["description"], "owner": r["owner"], "tier": r["tier"], "runbook": r["runbook"],
            "updated_by": r["updated_by"], "updated_ms": r["updated_ms"],
        }),
        None => Value::Null,
    }
}

pub async fn dependencies(
    State(s): State<Shared>,
    Path(org): Path<String>,
    Query(p): Query<Params>,
) -> Result<Response, ApiError> {
    let w = Window::parse(p.from, p.to, None)?;
    let self_loops = p.self_loops.unwrap_or(false);
    let org = s.org(&org)?;
    let (rows, meta) = s
        .run(bad_table(services::dependencies(
            &org.database,
            &org.traces_table,
            self_loops,
            w,
        ))?)
        .await?;
    let edges: Vec<EdgeIn> = rows
        .iter()
        .filter_map(|r| {
            Some(EdgeIn {
                from: r["caller"].as_str()?.to_string(),
                to: r["callee"].as_str()?.to_string(),
                calls: r["calls"].as_i64().unwrap_or(0),
                errors: r["errors"].as_i64().unwrap_or(0),
                avg_us: num(&r["avg_us"]),
            })
        })
        .collect();
    Ok(ok(layout::graph(&edges, self_loops), meta))
}

/// Every service seen in the window, plus services that only have a note, with RED and the note.
pub async fn catalog(
    State(s): State<Shared>,
    Path(org): Path<String>,
    Query(p): Query<Params>,
) -> Result<Response, ApiError> {
    let w = Window::parse(p.from, p.to, None)?;
    let org = s.org(&org)?;
    let (rows, meta) = s
        .run(bad_table(services::services(
            &org.database,
            &org.traces_table,
            None,
            w,
        ))?)
        .await?;
    let notes = s
        .read_store(
            build::service_notes(s.meta_db(), &org.name)
                .map_err(|_| ApiError::bad("invalid request"))?,
        )
        .await
        .map(|(r, _)| r)
        .unwrap_or_default();
    let mut by_name: BTreeMap<String, Value> = notes
        .iter()
        .filter_map(|n| Some((n["service"].as_str()?.to_string(), n.clone())))
        .collect();
    let mut data: Vec<Value> = rows
        .iter()
        .map(|r| {
            let mut v = with_rates(r, w.to_ms - w.from_ms);
            let note = r["service_name"].as_str().and_then(|n| by_name.remove(n));
            v["annotation"] = note_json(note);
            v["seen"] = json!(true);
            v
        })
        .collect();
    // Noted but not seen in this window.
    for (name, n) in by_name {
        data.push(json!({
            "service_name": name, "seen": false, "requests": 0, "errors": 0, "spans": 0,
            "annotation": note_json(Some(n)),
        }));
    }
    Ok(ok(data, meta))
}

#[derive(Deserialize)]
pub struct NoteBody {
    description: Option<String>,
    owner: Option<String>,
    tier: Option<String>,
    runbook: Option<String>,
}

const TIERS: &[&str] = &["", "critical", "high", "medium", "low"];

fn text(v: &Option<String>, what: &str, max: usize) -> Result<String, ApiError> {
    let t = v.as_deref().unwrap_or_default().trim();
    if t.chars().count() > max || t.chars().any(|c| c.is_control() && c != '\n') {
        return Err(ApiError::bad(format!(
            "{what} must be at most {max} characters, without control characters"
        )));
    }
    Ok(t.to_string())
}

fn runbook(v: &Option<String>) -> Result<String, ApiError> {
    let t = text(v, "runbook", 500)?;
    if !t.is_empty()
        && !((t.starts_with("https://") || t.starts_with("http://"))
            && !t.chars().any(char::is_whitespace))
    {
        return Err(ApiError::bad("runbook must be an http(s) link"));
    }
    Ok(t)
}

pub async fn annotate(
    State(s): State<Shared>,
    Path((org, service)): Path<(String, String)>,
    Json(b): Json<NoteBody>,
) -> Result<Response, ApiError> {
    let service = services::service_name(&service)?;
    let org = s.org(&org)?;
    let tier = text(&b.tier, "tier", 20)?;
    if !TIERS.contains(&tier.as_str()) {
        return Err(ApiError::bad("tier must be critical, high, medium or low"));
    }
    let note = ServiceNote {
        service,
        description: text(&b.description, "description", 500)?,
        owner: text(&b.owner, "owner", 100)?,
        tier,
        runbook: runbook(&b.runbook)?,
        updated_by: LOCAL_USER.into(),
        updated_ms: s.now_ms(),
        deleted: false,
    };
    let meta = s
        .write(
            build::service_note_put(s.meta_db(), &org.name, &note)
                .map_err(|_| ApiError::bad("invalid request"))?,
        )
        .await?;
    Ok(ok(
        json!({"service": note.service, "description": note.description, "owner": note.owner, "tier": note.tier,
               "runbook": note.runbook, "updated_by": note.updated_by, "updated_ms": note.updated_ms}),
        meta,
    ))
}

pub async fn clear_annotation(
    State(s): State<Shared>,
    Path((org, service)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let service = services::service_name(&service)?;
    let org = s.org(&org)?;
    let note = ServiceNote {
        service: service.clone(),
        updated_by: LOCAL_USER.into(),
        updated_ms: s.now_ms(),
        deleted: true,
        ..ServiceNote::default()
    };
    let meta = s
        .write(
            build::service_note_put(s.meta_db(), &org.name, &note)
                .map_err(|_| ApiError::bad("invalid request"))?,
        )
        .await?;
    Ok(ok(json!({"deleted": true, "service": service}), meta))
}
