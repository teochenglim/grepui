//! Admin writes (R-F4, R-P3, R-X1): the only DDL and bulk writes grepui sends to telemetry
//! databases. Everything here is generated from fixed templates with validated identifiers; the
//! [`check`] gate re-validates the exact statement text before the writer sees it, so a bug
//! elsewhere cannot turn into arbitrary SQL. Every operation needs `--admin-writes` (the stand-in
//! for an admin role until access control, v0.11.0) and is logged for the audit trail.

use crate::{
    error::ApiError,
    org::Org,
    query::{Bind, Q, ident, qualified, ts_string},
};

/// Every kind of write this module can produce; a test pins the list (R-F4).
pub const ADMIN_WRITES: &[&str] = &[
    "CREATE TABLE (rollup sink)",
    "CREATE FLOW (rollup)",
    "DROP FLOW (rollup)",
    "DROP TABLE (rollup sink)",
    "INSERT INTO rollup sink SELECT (backfill)",
    "ALTER TABLE SET 'ttl' (traces, logs, rollup sink)",
    "CREATE TABLE (enrichment)",
    "INSERT INTO enrichment table VALUES",
    "DROP TABLE (enrichment)",
];

/// Rollup templates. Metrics downsampling is a later template (ROADMAP).
pub const TEMPLATES: &[&str] = &["traces_red"];
const MINUTE_MS: i64 = 60_000;
pub const DAY_MS: i64 = 86_400_000;
/// Longest range one backfill request may cover.
pub const BACKFILL_MAX_MS: i64 = 31 * DAY_MS;

const ENTRY: &str = "`span_kind` IN ('SPAN_KIND_SERVER', 'SPAN_KIND_CONSUMER')";
const ERR: &str = "`span_status_code` = 'STATUS_CODE_ERROR'";
const DUR: &str = "(CAST(`timestamp_end` AS BIGINT) - CAST(`timestamp` AS BIGINT))";

/// The rollup of one org: its source table, sink table and flow, all derived from validated names.
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub template: &'static str,
    pub db: String,
    pub source: String,
    pub sink: String,
    pub flow: String,
}

impl Plan {
    pub fn for_org(org: &Org, template: &str) -> Result<Plan, ApiError> {
        let template = TEMPLATES
            .iter()
            .copied()
            .find(|t| *t == template)
            .ok_or_else(|| ApiError::not_found("unknown rollup template"))?;
        let bad = |_| ApiError::bad("unsupported table name for a rollup");
        let sink = format!("{}_red_1m", org.traces_table);
        let flow = format!("grepui_{}_red_1m", org.database);
        ident(&sink).map_err(bad)?;
        ident(&flow).map_err(bad)?;
        ident(&org.database).map_err(bad)?;
        Ok(Plan {
            template,
            db: org.database.clone(),
            source: org.traces_table.clone(),
            sink,
            flow,
        })
    }

    fn select(&self, range: bool) -> String {
        let src = qualified(&self.db, &self.source).unwrap_or_default();
        format!(
            "SELECT `service_name`, date_bin('1 minute'::INTERVAL, `timestamp`) AS `time_window`, \
count(*) AS `requests`, sum(CASE WHEN {ERR} THEN 1 ELSE 0 END) AS `errors`, \
sum({DUR}) AS `sum_ns`, max({DUR}) AS `max_ns` FROM {src} \
WHERE {ENTRY} AND `service_name` IS NOT NULL{} GROUP BY `service_name`, `time_window`",
            if range {
                " AND `timestamp` >= ? AND `timestamp` < ?"
            } else {
                ""
            }
        )
    }

    fn sink_q(&self) -> String {
        qualified(&self.db, &self.sink).unwrap_or_default()
    }

    /// Sink table, then the flow that fills it.
    pub fn create(&self) -> Vec<String> {
        vec![
            format!(
                "CREATE TABLE IF NOT EXISTS {} (`service_name` STRING, `time_window` TIMESTAMP(3) NOT NULL, `requests` BIGINT, `errors` BIGINT, `sum_ns` BIGINT, `max_ns` BIGINT, TIME INDEX (`time_window`), PRIMARY KEY (`service_name`))",
                self.sink_q()
            ),
            format!(
                "CREATE FLOW IF NOT EXISTS `{}` SINK TO {} AS {}",
                self.flow,
                self.sink_q(),
                self.select(false)
            ),
        ]
    }

    pub fn drop(&self) -> Vec<String> {
        vec![
            format!("DROP FLOW IF EXISTS `{}`", self.flow),
            format!("DROP TABLE IF EXISTS {}", self.sink_q()),
        ]
    }

    /// One chunk of a backfill: whole minutes in `[from_ms, to_ms)`, bound.
    pub fn backfill(&self, from_ms: i64, to_ms: i64) -> Q {
        Q::new(
            "rollup_backfill",
            format!(
                "INSERT INTO {} (`service_name`, `time_window`, `requests`, `errors`, `sum_ns`, `max_ns`) {}",
                self.sink_q(),
                self.select(true)
            ),
            vec![Bind::Str(ts_string(from_ms)), Bind::Str(ts_string(to_ms))],
        )
    }
}

/// Whole-minute, day-sized chunks covering `[from_ms, to_ms)`. Both ends are rounded down to the
/// minute: a half-covered window would overwrite a complete one with a partial count.
pub fn chunks(from_ms: i64, to_ms: i64) -> Result<Vec<(i64, i64)>, ApiError> {
    let (from, to) = (
        from_ms - from_ms.rem_euclid(MINUTE_MS),
        to_ms - to_ms.rem_euclid(MINUTE_MS),
    );
    if from < 0 || to <= from {
        return Err(ApiError::bad(
            "to must be after from (at least one whole minute)",
        ));
    }
    if to - from > BACKFILL_MAX_MS {
        return Err(ApiError::bad(
            "a backfill covers at most 31 days per request",
        ));
    }
    let mut out = vec![];
    let mut s = from;
    while s < to {
        let e = (s + DAY_MS).min(to);
        out.push((s, e));
        s = e;
    }
    Ok(out)
}

// ---- retention -------------------------------------------------------------------------

/// A TTL we are willing to set: `forever`, or n hours, days or weeks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ttl {
    Forever,
    Secs(i64, String),
}

impl Ttl {
    /// `7d`, `48h`, `2w` (1 hour to 3650 days) or `forever`.
    pub fn parse(raw: &str) -> Result<Ttl, ApiError> {
        let bad =
            || ApiError::bad("ttl must look like 12h, 7d or 4w (1 hour to 3650 days), or forever");
        let t = raw.trim();
        if t == "forever" {
            return Ok(Ttl::Forever);
        }
        let unit = t.chars().last().ok_or_else(bad)?;
        let digits = &t[..t.len() - unit.len_utf8()];
        if digits.is_empty() || digits.len() > 4 || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return Err(bad());
        }
        let n: i64 = digits.parse().map_err(|_| bad())?;
        let secs = n * match unit {
            'h' => 3600,
            'd' => 86_400,
            'w' => 7 * 86_400,
            _ => return Err(bad()),
        };
        if !(3600..=3650 * 86_400).contains(&secs) {
            return Err(bad());
        }
        Ok(Ttl::Secs(secs, format!("{n}{unit}")))
    }

    pub fn text(&self) -> &str {
        match self {
            Ttl::Forever => "forever",
            Ttl::Secs(_, t) => t,
        }
    }

    pub fn secs(&self) -> Option<i64> {
        match self {
            Ttl::Forever => None,
            Ttl::Secs(s, _) => Some(*s),
        }
    }
}

/// Seconds in GreptimeDB's echo of a TTL (`ttl=7days`, `ttl=2months 29days 2h`, `ttl=forever`);
/// `None` for forever or no TTL. Months and years are approximations, so the text is shown too.
pub fn ttl_secs_from_options(create_options: &str) -> Option<i64> {
    let rest = create_options
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let at = rest.find("ttl=")?;
    let body = &rest[at + 4..];
    if body.starts_with("forever") {
        return None;
    }
    let mut total = 0f64;
    let mut any = false;
    for tok in body.split(' ') {
        let num: String = tok.chars().take_while(char::is_ascii_digit).collect();
        let unit = &tok[num.len()..];
        let Ok(n) = num.parse::<f64>() else { break };
        let per = match unit {
            "y" | "year" | "years" => 365.25 * 86_400.0,
            "month" | "months" => 30.44 * 86_400.0,
            "w" | "week" | "weeks" => 7.0 * 86_400.0,
            "d" | "day" | "days" => 86_400.0,
            "h" | "hr" | "hour" | "hours" => 3600.0,
            "m" | "min" | "minute" | "minutes" => 60.0,
            "s" | "sec" | "second" | "seconds" => 1.0,
            _ => break,
        };
        total += n * per;
        any = true;
    }
    any.then_some(total.round() as i64)
}

/// The raw text after `ttl=` up to the next ` key=` option, for display.
pub fn ttl_text_from_options(create_options: &str) -> Option<String> {
    let at = create_options.find("ttl=")?;
    let rest = &create_options[at + 4..];
    // Options are space separated; a TTL value may itself contain spaces ("2months 29days").
    let end = rest
        .split(' ')
        .scan(false, |stop, tok| {
            if *stop || tok.contains('=') {
                *stop = true;
                None
            } else {
                Some(tok)
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
    Some(end)
}

/// The streams whose retention can be changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    Traces,
    Logs,
    Rollup,
}

impl Stream {
    pub const ALL: [Stream; 3] = [Stream::Traces, Stream::Logs, Stream::Rollup];

    pub fn parse(s: &str) -> Result<Stream, ApiError> {
        match s {
            "traces" => Ok(Stream::Traces),
            "logs" => Ok(Stream::Logs),
            "rollup" => Ok(Stream::Rollup),
            _ => Err(ApiError::bad("stream must be traces, logs or rollup")),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Stream::Traces => "traces",
            Stream::Logs => "logs",
            Stream::Rollup => "rollup",
        }
    }

    /// `(table, time column)` for the org.
    pub fn table(self, org: &Org) -> Result<(String, &'static str), ApiError> {
        Ok(match self {
            Stream::Traces => (org.traces_table.clone(), "timestamp"),
            Stream::Logs => (org.logs_table.clone(), "timestamp"),
            Stream::Rollup => (Plan::for_org(org, "traces_red")?.sink, "time_window"),
        })
    }
}

/// `ALTER TABLE db.table SET 'ttl' = '7d'`.
pub fn ttl_ddl(org: &Org, stream: Stream, ttl: &Ttl) -> Result<String, ApiError> {
    let (table, _) = stream.table(org)?;
    let t = qualified(&org.database, &table)
        .map_err(|_| ApiError::bad("unsupported table name for retention"))?;
    Ok(format!("ALTER TABLE {t} SET 'ttl' = '{}'", ttl.text()))
}

/// Tables and counts the retention page reads.
pub fn retention_options(db: &str, tables: &[String]) -> Q {
    let marks = vec!["?"; tables.len()].join(", ");
    let mut binds = vec![Bind::Str(db.into())];
    binds.extend(tables.iter().map(|t| Bind::Str(t.clone())));
    Q::new(
        "retention_options",
        format!(
            "SELECT table_name, create_options FROM information_schema.tables WHERE table_schema = ? AND table_name IN ({marks})"
        ),
        binds,
    )
}

/// Rows in the table and how many are older than `cutoff_ms`.
pub fn retention_counts(
    db: &str,
    table: &str,
    ts_col: &'static str,
    cutoff_ms: i64,
) -> anyhow::Result<Q> {
    let t = qualified(db, table)?;
    Ok(Q::new(
        "retention_counts",
        format!(
            "SELECT count(*) AS total, sum(CASE WHEN `{ts_col}` < ? THEN 1 ELSE 0 END) AS older FROM {t}"
        ),
        vec![Bind::Str(ts_string(cutoff_ms))],
    ))
}

/// The sink table and flow of an org's rollup, as the catalog reports them.
pub fn rollup_flow(flow: &str) -> Q {
    Q::new(
        "rollup_flow",
        "SELECT flow_name, created_time FROM information_schema.flows WHERE flow_name = ?",
        vec![Bind::Str(flow.into())],
    )
}

// ---- the gate --------------------------------------------------------------------------

/// Accept only a statement this module generates for `org`. Called right before the writer runs.
pub fn check(sql: &str, binds: usize, org: &Org) -> Result<(), ApiError> {
    let denied = || {
        ApiError(
            axum::http::StatusCode::FORBIDDEN,
            "refusing an admin statement that is not from a template".into(),
        )
    };
    if crate::enrich::gate(sql, binds, org) {
        return Ok(());
    }
    let plan = Plan::for_org(org, "traces_red")?;
    if plan
        .create()
        .iter()
        .chain(plan.drop().iter())
        .any(|s| s == sql)
    {
        return Ok(());
    }
    let backfill = plan.backfill(0, 0);
    let body = backfill
        .sql
        .strip_prefix(&format!("/* q:{} */ ", backfill.name))
        .unwrap_or_default();
    if sql == body && binds == 2 {
        return Ok(());
    }
    for stream in Stream::ALL {
        let (table, _) = stream.table(org)?;
        let prefix = format!(
            "ALTER TABLE {} SET 'ttl' = '",
            qualified(&org.database, &table).map_err(|_| denied())?
        );
        if let Some(rest) = sql.strip_prefix(&prefix)
            && let Some(dur) = rest.strip_suffix('\'')
            && Ttl::parse(dur).is_ok_and(|t| t.text() == dur)
        {
            return Ok(());
        }
    }
    Err(denied())
}
