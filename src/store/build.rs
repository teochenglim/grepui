//! Metadata builders (R-S1 – R-S5). Table names are fixed or validated; every value, and always
//! the org, is a bound parameter. Reads of tenant tables take the org as a required argument.

use super::{COMPONENT, HISTORY_DAYS, HISTORY_MAX};
use crate::query::{Bind, Q, qualified, ts_string};
use anyhow::{Result, bail};

/// An org name must look like one before it is ever bound (defence in depth).
fn org(o: &str) -> Result<Bind> {
    if !crate::org::valid_name(o) {
        bail!("invalid org name");
    }
    Ok(Bind::Str(o.to_string()))
}

pub fn schema_get(meta_db: &str) -> Result<Q> {
    let t = qualified(meta_db, "grepui_schema")?;
    Ok(Q::new(
        "schema_get",
        format!("SELECT `version` FROM {t} WHERE `component` = ?"),
        vec![Bind::Str(COMPONENT.into())],
    ))
}

pub fn schema_put(meta_db: &str, version: i64) -> Result<Q> {
    let t = qualified(meta_db, "grepui_schema")?;
    // Constant ts: the row is overwritten in place (upsert by primary key and time index).
    Ok(Q::new(
        "schema_put",
        format!("INSERT INTO {t} (`component`, `version`, `ts`) VALUES (?, ?, ?)"),
        vec![
            Bind::Str(COMPONENT.into()),
            Bind::Int(version),
            Bind::Str(ts_string(0)),
        ],
    ))
}

/// A saved view as stored.
#[derive(Debug, Clone, PartialEq)]
pub struct ViewRow {
    pub view_id: String,
    pub owner: String,
    pub name: String,
    pub kind: String,
    pub spec: String,
    pub created_ms: i64,
    pub updated_ms: i64,
    pub deleted: bool,
}

const VIEW_COLS: &str = "`view_id`, `owner`, `name`, `kind`, `spec`, `created_ms`, `updated_ms`";

pub fn view_list(meta_db: &str, org_name: &str, kind: Option<&str>) -> Result<Q> {
    let t = qualified(meta_db, "grepui_views")?;
    let mut binds = vec![org(org_name)?];
    let mut wh = "`org_id` = ? AND `deleted` = false".to_string();
    if let Some(k) = kind {
        wh.push_str(" AND `kind` = ?");
        binds.push(Bind::Str(k.into()));
    }
    Ok(Q::new(
        "view_list",
        format!("SELECT {VIEW_COLS} FROM {t} WHERE {wh} ORDER BY `updated_ms` DESC LIMIT 200"),
        binds,
    ))
}

pub fn view_get(meta_db: &str, org_name: &str, view_id: &str) -> Result<Q> {
    let t = qualified(meta_db, "grepui_views")?;
    Ok(Q::new(
        "view_get",
        format!(
            "SELECT {VIEW_COLS} FROM {t} WHERE `org_id` = ? AND `view_id` = ? AND `deleted` = false LIMIT 1"
        ),
        vec![org(org_name)?, Bind::Str(view_id.into())],
    ))
}

/// Insert or overwrite a view (same `org_id`, `view_id` and `ts` = creation time).
pub fn view_put(meta_db: &str, org_name: &str, v: &ViewRow) -> Result<Q> {
    let t = qualified(meta_db, "grepui_views")?;
    Ok(Q::new(
        "view_put",
        format!(
            "INSERT INTO {t} (`org_id`, `view_id`, `owner`, `name`, `kind`, `spec`, `created_ms`, `updated_ms`, `deleted`, `ts`) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
        ),
        vec![
            org(org_name)?,
            Bind::Str(v.view_id.clone()),
            Bind::Str(v.owner.clone()),
            Bind::Str(v.name.clone()),
            Bind::Str(v.kind.clone()),
            Bind::Str(v.spec.clone()),
            Bind::Int(v.created_ms),
            Bind::Int(v.updated_ms),
            Bind::Bool(v.deleted),
            Bind::Str(ts_string(v.created_ms)),
        ],
    ))
}

pub fn history_add(
    meta_db: &str,
    org_name: &str,
    user: &str,
    kind: &str,
    spec: &str,
    now_ms: i64,
) -> Result<Q> {
    let t = qualified(meta_db, "grepui_history")?;
    Ok(Q::new(
        "history_add",
        format!(
            "INSERT INTO {t} (`org_id`, `username`, `kind`, `spec`, `ts`) VALUES (?, ?, ?, ?, ?)"
        ),
        vec![
            org(org_name)?,
            Bind::Str(user.into()),
            Bind::Str(kind.into()),
            Bind::Str(spec.into()),
            Bind::Str(ts_string(now_ms)),
        ],
    ))
}

/// Newest first, at most [`HISTORY_MAX`] and never older than the retention window.
/// (`ts` is TIMESTAMP(3): its integer cast is already milliseconds; verified live.)
pub fn history_list(
    meta_db: &str,
    org_name: &str,
    user: &str,
    kind: Option<&str>,
    limit: u32,
    now_ms: i64,
) -> Result<Q> {
    let t = qualified(meta_db, "grepui_history")?;
    let since = now_ms - HISTORY_DAYS * 86_400_000;
    let mut binds = vec![org(org_name)?, Bind::Str(user.into())];
    let mut wh = "`org_id` = ? AND `username` = ?".to_string();
    if let Some(k) = kind {
        wh.push_str(" AND `kind` = ?");
        binds.push(Bind::Str(k.into()));
    }
    wh.push_str(" AND `ts` >= ?");
    binds.push(Bind::Str(ts_string(since)));
    Ok(Q::new(
        "history_list",
        format!(
            "SELECT `kind`, `spec`, CAST(`ts` AS BIGINT) AS at_ms FROM {t} WHERE {wh} ORDER BY `ts` DESC LIMIT {}",
            limit.clamp(1, HISTORY_MAX)
        ),
        binds,
    ))
}
