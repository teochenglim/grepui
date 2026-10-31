//! Saved views (R-S2) and query history (R-S3). State lives in the `grepui_*` tables; every call
//! is scoped to the org in the path (R-S5). Until access control (v0.11.0) the owner is `local`.

use super::{Shared, ok};
use crate::{
    error::ApiError,
    store::{
        HISTORY_MAX, LOCAL_USER,
        build::{self, ViewRow},
    },
};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;

const KINDS: &[&str] = &["logs", "sql", "metrics"];
/// What a view or history entry may remember: the search, never raw results or credentials.
const SPEC_KEYS: &[&str] = &[
    "q", "level", "range", "mode", "sql", "chart", "kind", "stream",
];
const MAX_VALUE: usize = 8_000;
const MAX_NAME: usize = 100;

fn kind(k: &str) -> Result<&str, ApiError> {
    KINDS
        .iter()
        .copied()
        .find(|x| *x == k)
        .ok_or_else(|| ApiError::bad("kind must be logs, sql or metrics"))
}

fn name(n: &str) -> Result<String, ApiError> {
    let n = n.trim();
    if n.is_empty() || n.chars().count() > MAX_NAME || n.chars().any(char::is_control) {
        return Err(ApiError::bad(
            "name must be 1-100 characters, without control characters",
        ));
    }
    Ok(n.to_string())
}

/// A spec is a flat object of allowlisted string keys. Returns it canonicalised (sorted keys).
pub fn spec(v: &Value) -> Result<String, ApiError> {
    let o = v
        .as_object()
        .ok_or_else(|| ApiError::bad("spec must be an object"))?;
    let mut out = BTreeMap::new();
    for (k, val) in o {
        if !SPEC_KEYS.contains(&k.as_str()) {
            return Err(ApiError::bad(format!("spec key {k:?} is not allowed")));
        }
        let s = val
            .as_str()
            .ok_or_else(|| ApiError::bad(format!("spec.{k} must be a string")))?;
        if s.chars().count() > MAX_VALUE {
            return Err(ApiError::bad(format!("spec.{k} is too long")));
        }
        out.insert(k.clone(), s.to_string());
    }
    Ok(serde_json::to_string(&out).unwrap_or_default())
}

fn view_id(id: &str) -> Result<&str, ApiError> {
    if id.len() == 16 && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        Ok(id)
    } else {
        Err(ApiError::bad("invalid view id"))
    }
}

fn view_json(r: &Value) -> Value {
    let parsed = r["spec"]
        .as_str()
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
        .unwrap_or_else(|| json!({}));
    json!({
        "id": r["view_id"], "name": r["name"], "kind": r["kind"], "spec": parsed, "owner": r["owner"],
        "created_ms": r["created_ms"], "updated_ms": r["updated_ms"],
    })
}

fn row_of(r: &Value) -> ViewRow {
    ViewRow {
        view_id: r["view_id"].as_str().unwrap_or_default().into(),
        owner: r["owner"].as_str().unwrap_or(LOCAL_USER).into(),
        name: r["name"].as_str().unwrap_or_default().into(),
        kind: r["kind"].as_str().unwrap_or_default().into(),
        spec: r["spec"].as_str().unwrap_or("{}").into(),
        created_ms: r["created_ms"].as_i64().unwrap_or(0),
        updated_ms: r["updated_ms"].as_i64().unwrap_or(0),
        deleted: false,
    }
}

fn status(code: StatusCode, resp: Response) -> Response {
    let (mut parts, body) = resp.into_parts();
    parts.status = code;
    Response::from_parts(parts, body)
}

#[derive(Deserialize)]
pub struct ListParams {
    kind: Option<String>,
}

pub async fn list(
    State(s): State<Shared>,
    Path(org): Path<String>,
    Query(p): Query<ListParams>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let k = p.kind.as_deref().map(kind).transpose()?;
    let q = build::view_list(s.meta_db(), &org.name, k)
        .map_err(|_| ApiError::bad("invalid request"))?;
    let (rows, meta) = s.read_store(q).await?;
    Ok(ok(rows.iter().map(view_json).collect::<Vec<_>>(), meta))
}

pub async fn get(
    State(s): State<Shared>,
    Path((org, id)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let q = build::view_get(s.meta_db(), &org.name, view_id(&id)?)
        .map_err(|_| ApiError::bad("invalid request"))?;
    let (rows, meta) = s.read_store(q).await?;
    let r = rows
        .first()
        .ok_or_else(|| ApiError::not_found("no such view"))?;
    Ok(ok(view_json(r), meta))
}

#[derive(Deserialize)]
pub struct NewView {
    name: String,
    kind: String,
    spec: Value,
}

pub async fn create(
    State(s): State<Shared>,
    Path(org): Path<String>,
    Json(b): Json<NewView>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let now = s.now_ms();
    let row = ViewRow {
        view_id: format!("{:016x}", rand::random::<u64>()),
        owner: LOCAL_USER.into(),
        name: name(&b.name)?,
        kind: kind(&b.kind)?.into(),
        spec: spec(&b.spec)?,
        created_ms: now,
        updated_ms: now,
        deleted: false,
    };
    let meta = s
        .write(
            build::view_put(s.meta_db(), &org.name, &row)
                .map_err(|_| ApiError::bad("invalid request"))?,
        )
        .await?;
    let data = view_json(
        &json!({"view_id": row.view_id, "name": row.name, "kind": row.kind, "spec": row.spec, "owner": row.owner, "created_ms": row.created_ms, "updated_ms": row.updated_ms}),
    );
    Ok(status(StatusCode::CREATED, ok(data, meta)))
}

#[derive(Deserialize)]
pub struct EditView {
    name: Option<String>,
    spec: Option<Value>,
}

pub async fn update(
    State(s): State<Shared>,
    Path((org, id)): Path<(String, String)>,
    Json(b): Json<EditView>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let id = view_id(&id)?;
    let (rows, _) = s
        .read_store(
            build::view_get(s.meta_db(), &org.name, id)
                .map_err(|_| ApiError::bad("invalid request"))?,
        )
        .await?;
    let mut row = row_of(
        rows.first()
            .ok_or_else(|| ApiError::not_found("no such view"))?,
    );
    if let Some(n) = &b.name {
        row.name = name(n)?;
    }
    if let Some(sp) = &b.spec {
        row.spec = spec(sp)?;
    }
    row.updated_ms = s.now_ms();
    let meta = s
        .write(
            build::view_put(s.meta_db(), &org.name, &row)
                .map_err(|_| ApiError::bad("invalid request"))?,
        )
        .await?;
    Ok(ok(
        view_json(
            &json!({"view_id": row.view_id, "name": row.name, "kind": row.kind, "spec": row.spec, "owner": row.owner, "created_ms": row.created_ms, "updated_ms": row.updated_ms}),
        ),
        meta,
    ))
}

pub async fn delete(
    State(s): State<Shared>,
    Path((org, id)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let id = view_id(&id)?;
    let (rows, _) = s
        .read_store(
            build::view_get(s.meta_db(), &org.name, id)
                .map_err(|_| ApiError::bad("invalid request"))?,
        )
        .await?;
    let mut row = row_of(
        rows.first()
            .ok_or_else(|| ApiError::not_found("no such view"))?,
    );
    row.deleted = true;
    row.updated_ms = s.now_ms();
    let meta = s
        .write(
            build::view_put(s.meta_db(), &org.name, &row)
                .map_err(|_| ApiError::bad("invalid request"))?,
        )
        .await?;
    Ok(ok(json!({"deleted": true, "id": id}), meta))
}

// ---- history ------------------------------------------------------------------

#[derive(Deserialize)]
pub struct NewEntry {
    kind: String,
    spec: Value,
}

/// Record a search. An entry identical to the newest one is not stored again.
pub async fn record(
    State(s): State<Shared>,
    Path(org): Path<String>,
    Json(b): Json<NewEntry>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let k = kind(&b.kind)?;
    let sp = spec(&b.spec)?;
    let now = s.now_ms();
    let (latest, _) = s
        .read_store(
            build::history_list(s.meta_db(), &org.name, LOCAL_USER, Some(k), 1, now)
                .map_err(|_| ApiError::bad("invalid request"))?,
        )
        .await?;
    if latest.first().and_then(|r| r["spec"].as_str()) == Some(sp.as_str()) {
        let meta = crate::query::Meta {
            query: "history_add".into(),
            sql: String::new(),
            binds: vec![],
            ms: 0,
            rows: 0,
            source: "raw",
            note: Some("identical to the newest entry; not stored again".into()),
        };
        return Ok(ok(json!({"recorded": false}), meta));
    }
    let meta = s
        .write(
            build::history_add(s.meta_db(), &org.name, LOCAL_USER, k, &sp, now)
                .map_err(|_| ApiError::bad("invalid request"))?,
        )
        .await?;
    Ok(status(
        StatusCode::CREATED,
        ok(json!({"recorded": true}), meta),
    ))
}

#[derive(Deserialize)]
pub struct HistoryParams {
    kind: Option<String>,
    limit: Option<u32>,
}

pub async fn history(
    State(s): State<Shared>,
    Path(org): Path<String>,
    Query(p): Query<HistoryParams>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let k = p.kind.as_deref().map(kind).transpose()?;
    let q = build::history_list(
        s.meta_db(),
        &org.name,
        LOCAL_USER,
        k,
        p.limit.unwrap_or(HISTORY_MAX),
        s.now_ms(),
    )
    .map_err(|_| ApiError::bad("invalid request"))?;
    let (rows, meta) = s.read_store(q).await?;
    let data: Vec<Value> = rows
        .iter()
        .map(|r| {
            let sp = r["spec"]
                .as_str()
                .and_then(|s| serde_json::from_str::<Value>(s).ok())
                .unwrap_or(Value::Object(Map::new()));
            json!({"kind": r["kind"], "spec": sp, "at_ms": r["at_ms"]})
        })
        .collect();
    Ok(ok(data, meta))
}

#[allow(dead_code)]
fn _assert_response(_: impl IntoResponse) {}
