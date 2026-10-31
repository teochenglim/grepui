//! The typed REST API (`/api/v1`). Every data route is org-scoped (R-F5).

pub mod connect;
pub mod dashboards;
pub mod health;
pub mod ingest;
pub mod logs;
pub mod metrics;
pub mod services;
pub mod sql;
pub mod storage;
pub mod streams;
pub mod traces;
pub mod views;

use crate::{
    config::Config,
    error::ApiError,
    org::{Org, Orgs},
    query::{self, Meta, Q, Reader, Writer},
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
use std::{sync::Arc, time::Instant};

pub struct AppState {
    pub cfg: Arc<Config>,
    pub orgs: Orgs,
    pub reader: Arc<dyn Reader>,
    /// The only way grepui writes (its own metadata tables); see `store::check_write`.
    pub writer: Arc<dyn Writer>,
    /// Milliseconds since the epoch; injectable so tests control time.
    pub clock: Clock,
    /// Whether each org's logs table has a fulltext index on `body` (R-L6), cached briefly.
    pub ft_cache: std::sync::Mutex<std::collections::HashMap<String, (std::time::Instant, bool)>>,
    /// Columns of each org's traces table (flat attribute columns included), cached briefly.
    pub cols_cache: ColsCache,
}

pub type Shared = Arc<AppState>;
pub type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;
/// Column names of a table by `org/table`, with when they were read.
pub type ColsCache =
    std::sync::Mutex<std::collections::HashMap<String, (std::time::Instant, Arc<Vec<String>>)>>;

pub fn system_clock() -> Clock {
    Arc::new(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0)
    })
}

/// A writer that refuses everything: used when no state store is configured (tests, read-only runs).
pub struct NoWriter;

#[async_trait::async_trait]
impl Writer for NoWriter {
    async fn exec(&self, _q: &Q) -> Result<u64, crate::error::ReadError> {
        Err(crate::error::ReadError::Db(
            "the state store is not configured".into(),
        ))
    }
    async fn ddl(&self, _sql: &str) -> Result<(), crate::error::ReadError> {
        Err(crate::error::ReadError::Db(
            "the state store is not configured".into(),
        ))
    }
}

impl AppState {
    /// Read-only state: the store refuses writes (`NoWriter`) and time is the system clock.
    pub fn new(cfg: Config, reader: Arc<dyn Reader>) -> anyhow::Result<Shared> {
        Self::with_parts(cfg, reader, Arc::new(NoWriter), system_clock())
    }

    pub fn with_parts(
        cfg: Config,
        reader: Arc<dyn Reader>,
        writer: Arc<dyn Writer>,
        clock: Clock,
    ) -> anyhow::Result<Shared> {
        cfg.validate()?;
        let orgs = Orgs::from_config(&cfg)?;
        Ok(Arc::new(AppState {
            cfg: Arc::new(cfg),
            orgs,
            reader,
            writer,
            clock,
            ft_cache: Default::default(),
            cols_cache: Default::default(),
        }))
    }

    pub fn now_ms(&self) -> i64 {
        (self.clock)()
    }

    /// The metadata database name (validated at startup).
    pub fn meta_db(&self) -> &str {
        &self.cfg.meta_database
    }

    /// The only write path: gate, execute, report like a read ("Show SQL" works for writes too).
    pub async fn write(&self, q: Q) -> Result<Meta, ApiError> {
        crate::store::check_write(&q, self.meta_db()).map_err(|e| {
            tracing::error!(error = %e, query = q.name, "blocked a write");
            ApiError(StatusCode::INTERNAL_SERVER_ERROR, "internal error".into())
        })?;
        let t = Instant::now();
        let n = self.writer.exec(&q).await.map_err(store_error)?;
        Ok(Meta {
            query: q.name.to_string(),
            sql: q.sql.clone(),
            binds: q.binds.iter().map(query::Bind::show).collect(),
            ms: t.elapsed().as_millis() as u64,
            rows: n as usize,
            source: "raw",
            note: None,
        })
    }

    /// Read a metadata table; a missing table means "run `grepui migrate`".
    pub async fn read_store(&self, q: Q) -> Result<(Vec<Value>, Meta), ApiError> {
        query::run(self.reader.as_ref(), q)
            .await
            .map_err(store_error)
    }

    fn admin_enabled(&self) -> Result<(), ApiError> {
        if self.cfg.admin_writes {
            Ok(())
        } else {
            Err(ApiError(
                StatusCode::FORBIDDEN,
                "admin writes are disabled; start grepui with --admin-writes (or GREPUI_ADMIN_WRITES=1)"
                    .into(),
            ))
        }
    }

    /// Admin DDL (rollup create/drop, TTL): needs `--admin-writes`, passes `admin::check`, is logged.
    pub async fn admin_ddl(
        &self,
        org: &Org,
        name: &'static str,
        sql: &str,
    ) -> Result<Meta, ApiError> {
        self.admin_enabled()?;
        crate::admin::check(sql, 0, org)?;
        tracing::info!(target: "audit", org = %org.name, actor = crate::store::LOCAL_USER, action = name, %sql, "admin write");
        let t = Instant::now();
        self.writer.ddl(sql).await.map_err(store_error)?;
        Ok(Meta {
            query: name.to_string(),
            sql: sql.to_string(),
            binds: vec![],
            ms: t.elapsed().as_millis() as u64,
            rows: 0,
            source: "raw",
            note: None,
        })
    }

    /// Admin bulk insert (rollup backfill chunk); same gate and audit line as [`Self::admin_ddl`].
    pub async fn admin_exec(&self, org: &Org, q: Q) -> Result<Meta, ApiError> {
        self.admin_enabled()?;
        let body = q
            .sql
            .strip_prefix(&format!("/* q:{} */ ", q.name))
            .unwrap_or_default();
        crate::admin::check(body, q.binds.len(), org)?;
        tracing::info!(target: "audit", org = %org.name, actor = crate::store::LOCAL_USER, action = q.name, binds = ?q.binds, "admin write");
        let t = Instant::now();
        let n = self.writer.exec(&q).await.map_err(store_error)?;
        Ok(Meta {
            query: q.name.to_string(),
            sql: q.sql.clone(),
            binds: q.binds.iter().map(query::Bind::show).collect(),
            ms: t.elapsed().as_millis() as u64,
            rows: n as usize,
            source: "raw",
            note: None,
        })
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
    "/api/v1/orgs/{org}/streams/{name}/info",
    "/api/v1/orgs/{org}/streams/{name}/ddl",
    "/api/v1/orgs/{org}/connect",
    "/api/v1/orgs/{org}/ingest/freshness",
    "/api/v1/orgs/{org}/ingest/rate",
    "/api/v1/orgs/{org}/ingest/services",
    "/api/v1/orgs/{org}/views",
    "/api/v1/orgs/{org}/views/{id}",
    "/api/v1/orgs/{org}/history",
    "/api/v1/orgs/{org}/traces",
    "/api/v1/orgs/{org}/traces/histogram",
    "/api/v1/orgs/{org}/traces/scatter",
    "/api/v1/orgs/{org}/traces/services",
    "/api/v1/orgs/{org}/traces/{trace_id}",
    "/api/v1/orgs/{org}/traces/{trace_id}/flame",
    "/api/v1/orgs/{org}/traces/{trace_id}/logs",
    "/api/v1/orgs/{org}/services",
    "/api/v1/orgs/{org}/services/{service}",
    "/api/v1/orgs/{org}/services/{service}/annotation",
    "/api/v1/orgs/{org}/dependencies",
    "/api/v1/orgs/{org}/catalog",
    "/api/v1/orgs/{org}/folders",
    "/api/v1/orgs/{org}/folders/{id}",
    "/api/v1/orgs/{org}/dashboards",
    "/api/v1/orgs/{org}/dashboards/export",
    "/api/v1/orgs/{org}/dashboards/import",
    "/api/v1/orgs/{org}/dashboards/move",
    "/api/v1/orgs/{org}/dashboards/{id}",
    "/api/v1/orgs/{org}/dashboards/{id}/export",
    "/api/v1/orgs/{org}/dashboards/{id}/variables",
    "/api/v1/orgs/{org}/dashboards/{id}/panels/{panel}/data",
    "/api/v1/orgs/{org}/dashboards/{id}/shares",
    "/api/v1/orgs/{org}/dashboards/{id}/shares/{share}",
    "/api/v1/orgs/{org}/public/{token}",
    "/api/v1/orgs/{org}/public/{token}/variables",
    "/api/v1/orgs/{org}/public/{token}/panels/{panel}/data",
    "/api/v1/orgs/{org}/playlists",
    "/api/v1/orgs/{org}/playlists/{id}",
    "/api/v1/orgs/{org}/playlists/{id}/now",
    "/api/v1/orgs/{org}/rollups",
    "/api/v1/orgs/{org}/rollups/{template}",
    "/api/v1/orgs/{org}/rollups/{template}/backfill",
    "/api/v1/orgs/{org}/retention",
    "/api/v1/orgs/{org}/retention/{stream}",
    "/api/v1/orgs/{org}/retention/{stream}/preview",
    "/api/v1/orgs/{org}/metrics",
    "/api/v1/orgs/{org}/metrics/query",
    "/api/v1/orgs/{org}/metrics/{name}/labels",
    "/api/v1/orgs/{org}/logs",
    "/api/v1/orgs/{org}/logs/histogram",
    "/api/v1/orgs/{org}/logs/fields",
    "/api/v1/orgs/{org}/logs/fields/{field}/values",
    "/api/v1/orgs/{org}/logs/record",
    "/api/v1/orgs/{org}/logs/around",
    "/api/v1/orgs/{org}/logs/export",
    "/api/v1/orgs/{org}/sql",
];

/// The only routes that change state, with their methods. Every other route is GET-only. Each
/// writes through `AppState::write`, which allows nothing but the `grepui_*` tables (R-F4).
pub const WRITE_ROUTES: &[(&str, &str)] = &[
    ("POST", "/api/v1/orgs/{org}/views"),
    ("PUT", "/api/v1/orgs/{org}/views/{id}"),
    ("DELETE", "/api/v1/orgs/{org}/views/{id}"),
    ("POST", "/api/v1/orgs/{org}/history"),
    ("POST", "/api/v1/orgs/{org}/folders"),
    ("PUT", "/api/v1/orgs/{org}/folders/{id}"),
    ("DELETE", "/api/v1/orgs/{org}/folders/{id}"),
    ("POST", "/api/v1/orgs/{org}/dashboards"),
    ("POST", "/api/v1/orgs/{org}/dashboards/import"),
    ("POST", "/api/v1/orgs/{org}/dashboards/move"),
    ("PUT", "/api/v1/orgs/{org}/dashboards/{id}"),
    ("DELETE", "/api/v1/orgs/{org}/dashboards/{id}"),
    ("POST", "/api/v1/orgs/{org}/dashboards/{id}/shares"),
    (
        "DELETE",
        "/api/v1/orgs/{org}/dashboards/{id}/shares/{share}",
    ),
    ("POST", "/api/v1/orgs/{org}/playlists"),
    ("PUT", "/api/v1/orgs/{org}/playlists/{id}"),
    ("DELETE", "/api/v1/orgs/{org}/playlists/{id}"),
    ("POST", "/api/v1/orgs/{org}/rollups/{template}"),
    ("POST", "/api/v1/orgs/{org}/rollups/{template}/backfill"),
    ("DELETE", "/api/v1/orgs/{org}/rollups/{template}"),
    ("PUT", "/api/v1/orgs/{org}/retention/{stream}"),
    ("PUT", "/api/v1/orgs/{org}/services/{service}/annotation"),
    ("DELETE", "/api/v1/orgs/{org}/services/{service}/annotation"),
];

/// Largest request body any route accepts.
pub const MAX_BODY: usize = 64 * 1024;

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
        .route("/api/v1/orgs/{org}/streams/{name}/info", get(streams::info))
        .route("/api/v1/orgs/{org}/streams/{name}/ddl", get(streams::ddl))
        .route("/api/v1/orgs/{org}/connect", get(connect::get))
        .route(
            "/api/v1/orgs/{org}/ingest/freshness",
            get(ingest::freshness),
        )
        .route("/api/v1/orgs/{org}/ingest/rate", get(ingest::rate))
        .route("/api/v1/orgs/{org}/ingest/services", get(ingest::services))
        .route(
            "/api/v1/orgs/{org}/views",
            get(views::list).post(views::create),
        )
        .route(
            "/api/v1/orgs/{org}/views/{id}",
            get(views::get).put(views::update).delete(views::delete),
        )
        .route(
            "/api/v1/orgs/{org}/history",
            get(views::history).post(views::record),
        )
        .route("/api/v1/orgs/{org}/services", get(services::list))
        .route(
            "/api/v1/orgs/{org}/services/{service}",
            get(services::detail),
        )
        .route(
            "/api/v1/orgs/{org}/services/{service}/annotation",
            axum::routing::put(services::annotate).delete(services::clear_annotation),
        )
        .route(
            "/api/v1/orgs/{org}/dependencies",
            get(services::dependencies),
        )
        .route("/api/v1/orgs/{org}/catalog", get(services::catalog))
        .route(
            "/api/v1/orgs/{org}/folders",
            get(dashboards::folder_list).post(dashboards::folder_create),
        )
        .route(
            "/api/v1/orgs/{org}/folders/{id}",
            axum::routing::put(dashboards::folder_rename).delete(dashboards::folder_delete),
        )
        .route(
            "/api/v1/orgs/{org}/dashboards",
            get(dashboards::dash_list).post(dashboards::dash_create),
        )
        .route(
            "/api/v1/orgs/{org}/dashboards/export",
            get(dashboards::export_all),
        )
        .route(
            "/api/v1/orgs/{org}/dashboards/import",
            axum::routing::post(dashboards::import),
        )
        .route(
            "/api/v1/orgs/{org}/dashboards/move",
            axum::routing::post(dashboards::move_many),
        )
        .route(
            "/api/v1/orgs/{org}/dashboards/{id}",
            get(dashboards::dash_get)
                .put(dashboards::dash_update)
                .delete(dashboards::dash_delete),
        )
        .route(
            "/api/v1/orgs/{org}/dashboards/{id}/export",
            get(dashboards::export_one),
        )
        .route(
            "/api/v1/orgs/{org}/dashboards/{id}/variables",
            get(dashboards::variables),
        )
        .route(
            "/api/v1/orgs/{org}/dashboards/{id}/panels/{panel}/data",
            get(dashboards::data),
        )
        .route(
            "/api/v1/orgs/{org}/dashboards/{id}/shares",
            get(dashboards::share_list).post(dashboards::share_create),
        )
        .route(
            "/api/v1/orgs/{org}/dashboards/{id}/shares/{share}",
            axum::routing::delete(dashboards::share_revoke),
        )
        .route(
            "/api/v1/orgs/{org}/public/{token}",
            get(dashboards::public_get),
        )
        .route(
            "/api/v1/orgs/{org}/public/{token}/variables",
            get(dashboards::public_variables),
        )
        .route(
            "/api/v1/orgs/{org}/public/{token}/panels/{panel}/data",
            get(dashboards::public_data),
        )
        .route(
            "/api/v1/orgs/{org}/playlists",
            get(dashboards::playlist_list).post(dashboards::playlist_create),
        )
        .route(
            "/api/v1/orgs/{org}/playlists/{id}",
            get(dashboards::playlist_get)
                .put(dashboards::playlist_update)
                .delete(dashboards::playlist_delete),
        )
        .route(
            "/api/v1/orgs/{org}/playlists/{id}/now",
            get(dashboards::playlist_now),
        )
        .route("/api/v1/orgs/{org}/rollups", get(storage::rollups))
        .route(
            "/api/v1/orgs/{org}/rollups/{template}",
            axum::routing::post(storage::rollup_create).delete(storage::rollup_drop),
        )
        .route(
            "/api/v1/orgs/{org}/rollups/{template}/backfill",
            axum::routing::post(storage::rollup_backfill),
        )
        .route("/api/v1/orgs/{org}/retention", get(storage::retention))
        .route(
            "/api/v1/orgs/{org}/retention/{stream}",
            axum::routing::put(storage::retention_set),
        )
        .route(
            "/api/v1/orgs/{org}/retention/{stream}/preview",
            get(storage::retention_preview),
        )
        .route("/api/v1/orgs/{org}/metrics", get(metrics::names))
        .route("/api/v1/orgs/{org}/metrics/query", get(metrics::query))
        .route(
            "/api/v1/orgs/{org}/metrics/{name}/labels",
            get(metrics::labels),
        )
        .route("/api/v1/orgs/{org}/traces", get(traces::list))
        .route(
            "/api/v1/orgs/{org}/traces/histogram",
            get(traces::histogram),
        )
        .route("/api/v1/orgs/{org}/traces/scatter", get(traces::scatter))
        .route("/api/v1/orgs/{org}/traces/services", get(traces::services))
        .route("/api/v1/orgs/{org}/traces/{trace_id}", get(traces::detail))
        .route(
            "/api/v1/orgs/{org}/traces/{trace_id}/flame",
            get(traces::flame),
        )
        .route(
            "/api/v1/orgs/{org}/traces/{trace_id}/logs",
            get(traces::logs),
        )
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
        .layer(axum::extract::DefaultBodyLimit::max(MAX_BODY))
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

/// Map a metadata failure: a missing table or database is a 503 with the fix, not a 502.
pub fn store_error(e: crate::error::ReadError) -> ApiError {
    if let crate::error::ReadError::Db(d) = &e
        && (crate::store::is_missing(d) || d.contains("not configured"))
    {
        return ApiError(
            StatusCode::SERVICE_UNAVAILABLE,
            "the state store is not initialised: run `grepui migrate`".into(),
        );
    }
    e.into()
}
