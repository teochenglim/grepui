//! Data health (R-I2): is data still arriving, how fast, and which services went quiet.

use super::{Shared, ok};
use crate::{
    error::ApiError,
    query::{Window, build},
};
use axum::{
    extract::{Path, Query, State},
    response::Response,
};
use serde::Deserialize;
use serde_json::{Value, json};

pub const QUIET_DEFAULT_SECS: i64 = 300;

#[derive(Deserialize)]
pub struct Params {
    from: Option<i64>,
    to: Option<i64>,
    stream: Option<String>,
    quiet_secs: Option<i64>,
    lookback_hours: Option<i64>,
}

/// `ok`, `quiet` (data exists but the newest row is older than the threshold) or `empty`.
pub fn state_of(
    newest_ms: Option<i64>,
    now_ms: i64,
    quiet_secs: i64,
) -> (&'static str, Option<i64>) {
    match newest_ms {
        None => ("empty", None),
        Some(n) => {
            let age = ((now_ms - n) / 1000).max(0);
            (if age > quiet_secs { "quiet" } else { "ok" }, Some(age))
        }
    }
}

/// Newest row of the logs and traces tables within a bounded look-back (default 24 h).
pub async fn freshness(
    State(s): State<Shared>,
    Path(org): Path<String>,
    Query(p): Query<Params>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let quiet = p.quiet_secs.unwrap_or(QUIET_DEFAULT_SECS).clamp(10, 86_400);
    let hours = p.lookback_hours.unwrap_or(24).clamp(1, 744);
    let now = s.now_ms();
    let q = build::ingest_freshness(
        &org.database,
        &org.traces_table,
        &org.logs_table,
        now - hours * 3_600_000,
    )
    .map_err(|_| ApiError::bad("unsupported table name"))?;
    let (rows, mut meta) = s.run(q).await?;
    let data: Vec<Value> = rows
        .iter()
        .map(|r| {
            let (state, age) = state_of(r["newest"].as_i64(), now, quiet);
            json!({"stream": r["stream"], "newest_ms": r["newest"], "age_secs": age, "state": state})
        })
        .collect();
    if data.iter().any(|d| d["state"] == "quiet") {
        meta.note = Some(format!(
            "a stream has received nothing for more than {quiet} s"
        ));
    }
    Ok(ok(
        json!({"now_ms": now, "quiet_secs": quiet, "lookback_hours": hours, "streams": data}),
        meta,
    ))
}

/// Rows per bucket for one stream (`logs` or `traces`).
pub async fn rate(
    State(s): State<Shared>,
    Path(org): Path<String>,
    Query(p): Query<Params>,
) -> Result<Response, ApiError> {
    let w = Window::parse(p.from, p.to, None)?;
    let org = s.org(&org)?;
    let table = match p.stream.as_deref().unwrap_or("logs") {
        "logs" => &org.logs_table,
        "traces" => &org.traces_table,
        _ => return Err(ApiError::bad("stream must be logs or traces")),
    };
    let q = build::ingest_rate(&org.database, table, w)
        .map_err(|_| ApiError::bad("unsupported table name"))?;
    let (rows, meta) = s.run(q).await?;
    let (bucket_ms, _) = crate::query::logs::bucket_for(w.to_ms - w.from_ms);
    let buckets: Vec<Value> = rows
        .iter()
        .map(|r| json!({"t": r["bucket_ms"], "count": r["n"]}))
        .collect();
    let total: i64 = rows.iter().filter_map(|r| r["n"].as_i64()).sum();
    let per_minute = total as f64 * 60_000.0 / (w.to_ms - w.from_ms) as f64;
    Ok(ok(
        json!({"bucket_ms": bucket_ms, "buckets": buckets, "total": total, "per_minute": per_minute}),
        meta,
    ))
}

/// Last time each service sent a span, within the window.
pub async fn services(
    State(s): State<Shared>,
    Path(org): Path<String>,
    Query(p): Query<Params>,
) -> Result<Response, ApiError> {
    let w = Window::parse(p.from, p.to, None)?;
    let org = s.org(&org)?;
    let q = build::ingest_services(&org.database, &org.traces_table, w)
        .map_err(|_| ApiError::bad("unsupported table name"))?;
    let (rows, meta) = s.run(q).await?;
    let now = s.now_ms();
    let quiet = p.quiet_secs.unwrap_or(QUIET_DEFAULT_SECS).clamp(10, 86_400);
    let data: Vec<Value> = rows
        .iter()
        .map(|r| {
            let (state, age) = state_of(r["last_seen"].as_i64(), now, quiet);
            json!({"service": r["service_name"], "last_seen_ms": r["last_seen"], "age_secs": age, "spans": r["spans"], "state": state})
        })
        .collect();
    Ok(ok(data, meta))
}
