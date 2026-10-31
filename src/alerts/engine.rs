//! The evaluator (R-A1 – R-A8). Every replica runs the same tick; a lease per rule in
//! `grepui_alert_items` decides which replica evaluates it (R-A7, ADR 0010). One tick reads the
//! rules, states, leases, destinations, downtimes and incidents of an org, then runs the due rules:
//! evaluate, apply the operator, gate by downtime / cooldown / dedup, notify, record.

use super::{
    Destination, Downtime, Kind, Rule,
    cond::{self, Gate, Lease, LeaseAction},
    incident::Incident,
    notify::{Message, Sender},
    template,
};
use crate::{
    api::Shared,
    error::{ApiError, ReadError},
    org::Org,
    query::{Q, Window, metrics, run_max, sqlmode},
    store::build,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
    time::Duration,
};

pub const TICK_S: u64 = 5;
pub const LEASE_S: i64 = 30;
const SETTLE_MS: u64 = 300;
const ROWS_MAX: usize = 1000;
const FIRE_MAX: usize = 10;
const SAMPLES_SHOWN: usize = 20;
/// A rule over its budget waits this many intervals before it is tried again.
const BUDGET_PAUSE: i64 = 6;

pub const STATE_KIND: &str = "state";

/// What the engine remembers about a rule between ticks.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RuleState {
    /// `ok`, `firing` or `error`; empty before the first evaluation.
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub last_eval_ms: i64,
    #[serde(default)]
    pub last_fire_ms: Option<i64>,
    #[serde(default)]
    pub last_value: Option<f64>,
    /// How long the last evaluation took.
    #[serde(default)]
    pub last_ms: i64,
    #[serde(default)]
    pub skip_until_ms: i64,
    /// Fingerprint -> when it last notified.
    #[serde(default)]
    pub fingerprints: BTreeMap<String, i64>,
    #[serde(default)]
    pub error: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Sample {
    pub labels: BTreeMap<String, String>,
    pub value: f64,
    /// The compared value (ratio or difference) when the rule looks at the past.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compared: Option<f64>,
    pub breached: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Series {
    pub labels: BTreeMap<String, String>,
    pub points: Vec<(i64, f64)>,
    /// Indices of the points that are outliers against the ones before them.
    pub flagged: Vec<usize>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Evaluation {
    pub samples: Vec<Sample>,
    pub series: Vec<Series>,
    pub notes: Vec<String>,
    pub ms: i64,
    pub sql: Vec<String>,
}

impl Evaluation {
    pub fn firing(&self) -> bool {
        self.samples.iter().any(|s| s.breached)
    }
}

// ---- store helpers ---------------------------------------------------------------------------

pub fn item_of(r: &Value) -> build::ItemRow {
    build::ItemRow {
        item_id: r["item_id"].as_str().unwrap_or_default().into(),
        name: r["name"].as_str().unwrap_or_default().into(),
        spec: r["spec"].as_str().unwrap_or_default().into(),
        owner: r["owner"].as_str().unwrap_or_default().into(),
        created_ms: r["created_ms"].as_i64().unwrap_or(0),
        updated_ms: r["updated_ms"].as_i64().unwrap_or(0),
        deleted: false,
    }
}

pub async fn items(s: &Shared, org: &Org, kind: &str) -> Result<Vec<build::ItemRow>, ApiError> {
    let q = build::item_list(s.meta_db(), &org.name, kind).map_err(internal)?;
    let (rows, _) = s.read_store(q).await?;
    Ok(rows.iter().map(item_of).collect())
}

pub async fn item(
    s: &Shared,
    org: &Org,
    kind: &str,
    id: &str,
) -> Result<Option<build::ItemRow>, ApiError> {
    let q = build::item_get(s.meta_db(), &org.name, kind, id).map_err(internal)?;
    let (rows, _) = s.read_store(q).await?;
    Ok(rows.first().map(item_of))
}

pub async fn save(
    s: &Shared,
    org: &Org,
    kind: &str,
    r: &build::ItemRow,
) -> Result<crate::query::Meta, ApiError> {
    let q = build::item_put(s.meta_db(), &org.name, kind, r).map_err(internal)?;
    s.write(q).await
}

fn internal(e: anyhow::Error) -> ApiError {
    tracing::error!(error = %e, "alert store");
    ApiError(
        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        "internal error".into(),
    )
}

fn rule_of(r: &build::ItemRow) -> Option<Rule> {
    serde_json::from_str(&r.spec).ok()
}

fn state_of(r: &build::ItemRow) -> RuleState {
    serde_json::from_str(&r.spec).unwrap_or_default()
}

pub async fn save_state(
    s: &Shared,
    org: &Org,
    id: &str,
    st: &RuleState,
    now: i64,
) -> Result<(), ApiError> {
    let row = build::ItemRow {
        item_id: id.into(),
        name: id.into(),
        spec: serde_json::to_string(st).unwrap_or_default(),
        owner: "engine".into(),
        // The row is overwritten in place: constant ts (= created_ms) and a new updated_ms.
        created_ms: 0,
        updated_ms: now,
        deleted: false,
    };
    save(s, org, STATE_KIND, &row).await.map(|_| ())
}

// ---- running a rule's query ------------------------------------------------------------------

/// A series' labels and its (time, value) points.
type Points = (BTreeMap<String, String>, Vec<(i64, f64)>);

struct Raw {
    labels: BTreeMap<String, String>,
    value: f64,
}

fn text_of(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// One row per series: the last numeric column is the value, the others are labels.
fn rows_to_raw(rows: &[Value]) -> Result<Vec<Raw>, String> {
    let Some(first) = rows.first().and_then(Value::as_object) else {
        return Ok(vec![]);
    };
    let value_key = first
        .keys()
        .rev()
        .find(|k| rows.iter().any(|r| r[k.as_str()].is_number()))
        .ok_or("the query returned no numeric column")?
        .clone();
    Ok(rows
        .iter()
        .filter_map(|r| {
            let v = r[value_key.as_str()].as_f64().filter(|v| v.is_finite())?;
            let labels = r
                .as_object()?
                .iter()
                .filter(|(k, _)| **k != value_key)
                .map(|(k, v)| (k.clone(), text_of(v)))
                .collect();
            Some(Raw { labels, value: v })
        })
        .collect())
}

fn db_error(e: ReadError) -> String {
    match e {
        ReadError::Timeout => "the query took too long".into(),
        ReadError::Db(d) => {
            tracing::warn!(detail = %d, "alert query failed");
            "the database could not run the query".into()
        }
    }
}

fn window_of(now: i64, lookback_s: i64, back_ms: i64) -> Window {
    Window {
        from_ms: (now - back_ms - lookback_s * 1000).max(1),
        to_ms: (now - back_ms).max(2),
        limit: 1000,
    }
}

async fn run_sql(
    s: &Shared,
    org: &Org,
    q: &super::Query,
    w: Window,
    ev: &mut Evaluation,
) -> Result<Vec<Value>, String> {
    let query = if q.kind == "builder" {
        let b = q.builder.as_ref().ok_or("the builder is missing")?;
        super::builder_sql(org, b, w).map_err(|e| e.to_string())?
    } else {
        let sql = sqlmode::expand_macros(&q.text, w.from_ms, w.to_ms);
        sqlmode::check(&sql, &org.database).map_err(|r| r.0)?;
        Q::new("alert_rule", sql, vec![])
    };
    ev.sql.push(query.sql.clone());
    run_max(s.reader.as_ref(), query, ROWS_MAX)
        .await
        .map(|(rows, _)| rows)
        .map_err(db_error)
}

async fn run_promql(
    s: &Shared,
    org: &Org,
    text: &str,
    grid: metrics::Grid,
    ev: &mut Evaluation,
) -> Result<Vec<Value>, String> {
    metrics::check(text).map_err(|e| e.1)?;
    if s.cfg.database().ok().as_deref() != Some(org.database.as_str()) {
        return Err("PromQL is only available for the org on the connection's database".into());
    }
    let q = metrics::tql(text, grid);
    ev.sql.push(q.sql.clone());
    let (rows, _) = run_max(s.reader.as_ref(), q, metrics::MAX_SAMPLES)
        .await
        .map_err(db_error)?;
    metrics::to_series(&rows).map_err(|e| e.1)
}

fn labels_of(v: &Value) -> BTreeMap<String, String> {
    v["labels"]
        .as_object()
        .map(|o| o.iter().map(|(k, v)| (k.clone(), text_of(v))).collect())
        .unwrap_or_default()
}

/// Current values of a threshold rule over the window ending `back_ms` before `now`.
async fn current(
    s: &Shared,
    org: &Org,
    rule: &Rule,
    now: i64,
    back_ms: i64,
    ev: &mut Evaluation,
) -> Result<Vec<Raw>, String> {
    let q = rule.query.as_ref().ok_or("the rule has no query")?;
    let w = window_of(now, rule.lookback_s, back_ms);
    if q.kind == "promql" {
        let series = run_promql(s, org, &q.text, metrics::Grid::instant(w.to_ms), ev).await?;
        return Ok(series
            .iter()
            .filter_map(|x| {
                let v = x["points"].as_array()?.last()?.get(1)?.as_f64()?;
                Some(Raw {
                    labels: labels_of(x),
                    value: v,
                })
            })
            .collect());
    }
    rows_to_raw(&run_sql(s, org, q, w, ev).await?)
}

/// Evaluate a scheduled or real-time rule's condition (R-A1, R-A5 compare-with-past).
async fn threshold(
    s: &Shared,
    org: &Org,
    rule: &Rule,
    now: i64,
    ev: &mut Evaluation,
) -> Result<(), String> {
    let c = rule.condition.as_ref().ok_or("the rule has no condition")?;
    let now_vals = current(s, org, rule, now, 0, ev).await?;
    let past = match &c.compare {
        Some(cmp) => Some(current(s, org, rule, now, cmp.offset_s * 1000, ev).await?),
        None => None,
    };
    for r in now_vals {
        let (compared, breached) = match (&c.compare, &past) {
            (Some(cmp), Some(past)) => {
                let then = past.iter().find(|p| p.labels == r.labels).map(|p| p.value);
                let v = cond::compare_value(&cmp.mode, r.value, then);
                (v, v.is_some_and(|v| cond::breach(c.op, v, c.threshold)))
            }
            _ => (None, cond::breach(c.op, r.value, c.threshold)),
        };
        ev.samples.push(Sample {
            labels: r.labels,
            value: r.value,
            compared,
            breached,
        });
    }
    if c.compare.is_some() && ev.samples.iter().any(|s| s.compared.is_none()) {
        ev.notes
            .push("some series had no value (or a zero) in the comparison window".into());
    }
    Ok(())
}

/// Evaluate an anomaly rule: the newest point against the median and MAD of the ones before it.
async fn anomaly(
    s: &Shared,
    org: &Org,
    rule: &Rule,
    now: i64,
    ev: &mut Evaluation,
) -> Result<(), String> {
    let a = rule
        .anomaly
        .as_ref()
        .ok_or("the rule has no anomaly options")?;
    let q = rule.query.as_ref().ok_or("the rule has no query")?;
    let w = window_of(now, rule.lookback_s, 0);
    let mut all: Vec<Points> = vec![];
    if q.kind == "promql" {
        for x in run_promql(s, org, &q.text, metrics::Grid::range(w, None), ev).await? {
            let pts = x["points"]
                .as_array()
                .map(|p| {
                    p.iter()
                        .filter_map(|p| Some((p.get(0)?.as_i64()?, p.get(1)?.as_f64()?)))
                        .collect()
                })
                .unwrap_or_default();
            all.push((labels_of(&x), pts));
        }
    } else {
        // SQL: the first column is the time, the last numeric one the value, the rest labels.
        let rows = run_sql(s, org, q, w, ev).await?;
        let mut by: BTreeMap<BTreeMap<String, String>, Vec<(i64, f64)>> = BTreeMap::new();
        if let Some(first) = rows.first().and_then(Value::as_object) {
            let keys: Vec<&String> = first.keys().collect();
            let value_key = keys
                .iter()
                .skip(1)
                .rev()
                .find(|k| rows.iter().any(|r| r[k.as_str()].is_number()))
                .ok_or("an anomaly query returns a time column first and a number after it")?;
            for (i, r) in rows.iter().enumerate() {
                let Some(v) = r[value_key.as_str()].as_f64().filter(|v| v.is_finite()) else {
                    continue;
                };
                let t = r[keys[0].as_str()].as_i64().unwrap_or(i as i64);
                let labels = keys
                    .iter()
                    .skip(1)
                    .filter(|k| **k != *value_key)
                    .map(|k| (k.to_string(), text_of(&r[k.as_str()])))
                    .collect();
                by.entry(labels).or_default().push((t, v));
            }
        }
        all.extend(by);
    }
    for (labels, points) in all {
        let values: Vec<f64> = points.iter().map(|p| p.1).collect();
        let Some((&last, base)) = values.split_last() else {
            continue;
        };
        let flagged = cond::flag_all(&values, a);
        let verdict = cond::judge(base, last, a);
        let breached = matches!(verdict, cond::Verdict::Outlier { .. });
        if let cond::Verdict::NeedsHistory { have, need } = verdict {
            ev.notes
                .push(format!("needs history: {have} of {need} baseline points"));
        }
        ev.samples.push(Sample {
            labels: labels.clone(),
            value: last,
            compared: None,
            breached,
        });
        ev.series.push(Series {
            labels,
            points,
            flagged,
        });
    }
    Ok(())
}

fn firing_in(states: &HashMap<String, RuleState>) -> impl Fn(&str) -> bool + '_ {
    move |id| states.get(id).is_some_and(|s| s.state == "firing")
}

/// Evaluate any rule without side effects (the preview and the tick share it).
pub async fn evaluate(
    s: &Shared,
    org: &Org,
    rule: &Rule,
    now: i64,
    states: &HashMap<String, RuleState>,
) -> Result<Evaluation, String> {
    let t = std::time::Instant::now();
    let mut ev = Evaluation::default();
    match rule.kind {
        Kind::Composite => {
            let e = rule
                .composite
                .as_ref()
                .ok_or("the rule has no expression")?;
            let on = super::eval_expr(e, &firing_in(states));
            ev.samples.push(Sample {
                labels: BTreeMap::new(),
                value: if on { 1.0 } else { 0.0 },
                compared: None,
                breached: on,
            });
        }
        Kind::Anomaly => anomaly(s, org, rule, now, &mut ev).await?,
        Kind::Scheduled | Kind::Realtime => threshold(s, org, rule, now, &mut ev).await?,
    }
    ev.ms = t.elapsed().as_millis() as i64;
    Ok(ev)
}

// ---- notifications ---------------------------------------------------------------------------

fn fmt_num(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        format!("{v:.4}")
    }
}

fn threshold_text(rule: &Rule) -> String {
    match (&rule.condition, &rule.anomaly) {
        (Some(c), _) => {
            let op = serde_json::to_value(c.op)
                .ok()
                .and_then(|v| v.as_str().map(String::from))
                .unwrap_or_default();
            format!("{op} {}", fmt_num(c.threshold))
        }
        (_, Some(a)) => format!("outside median ± {}·MAD", fmt_num(a.k)),
        _ => String::new(),
    }
}

/// Variables, rendered text and payload for one notification.
pub fn build_message(
    org: &str,
    alert_id: &str,
    rule: &Rule,
    state: &str,
    sample: &Sample,
    now: i64,
) -> (Message, Vec<String>) {
    let labels = sample
        .labels
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(", ");
    let mut vars: BTreeMap<String, String> = BTreeMap::from([
        ("alert.name".into(), rule.name.clone()),
        ("alert.id".into(), alert_id.into()),
        ("state".into(), state.into()),
        ("value".into(), fmt_num(sample.value)),
        ("threshold".into(), threshold_text(rule)),
        ("labels".into(), labels),
        (
            "time".into(),
            chrono::DateTime::from_timestamp_millis(now)
                .map(|d| d.to_rfc3339())
                .unwrap_or_default(),
        ),
        ("org".into(), org.into()),
        ("outcome".into(), state.into()),
    ]);
    for (k, v) in &sample.labels {
        vars.insert(format!("labels.{k}"), v.clone());
    }
    let subject = if rule.template.subject.trim().is_empty() {
        template::DEFAULT_SUBJECT
    } else {
        &rule.template.subject
    };
    let body = if rule.template.body.trim().is_empty() {
        template::DEFAULT_BODY
    } else {
        &rule.template.body
    };
    let (subject, mut unknown) = template::render(subject, &vars);
    let (body, u2) = template::render(body, &vars);
    unknown.extend(u2);
    let subject = subject.replace('\n', " ");
    let payload = json!({
        "alert": {"id": alert_id, "name": rule.name},
        "state": state, "value": sample.value, "labels": sample.labels,
        "time_ms": now, "org": org, "subject": subject, "message": body,
    });
    (
        Message {
            subject,
            body,
            payload,
        },
        unknown,
    )
}

/// Send to every destination; one result per destination.
pub async fn deliver(
    sender: &dyn Sender,
    dests: &[(String, Option<Destination>)],
    m: &Message,
) -> Vec<Value> {
    let mut out = vec![];
    for (id, d) in dests {
        let r = match d {
            Some(d) => sender.send(d, m).await,
            None => Err("the destination no longer exists".into()),
        };
        out.push(match r {
            Ok(()) => json!({"destination": id, "ok": true}),
            Err(e) => json!({"destination": id, "ok": false, "error": e}),
        });
    }
    out
}

// ---- the tick --------------------------------------------------------------------------------

/// Is the rule due at `now`?
pub fn due(rule: &Rule, st: &RuleState, now: i64) -> bool {
    if now < st.skip_until_ms {
        return false;
    }
    if let Some(expr) = &rule.schedule.cron {
        let Ok(c) = super::cron::Cron::parse(expr) else {
            return false;
        };
        let base = if st.last_eval_ms == 0 {
            now - 60_000
        } else {
            st.last_eval_ms
        };
        return c.next_after(base).is_some_and(|t| t <= now);
    }
    st.last_eval_ms == 0 || now - st.last_eval_ms >= rule.interval_ms()
}

pub struct Engine {
    s: Shared,
    sender: Arc<dyn Sender>,
    pub holder: String,
    settle: Duration,
}

struct Ctx {
    dests: HashMap<String, Destination>,
    downtimes: Vec<Downtime>,
    leases: HashMap<String, Lease>,
    states: HashMap<String, RuleState>,
    /// alert id -> (incident id, incident)
    incidents: HashMap<String, (String, Incident)>,
}

impl Engine {
    pub fn new(s: Shared, sender: Arc<dyn Sender>) -> Engine {
        Engine {
            s,
            sender,
            holder: format!("{:016x}", rand::random::<u64>()),
            settle: Duration::from_millis(SETTLE_MS),
        }
    }

    /// Tests do not wait for a claim to settle.
    pub fn with_settle(mut self, d: Duration) -> Engine {
        self.settle = d;
        self
    }

    async fn load(&self, org: &Org) -> Result<(Vec<(String, Rule)>, Ctx), ApiError> {
        let s = &self.s;
        let rules: Vec<(String, Rule)> = items(s, org, "alert")
            .await?
            .iter()
            .filter_map(|r| rule_of(r).map(|x| (r.item_id.clone(), x)))
            .collect();
        let dests = items(s, org, "destination")
            .await?
            .iter()
            .filter_map(|r| Some((r.item_id.clone(), serde_json::from_str(&r.spec).ok()?)))
            .collect();
        let downtimes = items(s, org, "downtime")
            .await?
            .iter()
            .filter_map(|r| serde_json::from_str(&r.spec).ok())
            .collect();
        let leases = items(s, org, "lease")
            .await?
            .iter()
            .filter_map(|r| {
                let v: Value = serde_json::from_str(&r.spec).ok()?;
                Some((
                    r.item_id.clone(),
                    Lease {
                        holder: v["holder"].as_str()?.into(),
                        expires_ms: v["expires_ms"].as_i64()?,
                    },
                ))
            })
            .collect();
        let states = items(s, org, STATE_KIND)
            .await?
            .iter()
            .map(|r| (r.item_id.clone(), state_of(r)))
            .collect();
        let incidents = items(s, org, "incident")
            .await?
            .iter()
            .filter_map(|r| {
                let i: Incident = serde_json::from_str(&r.spec).ok()?;
                i.is_active()
                    .then(|| (i.alert_id.clone(), (r.item_id.clone(), i)))
            })
            .collect();
        Ok((
            rules,
            Ctx {
                dests,
                downtimes,
                leases,
                states,
                incidents,
            },
        ))
    }

    fn lease_of(r: &build::ItemRow) -> Option<Lease> {
        let v: Value = serde_json::from_str(&r.spec).ok()?;
        Some(Lease {
            holder: v["holder"].as_str()?.into(),
            expires_ms: v["expires_ms"].as_i64()?,
        })
    }

    async fn write_lease(&self, org: &Org, id: &str, now: i64) -> Result<(), ApiError> {
        let row = build::ItemRow {
            item_id: id.into(),
            name: id.into(),
            spec: json!({"holder": self.holder, "expires_ms": now + LEASE_S * 1000}).to_string(),
            owner: self.holder.clone(),
            created_ms: 0,
            updated_ms: now,
            deleted: false,
        };
        save(&self.s, org, "lease", &row).await.map(|_| ())
    }

    /// Keep the leases this replica holds alive, so a rule stays with one replica between runs
    /// and a replica that stops loses them after `LEASE_S`.
    async fn renew(&self, org: &Org, rules: &[(String, Rule)], ctx: &mut Ctx, now: i64) {
        for (id, rule) in rules {
            if !rule.enabled {
                continue;
            }
            let mine = ctx.leases.get(id).is_some_and(|l| l.holder == self.holder);
            if mine
                && cond::lease_action(ctx.leases.get(id), &self.holder, now, LEASE_S * 1000)
                    == LeaseAction::Claim
                && self.write_lease(org, id, now).await.is_ok()
            {
                ctx.leases.insert(
                    id.clone(),
                    Lease {
                        holder: self.holder.clone(),
                        expires_ms: now + LEASE_S * 1000,
                    },
                );
            }
        }
    }

    /// Take or keep the lease of one rule; `true` when this replica evaluates it now. The lease
    /// is read again just before claiming (the tick's snapshot may be old), a claim is a write
    /// followed by a short wait and a read: the last writer holds it (GreptimeDB has no
    /// compare-and-set, ADR 0010).
    async fn lead(&self, org: &Org, id: &str, ctx: &mut Ctx, now: i64) -> bool {
        let ttl = LEASE_S * 1000;
        let fresh = item(&self.s, org, "lease", id)
            .await
            .ok()
            .flatten()
            .and_then(|r| Self::lease_of(&r));
        match fresh {
            Some(l) => ctx.leases.insert(id.to_string(), l),
            None => ctx.leases.remove(id),
        };
        match cond::lease_action(ctx.leases.get(id), &self.holder, now, ttl) {
            LeaseAction::Follow => false,
            LeaseAction::Hold => true,
            LeaseAction::Claim => {
                if let Err(e) = self.write_lease(org, id, now).await {
                    tracing::warn!(error = %e.1, "could not claim an alert lease");
                    return false;
                }
                if !self.settle.is_zero() {
                    tokio::time::sleep(self.settle).await;
                }
                let cur = item(&self.s, org, "lease", id)
                    .await
                    .ok()
                    .flatten()
                    .and_then(|r| Self::lease_of(&r));
                let mine = cur.as_ref().is_some_and(|l| l.holder == self.holder);
                if let Some(l) = cur {
                    ctx.leases.insert(id.to_string(), l);
                }
                mine
            }
        }
    }

    async fn record(
        &self,
        org: &Org,
        id: &str,
        state: &str,
        outcome: &str,
        detail: &Value,
        now: i64,
    ) {
        let r = build::alert_history_put(
            self.s.meta_db(),
            &org.name,
            id,
            state,
            outcome,
            &detail.to_string(),
            now,
        );
        match r {
            Ok(q) => {
                if let Err(e) = self.s.write(q).await {
                    tracing::warn!(error = %e.1, "could not record alert history");
                }
            }
            Err(e) => tracing::error!(error = %e, "alert history"),
        }
    }

    async fn open_incident(&self, org: &Org, id: &str, rule: &Rule, ctx: &mut Ctx, now: i64) {
        if let Some((iid, inc)) = ctx.incidents.get_mut(id) {
            // Already open: note that it fired again.
            inc.push(now, "fired_again", "engine", "");
            let row = build::ItemRow {
                item_id: iid.clone(),
                name: inc.title.clone(),
                spec: serde_json::to_string(inc).unwrap_or_default(),
                owner: "engine".into(),
                created_ms: inc.created_ms,
                updated_ms: now,
                deleted: false,
            };
            let _ = save(&self.s, org, "incident", &row).await;
            return;
        }
        let inc = Incident::open(id, &rule.name, now, "engine", "the alert fired");
        let iid = format!("{:08x}", rand::random::<u32>());
        let row = build::ItemRow {
            item_id: iid.clone(),
            name: inc.title.clone(),
            spec: serde_json::to_string(&inc).unwrap_or_default(),
            owner: "engine".into(),
            created_ms: now,
            updated_ms: now,
            deleted: false,
        };
        if save(&self.s, org, "incident", &row).await.is_ok() {
            ctx.incidents.insert(id.to_string(), (iid, inc));
        }
    }

    /// Evaluate one due rule and act on the result.
    async fn run_rule(&self, org: &Org, id: &str, rule: &Rule, ctx: &mut Ctx, now: i64) {
        let mut st = ctx.states.get(id).cloned().unwrap_or_default();
        // R-A2: a real-time rule whose last run cost more than its budget pauses for a while.
        if rule.kind == Kind::Realtime && cond::over_budget(st.last_ms, rule.budget_ms) {
            let over = st.last_ms;
            st.last_ms = 0;
            st.skip_until_ms = now + BUDGET_PAUSE * rule.interval_ms();
            let detail = json!({"last_ms": over, "budget_ms": rule.budget_ms, "paused_until_ms": st.skip_until_ms});
            self.record(org, id, &st.state, "skipped_budget", &detail, now)
                .await;
            let _ = save_state(&self.s, org, id, &st, now).await;
            ctx.states.insert(id.to_string(), st);
            return;
        }
        let ev = match evaluate(&self.s, org, rule, now, &ctx.states).await {
            Ok(ev) => ev,
            Err(e) => {
                st.state = "error".into();
                st.error = e.clone();
                st.last_eval_ms = now;
                self.record(org, id, "error", "error", &json!({"error": e}), now)
                    .await;
                let _ = save_state(&self.s, org, id, &st, now).await;
                ctx.states.insert(id.to_string(), st);
                return;
            }
        };
        let prev_fire = st.last_fire_ms;
        st.last_eval_ms = now;
        st.last_ms = ev.ms;
        st.error.clear();
        st.last_value = ev.samples.first().map(|s| s.value);
        let breached: Vec<&Sample> = ev.samples.iter().filter(|s| s.breached).collect();
        let (mut outcome, mut fired, mut suppressed, mut deliveries) =
            ("ok", vec![], vec![], vec![]);
        if breached.is_empty() {
            st.state = "ok".into();
            if ev.notes.iter().any(|n| n.starts_with("needs history")) {
                outcome = "needs_history";
            }
        } else {
            st.state = "firing".into();
            let silenced = ctx.downtimes.iter().any(|d| d.covers(id, now));
            if silenced {
                outcome = "silenced";
            }
            let dests: Vec<(String, Option<Destination>)> = rule
                .destinations
                .iter()
                .map(|d| (d.clone(), ctx.dests.get(d).cloned()))
                .collect();
            for sample in breached.iter().take(FIRE_MAX) {
                if silenced {
                    break;
                }
                let fp = cond::fingerprint(id, &sample.labels, &rule.dedup.fields);
                let g = cond::gate(
                    now,
                    prev_fire,
                    rule.cooldown_s,
                    st.fingerprints.get(&fp).copied(),
                    rule.dedup.window_s,
                );
                if g != Gate::Fire {
                    let why = if g == Gate::Cooldown {
                        "cooldown"
                    } else {
                        "duplicate"
                    };
                    suppressed.push(json!({"fingerprint": fp, "reason": why}));
                    if outcome == "ok" {
                        outcome = if g == Gate::Cooldown {
                            "suppressed_cooldown"
                        } else {
                            "suppressed_dedup"
                        };
                    }
                    continue;
                }
                let (m, unknown) = build_message(&org.name, id, rule, "firing", sample, now);
                if !unknown.is_empty() {
                    tracing::warn!(rule = id, ?unknown, "template uses unknown variables");
                }
                deliveries.extend(deliver(self.sender.as_ref(), &dests, &m).await);
                st.fingerprints.insert(fp.clone(), now);
                st.last_fire_ms = Some(now);
                fired.push(fp);
                outcome = "fired";
            }
            if !fired.is_empty() && rule.open_incident {
                self.open_incident(org, id, rule, ctx, now).await;
            }
        }
        // Forget fingerprints that can no longer suppress anything.
        let keep = (rule.dedup.window_s.max(rule.cooldown_s).max(60)) * 1000;
        st.fingerprints.retain(|_, t| now - *t < keep);
        let shown: Vec<&Sample> = ev.samples.iter().take(SAMPLES_SHOWN).collect();
        let detail = json!({
            "samples": shown, "fired": fired, "suppressed": suppressed,
            "deliveries": deliveries, "notes": ev.notes, "ms": ev.ms,
        });
        self.record(org, id, &st.state, outcome, &detail, now).await;
        let _ = save_state(&self.s, org, id, &st, now).await;
        ctx.states.insert(id.to_string(), st);
    }

    /// One pass over an org's rules. Returns the ids of the rules this replica evaluated.
    pub async fn tick(&self, org: &Org) -> Result<Vec<String>, ApiError> {
        let now = self.s.now_ms();
        let (rules, mut ctx) = self.load(org).await?;
        let mut ran = vec![];
        self.renew(org, &rules, &mut ctx, now).await;
        // Rules with a query first, so composites see this tick's states.
        let (composite, plain): (Vec<_>, Vec<_>) = rules
            .iter()
            .filter(|(_, r)| r.enabled)
            .partition(|(_, r)| r.kind == Kind::Composite);
        for (id, rule) in plain.into_iter().chain(composite) {
            let st = ctx.states.get(id).cloned().unwrap_or_default();
            if !due(rule, &st, now) || !self.lead(org, id, &mut ctx, now).await {
                continue;
            }
            // Another replica may have run it since the snapshot was taken.
            if let Ok(Some(r)) = item(&self.s, org, STATE_KIND, id).await {
                let fresh = state_of(&r);
                let still_due = due(rule, &fresh, now);
                ctx.states.insert(id.clone(), fresh);
                if !still_due {
                    continue;
                }
            }
            self.run_rule(org, id, rule, &mut ctx, now).await;
            ran.push(id.clone());
        }
        Ok(ran)
    }
}

/// Start the scheduler in the background. Every replica runs it; the leases keep a rule on one.
pub fn spawn(s: Shared, sender: Arc<dyn Sender>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let engine = Engine::new(s.clone(), sender);
        tracing::info!(holder = %engine.holder, "alert scheduler started");
        let mut last_err = String::new();
        loop {
            for org in s.orgs.list() {
                match engine.tick(org).await {
                    Ok(_) => last_err.clear(),
                    Err(e) if e.1 != last_err => {
                        tracing::warn!(org = %org.name, error = %e.1, "alert tick failed");
                        last_err = e.1;
                    }
                    Err(_) => {}
                }
            }
            tokio::time::sleep(Duration::from_secs(TICK_S)).await;
        }
    })
}
