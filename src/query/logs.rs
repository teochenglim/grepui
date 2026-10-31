//! Log query builders (R-L1 – R-L4, R-L6). Table names are validated identifiers; every filter
//! value, attribute key and timestamp is a bound parameter; bucket sizes come from a fixed set.

use super::{
    Bind, Q, Window,
    filter::{Cond, FtMode, LogFilter, Op, like_escape},
    qualified, ts_string,
};

pub const TS: &str = "`timestamp`";
pub const EXPORT_MAX: u32 = 10_000;
pub const MAX_OFFSET: u32 = 10_000;
pub const AROUND_MAX: u32 = 200;
pub const TOP_VALUES_MAX: u32 = 50;

/// Fields shown in "quick mode" (the interesting subset).
pub const QUICK_FIELDS: &[&str] = &[
    "timestamp",
    "severity_text",
    "body",
    "trace_id",
    "span_id",
    "scope_name",
];
/// Plain columns whose top values may be listed (low cardinality).
pub const TOP_VALUE_COLUMNS: &[&str] = &["severity_text", "scope_name", "scope_version"];

const COLS: &str = "`timestamp`, CAST(CAST(`timestamp` AS BIGINT) AS VARCHAR) AS ts_ns, `severity_text`, \
`severity_number`, `body`, `trace_id`, `span_id`, `scope_name`, `log_attributes`, `resource_attributes`";

/// A column a condition or a top-values request can point at.
#[derive(Debug, Clone, PartialEq)]
pub enum Col {
    Plain(&'static str),
    /// `json_get_string(column, path)` with `path` bound.
    Json(&'static str, String),
}

/// JSON path for a flat key that may itself contain dots: `["http.method"]`.
pub fn json_path(key: &str) -> String {
    format!("[\"{}\"]", key.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Map a filter key to a column. Unknown keys are log attributes.
pub fn column(key: &str) -> Col {
    match key {
        "level" | "severity" | "severity_text" => Col::Plain("severity_text"),
        "severity_number" => Col::Plain("severity_number"),
        "trace" | "trace_id" => Col::Plain("trace_id"),
        "span" | "span_id" => Col::Plain("span_id"),
        "scope" | "scope_name" => Col::Plain("scope_name"),
        "scope_version" => Col::Plain("scope_version"),
        "body" | "message" => Col::Plain("body"),
        _ => {
            if let Some(k) = key
                .strip_prefix("resource_attributes.")
                .or_else(|| key.strip_prefix("resource."))
            {
                Col::Json("resource_attributes", json_path(k))
            } else {
                let k = key
                    .strip_prefix("log_attributes.")
                    .or_else(|| key.strip_prefix("attr."))
                    .unwrap_or(key);
                Col::Json("log_attributes", json_path(k))
            }
        }
    }
}

/// Column for the top-values endpoint: an allowlisted plain column or a JSON attribute key.
pub fn top_value_column(field: &str) -> Option<Col> {
    if let Some(k) = field.strip_prefix("resource_attributes.") {
        (!k.is_empty() && k.len() <= 100).then(|| Col::Json("resource_attributes", json_path(k)))
    } else if let Some(k) = field.strip_prefix("log_attributes.") {
        (!k.is_empty() && k.len() <= 100).then(|| Col::Json("log_attributes", json_path(k)))
    } else {
        TOP_VALUE_COLUMNS
            .iter()
            .find(|c| **c == field)
            .map(|c| Col::Plain(c))
    }
}

/// SQL text of a column expression plus the binds it needs.
fn expr(c: &Col) -> (String, Vec<Bind>) {
    match c {
        Col::Plain("severity_number") => ("CAST(`severity_number` AS STRING)".into(), vec![]),
        Col::Plain(n) => (format!("`{n}`"), vec![]),
        Col::Json(col, path) => (
            format!("json_get_string(`{col}`, ?)"),
            vec![Bind::Str(path.clone())],
        ),
    }
}

fn cond_sql(c: &Cond) -> (String, Vec<Bind>) {
    let (x, mut binds) = expr(&column(&c.key));
    let sql = match c.op {
        Op::Eq => {
            binds.push(Bind::Str(c.value.clone()));
            format!("{x} = ?")
        }
        Op::Ne => {
            binds.push(Bind::Str(c.value.clone()));
            format!("COALESCE({x}, '') != ?")
        }
        Op::Like => {
            binds.push(Bind::Str(format!("%{}%", like_escape(&c.value))));
            format!("{x} LIKE ?")
        }
    };
    (sql, binds)
}

/// `WHERE` body: window, level, conditions, free text.
fn where_clause(f: &LogFilter, w: Window) -> (String, Vec<Bind>) {
    let mut parts = vec![format!("{TS} >= ?"), format!("{TS} < ?")];
    let mut binds = vec![
        Bind::Str(ts_string(w.from_ms)),
        Bind::Str(ts_string(w.to_ms)),
    ];
    if let Some(l) = &f.level {
        parts.push("`severity_text` = ?".into());
        binds.push(Bind::Str(l.clone()));
    }
    for c in &f.parsed.conds {
        let (s, b) = cond_sql(c);
        parts.push(s);
        binds.extend(b);
    }
    for t in &f.parsed.terms {
        match f.ft {
            FtMode::Term => {
                parts.push("matches_term(`body`, ?)".into());
                binds.push(Bind::Str(t.clone()));
            }
            FtMode::Like => {
                parts.push("`body` LIKE ?".into());
                binds.push(Bind::Str(format!("%{}%", like_escape(t))));
            }
        }
    }
    (parts.join(" AND "), binds)
}

pub fn search(db: &str, table: &str, f: &LogFilter, w: Window, offset: u32) -> anyhow::Result<Q> {
    let t = qualified(db, table)?;
    let (wh, binds) = where_clause(f, w);
    let offset = offset.min(MAX_OFFSET);
    Ok(Q::new(
        "logs_search",
        format!(
            "SELECT {COLS} FROM {t} WHERE {wh} ORDER BY {TS} DESC LIMIT {} OFFSET {offset}",
            w.limit
        ),
        binds,
    ))
}

/// Bucket sizes (ms, label). The window picks the smallest giving at most 90 buckets.
pub const BUCKETS: &[(i64, &str)] = &[
    (1_000, "1 second"),
    (5_000, "5 seconds"),
    (10_000, "10 seconds"),
    (30_000, "30 seconds"),
    (60_000, "1 minute"),
    (300_000, "5 minutes"),
    (600_000, "10 minutes"),
    (1_800_000, "30 minutes"),
    (3_600_000, "1 hour"),
    (10_800_000, "3 hours"),
    (21_600_000, "6 hours"),
    (43_200_000, "12 hours"),
    (86_400_000, "1 day"),
];

pub fn bucket_for(window_ms: i64) -> (i64, &'static str) {
    *BUCKETS
        .iter()
        .find(|(ms, _)| window_ms / ms <= 90)
        .unwrap_or(&BUCKETS[BUCKETS.len() - 1])
}

pub fn histogram(db: &str, table: &str, f: &LogFilter, w: Window) -> anyhow::Result<Q> {
    let t = qualified(db, table)?;
    let (wh, binds) = where_clause(f, w);
    let (ms, _) = bucket_for(w.to_ms - w.from_ms);
    // Integer arithmetic on the nanosecond key instead of `date_bin(INTERVAL ..)`: the MySQL
    // prepared-statement parser rejects the interval literal form (verified live). `ms` comes
    // from the fixed BUCKETS set, so it is a literal we control.
    Ok(Q::new(
        "logs_histogram",
        format!(
            "SELECT (CAST({TS} AS BIGINT) / {}) * {ms} AS bucket_ms, count(*) AS n FROM {t} WHERE {wh} GROUP BY 1 ORDER BY 1",
            ms * 1_000_000
        ),
        binds,
    ))
}

pub fn top_values(
    db: &str,
    table: &str,
    col: &Col,
    f: &LogFilter,
    w: Window,
    k: u32,
) -> anyhow::Result<Q> {
    let t = qualified(db, table)?;
    let (x, mut binds) = expr(col);
    let (wh, wb) = where_clause(f, w);
    binds.extend(wb);
    Ok(Q::new(
        "logs_top_values",
        format!(
            "SELECT {x} AS value, count(*) AS n FROM {t} WHERE {wh} GROUP BY 1 ORDER BY 2 DESC LIMIT {}",
            k.clamp(1, TOP_VALUES_MAX)
        ),
        binds,
    ))
}

/// One record by its nanosecond timestamp. The window around it is derived, never requested.
pub fn record(db: &str, table: &str, ts_ns: i64) -> anyhow::Result<Q> {
    let t = qualified(db, table)?;
    let ms = ts_ns.div_euclid(1_000_000);
    Ok(Q::new(
        "logs_record",
        format!(
            "SELECT {COLS} FROM {t} WHERE {TS} >= ? AND {TS} < ? AND CAST({TS} AS BIGINT) = ? LIMIT 10"
        ),
        vec![
            Bind::Str(ts_string(ms - 1)),
            Bind::Str(ts_string(ms + 2)),
            Bind::Int(ts_ns),
        ],
    ))
}

/// `before` rows older than the pivot and `after` rows from the pivot on, in storage (time)
/// order, ignoring any filters. Bounded to a day each side.
pub fn around(db: &str, table: &str, ts_ns: i64, before: u32, after: u32) -> anyhow::Result<Q> {
    let t = qualified(db, table)?;
    let ms = ts_ns.div_euclid(1_000_000);
    Ok(Q::new(
        "logs_around",
        format!(
            "SELECT * FROM ((SELECT {COLS} FROM {t} WHERE {TS} >= ? AND CAST({TS} AS BIGINT) < ? ORDER BY {TS} DESC LIMIT {before}) \
             UNION ALL (SELECT {COLS} FROM {t} WHERE CAST({TS} AS BIGINT) >= ? AND {TS} < ? ORDER BY {TS} ASC LIMIT {after})) AS around ORDER BY `timestamp` ASC"
        ),
        vec![
            Bind::Str(ts_string(ms - super::DAY_MS)),
            Bind::Int(ts_ns),
            Bind::Int(ts_ns),
            Bind::Str(ts_string(ms + super::DAY_MS)),
        ],
    ))
}

/// `SHOW CREATE TABLE`, to see whether `body` has a fulltext index.
pub fn ddl(db: &str, table: &str) -> anyhow::Result<Q> {
    Ok(Q::new(
        "logs_ddl",
        format!("SHOW CREATE TABLE {}", qualified(db, table)?),
        vec![],
    ))
}

/// True when the `body` column definition carries a fulltext index.
pub fn has_fulltext(create_table: &str) -> bool {
    // Identifiers are double-quoted over HTTP and backticked over the MySQL protocol.
    create_table.lines().any(|l| {
        let l = l.trim_start();
        (l.starts_with("\"body\"") || l.starts_with("`body`")) && l.contains("FULLTEXT")
    })
}
