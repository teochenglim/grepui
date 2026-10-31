//! Saved SQL functions API (R-N2) and the expansion used by SQL mode and dashboard panels.

use super::{Shared, ok};
use crate::{
    error::ApiError,
    functions::{self as f, Function},
    org::Org,
    query::{Bind, Meta},
    store::{LOCAL_USER, build::ItemRow},
};
use axum::{
    Json,
    extract::{Path, State},
    response::Response,
};
use serde_json::{Value, json};
use std::collections::HashMap;

const KIND: &str = "function";

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

async fn all(s: &Shared, org: &Org) -> Result<HashMap<String, Function>, ApiError> {
    Ok(super::alerts::engine_items(s, org, KIND)
        .await?
        .iter()
        .filter_map(|r| {
            serde_json::from_str::<Function>(&r.spec)
                .ok()
                .map(|x| (x.name.clone(), x))
        })
        .collect())
}

/// Expand `@name(...)` calls in `sql`, when there are any. Used only by SQL mode and panels.
pub async fn expand_sql(
    s: &Shared,
    org: &Org,
    sql: &str,
    binds: Vec<Bind>,
) -> Result<(String, Vec<Bind>), ApiError> {
    if !f::has_call(sql) {
        return Ok((sql.to_string(), binds));
    }
    let fns = all(s, org).await?;
    f::expand(sql, binds, &fns).map_err(ApiError::bad)
}

pub async fn list(State(s): State<Shared>, Path(org): Path<String>) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let mut v: Vec<Function> = all(&s, org).await?.into_values().collect();
    v.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(ok(v, meta("function_list")))
}

pub async fn put(
    State(s): State<Shared>,
    Path((org, name)): Path<(String, String)>,
    Json(b): Json<Value>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let mut b = b;
    b["name"] = json!(name);
    let func = serde_json::from_value::<Function>(b)
        .map_err(|e| ApiError::bad(format!("invalid function: {e}")))?
        .canonical()?;
    let now = s.now_ms();
    let old = super::alerts::engine_item(&s, org, KIND, &func.name).await?;
    let row = ItemRow {
        item_id: func.name.clone(),
        name: func.name.clone(),
        spec: serde_json::to_string(&func).map_err(|_| ApiError::bad("invalid function"))?,
        owner: LOCAL_USER.into(),
        created_ms: old.map_or(now, |o| o.created_ms),
        updated_ms: now,
        deleted: false,
    };
    let m = super::alerts::engine_save(&s, org, KIND, &row).await?;
    Ok(ok(func, m))
}

pub async fn delete(
    State(s): State<Shared>,
    Path((org, name)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let mut row = super::alerts::engine_item(&s, org, KIND, &name)
        .await?
        .ok_or_else(|| ApiError::not_found("no such function"))?;
    row.deleted = true;
    row.updated_ms = s.now_ms();
    let m = super::alerts::engine_save(&s, org, KIND, &row).await?;
    Ok(ok(json!({"deleted": true, "name": name}), m))
}
