//! The typed REST API (`/api/v1`). Every data route is org-scoped (R-F5).

pub mod connect;
pub mod health;
pub mod logs;
pub mod sql;
pub mod streams;

use crate::{
    config::Config,
    error::ApiError,
    org::{Org, Orgs},
    query::{self, Meta, Q, Reader},
    ui,
};
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, Method, StatusCode, Uri, header},
    response::{IntoResponse, Response},
    routing::get,
};
use serde::Serialize;
use serde_json::{Value, json};
use std::sync::Arc;

pub struct AppState {
    pub cfg: Arc<Config>,
    pub orgs: Orgs,
    pub reader: Arc<dyn Reader>,
    /// Whether each org's logs table has a fulltext index on `body` (R-L6), cached briefly.
    pub ft_cache: std::sync::Mutex<std::collections::HashMap<String, (std::time::Instant, bool)>>,
}

pub type Shared = Arc<AppState>;

impl AppState {
    pub fn new(cfg: Config, reader: Arc<dyn Reader>) -> anyhow::Result<Shared> {
        cfg.validate()?;
        let orgs = Orgs::from_config(&cfg)?;
        Ok(Arc::new(AppState {
            cfg: Arc::new(cfg),
            orgs,
            reader,
            ft_cache: Default::default(),
        }))
    }

    pub fn org(&self, name: &str) -> Result<&Org, ApiError> {
        self.orgs.resolve(name)
    }

    /// Run a builder's query, mapping failures to API errors.
    pub async fn run(&self, q: Q) -> Result<(Vec<Value>, Meta), ApiError> {
        Ok(query::run(self.reader.as_ref(), q).await?)
    }
}

/// `{ "data": ..., "meta": ... }` (R-Q4).
#[derive(Serialize)]
pub struct Envelope<T: Serialize> {
    pub data: T,
    pub meta: Meta,
}

pub fn ok<T: Serialize>(data: T, meta: Meta) -> Response {
    Json(Envelope { data, meta }).into_response()
}

/// Every route the router serves, with `{org}`/`{name}` placeholders. Tests walk this list
/// (read-only proof, method checks); `router` below must register exactly these.
pub const ROUTES: &[&str] = &[
    "/healthz",
    "/api/v1/health",
    "/api/v1/info",
    "/api/v1/orgs",
    "/api/v1/orgs/{org}/streams",
    "/api/v1/orgs/{org}/streams/{name}",
    "/api/v1/orgs/{org}/streams/{name}/stats",
    "/api/v1/orgs/{org}/streams/{name}/sample",
    "/api/v1/orgs/{org}/connect",
    "/api/v1/orgs/{org}/logs",
    "/api/v1/orgs/{org}/logs/histogram",
    "/api/v1/orgs/{org}/logs/fields",
    "/api/v1/orgs/{org}/logs/fields/{field}/values",
    "/api/v1/orgs/{org}/logs/record",
    "/api/v1/orgs/{org}/logs/around",
    "/api/v1/orgs/{org}/logs/export",
    "/api/v1/orgs/{org}/sql",
];

pub fn router(state: Shared) -> Router {
    Router::new()
        .route("/healthz", get(health::healthz))
        .route("/api/v1/health", get(health::health))
        .route("/api/v1/info", get(info))
        .route("/api/v1/orgs", get(orgs))
        .route("/api/v1/orgs/{org}/streams", get(streams::list))
        .route("/api/v1/orgs/{org}/streams/{name}", get(streams::schema))
        .route(
            "/api/v1/orgs/{org}/streams/{name}/stats",
            get(streams::stats),
        )
        .route(
            "/api/v1/orgs/{org}/streams/{name}/sample",
            get(streams::sample),
        )
        .route("/api/v1/orgs/{org}/connect", get(connect::get))
        .route("/api/v1/orgs/{org}/logs", get(logs::search))
        .route("/api/v1/orgs/{org}/logs/histogram", get(logs::histogram))
        .route("/api/v1/orgs/{org}/logs/fields", get(logs::fields))
        .route(
            "/api/v1/orgs/{org}/logs/fields/{field}/values",
            get(logs::top_values),
        )
        .route("/api/v1/orgs/{org}/logs/record", get(logs::record))
        .route("/api/v1/orgs/{org}/logs/around", get(logs::around))
        .route("/api/v1/orgs/{org}/logs/export", get(logs::export))
        .route("/api/v1/orgs/{org}/sql", get(sql::run))
        .fallback(fallback)
        .with_state(state)
}

async fn info() -> Json<Value> {
    Json(json!({"name": "grepui", "version": crate::VERSION}))
}

async fn orgs(State(s): State<Shared>) -> Json<Value> {
    let list: Vec<Value> = s
        .orgs
        .list()
        .iter()
        .map(|o| json!({"name": o.name}))
        .collect();
    Json(json!({"orgs": list}))
}

/// Unknown `/api/*` is JSON 404; everything else is the embedded UI (R-F3).
async fn fallback(method: Method, uri: Uri, headers: HeaderMap) -> Response {
    let path = uri.path();
    if path == "/api" || path.starts_with("/api/") {
        return ApiError::not_found("unknown API path").into_response();
    }
    if method != Method::GET && method != Method::HEAD {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    ui::serve(
        path,
        headers
            .get(header::IF_NONE_MATCH)
            .and_then(|v| v.to_str().ok()),
    )
}
