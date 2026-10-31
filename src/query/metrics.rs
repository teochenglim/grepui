//! Metrics (R-M1, R-M2): PromQL through `TQL EVAL`, the metric and label lists, and the shareable
//! spec of a metrics page.
//!
//! `TQL EVAL` cannot take bound parameters for its expression, so the PromQL text is embedded in
//! the statement. That is allowed only after [`check`]: one expression, no `;`, no control
//! characters, no backslash outside a string, and no `__schema__` / `__database__` matcher in any
//! spelling (a matcher on either selects another database; verified live on GreptimeDB v1.3.0).
//! The start, end and step are integers formatted by us.

use super::{Q, Window, ident};
use crate::error::ApiError;

pub const MAX_EXPR_LEN: usize = 2_000;
/// Most points of one series a range query may ask for; the step floor follows from it.
pub const MAX_POINTS: i64 = 1_000;
pub const MAX_STEP_S: i64 = 86_400;
/// Most rows read back from one query (series times points).
pub const MAX_SAMPLES: usize = 100_000;

const FORBIDDEN: &[&str] = &["__schema__", "__database__"];

/// Decode one PromQL string escape starting after the backslash; returns the character and how
/// many characters it consumed.
fn unescape(rest: &[char]) -> Option<(char, usize)> {
    let hex = |n: usize| -> Option<(char, usize)> {
        let digits: String = rest.get(1..=n)?.iter().collect();
        let c = char::from_u32(u32::from_str_radix(&digits, 16).ok()?)?;
        Some((c, n + 1))
    };
    match *rest.first()? {
        'a' => Some(('\u{7}', 1)),
        'b' => Some(('\u{8}', 1)),
        'f' => Some(('\u{c}', 1)),
        'n' => Some(('\n', 1)),
        'r' => Some(('\r', 1)),
        't' => Some(('\t', 1)),
        'v' => Some(('\u{b}', 1)),
        '\\' | '"' | '\'' | '`' => Some((rest[0], 1)),
        'x' => hex(2),
        'u' => hex(4),
        'U' => hex(8),
        '0'..='7' => {
            let digits: String = rest.get(..3)?.iter().collect();
            let c = char::from_u32(u32::from_str_radix(&digits, 8).ok()?)?;
            Some((c, 3))
        }
        _ => None,
    }
}

/// Accept `expr` only when it is safe to hand to `TQL EVAL`.
pub fn check(expr: &str) -> Result<(), ApiError> {
    let expr = expr.trim();
    let bad = |m: &str| Err(ApiError::bad(m.to_string()));
    if expr.is_empty() {
        return bad("the PromQL expression is empty");
    }
    if expr.chars().count() > MAX_EXPR_LEN {
        return bad("the PromQL expression is longer than 2000 characters");
    }
    let chars: Vec<char> = expr.chars().collect();
    // Every character the engine could read as a name, outside strings as written, inside strings
    // after decoding escapes.
    let mut seen = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == ';' || c == '#' || (c.is_control() && !c.is_whitespace()) {
            return bad("the PromQL expression contains a character that is not allowed");
        }
        match c {
            '\\' => return bad("a backslash is only allowed inside a quoted string"),
            '"' | '\'' | '`' => {
                i += 1;
                loop {
                    let Some(&d) = chars.get(i) else {
                        return bad("a string in the PromQL expression is not closed");
                    };
                    if d == c {
                        break;
                    }
                    if d == '\\' && c != '`' {
                        let Some((e, used)) = unescape(&chars[i + 1..]) else {
                            return bad("the PromQL expression has an invalid string escape");
                        };
                        seen.push(e);
                        i += 1 + used;
                    } else {
                        seen.push(d);
                        i += 1;
                    }
                }
                seen.push(' ');
            }
            _ => seen.push(c),
        }
        i += 1;
    }
    let lower = seen.to_ascii_lowercase();
    if FORBIDDEN.iter().any(|f| lower.contains(f)) {
        return bad(
            "__schema__ and __database__ select another database and are not allowed in PromQL",
        );
    }
    Ok(())
}

/// The evaluation grid: whole seconds, at most [`MAX_POINTS`] points.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Grid {
    pub start_s: i64,
    pub end_s: i64,
    pub step_s: i64,
}

impl Grid {
    /// A range over the window. The step is the one asked for, raised to the floor the window
    /// implies (so a month is never asked for per-second points) and capped at a day.
    pub fn range(w: Window, step_s: Option<i64>) -> Grid {
        let (start_s, end_s) = (w.from_ms / 1000, (w.to_ms / 1000).max(w.from_ms / 1000 + 1));
        let floor = ((end_s - start_s) + MAX_POINTS - 1) / MAX_POINTS;
        Grid {
            start_s,
            end_s,
            step_s: step_s.unwrap_or(0).max(floor).clamp(1, MAX_STEP_S),
        }
    }

    /// One evaluation at `t_ms`.
    pub fn instant(t_ms: i64) -> Grid {
        let t = t_ms / 1000;
        Grid {
            start_s: t,
            end_s: t,
            step_s: 1,
        }
    }
}

/// `TQL EVAL (start, end, 'step') <expr>`; the caller has run [`check`].
pub fn tql(expr: &str, g: Grid) -> Q {
    Q::new(
        "metrics_query",
        format!(
            "TQL EVAL ({}, {}, '{}s') {}",
            g.start_s,
            g.end_s,
            g.step_s,
            expr.trim()
        ),
        vec![],
    )
}

/// Logical metric tables of a database (not the physical table that stores them).
pub fn names(db: &str) -> Q {
    Q::new(
        "metrics_names",
        "SELECT table_name, create_options FROM information_schema.tables \
         WHERE table_schema = ? AND engine = 'metric' AND create_options NOT LIKE '%physical_metric_table%' \
         ORDER BY table_name LIMIT 2000",
        vec![super::Bind::Str(db.into())],
    )
}

/// `counter`, `gauge` or `unknown`, from the table options GreptimeDB records for OTLP metrics.
pub fn kind_of(create_options: &str) -> &'static str {
    if create_options.contains("greptime.semantic.metric.type=counter") {
        "counter"
    } else if create_options.contains("greptime.semantic.metric.type=gauge") {
        "gauge"
    } else {
        "unknown"
    }
}

/// A metric table name from a path.
pub fn metric_name(raw: &str) -> Result<&str, ApiError> {
    ident(raw).map_err(|_| ApiError::bad("invalid metric name"))
}

// ---- the shareable spec of a metrics page (R-M2) -------------------------------------------

pub const CHARTS: &[&str] = &["line", "bar", "table"];
pub const KINDS: &[&str] = &["promql", "sql"];
pub const RANGES: &[&str] = &["15m", "1h", "6h", "24h", "7d"];

/// What a metrics page shows: chart type, query language, the metric it is about, the query and
/// the time range. It travels in the URL query string and in saved views.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spec {
    pub chart: String,
    pub kind: String,
    pub stream: String,
    pub query: String,
    pub range: String,
}

impl Default for Spec {
    fn default() -> Self {
        Spec {
            chart: "line".into(),
            kind: "promql".into(),
            stream: String::new(),
            query: String::new(),
            range: "1h".into(),
        }
    }
}

fn pct_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn pct_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' => {
                let h = std::str::from_utf8(b.get(i + 1..i + 3)?).ok()?;
                out.push(u8::from_str_radix(h, 16).ok()?);
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

impl Spec {
    /// Check the closed sets and the lengths; the query itself is checked when it runs.
    pub fn validate(&self) -> Result<(), ApiError> {
        let bad = |m: &str| Err(ApiError::bad(m.to_string()));
        if !CHARTS.contains(&self.chart.as_str()) {
            return bad("chart must be line, bar or table");
        }
        if !KINDS.contains(&self.kind.as_str()) {
            return bad("kind must be promql or sql");
        }
        if !RANGES.contains(&self.range.as_str()) {
            return bad("unsupported range");
        }
        if !self.stream.is_empty() && ident(&self.stream).is_err() {
            return bad("invalid metric name");
        }
        if self.query.chars().count() > MAX_EXPR_LEN {
            return bad("the query is too long");
        }
        Ok(())
    }

    /// `chart=line&kind=promql&stream=...&q=...&range=1h`, keys in a fixed order, values encoded.
    pub fn to_query(&self) -> String {
        format!(
            "chart={}&kind={}&stream={}&q={}&range={}",
            pct_encode(&self.chart),
            pct_encode(&self.kind),
            pct_encode(&self.stream),
            pct_encode(&self.query),
            pct_encode(&self.range)
        )
    }

    /// The inverse of [`Spec::to_query`]. Missing keys keep their defaults; unknown keys are an
    /// error, so a mistyped link does not silently show something else.
    pub fn from_query(qs: &str) -> Result<Spec, ApiError> {
        let mut s = Spec::default();
        for pair in qs.split('&').filter(|p| !p.is_empty()) {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            let v = pct_decode(v).ok_or_else(|| ApiError::bad("invalid encoding in the link"))?;
            match k {
                "chart" => s.chart = v,
                "kind" => s.kind = v,
                "stream" => s.stream = v,
                "q" => s.query = v,
                "range" => s.range = v,
                other => return Err(ApiError::bad(format!("unknown key {other:?} in the link"))),
            }
        }
        s.validate()?;
        Ok(s)
    }
}

// ---- reading a TQL result -------------------------------------------------------------------

/// The labels of one series and its points so far.
type Grouped = (serde_json::Map<String, serde_json::Value>, Vec<(i64, f64)>);

/// Turn the rows of a `TQL EVAL` into series. A row has one time column, one value column and any
/// number of label columns; their names depend on the query (`ts`, `greptime_timestamp`,
/// `prom_rate(...)`), so the roles are told apart by type: labels are text (or null), the time is
/// the numeric column holding only integers (milliseconds), the value is the other numeric one.
pub fn to_series(rows: &[serde_json::Value]) -> Result<Vec<serde_json::Value>, ApiError> {
    use serde_json::{Map, Value, json};
    let Some(first) = rows.first().and_then(Value::as_object) else {
        return Ok(vec![]);
    };
    let keys: Vec<&String> = first.keys().collect();
    let numeric = |k: &str| rows.iter().all(|r| r[k].is_null() || r[k].is_number());
    let integral = |k: &str| {
        rows.iter()
            .all(|r| r[k].is_null() || r[k].is_i64() || r[k].is_u64())
    };
    let numbers: Vec<&&String> = keys
        .iter()
        .filter(|k| numeric(k) && rows.iter().any(|r| r[k.as_str()].is_number()))
        .collect();
    let time = numbers
        .iter()
        .find(|k| integral(k) && (k.contains("time") || k.starts_with("ts") || k.ends_with("ts")))
        .or_else(|| numbers.iter().find(|k| integral(k)))
        .map(|k| k.to_string());
    let value = numbers
        .iter()
        .find(|k| Some(k.to_string()) != time && rows.iter().any(|r| r[k.as_str()].is_f64()))
        .or_else(|| numbers.iter().find(|k| Some(k.to_string()) != time))
        .map(|k| k.to_string());
    let (Some(time), Some(value)) = (time, value) else {
        return Err(ApiError::bad(
            "the result has no time and value columns; use a PromQL expression that returns a vector",
        ));
    };
    let labels: Vec<&&String> = keys
        .iter()
        .filter(|k| ***k != time && ***k != value)
        .collect();
    let mut order: Vec<String> = vec![];
    let mut by: std::collections::HashMap<String, Grouped> = Default::default();
    for r in rows {
        let mut l = Map::new();
        for k in &labels {
            if !r[k.as_str()].is_null() {
                l.insert((***k).clone(), r[k.as_str()].clone());
            }
        }
        let key = Value::Object(l.clone()).to_string();
        let (Some(t), Some(v)) = (r[time.as_str()].as_i64(), r[value.as_str()].as_f64()) else {
            continue;
        };
        if !v.is_finite() {
            continue;
        }
        let e = by.entry(key.clone()).or_insert_with(|| {
            order.push(key.clone());
            (l, vec![])
        });
        e.1.push((t, v));
    }
    Ok(order
        .into_iter()
        .filter_map(|k| by.remove(&k))
        .map(|(l, mut p)| {
            p.sort_by_key(|(t, _)| *t);
            json!({"labels": l, "points": p.iter().map(|(t, v)| json!([t, v])).collect::<Vec<_>>()})
        })
        .collect())
}
