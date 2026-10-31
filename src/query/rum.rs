//! RUM view builders (R-U1): read-only queries over the tables GrepRUM writes. The `events` table
//! holds one row per browser event (`$pageview`, `$web_vital`, `$error`, ...) with JSON text in
//! `props`; every statement here is a fixed shape with bound parameters. The only INP web vital
//! GrepRUM collects is read (`props.name = 'INP'`, `props.value` in milliseconds).

use super::{Bind, Q, Window, qualified, ts_string};

pub const TABLE: &str = "events";
/// Columns a table must have to be taken for GrepRUM's `events`.
pub const REQUIRED: &[&str] = &[
    "ts",
    "site_id",
    "event_name",
    "visitor_id",
    "session_id",
    "path",
    "props",
];
/// INP thresholds (web.dev): good up to, poor above (milliseconds).
pub const INP_GOOD_MS: i64 = 200;
pub const INP_POOR_MS: i64 = 500;

const VALUE: &str = "json_get_float(parse_json(`props`), 'value')";
const NAME: &str = "json_get_string(parse_json(`props`), 'name')";
const MESSAGE: &str = "json_get_string(parse_json(`props`), 'message')";
const STACK: &str = "json_get_string(parse_json(`props`), 'stack')";

/// The columns the table lacks (empty when it is GrepRUM's).
pub fn missing(columns: &[String]) -> Vec<&'static str> {
    REQUIRED
        .iter()
        .copied()
        .filter(|c| !columns.iter().any(|x| x == c))
        .collect()
}

fn scope(w: Window, site: Option<&str>) -> (String, Vec<Bind>) {
    let mut wh = "`ts` >= ? AND `ts` < ?".to_string();
    let mut binds = vec![
        Bind::Str(ts_string(w.from_ms)),
        Bind::Str(ts_string(w.to_ms)),
    ];
    if let Some(s) = site.filter(|s| !s.is_empty()) {
        wh.push_str(" AND `site_id` = ?");
        binds.push(Bind::Str(s.to_string()));
    }
    (wh, binds)
}

fn with(mut binds: Vec<Bind>, extra: Vec<Bind>) -> Vec<Bind> {
    binds.extend(extra);
    binds
}

pub fn sites(db: &str, w: Window) -> anyhow::Result<Q> {
    let t = qualified(db, TABLE)?;
    let (wh, binds) = scope(w, None);
    Ok(Q::new(
        "rum_sites",
        format!(
            "SELECT DISTINCT `site_id` FROM {t} WHERE {wh} AND `site_id` IS NOT NULL ORDER BY 1 LIMIT 200"
        ),
        binds,
    ))
}

/// Totals for the window, one row.
pub fn overview(db: &str, w: Window, site: Option<&str>) -> anyhow::Result<Q> {
    let t = qualified(db, TABLE)?;
    let (wh, binds) = scope(w, site);
    Ok(Q::new(
        "rum_overview",
        format!(
            "SELECT CAST(coalesce(sum(CASE WHEN `event_name` = '$pageview' THEN 1 ELSE 0 END), 0) AS BIGINT) AS pageviews, \
             CAST(coalesce(sum(CASE WHEN `event_name` = '$error' THEN 1 ELSE 0 END), 0) AS BIGINT) AS errors, \
             CAST(coalesce(sum(CASE WHEN `event_name` = '$web_vital' THEN 1 ELSE 0 END), 0) AS BIGINT) AS vitals, \
             count(DISTINCT `session_id`) AS sessions, count(DISTINCT `visitor_id`) AS visitors \
             FROM {t} WHERE {wh}"
        ),
        binds,
    ))
}

/// Page views and errors per time bucket.
pub fn series(db: &str, w: Window, site: Option<&str>, bucket_ms: i64) -> anyhow::Result<Q> {
    let t = qualified(db, TABLE)?;
    let (wh, binds) = scope(w, site);
    // `bucket_ms` is one of a fixed set chosen by the caller; `ts` is a millisecond timestamp.
    let ms = bucket_ms.clamp(1000, 86_400_000);
    Ok(Q::new(
        "rum_series",
        format!(
            "SELECT (CAST(`ts` AS BIGINT) / {ms}) * {ms} AS bucket_ms, \
             CAST(coalesce(sum(CASE WHEN `event_name` = '$pageview' THEN 1 ELSE 0 END), 0) AS BIGINT) AS pageviews, \
             CAST(coalesce(sum(CASE WHEN `event_name` = '$error' THEN 1 ELSE 0 END), 0) AS BIGINT) AS errors \
             FROM {t} WHERE {wh} GROUP BY 1 ORDER BY 1"
        ),
        binds,
    ))
}

pub fn pages(db: &str, w: Window, site: Option<&str>, limit: u32) -> anyhow::Result<Q> {
    let t = qualified(db, TABLE)?;
    let (wh, binds) = scope(w, site);
    Ok(Q::new(
        "rum_pages",
        format!(
            "SELECT `path`, count(*) AS views, count(DISTINCT `session_id`) AS sessions FROM {t} \
             WHERE {wh} AND `event_name` = '$pageview' GROUP BY `path` ORDER BY views DESC, `path` LIMIT {}",
            limit.clamp(1, 200)
        ),
        binds,
    ))
}

/// INP for the whole window: percentiles and how many samples were good, in need of work, poor.
pub fn vitals_overall(db: &str, w: Window, site: Option<&str>) -> anyhow::Result<Q> {
    let t = qualified(db, TABLE)?;
    let (wh, binds) = scope(w, site);
    Ok(Q::new(
        "rum_vitals",
        format!(
            "SELECT count(*) AS samples, approx_percentile_cont(v, 0.5) AS p50, \
             approx_percentile_cont(v, 0.75) AS p75, approx_percentile_cont(v, 0.95) AS p95, \
             CAST(coalesce(sum(CASE WHEN v <= {INP_GOOD_MS} THEN 1 ELSE 0 END), 0) AS BIGINT) AS good, \
             CAST(coalesce(sum(CASE WHEN v > {INP_GOOD_MS} AND v <= {INP_POOR_MS} THEN 1 ELSE 0 END), 0) AS BIGINT) AS needs_improvement, \
             CAST(coalesce(sum(CASE WHEN v > {INP_POOR_MS} THEN 1 ELSE 0 END), 0) AS BIGINT) AS poor \
             FROM (SELECT {VALUE} AS v FROM {t} WHERE {wh} AND `event_name` = '$web_vital' AND {NAME} = ?) AS t \
             WHERE v IS NOT NULL"
        ),
        with(binds, vec![Bind::Str("INP".into())]),
    ))
}

/// INP per page, slowest 75th percentile first.
pub fn vitals_pages(db: &str, w: Window, site: Option<&str>, limit: u32) -> anyhow::Result<Q> {
    let t = qualified(db, TABLE)?;
    let (wh, binds) = scope(w, site);
    Ok(Q::new(
        "rum_vitals_pages",
        format!(
            "SELECT `path`, count(*) AS samples, approx_percentile_cont(v, 0.5) AS p50, \
             approx_percentile_cont(v, 0.75) AS p75, approx_percentile_cont(v, 0.95) AS p95 \
             FROM (SELECT `path`, {VALUE} AS v FROM {t} WHERE {wh} AND `event_name` = '$web_vital' AND {NAME} = ?) AS t \
             WHERE v IS NOT NULL GROUP BY `path` ORDER BY p75 DESC, `path` LIMIT {}",
            limit.clamp(1, 200)
        ),
        with(binds, vec![Bind::Str("INP".into())]),
    ))
}

/// JavaScript errors grouped by message, with one stack sample.
pub fn errors(db: &str, w: Window, site: Option<&str>, limit: u32) -> anyhow::Result<Q> {
    let t = qualified(db, TABLE)?;
    let (wh, binds) = scope(w, site);
    Ok(Q::new(
        "rum_errors",
        format!(
            "SELECT {MESSAGE} AS message, count(*) AS occurrences, count(DISTINCT `session_id`) AS sessions, \
             CAST(min(`ts`) AS BIGINT) AS first_ms, CAST(max(`ts`) AS BIGINT) AS last_ms, \
             max({STACK}) AS stack, max(`path`) AS path \
             FROM {t} WHERE {wh} AND `event_name` = '$error' GROUP BY 1 ORDER BY occurrences DESC, message LIMIT {}",
            limit.clamp(1, 200)
        ),
        binds,
    ))
}

/// Sessions seen in the window, newest first, built from the events themselves.
pub fn sessions(db: &str, w: Window, site: Option<&str>, offset: u32) -> anyhow::Result<Q> {
    let t = qualified(db, TABLE)?;
    let (wh, binds) = scope(w, site);
    Ok(Q::new(
        "rum_sessions",
        format!(
            "SELECT `session_id`, max(`visitor_id`) AS visitor_id, CAST(min(`ts`) AS BIGINT) AS started_ms, \
             CAST(max(`ts`) AS BIGINT) AS last_ms, count(*) AS events, \
             CAST(coalesce(sum(CASE WHEN `event_name` = '$pageview' THEN 1 ELSE 0 END), 0) AS BIGINT) AS pageviews, \
             CAST(coalesce(sum(CASE WHEN `event_name` = '$error' THEN 1 ELSE 0 END), 0) AS BIGINT) AS errors, \
             max(`path`) AS last_path \
             FROM {t} WHERE {wh} AND `session_id` IS NOT NULL GROUP BY `session_id` \
             ORDER BY started_ms DESC, `session_id` LIMIT {} OFFSET {}",
            w.limit,
            offset.min(10_000)
        ),
        binds,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w() -> Window {
        Window {
            from_ms: 1_000,
            to_ms: 9_000,
            limit: 50,
        }
    }

    #[test]
    fn every_value_is_a_bind_and_every_statement_is_tagged() {
        let qs = [
            sites("db", w()).unwrap(),
            overview("db", w(), Some("x'; DROP TABLE y; --")).unwrap(),
            series("db", w(), Some("s"), 60_000).unwrap(),
            pages("db", w(), None, 10).unwrap(),
            vitals_overall("db", w(), Some("s")).unwrap(),
            vitals_pages("db", w(), Some("s"), 10).unwrap(),
            errors("db", w(), Some("s"), 10).unwrap(),
            sessions("db", w(), Some("s"), 0).unwrap(),
        ];
        for q in &qs {
            assert!(
                q.sql.starts_with(&format!("/* q:{} */ ", q.name)),
                "{}",
                q.name
            );
            assert!(!q.sql.contains("DROP"), "{}", q.sql);
            assert_eq!(q.sql.matches('?').count(), q.binds.len(), "{}", q.name);
            assert!(q.sql.contains("`db`.`events`"));
        }
    }

    #[test]
    fn missing_columns_are_named() {
        let cols: Vec<String> = ["ts", "site_id", "event_name"].map(String::from).to_vec();
        let m = missing(&cols);
        assert!(m.contains(&"props") && m.contains(&"session_id") && !m.contains(&"ts"));
        assert!(missing(&REQUIRED.iter().map(|s| s.to_string()).collect::<Vec<_>>()).is_empty());
    }
}
