//! LLM views (R-AI3, R-AI4; ADR 0016): the per-org price table that fills a missing cost.

use super::{Shared, ok};
use crate::{
    auth::{Identity, sha_hex, store as items},
    error::ApiError,
    llm::{Price, valid_model},
    query::Meta,
};
use axum::{
    Extension, Json,
    extract::{Path, Query, State},
    response::Response,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;

pub const PRICE_KIND: &str = "price";
/// More models than this in one org is a mistake, not a price list.
const MAX_PRICES: usize = 500;

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

/// The org's price table, model name to price. Unreadable rows are skipped.
pub async fn prices(s: &Shared, org: &str) -> Result<HashMap<String, Price>, ApiError> {
    Ok(items::list(s, org, PRICE_KIND)
        .await?
        .iter()
        .filter_map(|r| {
            serde_json::from_str::<Price>(&r.spec)
                .ok()
                .map(|p| (r.name.clone(), p))
        })
        .collect())
}

pub async fn prices_list(
    State(s): State<Shared>,
    Path(org): Path<String>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let mut rows: Vec<Value> = prices(&s, &org.name)
        .await?
        .into_iter()
        .map(|(model, p)| json!({"model": model, "prompt_per_1m": p.prompt_per_1m, "completion_per_1m": p.completion_per_1m}))
        .collect();
    rows.sort_by(|a, b| a["model"].as_str().cmp(&b["model"].as_str()));
    Ok(ok(rows, meta("llm_prices_list")))
}

#[derive(Deserialize)]
pub struct PriceBody {
    model: String,
    prompt_per_1m: f64,
    completion_per_1m: f64,
}

pub async fn price_put(
    State(s): State<Shared>,
    Path(org): Path<String>,
    Json(b): Json<PriceBody>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let model = b.model.trim().to_string();
    if !valid_model(&model) {
        return Err(ApiError::bad(
            "the model name must be 1 to 128 characters without control characters",
        ));
    }
    let price = Price {
        prompt_per_1m: b.prompt_per_1m,
        completion_per_1m: b.completion_per_1m,
    };
    price.validate().map_err(ApiError::bad)?;
    let existing = items::get(&s, &org.name, PRICE_KIND, &model).await?;
    if existing.is_none() && items::list(&s, &org.name, PRICE_KIND).await?.len() >= MAX_PRICES {
        return Err(ApiError::bad("too many prices; remove some first"));
    }
    let now = s.now_ms();
    let created = existing.map(|r| r.created_ms).unwrap_or(now);
    let m = items::put(
        &s,
        &org.name,
        PRICE_KIND,
        &items::row(&model, &model, &price, "admin", created, now),
    )
    .await?;
    Ok(ok(
        json!({"model": model, "prompt_per_1m": price.prompt_per_1m, "completion_per_1m": price.completion_per_1m}),
        m,
    ))
}

#[derive(Deserialize)]
pub struct ModelParam {
    model: String,
}

pub async fn price_delete(
    State(s): State<Shared>,
    Path(org): Path<String>,
    Query(p): Query<ModelParam>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let Some(r) = items::get(&s, &org.name, PRICE_KIND, p.model.trim()).await? else {
        return Err(ApiError::not_found("no price for that model"));
    };
    let m = items::remove(&s, &org.name, PRICE_KIND, r, s.now_ms()).await?;
    Ok(ok(json!({"model": p.model.trim(), "removed": true}), m))
}

// ---- content protection -----------------------------------------------------------------------

const CONF_KIND: &str = "llmconf";
const CONTENT_ID: &str = "content";

#[derive(serde::Serialize, Deserialize)]
struct ContentConf {
    mode: crate::llm::Mode,
}

/// The mode for roles below admin in `org` (masked until an admin says otherwise).
pub fn content_mode(s: &Shared, org: &str) -> crate::llm::Mode {
    s.llm_content
        .read()
        .ok()
        .and_then(|m| m.get(org).copied())
        .unwrap_or_default()
}

/// What a caller sees of prompts and completions: admins see everything; anyone else gets the
/// org's mode, and a public link never more than masked (R-AI8).
pub fn effective_mode(admin: bool, setting: crate::llm::Mode, public: bool) -> crate::llm::Mode {
    use crate::llm::Mode;
    match (admin, setting, public) {
        (true, _, false) => Mode::Visible,
        (_, Mode::Visible, true) => Mode::Masked,
        (_, m, _) => m,
    }
}

/// Read the org's setting into memory (called with the redaction rules, every 5 s).
pub async fn load_content_mode(s: &Shared, org: &str) {
    if let Ok(Some(r)) = items::get(s, org, CONF_KIND, CONTENT_ID).await
        && let Ok(c) = serde_json::from_str::<ContentConf>(&r.spec)
        && let Ok(mut m) = s.llm_content.write()
    {
        m.insert(org.to_string(), c.mode);
    } else if let Ok(mut m) = s.llm_content.write() {
        m.remove(org);
    }
}

pub async fn content_get(
    State(s): State<Shared>,
    Path(org): Path<String>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    Ok(ok(
        json!({"mode": content_mode(&s, &org.name)}),
        meta("llm_content_get"),
    ))
}

#[derive(Deserialize)]
pub struct ContentBody {
    mode: crate::llm::Mode,
}

pub async fn content_put(
    State(s): State<Shared>,
    Path(org): Path<String>,
    Json(b): Json<ContentBody>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let now = s.now_ms();
    let conf = ContentConf { mode: b.mode };
    let m = items::put(
        &s,
        &org.name,
        CONF_KIND,
        &items::row(CONTENT_ID, CONTENT_ID, &conf, "admin", now, now),
    )
    .await?;
    if let Ok(mut map) = s.llm_content.write() {
        map.insert(org.name.clone(), b.mode);
    }
    Ok(ok(json!({"mode": b.mode}), m))
}

// ---- human feedback (R-AI7) -------------------------------------------------------------------

const FEEDBACK_KIND: &str = "feedback";
/// Feedback rows one org keeps; more is a sign the table is being used as a log.
const MAX_FEEDBACK: usize = 20_000;

#[derive(Serialize, Deserialize, Clone)]
struct Feedback {
    trace_id: String,
    #[serde(default)]
    span_id: Option<String>,
    /// `up`, `down` or empty.
    #[serde(default)]
    rating: String,
    #[serde(default)]
    score: Option<f64>,
    #[serde(default)]
    label: String,
    #[serde(default)]
    note: String,
    author: String,
}

use serde::Serialize;

#[derive(Deserialize)]
pub struct FeedbackBody {
    trace_id: String,
    span_id: Option<String>,
    #[serde(default)]
    rating: String,
    score: Option<f64>,
    #[serde(default)]
    label: String,
    #[serde(default)]
    note: String,
}

fn feedback_id(trace: &str, span: Option<&str>, author: &str) -> String {
    sha_hex(&format!("{trace}|{}|{author}", span.unwrap_or("")))[..24].to_string()
}

pub async fn feedback_put(
    State(s): State<Shared>,
    Extension(id): Extension<Identity>,
    Path(org): Path<String>,
    Json(b): Json<FeedbackBody>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let trace = crate::query::traces::trace_id(&b.trace_id)?;
    let span = b
        .span_id
        .as_deref()
        .map(crate::query::traces::span_id)
        .transpose()?;
    if !["", "up", "down"].contains(&b.rating.as_str()) {
        return Err(ApiError::bad("rating must be up, down or empty"));
    }
    if b.score.is_some_and(|v| !v.is_finite() || v.abs() > 1e6) {
        return Err(ApiError::bad(
            "score must be a number between -1000000 and 1000000",
        ));
    }
    if b.label.chars().count() > 64 || b.label.chars().any(char::is_control) {
        return Err(ApiError::bad("label is at most 64 characters"));
    }
    if b.note.chars().count() > 2000 {
        return Err(ApiError::bad("note is at most 2000 characters"));
    }
    if b.rating.is_empty() && b.score.is_none() && b.label.is_empty() && b.note.trim().is_empty() {
        return Err(ApiError::bad("give a rating, a score, a label or a note"));
    }
    let author = id.username.clone();
    let fid = feedback_id(&trace, span.as_deref(), &author);
    let existing = items::get(&s, &org.name, FEEDBACK_KIND, &fid).await?;
    if existing.is_none() && items::list(&s, &org.name, FEEDBACK_KIND).await?.len() >= MAX_FEEDBACK
    {
        return Err(ApiError::bad("this org has reached its feedback limit"));
    }
    let now = s.now_ms();
    let created = existing.map(|r| r.created_ms).unwrap_or(now);
    let fb = Feedback {
        trace_id: trace,
        span_id: span,
        rating: b.rating,
        score: b.score,
        label: b.label,
        note: b.note,
        author: author.clone(),
    };
    let m = items::put(
        &s,
        &org.name,
        FEEDBACK_KIND,
        &items::row(&fid, &fb.trace_id, &fb, &author, created, now),
    )
    .await?;
    Ok(ok(feedback_json(&fid, &fb, created, now), m))
}

fn feedback_json(id: &str, f: &Feedback, created: i64, updated: i64) -> Value {
    json!({"id": id, "trace_id": f.trace_id, "span_id": f.span_id, "rating": f.rating, "score": f.score,
           "label": f.label, "note": f.note, "author": f.author, "created_ms": created, "updated_ms": updated})
}

#[derive(Deserialize)]
pub struct TraceParam {
    trace: String,
    span: Option<String>,
}

pub async fn feedback_list(
    State(s): State<Shared>,
    Path(org): Path<String>,
    Query(p): Query<TraceParam>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let trace = crate::query::traces::trace_id(&p.trace)?;
    let mut rows: Vec<Value> = items::list(&s, &org.name, FEEDBACK_KIND)
        .await?
        .iter()
        .filter(|r| r.name == trace)
        .filter_map(|r| {
            serde_json::from_str::<Feedback>(&r.spec)
                .ok()
                .map(|f| (r, f))
        })
        .filter(|(_, f)| p.span.is_none() || f.span_id.as_deref() == p.span.as_deref())
        .map(|(r, f)| feedback_json(&r.item_id, &f, r.created_ms, r.updated_ms))
        .collect();
    rows.sort_by_key(|v| v["created_ms"].as_i64());
    Ok(ok(rows, meta("llm_feedback_list")))
}

/// A person removes their own feedback on a trace or span.
pub async fn feedback_delete(
    State(s): State<Shared>,
    Extension(id): Extension<Identity>,
    Path(org): Path<String>,
    Query(p): Query<TraceParam>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let trace = crate::query::traces::trace_id(&p.trace)?;
    let span = p
        .span
        .as_deref()
        .map(crate::query::traces::span_id)
        .transpose()?;
    let fid = feedback_id(&trace, span.as_deref(), &id.username);
    let Some(r) = items::get(&s, &org.name, FEEDBACK_KIND, &fid).await? else {
        return Err(ApiError::not_found("you have no feedback there"));
    };
    let m = items::remove(&s, &org.name, FEEDBACK_KIND, r, s.now_ms()).await?;
    Ok(ok(json!({"removed": true}), m))
}

// ---- analytics, sessions and users (R-AI5, R-AI6; ADR 0017) -----------------------------------

use crate::{
    org::Org,
    query::{
        Window, llm as q,
        traces::{Layout, TraceFilter},
    },
};
use std::collections::BTreeMap;

/// Analytics read attribute JSON columns; a flat table is refused with the way out.
async fn require_json(s: &Shared, org: &Org) -> Result<(), ApiError> {
    let cols = super::traces::columns(s, org).await?;
    if Layout::detect(&cols) != Layout::Json {
        return Err(ApiError::bad(
            "LLM analytics need a trace v2 table (attributes in JSON columns); see Connect for how to send OpenInference traces to one",
        ));
    }
    Ok(())
}

fn num(v: &Value, k: &str) -> f64 {
    v[k].as_f64().unwrap_or(0.0)
}

fn int(v: &Value, k: &str) -> i64 {
    v[k].as_i64().unwrap_or_else(|| num(v, k) as i64)
}

fn ms(ns: f64) -> f64 {
    (ns / 1e6 * 100.0).round() / 100.0
}

/// Cost of a row: what spans reported, plus the price table applied to the tokens of the model
/// calls that reported none (only when the row is for one model).
fn cost_parts(r: &Value, model: Option<&str>, prices: &HashMap<String, Price>) -> (f64, f64) {
    let reported = num(r, "cost");
    let estimated = model
        .and_then(|m| prices.get(m))
        .map(|p| p.cost(int(r, "uncosted_prompt"), int(r, "uncosted_completion")))
        .unwrap_or(0.0);
    (reported, estimated)
}

/// A breakdown row as the API returns it.
fn shape(r: &Value, model: Option<&str>, prices: &HashMap<String, Price>) -> Value {
    let calls = int(r, "calls");
    let errors = int(r, "errors");
    let (prompt, completion) = (int(r, "prompt_tokens"), int(r, "completion_tokens"));
    let (rep, est) = cost_parts(r, model, prices);
    json!({
        "grp": r["grp"], "calls": calls, "errors": errors,
        "error_rate": if calls > 0 { errors as f64 * 100.0 / calls as f64 } else { 0.0 },
        "p50_ms": ms(num(r, "p50_ns")), "p95_ms": ms(num(r, "p95_ns")), "p99_ms": ms(num(r, "p99_ns")), "avg_ms": ms(num(r, "avg_ns")),
        "prompt_tokens": prompt, "completion_tokens": completion, "total_tokens": prompt + completion,
        "cost": {"reported": rep, "estimated": est, "total": rep + est},
    })
}

/// The window and filter every analytics route takes; `model` narrows to one model.
async fn setup<'a>(
    s: &'a Shared,
    org: &str,
    p: &super::traces::TraceParams,
) -> Result<(&'a Org, Window, TraceFilter, Option<String>), ApiError> {
    let w = Window::parse(p.from, p.to, p.limit)?;
    let org = s.org(org)?;
    require_json(s, org).await?;
    let f = super::traces::filter(s, org, p).await?;
    let model = p
        .model
        .as_deref()
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .map(str::to_string);
    if model.as_deref().is_some_and(|m| !valid_model(m)) {
        return Err(ApiError::bad("invalid model"));
    }
    Ok((org, w, f, model))
}

fn bad_table<T>(r: anyhow::Result<T>) -> Result<T, ApiError> {
    r.map_err(|_| ApiError::bad("unsupported traces table name"))
}

pub async fn overview(
    State(s): State<Shared>,
    Path(org): Path<String>,
    Query(p): Query<super::traces::TraceParams>,
) -> Result<Response, ApiError> {
    let (org, w, f, model) = setup(&s, &org, &p).await?;
    let (db, table) = (&org.database, &org.traces_table);
    let m = model.as_deref();
    let (totals, meta) = s.run(bad_table(q::totals(db, table, &f, m, w))?).await?;
    let (models, _) = s
        .run(bad_table(q::breakdown(db, table, q::By::Model, &f, m, w))?)
        .await?;
    let (series, _) = s.run(bad_table(q::series(db, table, &f, m, w))?).await?;
    let prices = prices(&s, &org.name).await?;
    let model_rows: Vec<Value> = models
        .iter()
        .map(|r| shape(r, r["grp"].as_str(), &prices))
        .collect();
    // Whole-window latency and error rate come from the totals row; tokens and cost are the sum
    // over models, so a price estimate applies model by model.
    let mut total = totals
        .first()
        .map(|r| shape(r, None, &prices))
        .unwrap_or_else(|| shape(&json!({}), None, &prices));
    let sum = |k: &str| {
        model_rows
            .iter()
            .map(|r| r[k].as_i64().unwrap_or(0))
            .sum::<i64>()
    };
    let csum = |k: &str| {
        model_rows
            .iter()
            .map(|r| r["cost"][k].as_f64().unwrap_or(0.0))
            .sum::<f64>()
    };
    let (rep, est) = (csum("reported"), csum("estimated"));
    total["prompt_tokens"] = json!(sum("prompt_tokens"));
    total["completion_tokens"] = json!(sum("completion_tokens"));
    total["total_tokens"] = json!(sum("prompt_tokens") + sum("completion_tokens"));
    total["cost"] = json!({"reported": rep, "estimated": est, "total": rep + est});
    let points: Vec<Value> = series
        .iter()
        .map(|r| {
            json!({"t": r["bucket_ms"], "calls": int(r, "calls"), "errors": int(r, "errors"),
                   "p95_ms": ms(num(r, "p95_ns")), "prompt_tokens": int(r, "prompt_tokens"),
                   "completion_tokens": int(r, "completion_tokens"), "cost": num(r, "cost")})
        })
        .collect();
    let (bucket_ms, _) = crate::query::logs::bucket_for(w.to_ms - w.from_ms);
    Ok(ok(
        json!({"totals": total, "models": model_rows, "series": points, "bucket_ms": bucket_ms,
               "priced_models": prices.keys().collect::<Vec<_>>()}),
        meta,
    ))
}

pub async fn breakdown(
    State(s): State<Shared>,
    Path(org): Path<String>,
    Query(p): Query<super::traces::TraceParams>,
) -> Result<Response, ApiError> {
    let by = q::By::parse(p.by.as_deref().unwrap_or("model"))
        .ok_or_else(|| ApiError::bad("by must be model, provider, kind or tool"))?;
    let (org, w, f, model) = setup(&s, &org, &p).await?;
    let (rows, meta) = s
        .run(bad_table(q::breakdown(
            &org.database,
            &org.traces_table,
            by,
            &f,
            model.as_deref(),
            w,
        ))?)
        .await?;
    let prices = prices(&s, &org.name).await?;
    let data: Vec<Value> = rows
        .iter()
        .map(|r| {
            shape(
                r,
                (by == q::By::Model).then(|| r["grp"].as_str()).flatten(),
                &prices,
            )
        })
        .collect();
    Ok(ok(data, meta))
}

fn who(r: &Value, key: &str) -> Value {
    let (prompt, completion) = (int(r, "prompt_tokens"), int(r, "completion_tokens"));
    json!({
        key: r[key], "traces": int(r, "traces"), "spans": int(r, "spans"), "llm_calls": int(r, "llm_calls"),
        "errors": int(r, "errors"), "prompt_tokens": prompt, "completion_tokens": completion,
        "total_tokens": prompt + completion, "cost": num(r, "cost"),
        "first_ms": int(r, "first_ms"), "last_ms": int(r, "last_ms"),
        "user": r["user"], "users": r.get("users").cloned().unwrap_or(Value::Null),
        "sessions": r.get("sessions").cloned().unwrap_or(Value::Null),
    })
}

pub async fn sessions(
    State(s): State<Shared>,
    Path(org): Path<String>,
    Query(p): Query<super::traces::TraceParams>,
) -> Result<Response, ApiError> {
    let (org, w, f, _) = setup(&s, &org, &p).await?;
    let (rows, meta) = s
        .run(bad_table(q::sessions(
            &org.database,
            &org.traces_table,
            &f,
            w,
            p.offset.unwrap_or(0),
        ))?)
        .await?;
    Ok(ok(
        rows.iter()
            .map(|r| who(r, "session_id"))
            .collect::<Vec<_>>(),
        meta,
    ))
}

pub async fn users(
    State(s): State<Shared>,
    Path(org): Path<String>,
    Query(p): Query<super::traces::TraceParams>,
) -> Result<Response, ApiError> {
    let (org, w, f, _) = setup(&s, &org, &p).await?;
    let (rows, meta) = s
        .run(bad_table(q::users(
            &org.database,
            &org.traces_table,
            &f,
            w,
            p.offset.unwrap_or(0),
        ))?)
        .await?;
    Ok(ok(
        rows.iter().map(|r| who(r, "user_id")).collect::<Vec<_>>(),
        meta,
    ))
}

/// One session turn by turn: a row per trace, oldest first.
pub async fn session_detail(
    State(s): State<Shared>,
    Path((org, id)): Path<(String, String)>,
    Query(p): Query<super::traces::TraceParams>,
) -> Result<Response, ApiError> {
    if id.is_empty() || id.chars().count() > 200 || id.chars().any(char::is_control) {
        return Err(ApiError::bad("invalid session id"));
    }
    let w = Window::parse(p.from, p.to, p.limit)?;
    let org = s.org(&org)?;
    require_json(&s, org).await?;
    let (rows, meta) = s
        .run(bad_table(q::session_turns(
            &org.database,
            &org.traces_table,
            &id,
            w,
        ))?)
        .await?;
    if rows.is_empty() {
        return Err(ApiError::not_found(
            "no spans for that session in this time range; widen the range",
        ));
    }
    let turns: Vec<Value> = rows
        .iter()
        .map(|r| {
            let (prompt, completion) = (int(r, "prompt_tokens"), int(r, "completion_tokens"));
            json!({"trace_id": r["trace_id"], "start_ms": int(r, "start_ms"), "duration_us": int(r, "duration_us"),
                   "spans": int(r, "spans"), "errors": int(r, "errors"), "llm_calls": int(r, "llm_calls"),
                   "prompt_tokens": prompt, "completion_tokens": completion, "total_tokens": prompt + completion,
                   "cost": num(r, "cost"),
                   // Named like the attributes they come from, so the content rules (R-AI8) mask them.
                   "input.value": r["input"], "output.value": r["output"]})
        })
        .collect();
    Ok(ok(json!({"session_id": id, "turns": turns}), meta))
}

// ---- alerts on LLM metrics (R-AI10) -------------------------------------------------------------

fn metric_value(metric: &str, row: &Value) -> f64 {
    match metric {
        "cost" => row["cost"]["total"].as_f64().unwrap_or(0.0),
        "tokens" => row["total_tokens"].as_f64().unwrap_or(0.0),
        "calls" => row["calls"].as_f64().unwrap_or(0.0),
        "error_rate" => row["error_rate"].as_f64().unwrap_or(0.0),
        _ => row["p95_ms"].as_f64().unwrap_or(0.0),
    }
}

/// The values of an LLM alert metric over `w`, one per model or one in total, and the
/// statements that produced them. The same statements as the analytics page, so a rule and the
/// page can never disagree.
pub async fn alert_values(
    s: &Shared,
    org: &Org,
    m: &crate::alerts::LlmMetric,
    w: Window,
) -> Result<(Vec<(BTreeMap<String, String>, f64)>, Vec<String>), String> {
    let cols = super::traces::columns(s, org).await.map_err(|e| e.1)?;
    if Layout::detect(&cols) != Layout::Json {
        return Err("LLM metrics need a trace v2 table".into());
    }
    let (db, table) = (&org.database, &org.traces_table);
    let f = TraceFilter::default();
    let model = m.model.as_deref();
    let mut sql = vec![];
    let by_model =
        q::breakdown(db, table, q::By::Model, &f, model, w).map_err(|e| e.to_string())?;
    sql.push(by_model.sql.clone());
    let (rows, _) = s.run(by_model).await.map_err(|e| e.1)?;
    let prices = prices(s, &org.name).await.map_err(|e| e.1)?;
    let shaped: Vec<Value> = rows
        .iter()
        .map(|r| shape(r, r["grp"].as_str(), &prices))
        .collect();
    if m.per_model {
        return Ok((
            shaped
                .iter()
                .map(|r| {
                    let mut l = BTreeMap::new();
                    l.insert(
                        "model".to_string(),
                        r["grp"].as_str().unwrap_or("").to_string(),
                    );
                    (l, metric_value(&m.metric, r))
                })
                .collect(),
            sql,
        ));
    }
    let value = match m.metric.as_str() {
        "cost" | "tokens" | "calls" => shaped.iter().map(|r| metric_value(&m.metric, r)).sum(),
        _ => {
            let t = q::totals(db, table, &f, model, w).map_err(|e| e.to_string())?;
            sql.push(t.sql.clone());
            let (rows, _) = s.run(t).await.map_err(|e| e.1)?;
            match rows.first() {
                Some(r) if int(r, "calls") > 0 => metric_value(&m.metric, &shape(r, None, &prices)),
                // No model call in the window: there is no rate or latency to report.
                _ => return Ok((vec![], sql)),
            }
        }
    };
    Ok((vec![(BTreeMap::new(), value)], sql))
}
