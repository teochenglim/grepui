//! grepui's own state (ADR 0004): saved views and query history live in `grepui_*` tables of a
//! dedicated metadata database in GreptimeDB. Every tenant row carries `org_id` (R-S5).
//!
//! Everything that writes is here. `check_write` is the single gate: an `INSERT INTO` one of
//! [`crate::query::WRITE_TARGETS`] inside the metadata database, or a fixed migration statement.

pub mod build;

use crate::{
    error::ReadError,
    query::{Q, Reader, WRITE_TARGETS, Writer, ident},
};
use anyhow::{Result, bail};

/// Bumped when a migration changes the schema; the binary refuses to run against a newer one.
pub const SCHEMA_VERSION: i64 = 1;
/// The one row in `grepui_schema` (`component = 'core'`) holding the version.
pub const COMPONENT: &str = "core";
/// How long history entries are kept (also the table TTL).
pub const HISTORY_DAYS: i64 = 30;
pub const HISTORY_MAX: u32 = 100;
/// Until access control (v0.11.0) there is one implicit user.
pub const LOCAL_USER: &str = "local";

/// Fixed DDL, in order. `meta_db` is a validated identifier.
pub fn migration_statements(meta_db: &str) -> Result<Vec<String>> {
    let db = ident(meta_db)?;
    Ok(vec![
        format!("CREATE DATABASE IF NOT EXISTS `{db}`"),
        // Version row. Not tenant data, so no org_id (the one documented exemption to R-S5).
        format!(
            "CREATE TABLE IF NOT EXISTS `{db}`.`grepui_schema` (`component` STRING, `version` BIGINT, `ts` TIMESTAMP(3) NOT NULL, TIME INDEX (`ts`), PRIMARY KEY (`component`))"
        ),
        // Upsert by (org_id, view_id) at a constant `ts` (= creation time): the latest write wins;
        // delete is a soft delete (`deleted = true`) because that works on every table mode.
        format!(
            "CREATE TABLE IF NOT EXISTS `{db}`.`grepui_views` (`org_id` STRING, `view_id` STRING, `owner` STRING, `name` STRING, `kind` STRING, `spec` STRING, `created_ms` BIGINT, `updated_ms` BIGINT, `deleted` BOOLEAN, `ts` TIMESTAMP(3) NOT NULL, TIME INDEX (`ts`), PRIMARY KEY (`org_id`, `view_id`))"
        ),
        format!(
            "CREATE TABLE IF NOT EXISTS `{db}`.`grepui_history` (`org_id` STRING, `username` STRING, `kind` STRING, `spec` STRING, `ts` TIMESTAMP(3) NOT NULL, TIME INDEX (`ts`), PRIMARY KEY (`org_id`, `username`, `kind`)) WITH (ttl = '{HISTORY_DAYS}d')"
        ),
    ])
}

/// Tables that hold tenant data and therefore must carry `org_id` (R-S5).
pub const TENANT_TABLES: &[&str] = &["grepui_views", "grepui_history"];

/// Allow only statements the store itself builds: `INSERT INTO <meta_db>.<allowlisted table> ...`.
pub fn check_write(q: &Q, meta_db: &str) -> Result<()> {
    let db = ident(meta_db)?;
    let body = q
        .sql
        .strip_prefix(&format!("/* q:{} */ ", q.name))
        .unwrap_or_default();
    let ok = WRITE_TARGETS
        .iter()
        .any(|t| body.starts_with(&format!("INSERT INTO `{db}`.`{t}` (")));
    if !ok || body.contains(';') {
        bail!("refusing to write: only INSERT INTO the grepui_* metadata tables is allowed");
    }
    Ok(())
}

/// Allow only the fixed migration statements.
pub fn check_ddl(sql: &str, meta_db: &str) -> Result<()> {
    if migration_statements(meta_db)?.iter().any(|s| s == sql) {
        Ok(())
    } else {
        bail!("refusing to run DDL that is not part of the migration")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchemaState {
    /// No metadata database or table yet: run `grepui migrate`.
    Missing,
    Current,
    Older(i64),
    /// Written by a newer grepui: refuse to start rather than corrupt it.
    Newer(i64),
}

pub async fn schema_state(reader: &dyn Reader, meta_db: &str) -> Result<SchemaState> {
    let q = build::schema_get(meta_db)?;
    Ok(match reader.fetch(&q).await {
        Ok(rows) => match rows.first().and_then(|r| r["version"].as_i64()) {
            None => SchemaState::Missing,
            Some(v) if v == SCHEMA_VERSION => SchemaState::Current,
            Some(v) if v < SCHEMA_VERSION => SchemaState::Older(v),
            Some(v) => SchemaState::Newer(v),
        },
        // A missing database or table is an error from the server; that means "not migrated".
        Err(ReadError::Db(d)) if is_missing(&d) => SchemaState::Missing,
        Err(e) => bail!("cannot read the metadata schema: {}", e.detail()),
    })
}

/// Does a database error mean "the metadata table or database does not exist"?
pub fn is_missing(detail: &str) -> bool {
    let d = detail.to_ascii_lowercase();
    d.contains("not found")
        || d.contains("doesn't exist")
        || d.contains("unknown database")
        || d.contains("does not exist")
}

/// Create or upgrade the metadata schema. Idempotent: every statement is `IF NOT EXISTS` and the
/// version row is an upsert.
pub async fn migrate(
    reader: &dyn Reader,
    writer: &dyn Writer,
    meta_db: &str,
) -> Result<Vec<String>> {
    let mut done = vec![];
    if let SchemaState::Newer(v) = schema_state(reader, meta_db)
        .await
        .unwrap_or(SchemaState::Missing)
    {
        bail!(
            "the metadata schema is version {v}, newer than this binary ({SCHEMA_VERSION}); upgrade grepui"
        );
    }
    for sql in migration_statements(meta_db)? {
        check_ddl(&sql, meta_db)?;
        writer
            .ddl(&sql)
            .await
            .map_err(|e| anyhow::anyhow!("migration failed: {}", e.detail()))?;
        done.push(sql);
    }
    let q = build::schema_put(meta_db, SCHEMA_VERSION)?;
    check_write(&q, meta_db)?;
    writer
        .exec(&q)
        .await
        .map_err(|e| anyhow::anyhow!("migration failed: {}", e.detail()))?;
    Ok(done)
}
