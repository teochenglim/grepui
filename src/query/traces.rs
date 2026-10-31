//! Trace query builders (R-R1 – R-R3, R-R6, R-AI1). Table names are validated identifiers; every
//! filter value, attribute key, service name, trace id and timestamp is a bound parameter.
//!
//! A traces table has one of two layouts (ADR 0015). **Flat** (`greptime_trace_v1`): one column
//! per attribute, named `span_attributes.<key>`; a key is only used after it has been matched
//! against the table's real column list. **Json** (`greptime_trace_v2`, GreptimeDB v1.3.0+): three
//! JSON2 columns `span_attributes`, `scope_attributes`, `resource_attributes`; a key is read with
//! `json_get_string(col, '["key"]')` where the path is a bound parameter, so any key works.

use super::{
    Bind, DAY_MS, Q, Window,
    filter::{Op, Parsed, like_escape},
    logs, qualified, ts_string,
};
use crate::error::ApiError;

pub const SCATTER_MAX: u32 = 500;
pub const SPANS_MAX: u32 = 5_000;
pub const MAX_OFFSET: u32 = 10_000;
pub const TS: &str = "`timestamp`";
const ERR_SQL: &str = "`span_status_code` = 'STATUS_CODE_ERROR'";
const ROOT_SQL: &str = "(`parent_span_id` IS NULL OR `parent_span_id` = '')";

/// How a traces table stores attributes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Layout {
    /// One column per attribute (`span_attributes.<key>`).
    Flat,
    /// JSON2 attribute columns.
    Json,
}

impl Layout {
    /// A table with a column named exactly `span_attributes` is the JSON layout; the flat layout
    /// has only `span_attributes.<key>` columns.
    pub fn detect(columns: &[String]) -> Layout {
        if columns.iter().any(|c| c == "span_attributes") {
            Layout::Json
        } else {
            Layout::Flat
        }
    }
}

/// A resolved column a condition points at: a fixed plain column, or an attribute.
#[derive(Debug, Clone, PartialEq)]
pub enum TCol {
    Plain(&'static str),
    /// Flat layout: full column name, e.g. `span_attributes.http.route`; compared as text.
    Attr(String),
    /// Json layout: an attribute of `span_attributes` or `resource_attributes`, read as text
    /// (numbers and booleans too: `json_get_string` renders them).
    Json {
        col: &'static str,
        key: String,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct RCond {
    pub col: TCol,
    pub op: Op,
    pub value: String,
}

/// Everything a trace query filters on besides the window.
#[derive(Debug, Clone, Default)]
pub struct TraceFilter {
    pub conds: Vec<RCond>,
    pub terms: Vec<String>,
    pub service: Option<String>,
    pub errors: bool,
    pub min_ms: Option<i64>,
}

impl TraceFilter {
    pub fn is_empty(&self) -> bool {
        self.conds.is_empty() && self.terms.is_empty() && self.service.is_none() && !self.errors
    }
}

/// Known value shorthands: `status=error`, `kind=server`.
fn shorthand(col: &str, v: &str) -> String {
    let up = v.to_ascii_uppercase();
    match col {
        "span_status_code" if matches!(up.as_str(), "ERROR" | "OK" | "UNSET") => {
            format!("STATUS_CODE_{up}")
        }
        "span_kind"
            if matches!(
                up.as_str(),
                "SERVER" | "CLIENT" | "INTERNAL" | "PRODUCER" | "CONSUMER"
            ) =>
        {
            format!("SPAN_KIND_{up}")
        }
        _ => v.to_string(),
    }
}

/// A character set an identifier-quoted attribute column may contain.
fn safe_column(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 200
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
}

/// A key that can travel as a JSON path: printable, not too long.
fn json_key_ok(k: &str) -> bool {
    !k.is_empty() && k.chars().count() <= 200 && !k.chars().any(char::is_control)
}

/// Map filter-bar keys to columns, validating attribute keys against the table's real columns
/// (flat layout) or for shape only (JSON layout, where any key can be asked for).
pub fn resolve(parsed: &Parsed, columns: &[String]) -> Result<Vec<RCond>, ApiError> {
    let layout = Layout::detect(columns);
    let mut out = vec![];
    for c in &parsed.conds {
        let plain = match c.key.as_str() {
            "service" | "service_name" => Some("service_name"),
            "operation" | "name" | "span_name" => Some("span_name"),
            "status" | "span_status_code" => Some("span_status_code"),
            "kind" | "span_kind" => Some("span_kind"),
            "trace" | "trace_id" => Some("trace_id"),
            "span" | "span_id" => Some("span_id"),
            "parent" | "parent_span_id" => Some("parent_span_id"),
            "scope" | "scope_name" => Some("scope_name"),
            "message" | "span_status_message" => Some("span_status_message"),
            _ => None,
        };
        if let Some(p) = plain {
            out.push(RCond {
                col: TCol::Plain(p),
                op: c.op,
                value: shorthand(p, &c.value),
            });
            continue;
        }
        if layout == Layout::Json {
            let (col, k) = if let Some(k) = c
                .key
                .strip_prefix("resource.")
                .or_else(|| c.key.strip_prefix("resource_attributes."))
            {
                ("resource_attributes", k)
            } else {
                let k = c.key.strip_prefix("span_attributes.").unwrap_or(&c.key);
                ("span_attributes", k.strip_prefix("attr.").unwrap_or(k))
            };
            if !json_key_ok(k) {
                return Err(ApiError::bad(format!("invalid attribute name {:?}", c.key)));
            }
            out.push(RCond {
                col: TCol::Json {
                    col,
                    key: k.to_string(),
                },
                op: c.op,
                value: c.value.clone(),
            });
            continue;
        }
        let name = if let Some(k) = c
            .key
            .strip_prefix("resource.")
            .or_else(|| c.key.strip_prefix("resource_attributes."))
        {
            format!("resource_attributes.{k}")
        } else if c.key.starts_with("span_attributes.") {
            c.key.clone()
        } else {
            format!(
                "span_attributes.{}",
                c.key.strip_prefix("attr.").unwrap_or(&c.key)
            )
        };
        if !safe_column(&name) || !columns.contains(&name) {
            return Err(ApiError::bad(format!(
                "unknown attribute {:?}: it is not a column of the traces table",
                c.key
            )));
        }
        out.push(RCond {
            col: TCol::Attr(name),
            op: c.op,
            value: c.value.clone(),
        });
    }
    Ok(out)
}

/// The text of a column and the binds it needs (a JSON attribute binds its path).
fn expr(c: &TCol) -> (String, Vec<Bind>) {
    match c {
        TCol::Plain(n) => (format!("`{n}`"), vec![]),
        TCol::Attr(n) => (format!("CAST(`{n}` AS STRING)"), vec![]),
        TCol::Json { col, key } => (
            format!("json_get_string(`{col}`, ?)"),
            vec![Bind::Str(logs::json_path(key))],
        ),
    }
}

fn cond_sql(c: &RCond) -> (String, Vec<Bind>) {
    let (x, mut binds) = expr(&c.col);
    match c.op {
        Op::Eq => {
            binds.push(Bind::Str(c.value.clone()));
            (format!("{x} = ?"), binds)
        }
        Op::Ne => {
            binds.push(Bind::Str(c.value.clone()));
            (format!("COALESCE({x}, '') != ?"), binds)
        }
        Op::Like => {
            binds.push(Bind::Str(format!("%{}%", like_escape(&c.value))));
            (format!("{x} LIKE ?"), binds)
        }
    }
}

/// Span-level predicates without the window: service, errors, conditions, free text (matched on
/// the span name). `None` when there is nothing to filter on.
fn span_pred(f: &TraceFilter) -> Option<(String, Vec<Bind>)> {
    let mut parts: Vec<String> = vec![];
    let mut binds = vec![];
    if let Some(s) = &f.service {
        parts.push("`service_name` = ?".into());
        binds.push(Bind::Str(s.clone()));
    }
    if f.errors {
        parts.push(ERR_SQL.into());
    }
    for c in &f.conds {
        let (s, b) = cond_sql(c);
        parts.push(s);
        binds.extend(b);
    }
    for t in &f.terms {
        parts.push("`span_name` LIKE ?".into());
        binds.push(Bind::Str(format!("%{}%", like_escape(t))));
    }
    (!parts.is_empty()).then(|| (parts.join(" AND "), binds))
}

/// Window plus the span predicate, as one `WHERE` body (for queries over matching spans only).
fn span_where(f: &TraceFilter, w: Window) -> (String, Vec<Bind>) {
    let mut parts = vec![format!("{TS} >= ?"), format!("{TS} < ?")];
    let mut binds = vec![
        Bind::Str(ts_string(w.from_ms)),
        Bind::Str(ts_string(w.to_ms)),
    ];
    if let Some((p, b)) = span_pred(f) {
        parts.push(p);
        binds.extend(b);
    }
    (parts.join(" AND "), binds)
}

/// One row per trace, with exact whole-trace totals.
///
/// A trace matches the filters when at least one of its spans does, decided by a conditional
/// count in the same single-table aggregate (`matches`). GreptimeDB v1.2.1 returns wrong and
/// nondeterministic rows for `IN (subquery)`, `EXISTS` and self-joins on this table (verified
/// live), so no joins are used anywhere. Each aggregate is computed once, in a derived table
/// with explicit aliases, because the planner also rejects a repeated aggregate expression.
fn trace_rows(
    name: &'static str,
    db: &str,
    table: &str,
    f: &TraceFilter,
    w: Window,
    limit: u32,
    offset: u32,
) -> anyhow::Result<Q> {
    let t = qualified(db, table)?;
    // Textual order of placeholders: select list, then WHERE, then the outer WHERE.
    let mut binds = vec![];
    let matches = match span_pred(f) {
        Some((pred, b)) => {
            binds.extend(b);
            format!(", sum(CASE WHEN {pred} THEN 1 ELSE 0 END) AS `matches`")
        }
        None => String::new(),
    };
    binds.push(Bind::Str(ts_string(w.from_ms)));
    binds.push(Bind::Str(ts_string(w.to_ms)));
    let mut outer = vec![];
    if !matches.is_empty() {
        outer.push("`matches` > 0".to_string());
    }
    if let Some(ms) = f.min_ms.filter(|ms| *ms > 0) {
        outer.push("(`last_ns` - `first_ns`) >= ?".into());
        binds.push(Bind::Int(ms.saturating_mul(1_000_000)));
    }
    let outer_where = if outer.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", outer.join(" AND "))
    };
    Ok(Q::new(
        name,
        format!(
            "SELECT `trace_id`, `first_ns` / 1000000 AS start_ms, (`last_ns` - `first_ns`) / 1000 AS duration_us, \
`spans`, `services`, `errors`, `root_service`, `root_operation` FROM (\
SELECT `trace_id`, min(CAST({TS} AS BIGINT)) AS `first_ns`, max(CAST(`timestamp_end` AS BIGINT)) AS `last_ns`, \
count(*) AS `spans`, count(DISTINCT `service_name`) AS `services`, \
sum(CASE WHEN {ERR_SQL} THEN 1 ELSE 0 END) AS `errors`, \
max(CASE WHEN {ROOT_SQL} THEN `service_name` END) AS `root_service`, \
max(CASE WHEN {ROOT_SQL} THEN `span_name` END) AS `root_operation`{matches} \
FROM {t} WHERE {TS} >= ? AND {TS} < ? GROUP BY `trace_id`) AS `g`{outer_where} ORDER BY start_ms DESC LIMIT {limit} OFFSET {}",
            offset.min(MAX_OFFSET)
        ),
        binds,
    ))
}

pub fn list(db: &str, table: &str, f: &TraceFilter, w: Window, offset: u32) -> anyhow::Result<Q> {
    trace_rows("traces_list", db, table, f, w, w.limit, offset)
}

/// Latency scatter: the same rows as the list, capped, no paging.
pub fn scatter(db: &str, table: &str, f: &TraceFilter, w: Window) -> anyhow::Result<Q> {
    trace_rows("traces_scatter", db, table, f, w, SCATTER_MAX, 0)
}

/// Traces per bucket, each trace counted once in the bucket of its earliest matching span, so
/// the total equals the number of traces the list returns for the same filters.
pub fn histogram(db: &str, table: &str, f: &TraceFilter, w: Window) -> anyhow::Result<Q> {
    let t = qualified(db, table)?;
    let (wh, binds) = span_where(f, w);
    let (ms, _) = logs::bucket_for(w.to_ms - w.from_ms);
    Ok(Q::new(
        "traces_histogram",
        format!(
            "SELECT (CAST(`first_span` AS BIGINT) / {}) * {ms} AS bucket_ms, count(*) AS n \
FROM (SELECT `trace_id`, min({TS}) AS `first_span` FROM {t} WHERE {wh} GROUP BY `trace_id`) AS `s` GROUP BY 1 ORDER BY 1",
            ms * 1_000_000
        ),
        binds,
    ))
}

pub fn services(db: &str, table: &str, w: Window) -> anyhow::Result<Q> {
    let t = qualified(db, table)?;
    Ok(Q::new(
        "traces_services",
        format!(
            "SELECT DISTINCT `service_name` FROM {t} WHERE {TS} >= ? AND {TS} < ? AND `service_name` IS NOT NULL ORDER BY 1 LIMIT 200"
        ),
        vec![
            Bind::Str(ts_string(w.from_ms)),
            Bind::Str(ts_string(w.to_ms)),
        ],
    ))
}

/// All spans of one trace, with exact nanosecond start and end as strings (JS cannot hold ns).
/// `SELECT *` brings every flat attribute column along.
pub fn spans(db: &str, table: &str, trace_id: &str, w: Window) -> anyhow::Result<Q> {
    let t = qualified(db, table)?;
    Ok(Q::new(
        "trace_spans",
        format!(
            "SELECT *, CAST(CAST({TS} AS BIGINT) AS VARCHAR) AS start_ns, CAST(CAST(`timestamp_end` AS BIGINT) AS VARCHAR) AS end_ns \
FROM {t} WHERE `trace_id` = ? AND {TS} >= ? AND {TS} < ? ORDER BY {TS} ASC LIMIT {}",
            SPANS_MAX + 1
        ),
        vec![
            Bind::Str(trace_id.into()),
            Bind::Str(ts_string(w.from_ms)),
            Bind::Str(ts_string(w.to_ms)),
        ],
    ))
}

/// Logs of one trace (optionally one span), oldest first.
pub fn trace_logs(
    db: &str,
    logs_table: &str,
    trace_id: &str,
    span_id: Option<&str>,
    w: Window,
) -> anyhow::Result<Q> {
    let t = qualified(db, logs_table)?;
    let mut wh = format!("`trace_id` = ? AND {TS} >= ? AND {TS} < ?");
    let mut binds = vec![
        Bind::Str(trace_id.into()),
        Bind::Str(ts_string(w.from_ms)),
        Bind::Str(ts_string(w.to_ms)),
    ];
    if let Some(s) = span_id {
        wh.push_str(" AND `span_id` = ?");
        binds.push(Bind::Str(s.into()));
    }
    Ok(Q::new(
        "trace_logs",
        format!(
            "SELECT {} FROM {t} WHERE {wh} ORDER BY {TS} ASC LIMIT {}",
            logs::COLS,
            w.limit
        ),
        binds,
    ))
}

/// Default window for a trace opened without one: the last week.
pub fn default_window(now_ms: i64) -> Window {
    Window {
        from_ms: now_ms - 7 * DAY_MS,
        to_ms: now_ms + 3_600_000,
        limit: super::DEFAULT_LIMIT,
    }
}

/// A trace id: 16 to 32 hex digits, lower-cased.
pub fn trace_id(raw: &str) -> Result<String, ApiError> {
    let id = raw.to_ascii_lowercase();
    if (16..=32).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(id)
    } else {
        Err(ApiError::bad(
            "trace_id must be 16 to 32 hexadecimal digits",
        ))
    }
}

/// A span id: 8 to 16 hex digits.
pub fn span_id(raw: &str) -> Result<String, ApiError> {
    let id = raw.to_ascii_lowercase();
    if (8..=16).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(id)
    } else {
        Err(ApiError::bad("span_id must be 8 to 16 hexadecimal digits"))
    }
}
