//! Metrics (R-M1, R-M2): PromQL range and instant queries, metric names and their labels.

use super::{Shared, ok};
use crate::{
    error::{ApiError, ReadError},
    query::{
        Window, build,
        metrics::{self, Grid},
        run_max,
    },
};
use axum::{
    extract::{Path, Query, State},
    response::Response,
};
use serde::Deserialize;
use serde_json::{Value, json};

pub async fn names(State(s): State<Shared>, Path(org): Path<String>) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let (rows, meta) = s.run(metrics::names(&org.database)).await?;
    let data: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({"name": r["table_name"], "kind": metrics::kind_of(r["create_options"].as_str().unwrap_or_default())})
        })
        .collect();
    Ok(ok(data, meta))
}

/// The label names of one metric: the tag columns of its table.
pub async fn labels(
    State(s): State<Shared>,
    Path((org, name)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let name = metrics::metric_name(&name)?;
    let org = s.org(&org)?;
    let (rows, meta) = s.run(build::stream_schema(&org.database, name)).await?;
    if rows.is_empty() {
        return Err(ApiError::not_found("no such metric"));
    }
    let tags: Vec<&str> = rows
        .iter()
        .filter(|r| r["semantic_type"] == "TAG")
        .filter_map(|r| r["column_name"].as_str())
        .collect();
    Ok(ok(tags, meta))
}

#[derive(Deserialize)]
pub struct QueryParams {
    expr: Option<String>,
    from: Option<i64>,
    to: Option<i64>,
    step: Option<i64>,
    /// An instant query at this time (epoch ms); `from` and `to` are then not needed.
    time: Option<i64>,
}

pub async fn query(
    State(s): State<Shared>,
    Path(org): Path<String>,
    Query(p): Query<QueryParams>,
) -> Result<Response, ApiError> {
    let expr = p.expr.as_deref().unwrap_or_default();
    metrics::check(expr)?;
    let grid = match p.time {
        Some(t) if t >= 0 => Grid::instant(t),
        Some(_) => return Err(ApiError::bad("time must be epoch milliseconds")),
        None => Grid::range(Window::parse(p.from, p.to, None)?, p.step),
    };
    // TQL runs in the connection's database and cannot be qualified, so it is only served for the
    // org that lives there. Per-org connections (v0.11.0) lift this; until then another org's
    // PromQL would silently read the wrong tenant's data.
    let org = s.org(&org)?;
    if s.cfg.database().ok().as_deref() != Some(org.database.as_str()) {
        return Err(ApiError::bad(
            "PromQL is only available for the org on the connection's database",
        ));
    }
    let q = metrics::tql(expr, grid);
    let (rows, meta) = run_max(s.reader.as_ref(), q, metrics::MAX_SAMPLES)
        .await
        .map_err(|e| match e {
            ReadError::Db(d) => {
                tracing::warn!(detail = %d, "PromQL failed");
                ApiError::bad(
                    "the database could not evaluate this PromQL; check the metric and label names",
                )
            }
            other => other.into(),
        })?;
    let truncated = rows.len() >= metrics::MAX_SAMPLES;
    let series = metrics::to_series(&rows)?;
    Ok(ok(
        json!({
            "instant": p.time.is_some(), "step_s": grid.step_s, "start_s": grid.start_s, "end_s": grid.end_s,
            "series": series, "truncated": truncated,
        }),
        meta,
    ))
}
