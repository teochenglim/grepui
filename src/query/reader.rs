//! `SqlxReader`: runs a [`Q`] over the MySQL protocol and returns JSON rows.

use super::{Bind, Q, Reader, Writer};
use crate::error::ReadError;
use async_trait::async_trait;
use futures_util::StreamExt;
use serde_json::{Map, Value, json};
use sqlx::{Column, Row, TypeInfo, mysql::MySqlPoolOptions};
use std::time::Duration;

pub struct SqlxReader {
    pool: sqlx::MySqlPool,
    timeout: Duration,
}

impl SqlxReader {
    /// Lazy: grepui starts and serves `/healthz` even when the database is down.
    pub fn lazy(url: &str, timeout: Duration) -> anyhow::Result<Self> {
        Ok(Self {
            pool: MySqlPoolOptions::new()
                .max_connections(8)
                .acquire_timeout(Duration::from_secs(5))
                .connect_lazy(url)?,
            timeout,
        })
    }
}

fn cell(row: &sqlx::mysql::MySqlRow, i: usize) -> Value {
    let ty = row.column(i).type_info().name().to_ascii_uppercase();
    macro_rules! get {
        ($t:ty) => {
            row.try_get::<Option<$t>, _>(i)
                .ok()
                .flatten()
                .map(|v| json!(v))
                .unwrap_or(Value::Null)
        };
    }
    match ty.as_str() {
        "BOOLEAN" => get!(bool),
        "TINYINT" | "SMALLINT" | "INT" | "BIGINT" => get!(i64),
        "TINYINT UNSIGNED" | "SMALLINT UNSIGNED" | "INT UNSIGNED" | "BIGINT UNSIGNED" => get!(u64),
        "FLOAT" | "DOUBLE" => get!(f64),
        "DECIMAL" => row
            .try_get::<Option<sqlx::types::BigDecimal>, _>(i)
            .ok()
            .flatten()
            .and_then(|d| d.to_string().parse::<f64>().ok())
            .map(|v| json!(v))
            .unwrap_or(Value::Null),
        // Milliseconds since the epoch: nanosecond columns do not fit a JS number. sqlx maps the
        // MySQL TIMESTAMP type to DateTime<Utc>; NaiveDateTime only fits DATETIME.
        "TIMESTAMP" => row
            .try_get::<Option<chrono::DateTime<chrono::Utc>>, _>(i)
            .ok()
            .flatten()
            .map(|d| json!(d.timestamp_millis()))
            .unwrap_or(Value::Null),
        "DATETIME" => row
            .try_get::<Option<chrono::NaiveDateTime>, _>(i)
            .ok()
            .flatten()
            .map(|d| json!(d.and_utc().timestamp_millis()))
            .unwrap_or(Value::Null),
        "JSON" => row
            .try_get::<Option<Value>, _>(i)
            .ok()
            .flatten()
            .unwrap_or(Value::Null),
        _ => get!(String),
    }
}

fn row_to_json(r: &sqlx::mysql::MySqlRow) -> Value {
    let mut m = Map::new();
    for (i, c) in r.columns().iter().enumerate() {
        m.insert(c.name().to_string(), cell(r, i));
    }
    Value::Object(m)
}

fn bind_all<'q>(q: &'q Q) -> sqlx::query::Query<'q, sqlx::MySql, sqlx::mysql::MySqlArguments> {
    // `q.sql` comes from the fixed builders, or (SQL mode only) from the parser allowlist.
    let mut query = sqlx::query(sqlx::AssertSqlSafe(q.sql.as_str()));
    for b in &q.binds {
        query = match b {
            Bind::Str(s) => query.bind(s.clone()),
            Bind::Int(i) => query.bind(*i),
            Bind::Bool(b) => query.bind(*b),
            Bind::Float(f) => query.bind(*f),
            Bind::Null => query.bind(Option::<String>::None),
        };
    }
    query
}

#[async_trait]
impl Reader for SqlxReader {
    async fn fetch_max(&self, q: &Q, max: usize) -> Result<Vec<Value>, ReadError> {
        let read = async {
            let mut out = Vec::new();
            let mut stream = bind_all(q).fetch(&self.pool);
            while out.len() < max {
                match stream.next().await {
                    Some(Ok(r)) => out.push(row_to_json(&r)),
                    Some(Err(e)) => return Err(ReadError::Db(e.to_string())),
                    None => break,
                }
            }
            Ok(out)
        };
        tokio::time::timeout(self.timeout, read)
            .await
            .map_err(|_| ReadError::Timeout)?
    }

    async fn kill(&self, id: &str) -> Result<(), ReadError> {
        // `KILL` cannot be prepared. The id comes from GreptimeDB's own process list, and only
        // its characters (digits, `.`, `:`, `/`) are let through.
        if id.is_empty()
            || id.len() > 64
            || !id
                .bytes()
                .all(|b| b.is_ascii_digit() || matches!(b, b'.' | b':' | b'/'))
        {
            return Err(ReadError::Db("not a process id".into()));
        }
        let sql = format!("KILL '{id}'");
        tokio::time::timeout(
            self.timeout,
            sqlx::raw_sql(sqlx::AssertSqlSafe(sql)).execute(&self.pool),
        )
        .await
        .map_err(|_| ReadError::Timeout)?
        .map_err(|e| ReadError::Db(e.to_string()))?;
        Ok(())
    }

    async fn fetch(&self, q: &Q) -> Result<Vec<Value>, ReadError> {
        let rows = tokio::time::timeout(self.timeout, bind_all(q).fetch_all(&self.pool))
            .await
            .map_err(|_| ReadError::Timeout)?
            .map_err(|e| ReadError::Db(e.to_string()))?;
        Ok(rows.iter().map(row_to_json).collect())
    }
}

#[async_trait]
impl Writer for SqlxReader {
    async fn exec(&self, q: &Q) -> Result<u64, ReadError> {
        // The caller (`AppState::write`) has already passed `store::check_write`.
        let done = tokio::time::timeout(self.timeout, bind_all(q).execute(&self.pool))
            .await
            .map_err(|_| ReadError::Timeout)?
            .map_err(|e| ReadError::Db(e.to_string()))?;
        Ok(done.rows_affected())
    }

    async fn ddl(&self, sql: &str) -> Result<(), ReadError> {
        // `sql` is one of the fixed statements from `store::migration_statements`.
        tokio::time::timeout(
            self.timeout,
            sqlx::raw_sql(sqlx::AssertSqlSafe(sql.to_string())).execute(&self.pool),
        )
        .await
        .map_err(|_| ReadError::Timeout)?
        .map_err(|e| ReadError::Db(e.to_string()))?;
        Ok(())
    }
}
