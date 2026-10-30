//! Query layer (ARCH 01): fixed builders produce a [`Q`] (SQL text with `?` and binds);
//! a [`Reader`] runs it. Nothing from a request is ever formatted into SQL text (R-Q1).

pub mod build;
pub mod reader;

use crate::error::{ApiError, ReadError};
use async_trait::async_trait;
use serde::Serialize;
use serde_json::Value;
use std::time::Instant;

pub use reader::SqlxReader;

pub const DAY_MS: i64 = 86_400_000;
/// Longest window any data endpoint accepts (R-Q2).
pub const MAX_WINDOW_MS: i64 = 31 * DAY_MS;
pub const DEFAULT_LIMIT: u32 = 100;
pub const MAX_LIMIT: u32 = 1000;

/// Every table grepui may write to or run DDL against. Empty until the metadata store
/// (v0.3.0); a test pins the contents (R-F4, ADR 0004).
pub const WRITE_TARGETS: &[&str] = &[];

#[derive(Debug, Clone, PartialEq)]
pub enum Bind {
    Str(String),
    Int(i64),
}

impl Bind {
    pub fn show(&self) -> String {
        match self {
            Bind::Str(s) => s.clone(),
            Bind::Int(i) => i.to_string(),
        }
    }
}

/// A query: SQL text of fixed fragments and `?`, plus its binds. `name` is the `/* q:<name> */` tag.
#[derive(Debug, Clone, PartialEq)]
pub struct Q {
    pub name: &'static str,
    pub sql: String,
    pub binds: Vec<Bind>,
}

impl Q {
    pub fn new(name: &'static str, body: impl AsRef<str>, binds: Vec<Bind>) -> Q {
        Q {
            name,
            sql: format!("/* q:{name} */ {}", body.as_ref()),
            binds,
        }
    }
}

#[async_trait]
pub trait Reader: Send + Sync {
    /// Rows as JSON objects keyed by column alias.
    async fn fetch(&self, q: &Q) -> Result<Vec<Value>, ReadError>;
}

/// "Show SQL" (R-Q4): what ran, how long it took, how many rows, and where the data came from.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Meta {
    pub query: String,
    pub sql: String,
    pub binds: Vec<String>,
    pub ms: u64,
    pub rows: usize,
    pub source: &'static str,
}

/// Run a query and return rows with their [`Meta`].
pub async fn run(reader: &dyn Reader, q: Q) -> Result<(Vec<Value>, Meta), ReadError> {
    let t = Instant::now();
    let rows = reader.fetch(&q).await?;
    let meta = Meta {
        query: q.name.to_string(),
        sql: q.sql.clone(),
        binds: q.binds.iter().map(Bind::show).collect(),
        ms: t.elapsed().as_millis() as u64,
        rows: rows.len(),
        source: "raw",
    };
    Ok((rows, meta))
}

/// A validated SQL identifier: ASCII letters, digits and `_`, 1..=128 chars.
pub fn ident(s: &str) -> anyhow::Result<&str> {
    if s.is_empty() || s.len() > 128 || !s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
        anyhow::bail!("invalid identifier {s:?}");
    }
    Ok(s)
}

/// `` `db`.`table` ``, both validated.
pub fn qualified(db: &str, table: &str) -> anyhow::Result<String> {
    Ok(format!("`{}`.`{}`", ident(db)?, ident(table)?))
}

pub fn quote(col: &str) -> anyhow::Result<String> {
    Ok(format!("`{}`", ident(col)?))
}

/// A validated time window and row limit (R-Q2).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Window {
    pub from_ms: i64,
    pub to_ms: i64,
    pub limit: u32,
}

impl Window {
    pub fn parse(
        from: Option<i64>,
        to: Option<i64>,
        limit: Option<u32>,
    ) -> Result<Window, ApiError> {
        let (Some(from_ms), Some(to_ms)) = (from, to) else {
            return Err(ApiError::bad(
                "from and to are required (epoch milliseconds)",
            ));
        };
        if from_ms < 0 || to_ms <= from_ms {
            return Err(ApiError::bad("to must be greater than from"));
        }
        if to_ms - from_ms > MAX_WINDOW_MS {
            return Err(ApiError::bad("the window is longer than 31 days"));
        }
        Ok(Window {
            from_ms,
            to_ms,
            limit: limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT),
        })
    }
}

/// `YYYY-MM-DD HH:MM:SS.mmm` in UTC, the form GreptimeDB compares against timestamp columns.
pub fn ts_string(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .unwrap_or_default()
        .format("%Y-%m-%d %H:%M:%S%.3f")
        .to_string()
}
