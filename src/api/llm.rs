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
