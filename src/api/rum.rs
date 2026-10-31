//! RUM view (R-U1, ADR 0014): page views, INP, JavaScript errors and sessions, read from the
//! tables GrepRUM writes into the org's database. Nothing here writes. When the tables are not
//! there the answer says so and how to get them, instead of a database error.

use super::{Shared, ok};
use crate::{
    error::ApiError,
    org::Org,
    query::{Meta, Window, build, logs::bucket_for, rum},
};
use axum::{
    extract::{Path, Query, State},
    response::Response,
};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
pub struct Params {
    from: Option<i64>,
    to: Option<i64>,
    limit: Option<u32>,
    offset: Option<u32>,
    site: Option<String>,
}

fn static_meta(name: &str) -> Meta {
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

fn site(p: &Params) -> Result<Option<&str>, ApiError> {
    match p.site.as_deref().filter(|s| !s.is_empty()) {
        Some(s) if s.len() > 128 || s.chars().any(char::is_control) => {
            Err(ApiError::bad("invalid site"))
        }
        s => Ok(s),
    }
}

/// `Ok(missing columns)`: empty when GrepRUM's `events` table is there; a table that does not
/// exist lists every required column.
async fn check(s: &Shared, org: &Org) -> Result<Vec<&'static str>, ApiError> {
    let (rows, _) = s
        .run(build::stream_schema(&org.database, rum::TABLE))
        .await?;
    let cols: Vec<String> = rows
        .iter()
        .filter_map(|r| r["column_name"].as_str().map(String::from))
        .collect();
    Ok(if cols.is_empty() {
        rum::REQUIRED.to_vec()
    } else {
        rum::missing(&cols)
    })
}

async fn installed(s: &Shared, org: &Org) -> Result<(), ApiError> {
    if check(s, org).await?.is_empty() {
        Ok(())
    } else {
        Err(ApiError::not_found(
            "RUM is not installed in this org: its database has no GrepRUM `events` table (see the GrepRUM install guide)",
        ))
    }
}

fn bad<T>(r: anyhow::Result<T>) -> Result<T, ApiError> {
    r.map_err(|_| ApiError::bad("unsupported database name"))
}

/// Is GrepRUM's data here? The UI asks first and shows a plain state when it is not.
pub async fn status(
    State(s): State<Shared>,
    Path(org): Path<String>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let missing = check(&s, org).await?;
    Ok(ok(
        json!({"installed": missing.is_empty(), "table": rum::TABLE, "missing": missing}),
        static_meta("rum_status"),
    ))
}

pub async fn sites(
    State(s): State<Shared>,
    Path(org): Path<String>,
    Query(p): Query<Params>,
) -> Result<Response, ApiError> {
    let w = Window::parse(p.from, p.to, None)?;
    let org = s.org(&org)?;
    installed(&s, org).await?;
    let (rows, meta) = s.run(bad(rum::sites(&org.database, w))?).await?;
    let names: Vec<Value> = rows.iter().map(|r| r["site_id"].clone()).collect();
    Ok(ok(names, meta))
}

/// Totals, a time series of views and errors, and the busiest pages.
pub async fn overview(
    State(s): State<Shared>,
    Path(org): Path<String>,
    Query(p): Query<Params>,
) -> Result<Response, ApiError> {
    let w = Window::parse(p.from, p.to, None)?;
    let org = s.org(&org)?;
    let site = site(&p)?;
    installed(&s, org).await?;
    let (bucket_ms, _) = bucket_for(w.to_ms - w.from_ms);
    let (totals, meta) = s.run(bad(rum::overview(&org.database, w, site))?).await?;
    let (series, _) = s
        .run(bad(rum::series(&org.database, w, site, bucket_ms))?)
        .await?;
    let (pages, _) = s.run(bad(rum::pages(&org.database, w, site, 20))?).await?;
    Ok(ok(
        json!({
            "totals": totals.first().cloned().unwrap_or(Value::Null),
            "series": series, "bucket_ms": bucket_ms, "pages": pages,
        }),
        meta,
    ))
}

/// INP overall and per page, with the share of good, needs-improvement and poor samples.
pub async fn vitals(
    State(s): State<Shared>,
    Path(org): Path<String>,
    Query(p): Query<Params>,
) -> Result<Response, ApiError> {
    let w = Window::parse(p.from, p.to, None)?;
    let org = s.org(&org)?;
    let site = site(&p)?;
    installed(&s, org).await?;
    let (overall, meta) = s
        .run(bad(rum::vitals_overall(&org.database, w, site))?)
        .await?;
    let (pages, _) = s
        .run(bad(rum::vitals_pages(&org.database, w, site, 50))?)
        .await?;
    Ok(ok(
        json!({
            "metric": "INP", "unit": "ms",
            "good_ms": rum::INP_GOOD_MS, "poor_ms": rum::INP_POOR_MS,
            "overall": overall.first().cloned().unwrap_or(Value::Null), "pages": pages,
        }),
        meta,
    ))
}

pub async fn errors(
    State(s): State<Shared>,
    Path(org): Path<String>,
    Query(p): Query<Params>,
) -> Result<Response, ApiError> {
    let w = Window::parse(p.from, p.to, p.limit)?;
    let org = s.org(&org)?;
    let site = site(&p)?;
    installed(&s, org).await?;
    let (rows, meta) = s
        .run(bad(rum::errors(&org.database, w, site, w.limit.min(200)))?)
        .await?;
    Ok(ok(rows, meta))
}

pub async fn sessions(
    State(s): State<Shared>,
    Path(org): Path<String>,
    Query(p): Query<Params>,
) -> Result<Response, ApiError> {
    let w = Window::parse(p.from, p.to, p.limit)?;
    let org = s.org(&org)?;
    let site = site(&p)?;
    installed(&s, org).await?;
    let (rows, meta) = s
        .run(bad(rum::sessions(
            &org.database,
            w,
            site,
            p.offset.unwrap_or(0),
        ))?)
        .await?;
    Ok(ok(rows, meta))
}
