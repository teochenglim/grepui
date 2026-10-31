//! The typed REST API (`/api/v1`). Every data route is org-scoped (R-F5).

pub mod access;
pub mod alerts;
pub mod auth;
pub mod authz;
pub mod connect;
pub mod dashboards;
pub mod enrichment;
pub mod functions;
pub mod health;
pub mod ingest;
pub mod llm;
pub mod logs;
pub mod metrics;
pub mod pipelines;
pub mod redaction;
pub mod reports;
pub mod rum;
pub mod services;
pub mod sql;
pub mod storage;
pub mod streams;
pub mod traces;
pub mod views;
pub mod workload;

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
use std::{collections::HashMap, sync::Arc, time::Instant};

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
    /// Delivers alert notifications (webhook, SMTP); a fake in tests.
    pub sender: Arc<dyn crate::alerts::notify::Sender>,
    /// Redaction rules per org, kept in memory so no response waits on a read (ADR 0011).
    pub redact: std::sync::RwLock<HashMap<String, Arc<crate::redact::Rules>>>,
    /// `false` until the first load at startup: data responses wait rather than leak.
    pub redact_ready: std::sync::atomic::AtomicBool,
    /// Per org: what roles below admin see of LLM prompts and completions (refreshed with the redaction rules).
    pub llm_content: std::sync::RwLock<HashMap<String, crate::llm::Mode>>,
    /// Sessions, tokens and rate limits in memory (ADR 0012).
    pub auth: crate::auth::AuthState,
    /// Audit rows waiting to be written.
    pub audit: crate::audit::Audit,
    /// Work groups, running queries and statistics (ADR 0013).
    pub workload: Arc<crate::workload::Workload>,
    /// Extra GreptimeDB sources for fan-out reads.
    pub sources: crate::workload::Sources,
    /// `/metrics` counters (ADR 0014).
    pub metrics: Arc<crate::metrics::Metrics>,
    /// The result cache with single-flight, shared by the primary and every extra source.
    pub cache: Arc<crate::cache::ResultCache>,
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
        let sender = Arc::new(crate::alerts::notify::NetSender::from_config(&cfg));
        Self::with_sender(cfg, reader, writer, clock, sender)
    }

    pub fn with_sender(
        cfg: Config,
        reader: Arc<dyn Reader>,
        writer: Arc<dyn Writer>,
        clock: Clock,
        sender: Arc<dyn crate::alerts::notify::Sender>,
    ) -> anyhow::Result<Shared> {
        cfg.validate()?;
        let orgs = Orgs::from_config(&cfg)?;
        let workload = Arc::new(crate::workload::Workload::default());
        let metrics = Arc::new(crate::metrics::Metrics::default());
        let cache = Arc::new(crate::cache::ResultCache::new(
            std::time::Duration::from_secs(cfg.cache_ttl_secs),
            cfg.cache_max_entries,
            cfg.cache_max_mb as usize * 1024 * 1024,
            metrics.clone(),
        ));
        let reader: Arc<dyn Reader> = Arc::new(crate::cache::Cached::new(
            Arc::new(crate::workload::Governed::new(
                reader,
                workload.clone(),
                clock.clone(),
                metrics.clone(),
            )),
            cache.clone(),
            crate::workload::PRIMARY,
        ));
        Ok(Arc::new(AppState {
            cfg: Arc::new(cfg),
            orgs,
            reader,
            writer,
            clock,
            ft_cache: Default::default(),
            cols_cache: Default::default(),
            sender,
            redact: Default::default(),
            redact_ready: std::sync::atomic::AtomicBool::new(true),
            llm_content: Default::default(),
            auth: Default::default(),
            audit: Default::default(),
            workload,
            sources: Default::default(),
            metrics,
            cache,
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

    pub fn admin_enabled(&self) -> Result<(), ApiError> {
        // With sign-in the route table already demanded an org admin; `--admin-writes` is the
        // stand-in only when there is no sign-in (ADR 0012).
        if !self.cfg.no_auth || self.cfg.admin_writes {
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
        // Uploaded cells are data, not audit material: log how many values, not which.
        if q.name == "enrich_insert" {
            tracing::info!(target: "audit", org = %org.name, actor = crate::store::LOCAL_USER, action = q.name, binds = q.binds.len(), "admin write");
        } else {
            tracing::info!(target: "audit", org = %org.name, actor = crate::store::LOCAL_USER, action = q.name, binds = ?q.binds, "admin write");
        }
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

    /// `CREATE DATABASE IF NOT EXISTS` for a new org: a platform admin's action, one fixed shape.
    pub async fn platform_ddl(&self, sql: &str) -> Result<(), ApiError> {
        self.admin_enabled()?;
        crate::admin::check_platform(sql)?;
        tracing::info!(target: "audit", actor = crate::store::LOCAL_USER, %sql, "platform write");
        self.writer.ddl(sql).await.map_err(store_error)
    }

    /// One call to GreptimeDB's pipeline API. `write` calls need `--admin-writes`, may only touch
    /// this org's pipelines (`pipelines::check_path`) and are logged.
    pub async fn pipeline_call(
        &self,
        org: &Org,
        action: &'static str,
        method: &str,
        path: &str,
        body: Option<(&str, Vec<u8>)>,
        write: bool,
    ) -> Result<(u16, String), ApiError> {
        if write {
            self.admin_enabled()?;
            tracing::info!(target: "audit", org = %org.name, actor = crate::store::LOCAL_USER, action, %path, "admin write");
        }
        if !crate::pipelines::check_path(&org.name, path) {
            tracing::error!(%path, "blocked a pipeline call");
            return Err(ApiError(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal error".into(),
            ));
        }
        let base = self
            .cfg
            .pipeline_http
            .clone()
            .unwrap_or_else(|| self.cfg.greptime_http_public.clone());
        let (headers, bytes) = match &body {
            Some((ct, b)) => (vec![("content-type", *ct)], b.as_slice()),
            None => (vec![], &[][..]),
        };
        crate::http::request(method, &base, path, &headers, bytes)
            .await
            .map_err(|e| {
                tracing::warn!(error = %e, "pipeline API call failed");
                ApiError(
                    StatusCode::BAD_GATEWAY,
                    "cannot reach GreptimeDB's HTTP API (see --pipeline-http)".into(),
                )
            })
    }

    /// Register an extra source; its reads are held to the same work groups as the primary's.
    pub fn add_source(&self, name: &str, reader: Arc<dyn Reader>) {
        self.sources.add(
            name,
            Arc::new(crate::cache::Cached::new(
                Arc::new(crate::workload::Governed::new(
                    reader,
                    self.workload.clone(),
                    self.clock.clone(),
                    self.metrics.clone(),
                )),
                self.cache.clone(),
                name,
            )),
        );
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
    let rows = meta.rows;
    let mut r = Json(Envelope { data, meta }).into_response();
    // How many rows the answer holds, for the audit trail and clients.
    if let Ok(v) = header::HeaderValue::from_str(&rows.to_string()) {
        r.headers_mut().insert("x-grepui-rows", v);
    }
    r
}

/// Every route the router serves, with `{org}`/`{name}` placeholders. Tests walk this list
/// (read-only proof, method checks); `router` below must register exactly these.
pub const ROUTES: &[&str] = &[
    "/healthz",
    "/metrics",
    "/api/v1/health",
    "/api/v1/info",
    "/api/v1/orgs",
    "/api/v1/auth/me",
    "/api/v1/auth/setup",
    "/api/v1/auth/login",
    "/api/v1/auth/logout",
    "/api/v1/auth/password",
    "/api/v1/auth/oidc/login",
    "/api/v1/auth/oidc/callback",
    "/api/v1/platform/sources",
    "/api/v1/platform/sources/{name}",
    "/api/v1/platform/orgs",
    "/api/v1/platform/orgs/{org}",
    "/api/v1/platform/users",
    "/api/v1/platform/users/{user}",
    "/api/v1/platform/audit",
    "/api/v1/orgs/{org}/members",
    "/api/v1/orgs/{org}/members/{user}",
    "/api/v1/orgs/{org}/tokens",
    "/api/v1/orgs/{org}/tokens/{id}",
    "/api/v1/orgs/{org}/branding",
    "/api/v1/orgs/{org}/audit",
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
    "/api/v1/orgs/{org}/alerts",
    "/api/v1/orgs/{org}/alerts/info",
    "/api/v1/orgs/{org}/alerts/preview",
    "/api/v1/orgs/{org}/alerts/export",
    "/api/v1/orgs/{org}/alerts/import",
    "/api/v1/orgs/{org}/alerts/history",
    "/api/v1/orgs/{org}/alerts/{id}",
    "/api/v1/orgs/{org}/alerts/{id}/preview",
    "/api/v1/orgs/{org}/alerts/{id}/history",
    "/api/v1/orgs/{org}/alerts/{id}/incident",
    "/api/v1/orgs/{org}/destinations",
    "/api/v1/orgs/{org}/destinations/{id}",
    "/api/v1/orgs/{org}/destinations/{id}/test",
    "/api/v1/orgs/{org}/downtimes",
    "/api/v1/orgs/{org}/downtimes/{id}",
    "/api/v1/orgs/{org}/incidents",
    "/api/v1/orgs/{org}/incidents/{id}",
    "/api/v1/orgs/{org}/incidents/{id}/ack",
    "/api/v1/orgs/{org}/incidents/{id}/resolve",
    "/api/v1/orgs/{org}/pipelines",
    "/api/v1/orgs/{org}/pipelines/dry-run",
    "/api/v1/orgs/{org}/pipelines/{name}",
    "/api/v1/orgs/{org}/functions",
    "/api/v1/orgs/{org}/functions/{name}",
    "/api/v1/orgs/{org}/enrichment",
    "/api/v1/orgs/{org}/enrichment/{name}",
    "/api/v1/orgs/{org}/enrichment/{name}/join",
    "/api/v1/orgs/{org}/redaction",
    "/api/v1/orgs/{org}/redaction/{id}",
    "/api/v1/orgs/{org}/traces/layout",
    "/api/v1/orgs/{org}/rum/status",
    "/api/v1/orgs/{org}/rum/sites",
    "/api/v1/orgs/{org}/rum/overview",
    "/api/v1/orgs/{org}/rum/vitals",
    "/api/v1/orgs/{org}/rum/errors",
    "/api/v1/orgs/{org}/rum/sessions",
    "/api/v1/orgs/{org}/reports",
    "/api/v1/orgs/{org}/reports/{id}",
    "/api/v1/orgs/{org}/reports/{id}/preview",
    "/api/v1/orgs/{org}/reports/{id}/run",
    "/api/v1/orgs/{org}/reports/{id}/history",
    "/api/v1/orgs/{org}/llm/prices",
    "/api/v1/orgs/{org}/llm/content",
    "/api/v1/orgs/{org}/llm/feedback",
    "/api/v1/orgs/{org}/traces/{id}/agent-graph",
    "/api/v1/orgs/{org}/workgroups",
    "/api/v1/orgs/{org}/workgroups/{role}",
    "/api/v1/orgs/{org}/queries",
    "/api/v1/orgs/{org}/queries/stats",
    "/api/v1/orgs/{org}/queries/{id}/cancel",
    "/api/v1/orgs/{org}/sources",
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
    ("POST", "/api/v1/orgs/{org}/alerts"),
    ("POST", "/api/v1/orgs/{org}/alerts/import"),
    ("PUT", "/api/v1/orgs/{org}/alerts/{id}"),
    ("DELETE", "/api/v1/orgs/{org}/alerts/{id}"),
    ("POST", "/api/v1/orgs/{org}/alerts/{id}/incident"),
    ("POST", "/api/v1/orgs/{org}/destinations"),
    ("PUT", "/api/v1/orgs/{org}/destinations/{id}"),
    ("DELETE", "/api/v1/orgs/{org}/destinations/{id}"),
    ("POST", "/api/v1/orgs/{org}/destinations/{id}/test"),
    ("POST", "/api/v1/orgs/{org}/downtimes"),
    ("PUT", "/api/v1/orgs/{org}/downtimes/{id}"),
    ("DELETE", "/api/v1/orgs/{org}/downtimes/{id}"),
    ("POST", "/api/v1/orgs/{org}/incidents/{id}/ack"),
    ("POST", "/api/v1/orgs/{org}/incidents/{id}/resolve"),
    ("POST", "/api/v1/auth/setup"),
    ("POST", "/api/v1/auth/login"),
    ("POST", "/api/v1/auth/logout"),
    ("POST", "/api/v1/auth/password"),
    ("POST", "/api/v1/platform/orgs"),
    ("DELETE", "/api/v1/platform/orgs/{org}"),
    ("POST", "/api/v1/platform/users"),
    ("PUT", "/api/v1/platform/users/{user}"),
    ("DELETE", "/api/v1/platform/users/{user}"),
    ("POST", "/api/v1/orgs/{org}/members"),
    ("PUT", "/api/v1/orgs/{org}/members/{user}"),
    ("DELETE", "/api/v1/orgs/{org}/members/{user}"),
    ("POST", "/api/v1/orgs/{org}/tokens"),
    ("DELETE", "/api/v1/orgs/{org}/tokens/{id}"),
    ("PUT", "/api/v1/orgs/{org}/branding"),
    ("POST", "/api/v1/orgs/{org}/pipelines/dry-run"),
    ("PUT", "/api/v1/orgs/{org}/pipelines/{name}"),
    ("DELETE", "/api/v1/orgs/{org}/pipelines/{name}"),
    ("PUT", "/api/v1/orgs/{org}/functions/{name}"),
    ("DELETE", "/api/v1/orgs/{org}/functions/{name}"),
    ("POST", "/api/v1/orgs/{org}/enrichment/{name}"),
    ("DELETE", "/api/v1/orgs/{org}/enrichment/{name}"),
    ("POST", "/api/v1/orgs/{org}/redaction"),
    ("PUT", "/api/v1/orgs/{org}/redaction/{id}"),
    ("DELETE", "/api/v1/orgs/{org}/redaction/{id}"),
    ("POST", "/api/v1/orgs/{org}/reports"),
    ("PUT", "/api/v1/orgs/{org}/reports/{id}"),
    ("DELETE", "/api/v1/orgs/{org}/reports/{id}"),
    ("POST", "/api/v1/orgs/{org}/reports/{id}/run"),
    ("PUT", "/api/v1/orgs/{org}/llm/prices"),
    ("DELETE", "/api/v1/orgs/{org}/llm/prices"),
    ("PUT", "/api/v1/orgs/{org}/llm/content"),
    ("PUT", "/api/v1/orgs/{org}/llm/feedback"),
    ("DELETE", "/api/v1/orgs/{org}/llm/feedback"),
    ("PUT", "/api/v1/orgs/{org}/workgroups/{role}"),
    ("DELETE", "/api/v1/orgs/{org}/workgroups/{role}"),
    ("POST", "/api/v1/orgs/{org}/queries/{id}/cancel"),
    ("PUT", "/api/v1/platform/sources/{name}"),
    ("PUT", "/api/v1/orgs/{org}/services/{service}/annotation"),
    ("DELETE", "/api/v1/orgs/{org}/services/{service}/annotation"),
];

/// Largest request body any route accepts.
pub const MAX_BODY: usize = 64 * 1024;

pub fn router(state: Shared) -> Router {
    Router::new()
        .route("/healthz", get(health::healthz))
        .route("/metrics", get(health::metrics))
        .route("/api/v1/health", get(health::health))
        .route("/api/v1/info", get(info))
        .route("/api/v1/orgs", get(orgs))
        .route("/api/v1/auth/me", get(auth::me))
        .route("/api/v1/auth/setup", axum::routing::post(auth::setup))
        .route("/api/v1/auth/login", axum::routing::post(auth::login))
        .route("/api/v1/auth/logout", axum::routing::post(auth::logout))
        .route(
            "/api/v1/auth/password",
            axum::routing::post(auth::change_password),
        )
        .route("/api/v1/auth/oidc/login", get(auth::oidc_login))
        .route("/api/v1/auth/oidc/callback", get(auth::oidc_callback))
        .route(
            "/api/v1/platform/orgs",
            get(access::platform_orgs).post(access::platform_org_create),
        )
        .route(
            "/api/v1/platform/orgs/{org}",
            axum::routing::delete(access::platform_org_delete),
        )
        .route(
            "/api/v1/platform/users",
            get(access::platform_users).post(access::platform_user_create),
        )
        .route(
            "/api/v1/platform/users/{user}",
            axum::routing::put(access::platform_user_update).delete(access::platform_user_delete),
        )
        .route("/api/v1/platform/audit", get(access::audit_platform))
        .route(
            "/api/v1/orgs/{org}/members",
            get(access::members).post(access::member_add),
        )
        .route(
            "/api/v1/orgs/{org}/members/{user}",
            axum::routing::put(access::member_update).delete(access::member_remove),
        )
        .route(
            "/api/v1/orgs/{org}/tokens",
            get(access::tokens).post(access::token_create),
        )
        .route(
            "/api/v1/orgs/{org}/tokens/{id}",
            axum::routing::delete(access::token_revoke),
        )
        .route(
            "/api/v1/orgs/{org}/branding",
            get(access::branding).put(access::branding_set),
        )
        .route("/api/v1/orgs/{org}/audit", get(access::audit_org))
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
        .route(
            "/api/v1/orgs/{org}/alerts",
            get(alerts::alert_list).post(alerts::alert_create),
        )
        .route("/api/v1/orgs/{org}/alerts/info", get(alerts::info))
        .route(
            "/api/v1/orgs/{org}/alerts/preview",
            get(alerts::preview_draft),
        )
        .route("/api/v1/orgs/{org}/alerts/export", get(alerts::export_all))
        .route(
            "/api/v1/orgs/{org}/alerts/import",
            axum::routing::post(alerts::import),
        )
        .route(
            "/api/v1/orgs/{org}/alerts/history",
            get(alerts::history_all),
        )
        .route(
            "/api/v1/orgs/{org}/alerts/{id}",
            get(alerts::alert_get)
                .put(alerts::alert_update)
                .delete(alerts::alert_delete),
        )
        .route(
            "/api/v1/orgs/{org}/alerts/{id}/preview",
            get(alerts::preview_saved),
        )
        .route(
            "/api/v1/orgs/{org}/alerts/{id}/history",
            get(alerts::history_one),
        )
        .route(
            "/api/v1/orgs/{org}/alerts/{id}/incident",
            axum::routing::post(alerts::incident_from_alert),
        )
        .route(
            "/api/v1/orgs/{org}/destinations",
            get(alerts::dest_list).post(alerts::dest_create),
        )
        .route(
            "/api/v1/orgs/{org}/destinations/{id}",
            axum::routing::put(alerts::dest_update).delete(alerts::dest_delete),
        )
        .route(
            "/api/v1/orgs/{org}/destinations/{id}/test",
            axum::routing::post(alerts::dest_test),
        )
        .route(
            "/api/v1/orgs/{org}/downtimes",
            get(alerts::down_list).post(alerts::down_create),
        )
        .route(
            "/api/v1/orgs/{org}/downtimes/{id}",
            axum::routing::put(alerts::down_update).delete(alerts::down_delete),
        )
        .route("/api/v1/orgs/{org}/incidents", get(alerts::incident_list))
        .route(
            "/api/v1/orgs/{org}/incidents/{id}",
            get(alerts::incident_get),
        )
        .route(
            "/api/v1/orgs/{org}/incidents/{id}/ack",
            axum::routing::post(alerts::incident_ack),
        )
        .route(
            "/api/v1/orgs/{org}/incidents/{id}/resolve",
            axum::routing::post(alerts::incident_resolve),
        )
        .route("/api/v1/orgs/{org}/pipelines", get(pipelines::list))
        .route(
            "/api/v1/orgs/{org}/pipelines/dry-run",
            axum::routing::post(pipelines::dry_run),
        )
        .route(
            "/api/v1/orgs/{org}/pipelines/{name}",
            get(pipelines::get)
                .put(pipelines::put)
                .delete(pipelines::delete),
        )
        .route("/api/v1/orgs/{org}/functions", get(functions::list))
        .route(
            "/api/v1/orgs/{org}/functions/{name}",
            axum::routing::put(functions::put).delete(functions::delete),
        )
        .route("/api/v1/orgs/{org}/enrichment", get(enrichment::list))
        .route(
            "/api/v1/orgs/{org}/enrichment/{name}",
            get(enrichment::get)
                .post(enrichment::upload)
                .delete(enrichment::drop)
                .layer(enrichment::upload_limit()),
        )
        .route(
            "/api/v1/orgs/{org}/enrichment/{name}/join",
            get(enrichment::join),
        )
        .route(
            "/api/v1/orgs/{org}/redaction",
            get(redaction::list).post(redaction::create),
        )
        .route(
            "/api/v1/orgs/{org}/redaction/{id}",
            axum::routing::put(redaction::update).delete(redaction::delete),
        )
        .route("/api/v1/orgs/{org}/rum/status", get(rum::status))
        .route("/api/v1/orgs/{org}/rum/sites", get(rum::sites))
        .route("/api/v1/orgs/{org}/rum/overview", get(rum::overview))
        .route("/api/v1/orgs/{org}/rum/vitals", get(rum::vitals))
        .route("/api/v1/orgs/{org}/rum/errors", get(rum::errors))
        .route("/api/v1/orgs/{org}/rum/sessions", get(rum::sessions))
        .route(
            "/api/v1/orgs/{org}/reports",
            get(reports::list).post(reports::create),
        )
        .route(
            "/api/v1/orgs/{org}/reports/{id}",
            axum::routing::put(reports::update).delete(reports::delete),
        )
        .route(
            "/api/v1/orgs/{org}/reports/{id}/preview",
            get(reports::preview),
        )
        .route(
            "/api/v1/orgs/{org}/reports/{id}/run",
            axum::routing::post(reports::run_now),
        )
        .route(
            "/api/v1/orgs/{org}/reports/{id}/history",
            get(reports::history),
        )
        .route(
            "/api/v1/orgs/{org}/llm/prices",
            get(llm::prices_list)
                .put(llm::price_put)
                .delete(llm::price_delete),
        )
        .route(
            "/api/v1/orgs/{org}/llm/feedback",
            get(llm::feedback_list)
                .put(llm::feedback_put)
                .delete(llm::feedback_delete),
        )
        .route(
            "/api/v1/orgs/{org}/traces/{id}/agent-graph",
            get(traces::agent_graph),
        )
        .route(
            "/api/v1/orgs/{org}/llm/content",
            get(llm::content_get).put(llm::content_put),
        )
        .route("/api/v1/orgs/{org}/workgroups", get(workload::groups_list))
        .route(
            "/api/v1/orgs/{org}/workgroups/{role}",
            axum::routing::put(workload::group_put).delete(workload::group_delete),
        )
        .route("/api/v1/orgs/{org}/queries", get(workload::queries_list))
        .route(
            "/api/v1/orgs/{org}/queries/stats",
            get(workload::queries_stats),
        )
        .route(
            "/api/v1/orgs/{org}/queries/{id}/cancel",
            axum::routing::post(workload::query_cancel),
        )
        .route("/api/v1/orgs/{org}/sources", get(workload::sources_org))
        .route("/api/v1/platform/sources", get(workload::sources_platform))
        .route(
            "/api/v1/platform/sources/{name}",
            axum::routing::put(workload::source_put),
        )
        .route("/api/v1/orgs/{org}/metrics", get(metrics::names))
        .route("/api/v1/orgs/{org}/metrics/query", get(metrics::query))
        .route(
            "/api/v1/orgs/{org}/metrics/{name}/labels",
            get(metrics::labels),
        )
        .route("/api/v1/orgs/{org}/traces/layout", get(traces::layout_info))
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
        // A route layer runs after the route is matched, so the layer can read the template.
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            redaction::layer,
        ))
        // Outermost of the route layers: identify, authorize and audit before anything else.
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            authz::layer,
        ))
        .fallback(fallback)
        .layer(axum::extract::DefaultBodyLimit::max(MAX_BODY))
        .with_state(state)
}

async fn info() -> Json<Value> {
    Json(json!({"name": "grepui", "version": crate::VERSION}))
}

/// The orgs the caller may see: all of them for a platform admin (or with `--no-auth`), the
/// ones they are a member of otherwise.
async fn orgs(
    State(s): State<Shared>,
    axum::Extension(id): axum::Extension<crate::auth::Identity>,
) -> Json<Value> {
    let mut list = vec![];
    for o in s
        .orgs
        .list()
        .iter()
        .filter(|o| id.role_in(&o.name).is_some())
    {
        let b = crate::auth::store::get(&s, crate::store::build::PLATFORM_ORG, "orgdef", &o.name)
            .await
            .ok()
            .flatten()
            .and_then(|r| serde_json::from_str::<Value>(&r.spec).ok())
            .map(|v| v["branding"].clone())
            .filter(Value::is_object)
            .unwrap_or(json!({}));
        list.push(json!({"name": o.name, "role": id.role_in(&o.name).map(crate::auth::Role::name), "branding": b}));
    }
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
