//! Logs (R-L1 – R-L4, R-L6, R-L7): search, histogram, fields, record, search around, export.

use super::{Shared, ok};
use crate::{
    error::ApiError,
    org::Org,
    query::{
        MAX_LIMIT, Window, build,
        filter::{FtMode, LogFilter},
        ident, logs,
    },
};
use axum::{
    extract::{Path, Query, State},
    http::{HeaderValue, header},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::{Duration, Instant};

const FT_CACHE_TTL: Duration = Duration::from_secs(60);

#[derive(Deserialize)]
pub struct LogParams {
    from: Option<i64>,
    to: Option<i64>,
    limit: Option<u32>,
    offset: Option<u32>,
    q: Option<String>,
    level: Option<String>,
    format: Option<String>,
    /// `all` or `a,b`: read from these sources and merge by time (v0.12.0).
    sources: Option<String>,
}

/// Does `body` have a fulltext index? Asked of the database, cached briefly per org.
pub(crate) async fn ft_mode(s: &Shared, org: &Org) -> FtMode {
    let key = format!("{}/{}", org.name, org.logs_table);
    if let Some((at, has)) = s.ft_cache.lock().ok().and_then(|c| c.get(&key).copied())
        && at.elapsed() < FT_CACHE_TTL
    {
        return if has { FtMode::Term } else { FtMode::Like };
    }
    let Ok(q) = logs::ddl(&org.database, &org.logs_table) else {
        return FtMode::Like;
    };
    let has = match s.run(q).await {
        Ok((rows, _)) => rows
            .iter()
            .filter_map(|r| r["Create Table"].as_str())
            .any(logs::has_fulltext),
        Err(_) => return FtMode::Like,
    };
    if let Ok(mut c) = s.ft_cache.lock() {
        c.insert(key, (Instant::now(), has));
    }
    if has { FtMode::Term } else { FtMode::Like }
}

const LIKE_NOTE: &str =
    "free text was matched with LIKE because the body column has no fulltext index";

/// Build the filter; free text needs to know the match mode. Returns an optional note for `meta`.
async fn filter(
    s: &Shared,
    org: &Org,
    p: &LogParams,
) -> Result<(LogFilter, Option<String>), ApiError> {
    let probe = LogFilter::new(p.q.as_deref(), p.level.as_deref(), FtMode::Term)?;
    if probe.parsed.terms.is_empty() {
        return Ok((probe, None));
    }
    let ft = ft_mode(s, org).await;
    let note = (ft == FtMode::Like).then(|| LIKE_NOTE.to_string());
    Ok((LogFilter { ft, ..probe }, note))
}

fn bad_table<T>(r: anyhow::Result<T>) -> Result<T, ApiError> {
    r.map_err(|_| ApiError::bad("unsupported logs table name"))
}

pub async fn search(
    State(s): State<Shared>,
    axum::Extension(id): axum::Extension<crate::auth::Identity>,
    Path(org): Path<String>,
    Query(p): Query<LogParams>,
) -> Result<Response, ApiError> {
    let w = Window::parse(p.from, p.to, p.limit)?;
    let org = s.org(&org)?;
    let (f, note) = filter(&s, org, &p).await?;
    let q = bad_table(logs::search(
        &org.database,
        &org.logs_table,
        &f,
        w,
        p.offset.unwrap_or(0),
    ))?;
    if let Some(spec) = p.sources.as_deref() {
        if p.offset.unwrap_or(0) > 0 {
            return Err(ApiError::bad("offset cannot be combined with sources"));
        }
        return super::workload::fan_out(
            &s,
            &id,
            org,
            spec,
            q,
            ("timestamp", w.limit as usize),
            note,
        )
        .await;
    }
    let (rows, mut meta) = s.run(q).await?;
    meta.note = note;
    Ok(ok(rows, meta))
}

pub async fn histogram(
    State(s): State<Shared>,
    Path(org): Path<String>,
    Query(p): Query<LogParams>,
) -> Result<Response, ApiError> {
    let w = Window::parse(p.from, p.to, None)?;
    let org = s.org(&org)?;
    let (f, note) = filter(&s, org, &p).await?;
    let q = bad_table(logs::histogram(&org.database, &org.logs_table, &f, w))?;
    let (rows, mut meta) = s.run(q).await?;
    meta.note = note;
    let buckets: Vec<Value> = rows
        .iter()
        .map(|r| json!({"t": r["bucket_ms"], "count": r["n"]}))
        .collect();
    let total: i64 = rows.iter().filter_map(|r| r["n"].as_i64()).sum();
    let (bucket_ms, _) = logs::bucket_for(w.to_ms - w.from_ms);
    Ok(ok(
        json!({"bucket_ms": bucket_ms, "buckets": buckets, "total": total}),
        meta,
    ))
}

/// Columns of the logs table with type, role and whether quick mode shows them.
pub async fn fields(
    State(s): State<Shared>,
    Path(org): Path<String>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let (rows, meta) = s
        .run(build::stream_schema(&org.database, &org.logs_table))
        .await?;
    if rows.is_empty() {
        return Err(ApiError::not_found(
            "the logs table does not exist yet; see Connect",
        ));
    }
    let data: Vec<Value> = rows
        .iter()
        .map(|c| {
            let name = c["column_name"].as_str().unwrap_or_default();
            let ty = c["data_type"].as_str().unwrap_or_default();
            json!({
                "name": name,
                "type": ty,
                "role": match c["semantic_type"].as_str() { Some("TAG") => "tag", Some("TIMESTAMP") => "time_index", _ => "field" },
                "quick": logs::QUICK_FIELDS.contains(&name),
                "attributes": ty == "json",
            })
        })
        .collect();
    Ok(ok(data, meta))
}

pub async fn top_values(
    State(s): State<Shared>,
    Path((org, field)): Path<(String, String)>,
    Query(p): Query<LogParams>,
) -> Result<Response, ApiError> {
    let col = logs::top_value_column(&field)
        .ok_or_else(|| ApiError::bad("top values are not available for this field"))?;
    let w = Window::parse(p.from, p.to, None)?;
    let org = s.org(&org)?;
    let (f, note) = filter(&s, org, &p).await?;
    let q = bad_table(logs::top_values(
        &org.database,
        &org.logs_table,
        &col,
        &f,
        w,
        p.limit.unwrap_or(10),
    ))?;
    let (rows, mut meta) = s.run(q).await?;
    meta.note = note;
    let data: Vec<Value> = rows
        .iter()
        .map(|r| json!({"value": r["value"], "count": r["n"]}))
        .collect();
    Ok(ok(data, meta))
}

#[derive(Deserialize)]
pub struct KeyParams {
    ts_ns: Option<String>,
    n: Option<u32>,
}

/// A nanosecond timestamp as a string of digits (it does not fit a JS number).
fn ts_ns(raw: &Option<String>) -> Result<i64, ApiError> {
    let raw = raw
        .as_deref()
        .ok_or_else(|| ApiError::bad("ts_ns is required"))?;
    if raw.is_empty() || raw.len() > 19 || !raw.bytes().all(|b| b.is_ascii_digit()) {
        return Err(ApiError::bad("ts_ns must be a nanosecond timestamp"));
    }
    raw.parse()
        .map_err(|_| ApiError::bad("ts_ns is out of range"))
}

pub async fn record(
    State(s): State<Shared>,
    Path(org): Path<String>,
    Query(p): Query<KeyParams>,
) -> Result<Response, ApiError> {
    let ns = ts_ns(&p.ts_ns)?;
    let org = s.org(&org)?;
    let q = bad_table(logs::record(&org.database, &org.logs_table, ns))?;
    let (rows, meta) = s.run(q).await?;
    if rows.is_empty() {
        return Err(ApiError::not_found("no log record with that timestamp"));
    }
    Ok(ok(rows, meta))
}

pub async fn around(
    State(s): State<Shared>,
    Path(org): Path<String>,
    Query(p): Query<KeyParams>,
) -> Result<Response, ApiError> {
    let ns = ts_ns(&p.ts_ns)?;
    let n = p.n.unwrap_or(20).clamp(2, logs::AROUND_MAX);
    let org = s.org(&org)?;
    let q = bad_table(logs::around(
        &org.database,
        &org.logs_table,
        ns,
        n / 2,
        n - n / 2,
    ))?;
    let (rows, meta) = s.run(q).await?;
    Ok(ok(json!({"pivot_ns": ns.to_string(), "rows": rows}), meta))
}

const EXPORT_COLUMNS: &[&str] = &[
    "timestamp",
    "ts_ns",
    "severity_text",
    "severity_number",
    "body",
    "trace_id",
    "span_id",
    "scope_name",
    "log_attributes",
    "resource_attributes",
];

/// One CSV cell. Quotes when needed, and defuses spreadsheet formulas in text cells.
fn csv_cell(v: &Value, col: &str) -> String {
    let text = match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        Value::Number(n) if col == "timestamp" => {
            chrono::DateTime::from_timestamp_millis(n.as_i64().unwrap_or(0))
                .map(|d| d.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
                .unwrap_or_default()
        }
        other => other.to_string(),
    };
    let text =
        if matches!(v, Value::String(_)) && text.starts_with(['=', '+', '-', '@', '\t', '\r']) {
            format!("'{text}")
        } else {
            text
        };
    if text.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", text.replace('"', "\"\""))
    } else {
        text
    }
}

pub fn export_csv(rows: &[Value]) -> String {
    let mut out = EXPORT_COLUMNS.join(",");
    out.push('\n');
    for r in rows {
        let line: Vec<String> = EXPORT_COLUMNS.iter().map(|c| csv_cell(&r[*c], c)).collect();
        out.push_str(&line.join(","));
        out.push('\n');
    }
    out
}

pub fn export_ndjson(rows: &[Value]) -> String {
    rows.iter().map(|r| r.to_string() + "\n").collect()
}

pub async fn export(
    State(s): State<Shared>,
    Path(org): Path<String>,
    Query(p): Query<LogParams>,
) -> Result<Response, ApiError> {
    let csv = match p.format.as_deref().unwrap_or("csv") {
        "csv" => true,
        "ndjson" => false,
        _ => return Err(ApiError::bad("format must be csv or ndjson")),
    };
    let w = Window::parse_max(
        p.from,
        p.to,
        Some(p.limit.unwrap_or(logs::EXPORT_MAX)),
        logs::EXPORT_MAX,
    )?;
    let org = s.org(&org)?;
    let (f, _) = filter(&s, org, &p).await?;
    let q = bad_table(logs::search(&org.database, &org.logs_table, &f, w, 0))?;
    let (rows, meta) = s.run(q).await?;
    let (body, ctype, file) = if csv {
        (export_csv(&rows), "text/csv; charset=utf-8", "logs.csv")
    } else {
        (export_ndjson(&rows), "application/x-ndjson", "logs.ndjson")
    };
    let mut r = body.into_response();
    let h = r.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(ctype));
    if let Ok(v) = HeaderValue::from_str(&format!("attachment; filename=\"{file}\"")) {
        h.insert(header::CONTENT_DISPOSITION, v);
    }
    if let Ok(v) = HeaderValue::from_str(&meta.rows.to_string()) {
        h.insert("x-grepui-rows", v);
    }
    h.insert("x-grepui-query", HeaderValue::from_static("logs_search"));
    let _ = (MAX_LIMIT, ident("x"));
    Ok(r)
}
