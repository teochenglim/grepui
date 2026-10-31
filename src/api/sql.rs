//! SQL mode (R-L5): one parser-validated, read-only statement, with window macros, a row cap
//! and the query timeout. The statement is confined to the org's database.

use super::{Shared, ok};
use crate::{
    error::ApiError,
    query::{Q, Window, run_max, sqlmode},
};
use axum::{
    extract::{Path, Query, State},
    response::Response,
};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
pub struct SqlParams {
    sql: Option<String>,
    from: Option<i64>,
    to: Option<i64>,
    limit: Option<u32>,
}

pub async fn run(
    State(s): State<Shared>,
    Path(org): Path<String>,
    Query(p): Query<SqlParams>,
) -> Result<Response, ApiError> {
    let w = Window::parse(p.from, p.to, p.limit)?;
    let org = s.org(&org)?;
    // Saved functions expand here (SQL mode and panels only); their arguments become binds.
    let (sql, binds) =
        super::functions::expand_sql(&s, org, p.sql.as_deref().unwrap_or_default(), vec![]).await?;
    let sql = sqlmode::expand_macros(&sql, w.from_ms, w.to_ms);
    sqlmode::check(&sql, &org.database).map_err(|r| ApiError::bad(r.0))?;
    let q = Q::new("sql_mode", &sql, binds);
    let (rows, meta) = run_max(s.reader.as_ref(), q, w.limit as usize).await?;
    let columns: Vec<String> = rows
        .first()
        .and_then(Value::as_object)
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default();
    Ok(ok(
        json!({"columns": columns, "rows": rows, "truncated": rows.len() == w.limit as usize}),
        meta,
    ))
}
