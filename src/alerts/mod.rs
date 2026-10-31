//! Alerts (R-A1 – R-A8): the spec of a rule, destination and downtime, validated into one
//! canonical form. Rules run their saved query on the server; the browser never sends SQL to be
//! run on a schedule. Evaluation is `engine`, the pure decisions are `cond`, `cron`, `template`.

pub mod cond;
pub mod cron;
pub mod engine;
pub mod incident;
pub mod notify;
pub mod template;

use crate::{
    error::ApiError,
    org::Org,
    query::{Bind, Q, Window, qualified, ts_string},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

pub const EXPORT_MARK: &str = "grepui_alert";
pub const BUNDLE_MARK: &str = "grepui_alerts";
pub const MAX_NAME: usize = 100;
pub const MAX_QUERY: usize = 8_000;
pub const MAX_TEMPLATE: usize = 2_000;
pub const MAX_DESTINATIONS: usize = 10;
pub const MAX_EXPR_NODES: usize = 20;
pub const MAX_EXPR_DEPTH: usize = 6;
pub const MAX_LOOKBACK_S: i64 = 31 * 86_400;
pub const MAX_COOLDOWN_S: i64 = 7 * 86_400;
pub const DEFAULT_REALTIME_S: i64 = 10;
pub const MIN_REALTIME_S: i64 = 5;
pub const MAX_REALTIME_S: i64 = 60;
pub const MIN_SCHEDULED_S: i64 = 30;
pub const DEFAULT_BUDGET_MS: i64 = 2_000;

fn bad<T>(m: impl Into<String>) -> Result<T, ApiError> {
    Err(ApiError::bad(m))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Scheduled,
    Realtime,
    Composite,
    Anomaly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Op {
    #[serde(rename = ">")]
    Gt,
    #[serde(rename = ">=")]
    Ge,
    #[serde(rename = "<")]
    Lt,
    #[serde(rename = "<=")]
    Le,
    #[serde(rename = "==")]
    Eq,
    #[serde(rename = "!=")]
    Ne,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Filter {
    pub field: String,
    pub value: String,
}

/// A rule's query built from fields instead of written as SQL.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Builder {
    /// `logs` or `traces`: the org's table of that name.
    pub source: String,
    /// `count`, `avg`, `min`, `max` or `sum`.
    #[serde(default = "count")]
    pub agg: String,
    /// The column aggregated (not for `count`).
    #[serde(default)]
    pub field: String,
    #[serde(default)]
    pub filters: Vec<Filter>,
    #[serde(default)]
    pub group_by: Vec<String>,
}

fn count() -> String {
    "count".into()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Query {
    /// `sql`, `promql` or `builder`.
    pub kind: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub builder: Option<Builder>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Compare {
    /// How far back the comparison window sits.
    pub offset_s: i64,
    /// `ratio` (now / then) or `diff` (now - then).
    #[serde(default = "ratio")]
    pub mode: String,
}

fn ratio() -> String {
    "ratio".into()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Condition {
    pub op: Op,
    pub threshold: f64,
    #[serde(default)]
    pub compare: Option<Compare>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Schedule {
    #[serde(default)]
    pub every_s: Option<i64>,
    #[serde(default)]
    pub cron: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Dedup {
    /// Label names that identify "the same alert"; empty means one fingerprint per rule.
    #[serde(default)]
    pub fields: Vec<String>,
    #[serde(default)]
    pub window_s: i64,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Template {
    #[serde(default)]
    pub subject: String,
    #[serde(default)]
    pub body: String,
}

/// AND / OR / NOT over the states of other rules.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Expr {
    Alert(String),
    And(Vec<Expr>),
    Or(Vec<Expr>),
    Not(Box<Expr>),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Anomaly {
    /// Distance from the median in MADs that counts as an outlier.
    #[serde(default = "three")]
    pub k: f64,
    /// Points before the checked one that form the baseline.
    #[serde(default = "thirty")]
    pub window: usize,
    /// Fewer baseline points than this and the rule stays quiet.
    #[serde(default = "ten")]
    pub min_points: usize,
    /// `both`, `high` or `low`.
    #[serde(default = "both")]
    pub direction: String,
}

fn three() -> f64 {
    3.0
}
fn thirty() -> usize {
    30
}
fn ten() -> usize {
    10
}
fn both() -> String {
    "both".into()
}

fn yes() -> bool {
    true
}

fn five_min() -> i64 {
    300
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub name: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default)]
    pub description: String,
    /// A free label to group rules (the list page filters on it).
    #[serde(default)]
    pub folder: String,
    pub kind: Kind,
    #[serde(default)]
    pub query: Option<Query>,
    #[serde(default)]
    pub condition: Option<Condition>,
    #[serde(default)]
    pub schedule: Schedule,
    #[serde(default = "five_min")]
    pub lookback_s: i64,
    #[serde(default)]
    pub cooldown_s: i64,
    /// Real-time rules: a last evaluation slower than this pauses the rule for a while.
    #[serde(default)]
    pub budget_ms: Option<i64>,
    #[serde(default)]
    pub dedup: Dedup,
    #[serde(default)]
    pub destinations: Vec<String>,
    #[serde(default)]
    pub template: Template,
    #[serde(default)]
    pub composite: Option<Expr>,
    #[serde(default)]
    pub anomaly: Option<Anomaly>,
    #[serde(default)]
    pub open_incident: bool,
}

fn label(s: &str, what: &str, max: usize) -> Result<String, ApiError> {
    let t = s.trim();
    if t.is_empty() || t.chars().count() > max || t.chars().any(char::is_control) {
        return bad(format!(
            "{what} must be 1-{max} characters without control characters"
        ));
    }
    Ok(t.to_string())
}

fn plain(s: &str, what: &str, max: usize) -> Result<(), ApiError> {
    if s.chars().count() > max || s.chars().any(|c| c.is_control() && c != '\n' && c != '\t') {
        return bad(format!(
            "{what} must be at most {max} characters without control characters"
        ));
    }
    Ok(())
}

pub fn valid_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 32
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
}

fn ident_ok(s: &str) -> bool {
    crate::query::ident(s).is_ok()
}

fn walk(e: &Expr, depth: usize, nodes: &mut usize, refs: &mut Vec<String>) -> Result<(), ApiError> {
    *nodes += 1;
    if depth > MAX_EXPR_DEPTH || *nodes > MAX_EXPR_NODES {
        return bad(format!(
            "composite expression is too large (at most {MAX_EXPR_NODES} terms, {MAX_EXPR_DEPTH} deep)"
        ));
    }
    match e {
        Expr::Alert(id) if valid_id(id) => refs.push(id.clone()),
        Expr::Alert(_) => return bad("composite refers to an invalid alert id"),
        Expr::And(v) | Expr::Or(v) => {
            if v.is_empty() {
                return bad("and / or need at least one term");
            }
            for x in v {
                walk(x, depth + 1, nodes, refs)?;
            }
        }
        Expr::Not(x) => walk(x, depth + 1, nodes, refs)?,
    }
    Ok(())
}

/// Every alert a composite expression mentions.
pub fn expr_refs(e: &Expr) -> Result<Vec<String>, ApiError> {
    let (mut n, mut refs) = (0, vec![]);
    walk(e, 0, &mut n, &mut refs)?;
    Ok(refs)
}

impl Rule {
    /// Check a rule and return it in canonical form (trimmed, defaults filled in).
    pub fn canonical(mut self) -> Result<Rule, ApiError> {
        self.name = label(&self.name, "name", MAX_NAME)?;
        self.folder = self.folder.trim().to_string();
        if !self.folder.is_empty() {
            label(&self.folder, "folder", 50)?;
        }
        plain(&self.description, "description", 500)?;
        plain(&self.template.subject, "template subject", 200)?;
        plain(&self.template.body, "template body", MAX_TEMPLATE)?;
        if !(1..=MAX_LOOKBACK_S).contains(&self.lookback_s) {
            return bad("lookback_s must be between 1 second and 31 days");
        }
        if !(0..=MAX_COOLDOWN_S).contains(&self.cooldown_s) {
            return bad("cooldown_s must be between 0 and 7 days");
        }
        if self.destinations.len() > MAX_DESTINATIONS {
            return bad(format!("at most {MAX_DESTINATIONS} destinations"));
        }
        if let Some(d) = self.destinations.iter().find(|d| !valid_id(d)) {
            return bad(format!("invalid destination id {d:?}"));
        }
        self.destinations.sort();
        self.destinations.dedup();
        if self.dedup.fields.len() > 5 || self.dedup.fields.iter().any(|f| !ident_ok(f)) {
            return bad("dedup.fields: at most 5 label names (letters, digits, _)");
        }
        if !(0..=MAX_COOLDOWN_S).contains(&self.dedup.window_s) {
            return bad("dedup.window_s must be between 0 and 7 days");
        }
        self.schedule_check()?;
        match self.kind {
            Kind::Composite => {
                if self.query.is_some() || self.condition.is_some() || self.anomaly.is_some() {
                    return bad("a composite rule has an expression, not a query or condition");
                }
                let Some(e) = &self.composite else {
                    return bad("a composite rule needs a composite expression");
                };
                expr_refs(e)?;
            }
            Kind::Scheduled | Kind::Realtime | Kind::Anomaly => {
                if self.composite.is_some() {
                    return bad("only composite rules have a composite expression");
                }
                let Some(q) = &self.query else {
                    return bad("this rule needs a query");
                };
                query_check(q)?;
                if self.kind == Kind::Anomaly {
                    if self.condition.is_some() {
                        return bad("an anomaly rule has no threshold; set the anomaly options");
                    }
                    let a = self.anomaly.get_or_insert(Anomaly {
                        k: 3.0,
                        window: 30,
                        min_points: 10,
                        direction: "both".into(),
                    });
                    if !(a.k.is_finite() && (0.5..=20.0).contains(&a.k)) {
                        return bad("anomaly.k must be between 0.5 and 20");
                    }
                    if !(5..=500).contains(&a.window) || a.min_points < 3 || a.min_points > a.window
                    {
                        return bad(
                            "anomaly.window must be 5-500 and min_points 3 or more, at most the window",
                        );
                    }
                    if !["both", "high", "low"].contains(&a.direction.as_str()) {
                        return bad("anomaly.direction must be both, high or low");
                    }
                } else {
                    let Some(c) = &self.condition else {
                        return bad("this rule needs a condition");
                    };
                    if !c.threshold.is_finite() {
                        return bad("threshold must be a finite number");
                    }
                    if let Some(cmp) = &c.compare {
                        if !(1..=MAX_LOOKBACK_S).contains(&cmp.offset_s) {
                            return bad("compare.offset_s must be between 1 second and 31 days");
                        }
                        if !["ratio", "diff"].contains(&cmp.mode.as_str()) {
                            return bad("compare.mode must be ratio or diff");
                        }
                    }
                    if self.anomaly.is_some() {
                        return bad("only anomaly rules have anomaly options");
                    }
                }
            }
        }
        if self.kind == Kind::Realtime {
            let b = self.budget_ms.get_or_insert(DEFAULT_BUDGET_MS);
            if !(50..=60_000).contains(b) {
                return bad("budget_ms must be between 50 and 60000");
            }
        } else if self.budget_ms.is_some() {
            return bad("only real-time rules have a cost budget");
        }
        Ok(self)
    }

    fn schedule_check(&mut self) -> Result<(), ApiError> {
        let real = self.kind == Kind::Realtime;
        let (lo, hi) = if real {
            (MIN_REALTIME_S, MAX_REALTIME_S)
        } else {
            (MIN_SCHEDULED_S, 86_400)
        };
        match (&self.schedule.every_s, &self.schedule.cron) {
            (Some(_), Some(_)) => return bad("set every_s or cron, not both"),
            (None, None) => {
                self.schedule.every_s = Some(if real { DEFAULT_REALTIME_S } else { 60 });
            }
            (Some(e), None) if !(lo..=hi).contains(e) => {
                return bad(format!("every_s must be between {lo} and {hi} seconds"));
            }
            (None, Some(c)) => {
                if real {
                    return bad("real-time rules use every_s, not cron");
                }
                cron::Cron::parse(c).map_err(ApiError::bad)?;
            }
            _ => {}
        }
        Ok(())
    }

    pub fn interval_ms(&self) -> i64 {
        self.schedule.every_s.unwrap_or(60) * 1000
    }
}

/// Reject a rule's query before it is ever saved or run.
fn query_check(q: &Query) -> Result<(), ApiError> {
    match q.kind.as_str() {
        "sql" => {
            if q.text.trim().is_empty() || q.text.len() > MAX_QUERY || q.builder.is_some() {
                return bad("the SQL must be 1-8000 characters");
            }
            // The window macros expand to literals; check the statement the way a run would.
            let sql = crate::query::sqlmode::expand_macros(&q.text, 0, 1);
            crate::query::sqlmode::check(&sql, "x_checked_at_run").or_else(|r| {
                // The database name is only known at run time; a statement that names another
                // database is rejected then. Everything else is rejected now.
                if r.0.contains("database") {
                    Ok(())
                } else {
                    bad(r.0)
                }
            })
        }
        "promql" => {
            if q.text.len() > MAX_QUERY || q.builder.is_some() {
                return bad("the PromQL must be at most 8000 characters");
            }
            crate::query::metrics::check(&q.text)
        }
        "builder" => {
            if !q.text.is_empty() {
                return bad("a builder query has no text");
            }
            let Some(b) = &q.builder else {
                return bad("a builder query needs a builder");
            };
            builder_check(b)
        }
        _ => bad("query.kind must be sql, promql or builder"),
    }
}

fn builder_check(b: &Builder) -> Result<(), ApiError> {
    if !["logs", "traces"].contains(&b.source.as_str()) {
        return bad("builder.source must be logs or traces");
    }
    if !["count", "avg", "min", "max", "sum"].contains(&b.agg.as_str()) {
        return bad("builder.agg must be count, avg, min, max or sum");
    }
    if b.agg == "count" {
        if !b.field.is_empty() {
            return bad("builder.field is only for avg, min, max and sum");
        }
    } else if !ident_ok(&b.field) {
        return bad("builder.field must be a column name");
    }
    if b.filters.len() > 10 || b.filters.iter().any(|f| !ident_ok(&f.field)) {
        return bad("builder.filters: at most 10, each on a column name");
    }
    if b.filters.iter().any(|f| f.value.chars().count() > 500) {
        return bad("builder.filters values are limited to 500 characters");
    }
    if b.group_by.len() > 3 || b.group_by.iter().any(|g| !ident_ok(g)) {
        return bad("builder.group_by: at most 3 column names");
    }
    Ok(())
}

/// The SELECT for a builder query over a window: group columns first (as labels), the value last.
pub fn builder_sql(org: &Org, b: &Builder, w: Window) -> anyhow::Result<Q> {
    let table = if b.source == "logs" {
        &org.logs_table
    } else {
        &org.traces_table
    };
    let t = qualified(&org.database, table)?;
    let ts = "`timestamp`";
    let mut cols: Vec<String> = vec![];
    for g in &b.group_by {
        cols.push(format!(
            "CAST(`{}` AS STRING) AS `{}`",
            crate::query::ident(g)?,
            g
        ));
    }
    let agg = if b.agg == "count" {
        "CAST(count(*) AS DOUBLE)".to_string()
    } else {
        format!(
            "CAST({}(`{}`) AS DOUBLE)",
            b.agg,
            crate::query::ident(&b.field)?
        )
    };
    cols.push(format!("{agg} AS `value`"));
    let mut binds = vec![
        Bind::Str(ts_string(w.from_ms)),
        Bind::Str(ts_string(w.to_ms)),
    ];
    let mut wh = format!("{ts} >= ? AND {ts} < ?");
    for f in &b.filters {
        wh.push_str(&format!(
            " AND CAST(`{}` AS STRING) = ?",
            crate::query::ident(&f.field)?
        ));
        binds.push(Bind::Str(f.value.clone()));
    }
    let mut sql = format!("SELECT {} FROM {t} WHERE {wh}", cols.join(", "));
    if !b.group_by.is_empty() {
        let n = b.group_by.len();
        let idx: Vec<String> = (1..=n).map(|i| i.to_string()).collect();
        sql.push_str(&format!(
            " GROUP BY {} ORDER BY {} LIMIT 200",
            idx.join(", "),
            idx.join(", ")
        ));
    }
    Ok(Q::new("alert_builder", sql, binds))
}

// ---- destinations and downtimes ------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Destination {
    pub name: String,
    /// `webhook` or `email`.
    pub kind: String,
    /// Webhook: an http:// URL.
    #[serde(default)]
    pub url: String,
    /// Email: recipient addresses.
    #[serde(default)]
    pub to: Vec<String>,
}

pub fn valid_email(a: &str) -> bool {
    let Some((local, domain)) = a.split_once('@') else {
        return false;
    };
    a.len() <= 254
        && !local.is_empty()
        && local.len() <= 64
        && domain.contains('.')
        && !a
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || "<>\",;:\\()[]".contains(c))
        && a.matches('@').count() == 1
}

/// A webhook target: plain http, a host, no credentials, not the unspecified or link-local
/// addresses. (grepui has no TLS; an https endpoint needs a relay on the local network.)
pub fn webhook_target(raw: &str) -> Result<url::Url, ApiError> {
    let u =
        url::Url::parse(raw.trim()).map_err(|_| ApiError::bad("the webhook URL is not valid"))?;
    if u.scheme() != "http" {
        return bad(
            "webhooks must be http:// URLs (grepui has no TLS client; use a relay for https)",
        );
    }
    if !u.username().is_empty() || u.password().is_some() {
        return bad("the webhook URL must not contain credentials");
    }
    match u.host() {
        None => return bad("the webhook URL has no host"),
        Some(url::Host::Ipv4(ip))
            if ip.is_unspecified() || ip.is_link_local() || ip.is_broadcast() =>
        {
            return bad("that webhook address is not allowed");
        }
        Some(url::Host::Ipv6(ip)) if ip.is_unspecified() => {
            return bad("that webhook address is not allowed");
        }
        _ => {}
    }
    Ok(u)
}

impl Destination {
    pub fn canonical(mut self) -> Result<Destination, ApiError> {
        self.name = label(&self.name, "name", MAX_NAME)?;
        match self.kind.as_str() {
            "webhook" => {
                if !self.to.is_empty() {
                    return bad("a webhook has a url, not recipients");
                }
                self.url = webhook_target(&self.url)?.to_string();
                if self.url.len() > 500 {
                    return bad("the webhook URL is longer than 500 characters");
                }
            }
            "email" => {
                if !self.url.is_empty() {
                    return bad("an email destination has recipients, not a url");
                }
                if self.to.is_empty() || self.to.len() > 20 {
                    return bad("an email destination needs 1-20 recipients");
                }
                if let Some(a) = self.to.iter().find(|a| !valid_email(a)) {
                    return bad(format!("{a:?} is not a valid email address"));
                }
            }
            _ => return bad("kind must be webhook or email"),
        }
        Ok(self)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Downtime {
    pub name: String,
    pub from_ms: i64,
    pub to_ms: i64,
    /// Rule ids this silences; empty means every rule.
    #[serde(default)]
    pub alerts: Vec<String>,
}

impl Downtime {
    pub fn canonical(mut self) -> Result<Downtime, ApiError> {
        self.name = label(&self.name, "name", MAX_NAME)?;
        if self.from_ms < 0 || self.to_ms <= self.from_ms {
            return bad("to_ms must be after from_ms");
        }
        if self.to_ms - self.from_ms > 366 * 86_400_000 {
            return bad("a downtime lasts at most a year");
        }
        if self.alerts.len() > 100 || self.alerts.iter().any(|a| !valid_id(a)) {
            return bad("alerts: at most 100 valid alert ids");
        }
        Ok(self)
    }

    pub fn covers(&self, alert_id: &str, now_ms: i64) -> bool {
        self.from_ms <= now_ms
            && now_ms < self.to_ms
            && (self.alerts.is_empty() || self.alerts.iter().any(|a| a == alert_id))
    }
}

// ---- composite cycles --------------------------------------------------------------------

/// Would saving `id` with `expr` close a loop among the composite rules? `others` maps every
/// other composite rule's id to the alerts its expression mentions.
pub fn has_cycle(id: &str, expr: &Expr, others: &BTreeMap<String, Vec<String>>) -> bool {
    let mut seen = BTreeSet::new();
    let mut stack: Vec<String> = expr_refs(expr).unwrap_or_default();
    while let Some(n) = stack.pop() {
        if n == id {
            return true;
        }
        if !seen.insert(n.clone()) {
            continue;
        }
        if let Some(next) = others.get(&n) {
            stack.extend(next.iter().cloned());
        }
    }
    false
}

/// Evaluate a composite expression over the known states (`true` = firing). An alert that has
/// no state yet counts as not firing.
pub fn eval_expr(e: &Expr, firing: &dyn Fn(&str) -> bool) -> bool {
    match e {
        Expr::Alert(id) => firing(id),
        Expr::And(v) => v.iter().all(|x| eval_expr(x, firing)),
        Expr::Or(v) => v.iter().any(|x| eval_expr(x, firing)),
        Expr::Not(x) => !eval_expr(x, firing),
    }
}

/// Parse an `Rule` from request JSON, mapping serde errors to a readable 400.
pub fn parse_rule(v: &Value) -> Result<Rule, ApiError> {
    serde_json::from_value::<Rule>(v.clone())
        .map_err(|e| ApiError::bad(format!("invalid alert: {e}")))?
        .canonical()
}
