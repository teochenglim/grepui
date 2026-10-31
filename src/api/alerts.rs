//! Alerts API (R-A1 – R-A8): rules, destinations, downtimes, incidents and history. All of it is
//! stored in the `grepui_alert_*` tables through `AppState::write`; evaluation is `alerts::engine`.
//! A preview runs a rule once and writes nothing.

use super::{Shared, ok};
use crate::{
    alerts::{
        self, Destination, Downtime, Kind, Rule,
        engine::{self, RuleState},
        incident::Incident,
        notify::Message,
    },
    error::ApiError,
    org::Org,
    query::Meta,
    store::{
        LOCAL_USER,
        build::{self, ItemRow},
    },
};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::Response,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};

fn invalid<T>(_: T) -> ApiError {
    ApiError::bad("invalid request")
}

fn new_id() -> String {
    format!("{:016x}", rand::random::<u64>())
}

fn id_of(id: &str, what: &str) -> Result<String, ApiError> {
    if alerts::valid_id(id) {
        Ok(id.to_string())
    } else {
        Err(ApiError::bad(format!("invalid {what} id")))
    }
}

fn static_meta(name: &str) -> Meta {
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

fn created(resp: Response) -> Response {
    let (mut parts, body) = resp.into_parts();
    parts.status = StatusCode::CREATED;
    Response::from_parts(parts, body)
}

fn spec_json(r: &ItemRow) -> Value {
    serde_json::from_str(&r.spec).unwrap_or(Value::Null)
}

fn item_json(r: &ItemRow) -> Value {
    json!({
        "id": r.item_id, "name": r.name, "owner": r.owner,
        "created_ms": r.created_ms, "updated_ms": r.updated_ms, "spec": spec_json(r),
    })
}

async fn load(s: &Shared, org: &Org, kind: &str, id: &str) -> Result<ItemRow, ApiError> {
    engine::item(s, org, kind, &id_of(id, kind)?)
        .await?
        .ok_or_else(|| ApiError::not_found(format!("no such {kind}")))
}

async fn list_kind(s: &Shared, org: &Org, kind: &str) -> Result<(Vec<Value>, Meta), ApiError> {
    let q = build::item_list(s.meta_db(), &org.name, kind).map_err(invalid)?;
    let (rows, meta) = s.read_store(q).await?;
    Ok((
        rows.iter()
            .map(|r| item_json(&engine::item_of(r)))
            .collect(),
        meta,
    ))
}

// ---- rules -----------------------------------------------------------------------------------

/// Rules by id, with the alerts each composite one mentions.
async fn rules_map(s: &Shared, org: &Org) -> Result<BTreeMap<String, Rule>, ApiError> {
    Ok(engine::items(s, org, "alert")
        .await?
        .iter()
        .filter_map(|r| {
            Some((
                r.item_id.clone(),
                serde_json::from_str::<Rule>(&r.spec).ok()?,
            ))
        })
        .collect())
}

fn composite_refs(rules: &BTreeMap<String, Rule>) -> BTreeMap<String, Vec<String>> {
    rules
        .iter()
        .filter_map(|(id, r)| Some((id.clone(), alerts::expr_refs(r.composite.as_ref()?).ok()?)))
        .collect()
}

/// What a rule may refer to must exist, and a composite must not close a loop (R-A3).
async fn check_refs(
    s: &Shared,
    org: &Org,
    id: &str,
    rule: &Rule,
    existing: &BTreeMap<String, Rule>,
) -> Result<(), ApiError> {
    if let Some(e) = &rule.composite {
        for r in alerts::expr_refs(e)? {
            if r != id && !existing.contains_key(&r) {
                return Err(ApiError::bad(format!(
                    "the composite refers to an alert that does not exist: {r}"
                )));
            }
        }
        let mut others = composite_refs(existing);
        others.remove(id);
        if alerts::has_cycle(id, e, &others) {
            return Err(ApiError::bad(
                "the composite would refer to itself, directly or through other composites",
            ));
        }
    }
    if !rule.destinations.is_empty() {
        let known: Vec<String> = engine::items(s, org, "destination")
            .await?
            .into_iter()
            .map(|r| r.item_id)
            .collect();
        if let Some(d) = rule.destinations.iter().find(|d| !known.contains(d)) {
            return Err(ApiError::bad(format!("unknown destination {d}")));
        }
    }
    Ok(())
}

fn rule_row(
    id: &str,
    rule: &Rule,
    created_ms: i64,
    now: i64,
    owner: &str,
) -> Result<ItemRow, ApiError> {
    Ok(ItemRow {
        item_id: id.into(),
        name: rule.name.clone(),
        spec: serde_json::to_string(rule).map_err(invalid)?,
        owner: owner.into(),
        created_ms,
        updated_ms: now,
        deleted: false,
    })
}

pub async fn alert_list(
    State(s): State<Shared>,
    Path(org): Path<String>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let (rules, meta) = list_kind(&s, org, "alert").await?;
    let states: HashMap<String, Value> = engine::items(&s, org, engine::STATE_KIND)
        .await?
        .into_iter()
        .map(|r| (r.item_id.clone(), spec_json(&r)))
        .collect();
    let leases: HashMap<String, Value> = engine::items(&s, org, "lease")
        .await?
        .into_iter()
        .map(|r| (r.item_id.clone(), spec_json(&r)))
        .collect();
    let now = s.now_ms();
    let out: Vec<Value> = rules
        .into_iter()
        .map(|mut r| {
            let id = r["id"].as_str().unwrap_or_default().to_string();
            let st = states.get(&id).cloned().unwrap_or(json!({}));
            let lease = leases.get(&id);
            r["status"] = json!({
                "state": st["state"], "last_eval_ms": st["last_eval_ms"], "last_fire_ms": st["last_fire_ms"],
                "last_value": st["last_value"], "error": st["error"],
                "leader": lease.filter(|l| l["expires_ms"].as_i64().unwrap_or(0) > now).map(|l| l["holder"].clone()),
            });
            r
        })
        .collect();
    Ok(ok(out, meta))
}

pub async fn alert_get(
    State(s): State<Shared>,
    Path((org, id)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let row = load(&s, org, "alert", &id).await?;
    Ok(ok(item_json(&row), static_meta("alert_get")))
}

pub async fn alert_create(
    State(s): State<Shared>,
    Path(org): Path<String>,
    Json(b): Json<Value>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let rule = alerts::parse_rule(&b)?;
    let id = new_id();
    check_refs(&s, org, &id, &rule, &rules_map(&s, org).await?).await?;
    let now = s.now_ms();
    let row = rule_row(&id, &rule, now, now, LOCAL_USER)?;
    let meta = engine::save(&s, org, "alert", &row).await?;
    Ok(created(ok(item_json(&row), meta)))
}

pub async fn alert_update(
    State(s): State<Shared>,
    Path((org, id)): Path<(String, String)>,
    Json(b): Json<Value>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let old = load(&s, org, "alert", &id).await?;
    let rule = alerts::parse_rule(&b)?;
    check_refs(&s, org, &old.item_id, &rule, &rules_map(&s, org).await?).await?;
    let row = rule_row(&old.item_id, &rule, old.created_ms, s.now_ms(), &old.owner)?;
    let meta = engine::save(&s, org, "alert", &row).await?;
    Ok(ok(item_json(&row), meta))
}

pub async fn alert_delete(
    State(s): State<Shared>,
    Path((org, id)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let mut row = load(&s, org, "alert", &id).await?;
    let used_by: Vec<String> = composite_refs(&rules_map(&s, org).await?)
        .into_iter()
        .filter(|(other, refs)| *other != row.item_id && refs.contains(&row.item_id))
        .map(|(o, _)| o)
        .collect();
    if !used_by.is_empty() {
        return Err(ApiError(
            StatusCode::CONFLICT,
            format!(
                "composite alerts still use this one: {}",
                used_by.join(", ")
            ),
        ));
    }
    row.deleted = true;
    row.updated_ms = s.now_ms();
    let meta = engine::save(&s, org, "alert", &row).await?;
    Ok(ok(json!({"deleted": true, "id": row.item_id}), meta))
}

// ---- preview ---------------------------------------------------------------------------------

async fn states_map(s: &Shared, org: &Org) -> Result<HashMap<String, RuleState>, ApiError> {
    Ok(engine::items(s, org, engine::STATE_KIND)
        .await?
        .into_iter()
        .map(|r| {
            (
                r.item_id.clone(),
                serde_json::from_str(&r.spec).unwrap_or_default(),
            )
        })
        .collect())
}

/// Run a rule once: no state change, no history, no notification (R-A1).
async fn preview_rule(s: &Shared, org: &Org, id: &str, rule: &Rule) -> Result<Response, ApiError> {
    let states = states_map(s, org).await?;
    let now = s.now_ms();
    let ev = engine::evaluate(s, org, rule, now, &states)
        .await
        .map_err(ApiError::bad)?;
    let first = ev
        .samples
        .iter()
        .find(|x| x.breached)
        .or(ev.samples.first())
        .cloned();
    let message = first.map(|sm| {
        let (m, unknown) = engine::build_message(&org.name, id, rule, "preview", &sm, now);
        json!({"subject": m.subject, "body": m.body, "unknown_variables": unknown})
    });
    let mut meta = static_meta("alert_preview");
    meta.sql = ev.sql.join(";\n");
    meta.ms = ev.ms as u64;
    meta.rows = ev.samples.len();
    Ok(ok(
        json!({
            "state": if ev.firing() { "firing" } else { "ok" },
            "samples": ev.samples, "series": ev.series, "notes": ev.notes, "ms": ev.ms,
            "message": message, "would_notify": false,
        }),
        meta,
    ))
}

pub async fn preview_saved(
    State(s): State<Shared>,
    Path((org, id)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let row = load(&s, org, "alert", &id).await?;
    let rule: Rule = serde_json::from_str(&row.spec).map_err(invalid)?;
    preview_rule(&s, org, &row.item_id, &rule).await
}

#[derive(Deserialize)]
pub struct PreviewParams {
    rule: String,
}

/// An unsaved rule travels as JSON in the query string (the route is GET: it only reads).
pub async fn preview_draft(
    State(s): State<Shared>,
    Path(org): Path<String>,
    Query(p): Query<PreviewParams>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    if p.rule.len() > 24_000 {
        return Err(ApiError::bad("the rule is too large"));
    }
    let v: Value = serde_json::from_str(&p.rule).map_err(|_| ApiError::bad("rule must be JSON"))?;
    let rule = alerts::parse_rule(&v)?;
    preview_rule(&s, org, "draft", &rule).await
}

// ---- history ---------------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct HistoryParams {
    limit: Option<u32>,
}

async fn history(
    s: &Shared,
    org: &Org,
    alert: Option<&str>,
    limit: Option<u32>,
) -> Result<Response, ApiError> {
    let q = build::alert_history_list(
        s.meta_db(),
        &org.name,
        alert,
        limit.unwrap_or(100),
        s.now_ms(),
    )
    .map_err(invalid)?;
    let (rows, meta) = s.read_store(q).await?;
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
    Ok(ok(out, meta))
}

pub async fn history_all(
    State(s): State<Shared>,
    Path(org): Path<String>,
    Query(p): Query<HistoryParams>,
) -> Result<Response, ApiError> {
    history(&s, s.org(&org)?, None, p.limit).await
}

pub async fn history_one(
    State(s): State<Shared>,
    Path((org, id)): Path<(String, String)>,
    Query(p): Query<HistoryParams>,
) -> Result<Response, ApiError> {
    history(&s, s.org(&org)?, Some(&id_of(&id, "alert")?), p.limit).await
}

// ---- import and export -------------------------------------------------------------------------

pub async fn export_all(
    State(s): State<Shared>,
    Path(org): Path<String>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let (rules, meta) = list_kind(&s, org, "alert").await?;
    let docs: Vec<Value> = rules
        .iter()
        .map(|r| json!({alerts::EXPORT_MARK: 1, "id": r["id"], "spec": r["spec"]}))
        .collect();
    Ok(ok(json!({alerts::BUNDLE_MARK: 1, "alerts": docs}), meta))
}

pub async fn import(
    State(s): State<Shared>,
    Path(org): Path<String>,
    Json(b): Json<Value>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let docs: Vec<Value> = if b.get(alerts::BUNDLE_MARK).is_some() {
        b["alerts"].as_array().cloned().unwrap_or_default()
    } else if b.get(alerts::EXPORT_MARK).is_some() {
        vec![b.clone()]
    } else {
        return Err(ApiError::bad("this is not a grepui alert export"));
    };
    if docs.is_empty() || docs.len() > 100 {
        return Err(ApiError::bad("an import holds 1-100 alerts"));
    }
    let mut existing = rules_map(&s, org).await?;
    let known_dests: Vec<String> = engine::items(&s, org, "destination")
        .await?
        .into_iter()
        .map(|r| r.item_id)
        .collect();
    let (mut imported, mut skipped, mut notes) = (vec![], vec![], vec![]);
    let now = s.now_ms();
    for d in &docs {
        let mut rule = alerts::parse_rule(&d["spec"])?;
        let id = match d["id"].as_str() {
            Some(i) if alerts::valid_id(i) => i.to_string(),
            _ => new_id(),
        };
        if existing.contains_key(&id) {
            skipped.push(id);
            continue;
        }
        let before = rule.destinations.len();
        rule.destinations.retain(|x| known_dests.contains(x));
        if rule.destinations.len() != before {
            notes.push(format!(
                "{}: destinations that do not exist here were dropped",
                rule.name
            ));
        }
        check_refs(&s, org, &id, &rule, &existing).await?;
        let row = rule_row(&id, &rule, now, now, LOCAL_USER)?;
        engine::save(&s, org, "alert", &row).await?;
        existing.insert(id.clone(), rule);
        imported.push(id);
    }
    Ok(created(ok(
        json!({"imported": imported, "skipped": skipped, "notes": notes}),
        static_meta("alert_import"),
    )))
}

// ---- destinations ----------------------------------------------------------------------------

pub async fn dest_list(
    State(s): State<Shared>,
    Path(org): Path<String>,
) -> Result<Response, ApiError> {
    let (rows, meta) = list_kind(&s, s.org(&org)?, "destination").await?;
    Ok(ok(rows, meta))
}

fn parse_dest(v: &Value) -> Result<Destination, ApiError> {
    serde_json::from_value::<Destination>(v.clone())
        .map_err(|e| ApiError::bad(format!("invalid destination: {e}")))?
        .canonical()
}

fn dest_row(id: &str, d: &Destination, created_ms: i64, now: i64) -> Result<ItemRow, ApiError> {
    Ok(ItemRow {
        item_id: id.into(),
        name: d.name.clone(),
        spec: serde_json::to_string(d).map_err(invalid)?,
        owner: LOCAL_USER.into(),
        created_ms,
        updated_ms: now,
        deleted: false,
    })
}

pub async fn dest_create(
    State(s): State<Shared>,
    Path(org): Path<String>,
    Json(b): Json<Value>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let d = parse_dest(&b)?;
    let now = s.now_ms();
    let row = dest_row(&new_id(), &d, now, now)?;
    let meta = engine::save(&s, org, "destination", &row).await?;
    Ok(created(ok(item_json(&row), meta)))
}

pub async fn dest_update(
    State(s): State<Shared>,
    Path((org, id)): Path<(String, String)>,
    Json(b): Json<Value>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let old = load(&s, org, "destination", &id).await?;
    let row = dest_row(&old.item_id, &parse_dest(&b)?, old.created_ms, s.now_ms())?;
    let meta = engine::save(&s, org, "destination", &row).await?;
    Ok(ok(item_json(&row), meta))
}

pub async fn dest_delete(
    State(s): State<Shared>,
    Path((org, id)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let mut row = load(&s, org, "destination", &id).await?;
    let users: Vec<String> = rules_map(&s, org)
        .await?
        .into_iter()
        .filter(|(_, r)| r.destinations.contains(&row.item_id))
        .map(|(i, _)| i)
        .collect();
    if !users.is_empty() {
        return Err(ApiError(
            StatusCode::CONFLICT,
            format!(
                "alerts still send to this destination: {}",
                users.join(", ")
            ),
        ));
    }
    row.deleted = true;
    row.updated_ms = s.now_ms();
    let meta = engine::save(&s, org, "destination", &row).await?;
    Ok(ok(json!({"deleted": true, "id": row.item_id}), meta))
}

/// Send a test message and report what happened (R-A4). The answer is 200 either way: a failed
/// delivery is the result being asked for.
pub async fn dest_test(
    State(s): State<Shared>,
    Path((org, id)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let row = load(&s, org, "destination", &id).await?;
    let d: Destination = serde_json::from_str(&row.spec).map_err(invalid)?;
    let now = s.now_ms();
    let m = Message {
        subject: "[grepui] test notification".into(),
        body: format!(
            "This is a test from grepui for the destination {:?}.",
            d.name
        ),
        payload: json!({"test": true, "org": org.name, "time_ms": now, "message": "test notification from grepui"}),
    };
    let r = s.sender.send(&d, &m).await;
    Ok(ok(
        match r {
            Ok(()) => json!({"ok": true}),
            Err(e) => json!({"ok": false, "error": e}),
        },
        static_meta("destination_test"),
    ))
}

// ---- downtimes -------------------------------------------------------------------------------

pub async fn down_list(
    State(s): State<Shared>,
    Path(org): Path<String>,
) -> Result<Response, ApiError> {
    let (rows, meta) = list_kind(&s, s.org(&org)?, "downtime").await?;
    Ok(ok(rows, meta))
}

fn parse_down(v: &Value) -> Result<Downtime, ApiError> {
    serde_json::from_value::<Downtime>(v.clone())
        .map_err(|e| ApiError::bad(format!("invalid downtime: {e}")))?
        .canonical()
}

fn down_row(id: &str, d: &Downtime, created_ms: i64, now: i64) -> Result<ItemRow, ApiError> {
    Ok(ItemRow {
        item_id: id.into(),
        name: d.name.clone(),
        spec: serde_json::to_string(d).map_err(invalid)?,
        owner: LOCAL_USER.into(),
        created_ms,
        updated_ms: now,
        deleted: false,
    })
}

pub async fn down_create(
    State(s): State<Shared>,
    Path(org): Path<String>,
    Json(b): Json<Value>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let d = parse_down(&b)?;
    let now = s.now_ms();
    let row = down_row(&new_id(), &d, now, now)?;
    let meta = engine::save(&s, org, "downtime", &row).await?;
    Ok(created(ok(item_json(&row), meta)))
}

pub async fn down_update(
    State(s): State<Shared>,
    Path((org, id)): Path<(String, String)>,
    Json(b): Json<Value>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let old = load(&s, org, "downtime", &id).await?;
    let row = down_row(&old.item_id, &parse_down(&b)?, old.created_ms, s.now_ms())?;
    let meta = engine::save(&s, org, "downtime", &row).await?;
    Ok(ok(item_json(&row), meta))
}

pub async fn down_delete(
    State(s): State<Shared>,
    Path((org, id)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let mut row = load(&s, org, "downtime", &id).await?;
    row.deleted = true;
    row.updated_ms = s.now_ms();
    let meta = engine::save(&s, org, "downtime", &row).await?;
    Ok(ok(json!({"deleted": true, "id": row.item_id}), meta))
}

// ---- incidents ---------------------------------------------------------------------------------

fn incident_row(id: &str, inc: &Incident, now: i64) -> Result<ItemRow, ApiError> {
    Ok(ItemRow {
        item_id: id.into(),
        name: inc.title.clone(),
        spec: serde_json::to_string(inc).map_err(invalid)?,
        owner: LOCAL_USER.into(),
        created_ms: inc.created_ms,
        updated_ms: now,
        deleted: false,
    })
}

#[derive(Deserialize)]
pub struct IncidentParams {
    state: Option<String>,
}

pub async fn incident_list(
    State(s): State<Shared>,
    Path(org): Path<String>,
    Query(p): Query<IncidentParams>,
) -> Result<Response, ApiError> {
    let (mut rows, meta) = list_kind(&s, s.org(&org)?, "incident").await?;
    if let Some(st) = &p.state {
        rows.retain(|r| r["spec"]["state"] == json!(st));
    }
    rows.sort_by_key(|r| std::cmp::Reverse(r["spec"]["created_ms"].as_i64().unwrap_or(0)));
    Ok(ok(rows, meta))
}

pub async fn incident_get(
    State(s): State<Shared>,
    Path((org, id)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let row = load(&s, org, "incident", &id).await?;
    Ok(ok(item_json(&row), static_meta("incident_get")))
}

#[derive(Deserialize, Default)]
pub struct NoteBody {
    #[serde(default)]
    note: String,
}

async fn step(
    s: &Shared,
    org: &Org,
    id: &str,
    note: &str,
    f: fn(&mut Incident, i64, &str, &str) -> Result<(), String>,
) -> Result<Response, ApiError> {
    let row = load(s, org, "incident", id).await?;
    let mut inc: Incident = serde_json::from_str(&row.spec).map_err(invalid)?;
    if note.chars().count() > 500 || note.chars().any(|c| c.is_control() && c != '\n') {
        return Err(ApiError::bad("the note is at most 500 characters"));
    }
    f(&mut inc, s.now_ms(), LOCAL_USER, note).map_err(|e| ApiError(StatusCode::CONFLICT, e))?;
    let out = incident_row(&row.item_id, &inc, s.now_ms())?;
    let meta = engine::save(s, org, "incident", &out).await?;
    Ok(ok(item_json(&out), meta))
}

pub async fn incident_ack(
    State(s): State<Shared>,
    Path((org, id)): Path<(String, String)>,
    body: Option<Json<NoteBody>>,
) -> Result<Response, ApiError> {
    let note = body.map(|b| b.0.note).unwrap_or_default();
    step(&s, s.org(&org)?, &id, &note, Incident::acknowledge).await
}

pub async fn incident_resolve(
    State(s): State<Shared>,
    Path((org, id)): Path<(String, String)>,
    body: Option<Json<NoteBody>>,
) -> Result<Response, ApiError> {
    let note = body.map(|b| b.0.note).unwrap_or_default();
    step(&s, s.org(&org)?, &id, &note, Incident::resolve).await
}

/// Open an incident for a rule by hand (R-A6); one active incident per rule.
pub async fn incident_from_alert(
    State(s): State<Shared>,
    Path((org, id)): Path<(String, String)>,
    body: Option<Json<NoteBody>>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let alert = load(&s, org, "alert", &id).await?;
    let active = engine::items(&s, org, "incident")
        .await?
        .into_iter()
        .any(|r| {
            serde_json::from_str::<Incident>(&r.spec)
                .is_ok_and(|i| i.alert_id == alert.item_id && i.is_active())
        });
    if active {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "this alert already has an active incident".into(),
        ));
    }
    let note = body.map(|b| b.0.note).unwrap_or_default();
    let now = s.now_ms();
    let inc = Incident::open(&alert.item_id, &alert.name, now, LOCAL_USER, &note);
    let row = incident_row(&new_id(), &inc, now)?;
    let meta = engine::save(&s, org, "incident", &row).await?;
    Ok(created(ok(item_json(&row), meta)))
}

// ---- the scheduler's view ------------------------------------------------------------------------

/// Who the replica is and how the lease works (for the status line on the alerts page).
pub async fn info(State(s): State<Shared>, Path(org): Path<String>) -> Result<Response, ApiError> {
    s.org(&org)?;
    Ok(ok(
        json!({
            "scheduler": !s.cfg.no_alerts,
            "tick_s": engine::TICK_S, "lease_s": engine::LEASE_S,
            "email": s.cfg.smtp.is_some(),
            "kinds": [Kind::Scheduled, Kind::Realtime, Kind::Composite, Kind::Anomaly],
            "variables": alerts::template::VARIABLES,
            "default_subject": alerts::template::DEFAULT_SUBJECT,
            "default_body": alerts::template::DEFAULT_BODY,
            "min_realtime_s": alerts::MIN_REALTIME_S,
            "default_realtime_s": alerts::DEFAULT_REALTIME_S,
        }),
        static_meta("alerts_info"),
    ))
}

// Shared by the other small-config APIs (functions, redaction): the same kind-tagged store.
pub async fn engine_items(s: &Shared, org: &Org, kind: &str) -> Result<Vec<ItemRow>, ApiError> {
    engine::items(s, org, kind).await
}

pub async fn engine_item(
    s: &Shared,
    org: &Org,
    kind: &str,
    id: &str,
) -> Result<Option<ItemRow>, ApiError> {
    engine::item(s, org, kind, id).await
}

pub async fn engine_save(s: &Shared, org: &Org, kind: &str, r: &ItemRow) -> Result<Meta, ApiError> {
    engine::save(s, org, kind, r).await
}
