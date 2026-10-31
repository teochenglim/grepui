//! Service builders (R-V1 – R-V4). A service's RED numbers (rate, errors, duration) are computed
//! over its entry spans, server and consumer kinds, because a client span is the caller's view of
//! the same request. Table names are validated identifiers; the service name, every timestamp and
//! the bucket are bound or drawn from a fixed set.
//!
//! Dependencies pair a child span with its parent span in one self-join. That was unsafe on
//! GreptimeDB v1.2.1 (ADR 0007) and is verified correct on the pinned v1.3.0 by a live test that
//! compares the result with an independently computed answer.

use super::{Bind, Q, Window, logs, qualified, ts_string};
use crate::error::ApiError;

pub const SERVICES_MAX: u32 = 500;
pub const OPERATIONS_MAX: u32 = 200;
pub const EDGES_MAX: u32 = 500;
const TS: &str = "`timestamp`";
const SERVICE_NAME_MAX: usize = 200;

/// Entry spans: the server side of an RPC or the consumer of a message.
const ENTRY: &str = "`span_kind` IN ('SPAN_KIND_SERVER', 'SPAN_KIND_CONSUMER')";
const ERR: &str = "`span_status_code` = 'STATUS_CODE_ERROR'";
const DUR: &str = "(CAST(`timestamp_end` AS BIGINT) - CAST(`timestamp` AS BIGINT))";

/// A service name from a path or query: non-empty, bounded, no control characters.
pub fn service_name(raw: &str) -> Result<String, ApiError> {
    let s = raw.trim();
    if s.is_empty() || s.chars().count() > SERVICE_NAME_MAX || s.chars().any(char::is_control) {
        return Err(ApiError::bad(
            "service must be 1-200 characters, without control characters",
        ));
    }
    Ok(s.to_string())
}

/// The four RED aggregates over entry spans, each computed once.
fn red_columns() -> String {
    format!(
        "sum(CASE WHEN {ENTRY} THEN 1 ELSE 0 END) AS `requests`, \
sum(CASE WHEN {ENTRY} AND {ERR} THEN 1 ELSE 0 END) AS `errors`, \
avg(CASE WHEN {ENTRY} THEN {DUR} END) / 1000 AS `avg_us`, \
approx_percentile_cont(CASE WHEN {ENTRY} THEN {DUR} END, 0.95) / 1000 AS `p95_us`"
    )
}

fn window_binds(w: Window) -> Vec<Bind> {
    vec![
        Bind::Str(ts_string(w.from_ms)),
        Bind::Str(ts_string(w.to_ms)),
    ]
}

/// One row per service: first and last span seen, span count and RED over entry spans
/// (R-V1, R-V4). With `only`, just that service.
pub fn services(db: &str, table: &str, only: Option<&str>, w: Window) -> anyhow::Result<Q> {
    let t = qualified(db, table)?;
    let mut binds = window_binds(w);
    let mut wh = format!("{TS} >= ? AND {TS} < ? AND `service_name` IS NOT NULL");
    if let Some(s) = only {
        wh.push_str(" AND `service_name` = ?");
        binds.push(Bind::Str(s.to_string()));
    }
    Ok(Q::new(
        "services_red",
        format!(
            "SELECT `service_name`, min(CAST({TS} AS BIGINT)) / 1000000 AS `first_seen_ms`, \
max(CAST({TS} AS BIGINT)) / 1000000 AS `last_seen_ms`, count(*) AS `spans`, {} \
FROM {t} WHERE {wh} GROUP BY `service_name` ORDER BY `requests` DESC, `service_name` ASC LIMIT {SERVICES_MAX}",
            red_columns()
        ),
        binds,
    ))
}

/// Requests, errors and latency per time bucket for one service (R-V2). The bucket is one of the
/// fixed set in `logs::BUCKETS`, so it is safe as a literal.
pub fn series(db: &str, table: &str, service: &str, w: Window) -> anyhow::Result<Q> {
    let t = qualified(db, table)?;
    let (ms, _) = logs::bucket_for(w.to_ms - w.from_ms);
    let mut binds = window_binds(w);
    binds.push(Bind::Str(service.to_string()));
    Ok(Q::new(
        "service_series",
        format!(
            "SELECT (CAST({TS} AS BIGINT) / {}) * {ms} AS `bucket_ms`, {} \
FROM {t} WHERE {TS} >= ? AND {TS} < ? AND `service_name` = ? GROUP BY 1 ORDER BY 1",
            ms * 1_000_000,
            red_columns()
        ),
        binds,
    ))
}

/// RED per operation (span name) of one service (R-V2).
pub fn operations(db: &str, table: &str, service: &str, w: Window) -> anyhow::Result<Q> {
    let t = qualified(db, table)?;
    let mut binds = window_binds(w);
    binds.push(Bind::Str(service.to_string()));
    Ok(Q::new(
        "service_operations",
        format!(
            "SELECT `span_name` AS `operation`, {} \
FROM {t} WHERE {TS} >= ? AND {TS} < ? AND `service_name` = ? AND {ENTRY} GROUP BY `span_name` \
ORDER BY `requests` DESC, `operation` ASC LIMIT {OPERATIONS_MAX}",
            red_columns()
        ),
        binds,
    ))
}

/// Caller to callee edges: each child span joined to its parent span in the same trace (R-V3).
/// Calls within one service are left out unless `self_loops`. Errors are the callee's. Both spans
/// must start inside the window, so a call that straddles its edge is not counted.
pub fn dependencies(db: &str, table: &str, self_loops: bool, w: Window) -> anyhow::Result<Q> {
    let t = qualified(db, table)?;
    let mut binds = window_binds(w);
    binds.extend(window_binds(w));
    let cross = if self_loops {
        ""
    } else {
        " AND p.`service_name` <> c.`service_name`"
    };
    Ok(Q::new(
        "service_dependencies",
        format!(
            "SELECT p.`service_name` AS `caller`, c.`service_name` AS `callee`, count(*) AS `calls`, \
sum(CASE WHEN c.{ERR} THEN 1 ELSE 0 END) AS `errors`, \
avg(CAST(c.`timestamp_end` AS BIGINT) - CAST(c.`timestamp` AS BIGINT)) / 1000 AS `avg_us` \
FROM {t} AS c JOIN {t} AS p ON c.`trace_id` = p.`trace_id` AND c.`parent_span_id` = p.`span_id` \
WHERE c.{TS} >= ? AND c.{TS} < ? AND p.{TS} >= ? AND p.{TS} < ? \
AND c.`service_name` IS NOT NULL AND p.`service_name` IS NOT NULL{cross} \
GROUP BY 1, 2 ORDER BY `calls` DESC, `caller` ASC, `callee` ASC LIMIT {EDGES_MAX}"
        ),
        binds,
    ))
}

// ---- rollup (R-P1, R-P2) ---------------------------------------------------------------

/// Windows longer than this are answered from the rollup when one exists.
pub const RAW_MAX_WINDOW_MS: i64 = 24 * 3_600_000;
pub const SOURCE_ROLLUP: &str = "rollup_1m";
pub const ROLLUP_NOTE: &str = "answered from the 1-minute rollup: p95 is not available because percentiles cannot be combined across minutes";
pub const RAW_WARNING: &str = "this window is longer than 24 hours and there is no rollup, so raw spans were scanned (slow); create the traces RED rollup on the Rollups page";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Raw,
    Rollup,
    /// Raw spans because no rollup exists, although the window is long.
    RawWithWarning,
}

/// Which table answers a window (R-P1).
pub fn plan(window_ms: i64, has_rollup: bool) -> Source {
    if window_ms <= RAW_MAX_WINDOW_MS {
        Source::Raw
    } else if has_rollup {
        Source::Rollup
    } else {
        Source::RawWithWarning
    }
}

/// The rollup's RED columns, each aggregate once. `p95_us` is NULL: it cannot be rebuilt.
fn rollup_columns() -> &'static str {
    "sum(`requests`) AS `requests`, sum(`errors`) AS `errors`, \
CAST(sum(`sum_ns`) AS DOUBLE) / sum(`requests`) / 1000 AS `avg_us`, CAST(NULL AS DOUBLE) AS `p95_us`, \
max(`max_ns`) / 1000 AS `max_us`"
}

/// Same row shape as [`services`], read from the sink table. `time_window` is TIMESTAMP(3), whose
/// integer cast is already milliseconds.
pub fn services_rollup(db: &str, sink: &str, only: Option<&str>, w: Window) -> anyhow::Result<Q> {
    let t = qualified(db, sink)?;
    let mut binds = window_binds(w);
    let mut wh = "`time_window` >= ? AND `time_window` < ?".to_string();
    if let Some(s) = only {
        wh.push_str(" AND `service_name` = ?");
        binds.push(Bind::Str(s.to_string()));
    }
    Ok(Q::new(
        "services_red_rollup",
        format!(
            "SELECT `service_name`, min(CAST(`time_window` AS BIGINT)) AS `first_seen_ms`, \
max(CAST(`time_window` AS BIGINT)) AS `last_seen_ms`, sum(`requests`) AS `spans`, {} \
FROM {t} WHERE {wh} GROUP BY `service_name` ORDER BY `requests` DESC, `service_name` ASC LIMIT {SERVICES_MAX}",
            rollup_columns()
        ),
        binds,
    ))
}

pub fn series_rollup(db: &str, sink: &str, service: &str, w: Window) -> anyhow::Result<Q> {
    let t = qualified(db, sink)?;
    let (ms, _) = logs::bucket_for(w.to_ms - w.from_ms);
    let mut binds = window_binds(w);
    binds.push(Bind::Str(service.to_string()));
    Ok(Q::new(
        "service_series_rollup",
        format!(
            "SELECT (CAST(`time_window` AS BIGINT) / {ms}) * {ms} AS `bucket_ms`, {} \
FROM {t} WHERE `time_window` >= ? AND `time_window` < ? AND `service_name` = ? GROUP BY 1 ORDER BY 1",
            rollup_columns()
        ),
        binds,
    ))
}
