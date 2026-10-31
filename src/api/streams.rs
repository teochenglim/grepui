//! Streams: the tables of an org's database (R-T1).

use super::{Shared, ok};
use crate::{
    error::ApiError,
    org::Org,
    query::{Window, build, ident},
};
use axum::{
    extract::{Path, Query, State},
    response::Response,
};
use serde::Deserialize;
use serde_json::{Value, json};

/// What a table holds, from its name and GreptimeDB's creation options.
pub fn classify(org: &Org, name: &str, create_options: &str) -> &'static str {
    let aux = |base: &str| {
        name.strip_prefix(base)
            .is_some_and(|rest| rest.starts_with('_'))
    };
    if name == org.traces_table || create_options.contains("signal_type=trace") {
        "traces"
    } else if name == org.logs_table || create_options.contains("signal_type=log") {
        "logs"
    } else if aux(&org.traces_table) {
        "traces_aux"
    } else if aux(&org.logs_table) {
        "logs_aux"
    } else {
        "table"
    }
}

pub async fn list(State(s): State<Shared>, Path(org): Path<String>) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let (rows, meta) = s.run(build::streams_list(&org.database)).await?;
    let data: Vec<Value> = rows
        .iter()
        .map(|r| {
            let name = r["table_name"].as_str().unwrap_or_default();
            let opts = r["create_options"].as_str().unwrap_or_default();
            json!({
                "name": name,
                "kind": classify(org, name, opts),
                "table_type": r["table_type"],
                "engine": r["engine"],
                "rows_estimate": r["table_rows"],
            })
        })
        .collect();
    Ok(ok(data, meta))
}

/// Columns of one table; `Err(404)` when it does not exist.
async fn columns(
    s: &Shared,
    org: &Org,
    name: &str,
) -> Result<(Vec<Value>, crate::query::Meta), ApiError> {
    ident(name).map_err(|_| ApiError::bad("invalid stream name"))?;
    let (rows, meta) = s.run(build::stream_schema(&org.database, name)).await?;
    if rows.is_empty() {
        return Err(ApiError::not_found("unknown stream"));
    }
    Ok((rows, meta))
}

fn role(semantic: &str) -> &'static str {
    match semantic {
        "TAG" => "tag",
        "TIMESTAMP" => "time_index",
        _ => "field",
    }
}

fn time_index(cols: &[Value]) -> Result<String, ApiError> {
    cols.iter()
        .find(|c| c["semantic_type"] == "TIMESTAMP")
        .and_then(|c| c["column_name"].as_str())
        .map(str::to_string)
        .ok_or_else(|| ApiError::bad("the stream has no time index"))
}

pub async fn schema(
    State(s): State<Shared>,
    Path((org, name)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let (cols, meta) = columns(&s, org, &name).await?;
    let data = json!({
        "name": name,
        "time_index": time_index(&cols).ok(),
        "columns": cols.iter().map(|c| json!({
            "name": c["column_name"], "type": c["data_type"], "role": role(c["semantic_type"].as_str().unwrap_or_default()),
        })).collect::<Vec<_>>(),
    });
    Ok(ok(data, meta))
}

pub async fn stats(
    State(s): State<Shared>,
    Path((org, name)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let (cols, _) = columns(&s, org, &name).await?;
    let ts = time_index(&cols)?;
    let q = build::stream_stats(&org.database, &name, &ts)
        .map_err(|_| ApiError::bad("unsupported stream or column name"))?;
    let (rows, meta) = s.run(q).await?;
    let r = rows.into_iter().next().unwrap_or(Value::Null);
    Ok(ok(
        json!({"rows": r["total"], "oldest": r["oldest"], "newest": r["newest"]}),
        meta,
    ))
}

#[derive(Deserialize)]
pub struct WindowParams {
    from: Option<i64>,
    to: Option<i64>,
    limit: Option<u32>,
}

pub async fn sample(
    State(s): State<Shared>,
    Path((org, name)): Path<(String, String)>,
    Query(p): Query<WindowParams>,
) -> Result<Response, ApiError> {
    let w = Window::parse(p.from, p.to, p.limit)?;
    let org = s.org(&org)?;
    let (cols, _) = columns(&s, org, &name).await?;
    let ts = time_index(&cols)?;
    let q = build::stream_sample(&org.database, &name, &ts, w)
        .map_err(|_| ApiError::bad("unsupported stream or column name"))?;
    let (rows, meta) = s.run(q).await?;
    Ok(ok(rows, meta))
}

/// What `SHOW CREATE TABLE` says about a table (R-S4): indexes, partitioning, TTL, keys.
#[derive(Debug, Default, PartialEq, serde::Serialize)]
pub struct DdlInfo {
    pub time_index: Option<String>,
    pub primary_key: Vec<String>,
    /// `(column, kind)` with kind one of `fulltext`, `skipping`, `inverted`.
    pub indexes: Vec<(String, String)>,
    pub partition_columns: Vec<String>,
    pub ttl: Option<String>,
    pub append_mode: bool,
}

/// Identifiers inside a column list like `(`a`, `b`)`; works for backtick or double-quote quoting.
fn quoted_names(s: &str) -> Vec<String> {
    let mut out = vec![];
    let mut cur: Option<(char, String)> = None;
    for c in s.chars() {
        match (&mut cur, c) {
            (None, '`' | '"') => cur = Some((c, String::new())),
            (Some((q, name)), c) if c == *q => {
                out.push(std::mem::take(name));
                cur = None;
            }
            (Some((_, name)), c) => name.push(c),
            _ => {}
        }
    }
    out
}

pub fn parse_ddl(ddl: &str) -> DdlInfo {
    let mut info = DdlInfo::default();
    for line in ddl.lines() {
        let l = line.trim();
        let upper = l.to_ascii_uppercase();
        if let Some(rest) = upper.strip_prefix("TIME INDEX") {
            info.time_index = quoted_names(&l[l.len() - rest.len()..]).into_iter().next();
        } else if let Some(rest) = upper.strip_prefix("PRIMARY KEY") {
            info.primary_key = quoted_names(&l[l.len() - rest.len()..]);
        } else if upper.starts_with("PARTITION ON COLUMNS") {
            info.partition_columns = quoted_names(l);
        } else if l.starts_with(['`', '"']) {
            // A column line: `col` TYPE ... [FULLTEXT INDEX|SKIPPING INDEX|INVERTED INDEX] ...
            let col = quoted_names(l).into_iter().next().unwrap_or_default();
            for (kw, kind) in [
                ("FULLTEXT INDEX", "fulltext"),
                ("SKIPPING INDEX", "skipping"),
                ("INVERTED INDEX", "inverted"),
            ] {
                if upper.contains(kw) {
                    info.indexes.push((col.clone(), kind.to_string()));
                }
            }
        }
        let lower = l.to_ascii_lowercase();
        if let Some(i) = lower.find("ttl") {
            let after = &l[i + 3..];
            if let Some(v) = after
                .split('\'')
                .nth(1)
                .filter(|_| after.trim_start().starts_with('='))
            {
                info.ttl = Some(v.to_string());
            }
        }
        if lower.contains("append_mode") && lower.contains("'true'") {
            info.append_mode = true;
        }
    }
    info
}

/// Size, row estimate, engine and options of one table, from `information_schema`.
pub async fn info(
    State(s): State<Shared>,
    Path((org, name)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    ident(&name).map_err(|_| ApiError::bad("invalid stream name"))?;
    let (rows, meta) = s.run(build::stream_info(&org.database, &name)).await?;
    let r = rows
        .first()
        .ok_or_else(|| ApiError::not_found("unknown stream"))?;
    Ok(ok(
        json!({
            "name": name, "kind": classify(org, &name, r["create_options"].as_str().unwrap_or_default()),
            "engine": r["engine"], "rows_estimate": r["table_rows"],
            "data_bytes": r["data_length"], "index_bytes": r["index_length"], "options": r["create_options"],
        }),
        meta,
    ))
}

/// Indexes, partitioning, TTL and keys, parsed from `SHOW CREATE TABLE`.
pub async fn ddl(
    State(s): State<Shared>,
    Path((org, name)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    ident(&name).map_err(|_| ApiError::bad("invalid stream name"))?;
    let q = build::show_create(&org.database, &name)
        .map_err(|_| ApiError::bad("invalid stream name"))?;
    let (rows, mut meta) = s.run(q).await.map_err(|e| {
        if e.0 == axum::http::StatusCode::BAD_GATEWAY {
            ApiError::not_found("unknown stream")
        } else {
            e
        }
    })?;
    let text = rows
        .first()
        .and_then(|r| r["Create Table"].as_str())
        .ok_or_else(|| ApiError::not_found("unknown stream"))?;
    meta.note = None;
    Ok(ok(
        json!({"name": name, "parsed": parse_ddl(text), "ddl": text}),
        meta,
    ))
}
