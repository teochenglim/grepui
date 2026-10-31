//! Enrichment tables API (R-N3): upload a CSV as `enrich_<name>`, list, preview, drop, and build a
//! join query for SQL mode. Creating and dropping tables is an admin write (`--admin-writes`).

use super::{Shared, ok};
use crate::{
    enrich::{self as e},
    error::ApiError,
    query::{self, Meta, run},
};
use axum::{
    extract::{DefaultBodyLimit, Path, Query, State},
    http::StatusCode,
    response::Response,
};
use serde::Deserialize;
use serde_json::{Value, json};

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

/// The body limit of the upload route (the rest of the API keeps 64 KB).
pub fn upload_limit() -> DefaultBodyLimit {
    DefaultBodyLimit::max(e::MAX_BYTES + 1024)
}

async fn existing(s: &Shared, db: &str) -> Result<Vec<String>, ApiError> {
    let (rows, _) = run(s.reader.as_ref(), e::tables(db)).await?;
    Ok(rows
        .iter()
        .filter_map(|r| r["table_name"].as_str())
        .filter(|t| t.starts_with(e::PREFIX))
        .map(String::from)
        .collect())
}

pub async fn list(State(s): State<Shared>, Path(org): Path<String>) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let mut out = vec![];
    for t in existing(&s, &org.database).await? {
        let n = match e::count(&org.database, &t) {
            Ok(q) => run(s.reader.as_ref(), q)
                .await
                .ok()
                .and_then(|(r, _)| r.first().and_then(|x| x["n"].as_i64())),
            Err(_) => None,
        };
        out.push(json!({"name": &t[e::PREFIX.len()..], "table": t, "rows": n}));
    }
    Ok(ok(
        json!({"tables": out, "admin_writes": s.cfg.admin_writes}),
        meta("enrichment_list"),
    ))
}

pub async fn get(
    State(s): State<Shared>,
    Path((org, name)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let table = e::table_for(&name)?;
    if !existing(&s, &org.database).await?.contains(&table) {
        return Err(ApiError::not_found("no such enrichment table"));
    }
    let (cols, _) = s
        .run(query::build::stream_schema(&org.database, &table))
        .await?;
    let q =
        e::preview(&org.database, &table).map_err(|_| ApiError::bad("unsupported table name"))?;
    let (rows, m) = s.run(q).await?;
    Ok(ok(json!({"name": name, "columns": cols, "rows": rows}), m))
}

#[derive(Deserialize)]
pub struct UploadParams {
    /// Comma-separated primary key columns.
    key: Option<String>,
    #[serde(default)]
    replace: bool,
}

pub async fn upload(
    State(s): State<Shared>,
    Path((org, name)): Path<(String, String)>,
    Query(p): Query<UploadParams>,
    body: String,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    s.admin_enabled()?;
    let key: Vec<String> = p
        .key
        .as_deref()
        .unwrap_or_default()
        .split(',')
        .map(|k| k.trim().to_string())
        .filter(|k| !k.is_empty())
        .collect();
    let up = e::parse_upload(org, &name, &body, &key)?;
    if existing(&s, &org.database).await?.contains(&up.spec.table) {
        if !p.replace {
            return Err(ApiError(
                StatusCode::CONFLICT,
                "that enrichment table exists; upload with replace=true to replace it".into(),
            ));
        }
        s.admin_ddl(org, "enrich_drop", &up.spec.drop_sql()).await?;
    }
    let m = s
        .admin_ddl(org, "enrich_create", &up.spec.create_sql())
        .await?;
    for chunk in up.rows.chunks(e::CHUNK_ROWS) {
        s.admin_exec(org, up.spec.insert(chunk)).await?;
    }
    let mut m = m;
    m.rows = up.rows.len();
    let (mut parts, b) = ok(
        json!({"name": name, "table": up.spec.table, "rows": up.rows.len(),
               "columns": up.spec.columns.iter().map(|(c, t)| json!({"name": c, "type": t.sql()})).collect::<Vec<Value>>(), "key": up.spec.key}),
        m,
    )
    .into_parts();
    parts.status = StatusCode::CREATED;
    Ok(Response::from_parts(parts, b))
}

pub async fn drop(
    State(s): State<Shared>,
    Path((org, name)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    s.admin_enabled()?;
    let table = e::table_for(&name)?;
    if !existing(&s, &org.database).await?.contains(&table) {
        return Err(ApiError::not_found("no such enrichment table"));
    }
    let spec = e::Spec {
        db: org.database.clone(),
        table,
        columns: vec![],
        key: vec![],
    };
    let m = s.admin_ddl(org, "enrich_drop", &spec.drop_sql()).await?;
    Ok(ok(json!({"dropped": name}), m))
}

#[derive(Deserialize)]
pub struct JoinParams {
    stream: String,
    left: String,
    key: String,
    columns: String,
}

/// A starter query joining a stream with the table, to paste into SQL mode.
pub async fn join(
    State(s): State<Shared>,
    Path((org, name)): Path<(String, String)>,
    Query(p): Query<JoinParams>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let table = e::table_for(&name)?;
    let cols: Vec<String> = p
        .columns
        .split(',')
        .map(|c| c.trim().to_string())
        .filter(|c| !c.is_empty())
        .collect();
    let sql = e::join_sql(org, &p.stream, &table, &p.key, &p.left, &cols)?;
    Ok(ok(json!({"sql": sql}), meta("enrichment_join")))
}
