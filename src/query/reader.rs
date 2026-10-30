//! `SqlxReader`: runs a [`Q`] over the MySQL protocol and returns JSON rows.

use super::{Bind, Q, Reader};
use crate::error::ReadError;
use async_trait::async_trait;
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

#[async_trait]
impl Reader for SqlxReader {
    async fn fetch(&self, q: &Q) -> Result<Vec<Value>, ReadError> {
        // `q.sql` comes from the fixed builders in `query::build`: fixed text and `?` only.
        let mut query = sqlx::query(sqlx::AssertSqlSafe(q.sql.as_str()));
        for b in &q.binds {
            query = match b {
                Bind::Str(s) => query.bind(s.clone()),
                Bind::Int(i) => query.bind(*i),
            };
        }
        let rows = tokio::time::timeout(self.timeout, query.fetch_all(&self.pool))
            .await
            .map_err(|_| ReadError::Timeout)?
            .map_err(|e| ReadError::Db(e.to_string()))?;
        Ok(rows
            .iter()
            .map(|r| {
                let mut m = Map::new();
                for (i, c) in r.columns().iter().enumerate() {
                    m.insert(c.name().to_string(), cell(r, i));
                }
                Value::Object(m)
            })
            .collect())
    }
}
