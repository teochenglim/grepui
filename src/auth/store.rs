//! Reads and writes of access-control rows in the state store (kind-tagged items; ADR 0012).

use crate::{
    api::Shared,
    error::ApiError,
    query::Meta,
    store::build::{self, ItemRow},
};
use axum::http::StatusCode;

fn internal(e: anyhow::Error) -> ApiError {
    tracing::error!(error = %e, "auth store");
    ApiError(StatusCode::INTERNAL_SERVER_ERROR, "internal error".into())
}

pub async fn get(s: &Shared, org: &str, kind: &str, id: &str) -> Result<Option<ItemRow>, ApiError> {
    let q = build::item_get(s.meta_db(), org, kind, id).map_err(internal)?;
    let (rows, _) = s.read_store(q).await?;
    Ok(rows.first().map(crate::alerts::engine::item_of))
}

pub async fn list(s: &Shared, org: &str, kind: &str) -> Result<Vec<ItemRow>, ApiError> {
    let q = build::item_list(s.meta_db(), org, kind).map_err(internal)?;
    let (rows, _) = s.read_store(q).await?;
    Ok(rows.iter().map(crate::alerts::engine::item_of).collect())
}

pub async fn put(s: &Shared, org: &str, kind: &str, r: &ItemRow) -> Result<Meta, ApiError> {
    let q = build::item_put(s.meta_db(), org, kind, r).map_err(internal)?;
    s.write(q).await
}

/// Soft-delete an item.
pub async fn remove(
    s: &Shared,
    org: &str,
    kind: &str,
    mut r: ItemRow,
    now: i64,
) -> Result<Meta, ApiError> {
    r.deleted = true;
    r.updated_ms = now;
    put(s, org, kind, &r).await
}

pub fn row(
    id: &str,
    name: &str,
    spec: &impl serde::Serialize,
    owner: &str,
    created_ms: i64,
    now: i64,
) -> ItemRow {
    ItemRow {
        item_id: id.into(),
        name: name.into(),
        spec: serde_json::to_string(spec).unwrap_or_default(),
        owner: owner.into(),
        created_ms,
        updated_ms: now,
        deleted: false,
    }
}
