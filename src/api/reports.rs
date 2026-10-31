//! Scheduled reports (R-E1, ADR 0014): CRUD, a preview, "send now", and the scheduler that sends a
//! report once per schedule slot across replicas. A report is rendered in-process, so it is masked
//! by the org's redaction rules as a viewer (the recipients are not known to be org members).

use super::{Shared, ok};
use crate::{
    alerts::{
        Destination,
        cond::{Lease, LeaseAction, lease_action},
        cron::Cron,
        engine::{item, items, save},
        notify::{Message, Sender},
    },
    auth::Identity,
    dash::panels,
    error::ApiError,
    org::Org,
    query::{
        Meta, Q, Window,
        filter::{FtMode, LogFilter},
        ident, logs, run_max, sqlmode,
    },
    reports::{self, Report, ReportState, Section},
    store::build::{self as sbuild, ItemRow},
};
use axum::{
    Extension, Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::Response,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};

const KIND: &str = "report";
const STATE_KIND: &str = "reportstate";
const LEASE_S: i64 = 120;
const SETTLE_MS: u64 = 800;
const TICK_S: u64 = 30;
const VIEW_ROWS: usize = 1000;

fn meta(name: &str) -> Meta {
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

fn new_id() -> String {
    format!("{:016x}", rand::random::<u64>())
}

fn id_of(id: &str) -> Result<String, ApiError> {
    if id.len() == 16 && id.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(id.to_ascii_lowercase())
    } else {
        Err(ApiError::bad("invalid report id"))
    }
}

fn report_of(r: &ItemRow) -> Option<Report> {
    serde_json::from_str(&r.spec).ok()
}

fn state_of(r: &ItemRow) -> ReportState {
    serde_json::from_str(&r.spec).unwrap_or_default()
}

// ---- rendering ---------------------------------------------------------------------------------

/// Mask rows as a viewer would see them. Without loaded rules nothing is sent.
fn mask(s: &Shared, org: &str, rows: &mut Value) -> Result<(), String> {
    if !s.redact_ready.load(std::sync::atomic::Ordering::SeqCst) {
        return Err("redaction rules are not loaded yet; the report was not sent".into());
    }
    let rules = s.redact.read().ok().and_then(|m| m.get(org).cloned());
    if let Some(r) = rules {
        r.apply_value(rows, "viewer");
    }
    // A report goes to people outside the app: prompts and completions are never in it whole.
    let mode = super::llm::effective_mode(false, super::llm::content_mode(s, org), true);
    crate::llm::protect(rows, mode);
    Ok(())
}

fn section(title: &str, columns: Vec<String>, rows: Vec<Value>, note: Option<String>) -> Section {
    Section {
        title: title.into(),
        rows: reports::table(&columns, &rows),
        columns,
        note,
    }
}

async fn dashboard_sections(
    s: &Shared,
    org: &Org,
    id: &str,
    w: Window,
) -> Result<(String, Vec<Section>), String> {
    let row = super::dashboards::load(s, org, id, "dashboard")
        .await
        .map_err(|e| e.1)?;
    let spec: Value = serde_json::from_str(&row.spec).map_err(|_| "the dashboard is damaged")?;
    let dq = super::dashboards::query_for(w).map_err(|e| e.1)?;
    let mut out = vec![];
    for tab in spec["tabs"].as_array().into_iter().flatten() {
        for p in tab["panels"].as_array().into_iter().flatten() {
            let (Some(pid), Some(ty)) = (p["id"].as_str(), p["type"].as_str()) else {
                continue;
            };
            if panels::info(ty).is_none_or(|i| i.static_content) {
                continue;
            }
            let title = p["title"].as_str().filter(|t| !t.is_empty()).unwrap_or(pid);
            match super::dashboards::panel_data(s, org, &spec, pid, &dq).await {
                Ok((mut v, _)) => {
                    let mut rows = v["rows"].take();
                    mask(s, &org.name, &mut rows)?;
                    let cols: Vec<String> = v["columns"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|c| c.as_str().map(String::from))
                        .collect();
                    let note = v["shape_error"].as_str().map(String::from);
                    out.push(section(
                        title,
                        cols,
                        rows.as_array().cloned().unwrap_or_default(),
                        note,
                    ));
                }
                // One broken panel does not stop the report; it says so where the panel would be.
                Err(e) => out.push(Section {
                    title: title.into(),
                    columns: vec![],
                    rows: vec![],
                    note: Some(format!("this panel could not be run: {}", e.1)),
                }),
            }
        }
    }
    Ok((row.title, out))
}

fn iso(ms: &Value) -> String {
    ms.as_i64()
        .and_then(chrono::DateTime::from_timestamp_millis)
        .map(|d| d.format("%Y-%m-%d %H:%M:%S%.3f").to_string())
        .unwrap_or_default()
}

async fn view_sections(
    s: &Shared,
    org: &Org,
    id: &str,
    w: Window,
) -> Result<(String, Vec<Section>), String> {
    let (rows, _) = s
        .read_store(sbuild::view_get(s.meta_db(), &org.name, id).map_err(|e| e.to_string())?)
        .await
        .map_err(|e| e.1)?;
    let row = rows.first().ok_or("no such saved view")?;
    let title = row["name"].as_str().unwrap_or("Saved view").to_string();
    if row["kind"] != "logs" {
        return Err("only saved logs views can be reported on".into());
    }
    let spec: Value = row["spec"]
        .as_str()
        .and_then(|t| serde_json::from_str(t).ok())
        .unwrap_or_else(|| json!({}));
    let w = Window {
        limit: VIEW_ROWS as u32,
        ..w
    };
    let sql = spec["sql"].as_str().unwrap_or_default();
    let (cols, mut data) = if spec["mode"] == "sql" && !sql.trim().is_empty() {
        let (sql, binds) = super::functions::expand_sql(s, org, sql, vec![])
            .await
            .map_err(|e| e.1)?;
        let sql = sqlmode::expand_macros(&sql, w.from_ms, w.to_ms);
        sqlmode::check(&sql, &org.database).map_err(|r| r.0)?;
        let (rows, _) = run_max(
            s.reader.as_ref(),
            Q::new("report_view", &sql, binds),
            VIEW_ROWS,
        )
        .await
        .map_err(|e| ApiError::from(e).1)?;
        let cols: Vec<String> = rows
            .first()
            .and_then(Value::as_object)
            .map(|o| o.keys().cloned().collect())
            .unwrap_or_default();
        (cols, rows)
    } else {
        let probe = LogFilter::new(spec["q"].as_str(), spec["level"].as_str(), FtMode::Term)
            .map_err(|e| e.1)?;
        let f = if probe.parsed.terms.is_empty() {
            probe
        } else {
            LogFilter {
                ft: super::logs::ft_mode(s, org).await,
                ..probe
            }
        };
        let q = logs::search(&org.database, &org.logs_table, &f, w, 0)
            .map_err(|_| "unsupported logs table name")?;
        let (mut rows, _) = run_max(s.reader.as_ref(), q, VIEW_ROWS)
            .await
            .map_err(|e| ApiError::from(e).1)?;
        for r in &mut rows {
            r["timestamp"] = Value::String(iso(&r["timestamp"]));
        }
        let cols = ["timestamp", "severity_text", "body", "trace_id"]
            .map(String::from)
            .to_vec();
        (cols, rows)
    };
    let mut v = Value::Array(std::mem::take(&mut data));
    mask(s, &org.name, &mut v)?;
    let note = (v.as_array().map_or(0, Vec::len) >= VIEW_ROWS)
        .then(|| format!("limited to the newest {VIEW_ROWS} rows"));
    let sec = section(
        &title,
        cols,
        v.as_array().cloned().unwrap_or_default(),
        note,
    );
    Ok((title, vec![sec]))
}

/// A rendered report: what would be mailed.
pub struct Rendered {
    pub subject: String,
    pub body: String,
    pub html: bool,
    pub sections: usize,
    pub rows: usize,
}

pub async fn render(s: &Shared, org: &Org, rep: &Report, at_ms: i64) -> Result<Rendered, String> {
    let w = Window {
        from_ms: (at_ms - rep.range_hours as i64 * 3_600_000).max(1),
        to_ms: at_ms.max(2),
        limit: VIEW_ROWS as u32,
    };
    let (title, sections) = match rep.kind.as_str() {
        "dashboard" => dashboard_sections(s, org, &rep.target, w).await?,
        _ => view_sections(s, org, &rep.target, w).await?,
    };
    let day = chrono::DateTime::from_timestamp_millis(at_ms)
        .map(|d| d.format("%Y-%m-%d").to_string())
        .unwrap_or_default();
    let range = format!(
        "{} · {} to {} UTC",
        org.name,
        iso(&json!(w.from_ms)),
        iso(&json!(w.to_ms))
    );
    let html = rep.format == "html";
    Ok(Rendered {
        subject: format!("[grepui] {} ({day})", rep.name),
        body: if html {
            reports::render_html(&format!("{} · {title}", rep.name), &range, &sections)
        } else {
            reports::render_csv(&sections)
        },
        html,
        sections: sections.len(),
        rows: sections.iter().map(|x| x.rows.len()).sum(),
    })
}

async fn deliver(
    s: &Shared,
    sender: &dyn Sender,
    org: &Org,
    rep: &Report,
    r: &Rendered,
) -> Result<(), String> {
    let to = Destination {
        name: rep.name.clone(),
        kind: "email".into(),
        url: String::new(),
        to: rep.recipients.clone(),
    };
    let m = Message {
        subject: r.subject.clone(),
        body: r.body.clone(),
        payload: json!({"report": rep.name, "org": org.name}),
        html: r.html,
    };
    let _ = s;
    sender.send(&to, &m).await
}

async fn record(s: &Shared, org: &Org, id: &str, outcome: &str, detail: &Value) {
    match sbuild::alert_history_put(
        s.meta_db(),
        &org.name,
        &format!("report:{id}"),
        "report",
        outcome,
        &detail.to_string(),
        s.now_ms(),
    ) {
        Ok(q) => {
            if let Err(e) = s.write(q).await {
                tracing::warn!(error = %e.1, "could not record report history");
            }
        }
        Err(e) => tracing::error!(error = %e, "report history"),
    }
}

async fn save_state(s: &Shared, org: &Org, id: &str, st: &ReportState) -> Result<(), ApiError> {
    let now = s.now_ms();
    let row = ItemRow {
        item_id: id.into(),
        name: id.into(),
        spec: serde_json::to_string(st).unwrap_or_default(),
        owner: "scheduler".into(),
        created_ms: 0,
        updated_ms: now,
        deleted: false,
    };
    save(s, org, STATE_KIND, &row).await.map(|_| ())
}

/// Render and send one report, and record what happened. `Err` is the reason, in words.
async fn run_once(
    s: &Shared,
    sender: &dyn Sender,
    org: &Org,
    id: &str,
    rep: &Report,
    at_ms: i64,
    how: &str,
) -> Result<Value, String> {
    let t = std::time::Instant::now();
    let res = async {
        let r = render(s, org, rep, at_ms).await?;
        deliver(s, sender, org, rep, &r).await?;
        Ok::<_, String>(r)
    }
    .await;
    let ms = t.elapsed().as_millis() as u64;
    match res {
        Ok(r) => {
            let d = json!({"sections": r.sections, "rows": r.rows, "to": rep.recipients, "ms": ms, "format": rep.format});
            record(s, org, id, &format!("{how}_sent"), &d).await;
            Ok(d)
        }
        Err(e) => {
            record(
                s,
                org,
                id,
                &format!("{how}_failed"),
                &json!({"error": e, "ms": ms}),
            )
            .await;
            Err(e)
        }
    }
}

// ---- the scheduler -----------------------------------------------------------------------------

pub struct Scheduler {
    s: Shared,
    sender: Arc<dyn Sender>,
    pub holder: String,
    settle: Duration,
}

impl Scheduler {
    pub fn new(s: Shared, sender: Arc<dyn Sender>) -> Scheduler {
        Scheduler {
            s,
            sender,
            holder: format!("{:016x}", rand::random::<u64>()),
            settle: Duration::from_millis(SETTLE_MS),
        }
    }

    pub fn with_settle(mut self, d: Duration) -> Scheduler {
        self.settle = d;
        self
    }

    fn lease_of(r: &ItemRow) -> Option<Lease> {
        let v: Value = serde_json::from_str(&r.spec).ok()?;
        Some(Lease {
            holder: v["holder"].as_str()?.into(),
            expires_ms: v["expires_ms"].as_i64()?,
        })
    }

    /// Own one report's slot: the lease is a write then a read (the last writer holds it, ADR 0010),
    /// and the slot is written before sending, so no other replica sends the same one.
    async fn claim(&self, org: &Org, id: &str, slot: i64, now: i64) -> bool {
        let lid = format!("report:{id}");
        let cur = item(&self.s, org, "lease", &lid)
            .await
            .ok()
            .flatten()
            .and_then(|r| Self::lease_of(&r));
        match lease_action(cur.as_ref(), &self.holder, now, LEASE_S * 1000) {
            LeaseAction::Follow => return false,
            LeaseAction::Hold => {}
            LeaseAction::Claim => {
                let row = ItemRow {
                    item_id: lid.clone(),
                    name: lid.clone(),
                    spec: json!({"holder": self.holder, "expires_ms": now + LEASE_S * 1000})
                        .to_string(),
                    owner: self.holder.clone(),
                    created_ms: 0,
                    updated_ms: now,
                    deleted: false,
                };
                if save(&self.s, org, "lease", &row).await.is_err() {
                    return false;
                }
                if !self.settle.is_zero() {
                    tokio::time::sleep(self.settle).await;
                }
                let mine = item(&self.s, org, "lease", &lid)
                    .await
                    .ok()
                    .flatten()
                    .and_then(|r| Self::lease_of(&r))
                    .is_some_and(|l| l.holder == self.holder);
                if !mine {
                    return false;
                }
            }
        }
        // Another replica may have taken this slot since the tick began.
        let fresh = item(&self.s, org, STATE_KIND, id)
            .await
            .ok()
            .flatten()
            .map(|r| state_of(&r))
            .unwrap_or_default();
        if fresh.last_slot_ms >= slot {
            return false;
        }
        let taken = ReportState {
            last_slot_ms: slot,
            outcome: "running".into(),
            ..fresh
        };
        save_state(&self.s, org, id, &taken).await.is_ok()
    }

    /// One pass over an org's reports; returns the ids this replica sent (or tried to send).
    pub async fn tick(&self, org: &Org) -> Result<Vec<String>, ApiError> {
        let now = self.s.now_ms();
        let rows = items(&self.s, org, KIND).await?;
        let states: std::collections::HashMap<String, ReportState> =
            items(&self.s, org, STATE_KIND)
                .await?
                .iter()
                .map(|r| (r.item_id.clone(), state_of(r)))
                .collect();
        let mut ran = vec![];
        for row in rows {
            let Some(rep) = report_of(&row).filter(|r| r.enabled) else {
                continue;
            };
            let Ok(cron) = Cron::parse(&rep.schedule) else {
                continue;
            };
            let last = states
                .get(&row.item_id)
                .map(|s| s.last_slot_ms)
                .unwrap_or(row.created_ms);
            let Some(slot) = reports::due_slot(&cron, last, now) else {
                continue;
            };
            if !self.claim(org, &row.item_id, slot, now).await {
                continue;
            }
            let res = run_once(
                &self.s,
                self.sender.as_ref(),
                org,
                &row.item_id,
                &rep,
                slot,
                "scheduled",
            )
            .await;
            let mut st = item(&self.s, org, STATE_KIND, &row.item_id)
                .await
                .ok()
                .flatten()
                .map(|r| state_of(&r))
                .unwrap_or_default();
            st.last_slot_ms = slot;
            st.last_run_ms = self.s.now_ms();
            st.runs += 1;
            match res {
                Ok(_) => {
                    st.outcome = "ok".into();
                    st.error.clear();
                }
                Err(e) => {
                    tracing::warn!(org = %org.name, report = %row.item_id, error = %e, "report failed");
                    st.outcome = "failed".into();
                    st.error = e;
                }
            }
            let _ = save_state(&self.s, org, &row.item_id, &st).await;
            ran.push(row.item_id.clone());
        }
        Ok(ran)
    }
}

/// Start the scheduler in the background; every replica runs it, the claim keeps each slot to one.
pub fn spawn(s: Shared, sender: Arc<dyn Sender>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let sched = Scheduler::new(s.clone(), sender);
        let mut last_err = String::new();
        loop {
            for org in s.orgs.list() {
                match sched.tick(org).await {
                    Ok(_) => last_err.clear(),
                    Err(e) if e.1 != last_err => {
                        tracing::warn!(org = %org.name, error = %e.1, "report tick failed");
                        last_err = e.1;
                    }
                    Err(_) => {}
                }
            }
            tokio::time::sleep(Duration::from_secs(TICK_S)).await;
        }
    })
}

// ---- handlers ----------------------------------------------------------------------------------

fn report_json(r: &ItemRow, st: Option<&ReportState>) -> Value {
    let rep = report_of(r);
    json!({
        "id": r.item_id, "owner": r.owner, "created_ms": r.created_ms, "updated_ms": r.updated_ms,
        "report": rep, "state": st,
    })
}

pub async fn list(State(s): State<Shared>, Path(org): Path<String>) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let states: std::collections::HashMap<String, ReportState> = items(&s, org, STATE_KIND)
        .await?
        .iter()
        .map(|r| (r.item_id.clone(), state_of(r)))
        .collect();
    let data: Vec<Value> = items(&s, org, KIND)
        .await?
        .iter()
        .map(|r| report_json(r, states.get(&r.item_id)))
        .collect();
    Ok(ok(data, meta("reports_list")))
}

async fn check_target(s: &Shared, org: &Org, rep: &Report) -> Result<(), ApiError> {
    ident(&org.database).map_err(|_| ApiError::bad("invalid org"))?;
    if rep.kind == "dashboard" {
        super::dashboards::load(s, org, &rep.target, "dashboard").await?;
    } else {
        let (rows, _) = s
            .read_store(
                sbuild::view_get(s.meta_db(), &org.name, &rep.target)
                    .map_err(|_| ApiError::bad("invalid view id"))?,
            )
            .await?;
        match rows.first() {
            None => return Err(ApiError::not_found("no such saved view")),
            Some(r) if r["kind"] != "logs" => {
                return Err(ApiError::bad("only saved logs views can be reported on"));
            }
            _ => {}
        }
    }
    Ok(())
}

async fn store(
    s: &Shared,
    org: &Org,
    id: &str,
    rep: &Report,
    owner: &str,
    created: i64,
) -> Result<Meta, ApiError> {
    let now = s.now_ms();
    let row = ItemRow {
        item_id: id.into(),
        name: rep.name.clone(),
        spec: serde_json::to_string(rep).map_err(|_| ApiError::bad("invalid report"))?,
        owner: owner.into(),
        created_ms: created,
        updated_ms: now,
        deleted: false,
    };
    save(s, org, KIND, &row).await
}

pub async fn create(
    State(s): State<Shared>,
    Extension(who): Extension<Identity>,
    Path(org): Path<String>,
    Json(rep): Json<Report>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    rep.validate().map_err(ApiError::bad)?;
    check_target(&s, org, &rep).await?;
    let id = new_id();
    let now = s.now_ms();
    let m = store(&s, org, &id, &rep, &who.username, now).await?;
    // The first run is the next slot after now, not a catch-up of earlier ones.
    save_state(
        &s,
        org,
        &id,
        &ReportState {
            last_slot_ms: now,
            ..Default::default()
        },
    )
    .await?;
    let mut r = ok(json!({"id": id, "report": rep}), m);
    *r.status_mut() = StatusCode::CREATED;
    Ok(r)
}

pub async fn update(
    State(s): State<Shared>,
    Path((org, id)): Path<(String, String)>,
    Json(rep): Json<Report>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let id = id_of(&id)?;
    rep.validate().map_err(ApiError::bad)?;
    let Some(cur) = item(&s, org, KIND, &id).await? else {
        return Err(ApiError::not_found("no such report"));
    };
    check_target(&s, org, &rep).await?;
    let m = store(&s, org, &id, &rep, &cur.owner, cur.created_ms).await?;
    // A changed schedule or a re-enabled report starts from now, so it does not fire for the past.
    let old = report_of(&cur);
    if old
        .as_ref()
        .is_none_or(|o| o.schedule != rep.schedule || (!o.enabled && rep.enabled))
    {
        let mut st = item(&s, org, STATE_KIND, &id)
            .await?
            .map(|r| state_of(&r))
            .unwrap_or_default();
        st.last_slot_ms = s.now_ms();
        save_state(&s, org, &id, &st).await?;
    }
    Ok(ok(json!({"id": id, "report": rep}), m))
}

pub async fn delete(
    State(s): State<Shared>,
    Path((org, id)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let id = id_of(&id)?;
    let Some(mut row) = item(&s, org, KIND, &id).await? else {
        return Err(ApiError::not_found("no such report"));
    };
    row.deleted = true;
    row.updated_ms = s.now_ms();
    let m = save(&s, org, KIND, &row).await?;
    Ok(ok(json!({"id": id, "removed": true}), m))
}

async fn loaded(s: &Shared, org: &Org, id: &str) -> Result<(String, Report), ApiError> {
    let id = id_of(id)?;
    let row = item(s, org, KIND, &id)
        .await?
        .ok_or_else(|| ApiError::not_found("no such report"))?;
    let rep = report_of(&row).ok_or_else(|| ApiError::bad("the report is damaged"))?;
    Ok((id, rep))
}

/// What would be sent right now, without sending it.
pub async fn preview(
    State(s): State<Shared>,
    Path((org, id)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let (_, rep) = loaded(&s, org, &id).await?;
    let r = render(&s, org, &rep, s.now_ms())
        .await
        .map_err(ApiError::bad)?;
    Ok(ok(
        json!({"subject": r.subject, "format": rep.format, "body": r.body, "sections": r.sections, "rows": r.rows}),
        meta("report_preview"),
    ))
}

/// Send now, outside the schedule. A failure is recorded and returned.
pub async fn run_now(
    State(s): State<Shared>,
    Path((org, id)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let (id, rep) = loaded(&s, org, &id).await?;
    let d = run_once(&s, s.sender.as_ref(), org, &id, &rep, s.now_ms(), "manual")
        .await
        .map_err(|e| {
            ApiError(
                StatusCode::BAD_GATEWAY,
                format!("the report was not sent: {e}"),
            )
        })?;
    Ok(ok(json!({"sent": true, "detail": d}), meta("report_run")))
}

#[derive(Deserialize)]
pub struct HistoryParams {
    limit: Option<u32>,
}

pub async fn history(
    State(s): State<Shared>,
    Path((org, id)): Path<(String, String)>,
    Query(p): Query<HistoryParams>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let id = id_of(&id)?;
    let q = sbuild::alert_history_list(
        s.meta_db(),
        &org.name,
        Some(&format!("report:{id}")),
        p.limit.unwrap_or(50),
        s.now_ms(),
    )
    .map_err(|_| ApiError::bad("invalid request"))?;
    let (rows, m) = s.read_store(q).await?;
    let out: Vec<Value> = rows
        .into_iter()
        .map(|mut r| {
            let d = r["detail"]
                .as_str()
                .and_then(|d| serde_json::from_str::<Value>(d).ok());
            r["detail"] = d.unwrap_or(Value::Null);
            r
        })
        .collect();
    Ok(ok(out, m))
}
