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

/// A service catalog note as stored (R-V4).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ServiceNote {
    pub service: String,
    pub description: String,
    pub owner: String,
    pub tier: String,
    pub runbook: String,
    pub updated_by: String,
    pub updated_ms: i64,
    pub deleted: bool,
}

const NOTE_COLS: &str =
    "`service`, `description`, `owner`, `tier`, `runbook`, `updated_by`, `updated_ms`";

pub fn service_notes(meta_db: &str, org_name: &str) -> Result<Q> {
    let t = qualified(meta_db, "grepui_services")?;
    Ok(Q::new(
        "service_notes",
        format!(
            "SELECT {NOTE_COLS} FROM {t} WHERE `org_id` = ? AND `deleted` = false ORDER BY `service` LIMIT 1000"
        ),
        vec![org(org_name)?],
    ))
}

pub fn service_note_get(meta_db: &str, org_name: &str, service: &str) -> Result<Q> {
    let t = qualified(meta_db, "grepui_services")?;
    Ok(Q::new(
        "service_note_get",
        format!(
            "SELECT {NOTE_COLS} FROM {t} WHERE `org_id` = ? AND `service` = ? AND `deleted` = false LIMIT 1"
        ),
        vec![org(org_name)?, Bind::Str(service.to_string())],
    ))
}

/// Insert or overwrite a note (same `org_id`, `service` and constant `ts`).
pub fn service_note_put(meta_db: &str, org_name: &str, n: &ServiceNote) -> Result<Q> {
    let t = qualified(meta_db, "grepui_services")?;
    Ok(Q::new(
        "service_note_put",
        format!(
            "INSERT INTO {t} (`org_id`, `service`, `description`, `owner`, `tier`, `runbook`, `updated_by`, `updated_ms`, `deleted`, `ts`) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
        ),
        vec![
            org(org_name)?,
            Bind::Str(n.service.clone()),
            Bind::Str(n.description.clone()),
            Bind::Str(n.owner.clone()),
            Bind::Str(n.tier.clone()),
            Bind::Str(n.runbook.clone()),
            Bind::Str(n.updated_by.clone()),
            Bind::Int(n.updated_ms),
            Bind::Bool(n.deleted),
            Bind::Str(ts_string(0)),
        ],
    ))
}

// ---- dashboards (R-B1, R-B6) ----------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Default)]
pub struct FolderRow {
    pub folder_id: String,
    pub name: String,
    pub owner: String,
    pub created_ms: i64,
    pub updated_ms: i64,
    pub deleted: bool,
}

pub fn folder_list(meta_db: &str, org_name: &str) -> Result<Q> {
    let t = qualified(meta_db, "grepui_folders")?;
    Ok(Q::new(
        "folder_list",
        format!(
            "SELECT `folder_id`, `name`, `owner`, `created_ms`, `updated_ms` FROM {t} WHERE `org_id` = ? AND `deleted` = false ORDER BY `name` LIMIT 500"
        ),
        vec![org(org_name)?],
    ))
}

pub fn folder_put(meta_db: &str, org_name: &str, f: &FolderRow) -> Result<Q> {
    let t = qualified(meta_db, "grepui_folders")?;
    Ok(Q::new(
        "folder_put",
        format!(
            "INSERT INTO {t} (`org_id`, `folder_id`, `name`, `owner`, `created_ms`, `updated_ms`, `deleted`, `ts`) VALUES (?, ?, ?, ?, ?, ?, ?, ?)"
        ),
        vec![
            org(org_name)?,
            Bind::Str(f.folder_id.clone()),
            Bind::Str(f.name.clone()),
            Bind::Str(f.owner.clone()),
            Bind::Int(f.created_ms),
            Bind::Int(f.updated_ms),
            Bind::Bool(f.deleted),
            Bind::Str(ts_string(f.created_ms)),
        ],
    ))
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct DashRow {
    pub dash_id: String,
    pub folder_id: String,
    pub title: String,
    pub kind: String,
    pub spec: String,
    pub owner: String,
    pub created_ms: i64,
    pub updated_ms: i64,
    pub deleted: bool,
}

const DASH_META: &str =
    "`dash_id`, `folder_id`, `title`, `kind`, `owner`, `created_ms`, `updated_ms`";

/// Dashboards without their specs (the list page), optionally of one folder or kind.
pub fn dash_list(
    meta_db: &str,
    org_name: &str,
    folder: Option<&str>,
    kind: Option<&str>,
) -> Result<Q> {
    let t = qualified(meta_db, "grepui_dashboards")?;
    let mut binds = vec![org(org_name)?];
    let mut wh = "`org_id` = ? AND `deleted` = false".to_string();
    if let Some(f) = folder {
        wh.push_str(" AND `folder_id` = ?");
        binds.push(Bind::Str(f.into()));
    }
    if let Some(k) = kind {
        wh.push_str(" AND `kind` = ?");
        binds.push(Bind::Str(k.into()));
    }
    Ok(Q::new(
        "dash_list",
        format!("SELECT {DASH_META} FROM {t} WHERE {wh} ORDER BY `title` LIMIT 1000"),
        binds,
    ))
}

/// The same with specs, for bulk export.
pub fn dash_export_list(meta_db: &str, org_name: &str, folder: Option<&str>) -> Result<Q> {
    let t = qualified(meta_db, "grepui_dashboards")?;
    let mut binds = vec![org(org_name)?];
    let mut wh = "`org_id` = ? AND `deleted` = false AND `kind` = 'dashboard'".to_string();
    if let Some(f) = folder {
        wh.push_str(" AND `folder_id` = ?");
        binds.push(Bind::Str(f.into()));
    }
    Ok(Q::new(
        "dash_export_list",
        format!("SELECT {DASH_META}, `spec` FROM {t} WHERE {wh} ORDER BY `title` LIMIT 100"),
        binds,
    ))
}

pub fn dash_get(meta_db: &str, org_name: &str, dash_id: &str) -> Result<Q> {
    let t = qualified(meta_db, "grepui_dashboards")?;
    Ok(Q::new(
        "dash_get",
        format!(
            "SELECT {DASH_META}, `spec` FROM {t} WHERE `org_id` = ? AND `dash_id` = ? AND `deleted` = false LIMIT 1"
        ),
        vec![org(org_name)?, Bind::Str(dash_id.into())],
    ))
}

pub fn dash_put(meta_db: &str, org_name: &str, d: &DashRow) -> Result<Q> {
    let t = qualified(meta_db, "grepui_dashboards")?;
    Ok(Q::new(
        "dash_put",
        format!(
            "INSERT INTO {t} (`org_id`, `dash_id`, `folder_id`, `title`, `kind`, `spec`, `owner`, `created_ms`, `updated_ms`, `deleted`, `ts`) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
        ),
        vec![
            org(org_name)?,
            Bind::Str(d.dash_id.clone()),
            Bind::Str(d.folder_id.clone()),
            Bind::Str(d.title.clone()),
            Bind::Str(d.kind.clone()),
            Bind::Str(d.spec.clone()),
            Bind::Str(d.owner.clone()),
            Bind::Int(d.created_ms),
            Bind::Int(d.updated_ms),
            Bind::Bool(d.deleted),
            Bind::Str(ts_string(d.created_ms)),
        ],
    ))
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ShareRow {
    pub share_id: String,
    pub dash_id: String,
    pub token_hash: String,
    pub created_by: String,
    pub created_ms: i64,
    pub expires_ms: i64,
    pub revoked: bool,
}

const SHARE_COLS: &str =
    "`share_id`, `dash_id`, `token_hash`, `created_by`, `created_ms`, `expires_ms`, `revoked`";

pub fn share_list(meta_db: &str, org_name: &str, dash_id: &str) -> Result<Q> {
    let t = qualified(meta_db, "grepui_shares")?;
    Ok(Q::new(
        "share_list",
        format!(
            "SELECT {SHARE_COLS} FROM {t} WHERE `org_id` = ? AND `dash_id` = ? ORDER BY `created_ms` DESC LIMIT 100"
        ),
        vec![org(org_name)?, Bind::Str(dash_id.into())],
    ))
}

pub fn share_get(meta_db: &str, org_name: &str, share_id: &str) -> Result<Q> {
    let t = qualified(meta_db, "grepui_shares")?;
    Ok(Q::new(
        "share_get",
        format!("SELECT {SHARE_COLS} FROM {t} WHERE `org_id` = ? AND `share_id` = ? LIMIT 1"),
        vec![org(org_name)?, Bind::Str(share_id.into())],
    ))
}

pub fn share_by_hash(meta_db: &str, org_name: &str, token_hash: &str) -> Result<Q> {
    let t = qualified(meta_db, "grepui_shares")?;
    Ok(Q::new(
        "share_by_hash",
        format!("SELECT {SHARE_COLS} FROM {t} WHERE `org_id` = ? AND `token_hash` = ? LIMIT 1"),
        vec![org(org_name)?, Bind::Str(token_hash.into())],
    ))
}

pub fn share_put(meta_db: &str, org_name: &str, s: &ShareRow) -> Result<Q> {
    let t = qualified(meta_db, "grepui_shares")?;
    Ok(Q::new(
        "share_put",
        format!(
            "INSERT INTO {t} (`org_id`, `share_id`, `dash_id`, `token_hash`, `created_by`, `created_ms`, `expires_ms`, `revoked`, `ts`) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)"
        ),
        vec![
            org(org_name)?,
            Bind::Str(s.share_id.clone()),
            Bind::Str(s.dash_id.clone()),
            Bind::Str(s.token_hash.clone()),
            Bind::Str(s.created_by.clone()),
            Bind::Int(s.created_ms),
            Bind::Int(s.expires_ms),
            Bind::Bool(s.revoked),
            Bind::Str(ts_string(s.created_ms)),
        ],
    ))
}
