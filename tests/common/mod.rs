#![allow(dead_code)]
use async_trait::async_trait;
use axum::{
    Router,
    body::Body,
    http::{HeaderMap, Method, Request, StatusCode},
};
use grepui::{
    api::{AppState, router},
    config::Config,
    error::ReadError,
    query::{Q, Reader},
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tower::ServiceExt;

type Script = Box<dyn Fn(&Q) -> Result<Vec<Value>, ReadError> + Send + Sync>;

/// A scripted `Reader` that records every query it receives.
pub struct Fake {
    script: Script,
    pub seen: Mutex<Vec<Q>>,
}

impl Fake {
    pub fn new(
        f: impl Fn(&Q) -> Result<Vec<Value>, ReadError> + Send + Sync + 'static,
    ) -> Arc<Fake> {
        Arc::new(Fake {
            script: Box::new(f),
            seen: Mutex::new(vec![]),
        })
    }
    pub fn seen_names(&self) -> Vec<&'static str> {
        self.seen.lock().unwrap().iter().map(|q| q.name).collect()
    }
}

#[async_trait]
impl Reader for Fake {
    async fn fetch(&self, q: &Q) -> Result<Vec<Value>, ReadError> {
        self.seen.lock().unwrap().push(q.clone());
        (self.script)(q)
    }
}

/// Plausible rows for every builder in v0.1.0.
pub fn sample_db(q: &Q) -> Result<Vec<Value>, ReadError> {
    Ok(match q.name {
        "health" => vec![json!({"ok": 1})],
        "streams_list" => vec![
            json!({"table_name": "opentelemetry_logs", "table_type": "BASE TABLE", "engine": "mito", "table_rows": 1499, "create_options": "greptime.semantic.signal_type=log append_mode=true"}),
            json!({"table_name": "opentelemetry_traces", "table_type": "BASE TABLE", "engine": "mito", "table_rows": 1499, "create_options": ""}),
            json!({"table_name": "opentelemetry_traces_services", "table_type": "BASE TABLE", "engine": "mito", "table_rows": 6, "create_options": ""}),
            json!({"table_name": "orders", "table_type": "BASE TABLE", "engine": "mito", "table_rows": 3, "create_options": ""}),
        ],
        "stream_schema" => vec![
            json!({"column_name": "timestamp", "data_type": "timestamp(9)", "semantic_type": "TIMESTAMP"}),
            json!({"column_name": "service_name", "data_type": "string", "semantic_type": "TAG"}),
            json!({"column_name": "body", "data_type": "string", "semantic_type": "FIELD"}),
        ],
        "stream_stats" => vec![
            json!({"total": 1499, "oldest": 1_760_000_000_000i64, "newest": 1_760_003_600_000i64}),
        ],
        "logs_ddl" => vec![json!({"Table": "opentelemetry_logs", "Create Table": FULLTEXT_DDL})],
        "logs_search" | "logs_record" => vec![log_row(
            "1793399269857388008",
            "ERROR",
            "Refund failed: upstream timeout in 29 ms",
        )],
        "logs_around" => vec![
            log_row("1793399269857388000", "INFO", "before"),
            log_row("1793399269857388008", "ERROR", "pivot"),
        ],
        "logs_histogram" => vec![
            json!({"bucket_ms": 1_793_399_100_000i64, "n": 45}),
            json!({"bucket_ms": 1_793_399_400_000i64, "n": 194}),
        ],
        "logs_top_values" => vec![
            json!({"value": "INFO", "n": 90}),
            json!({"value": "ERROR", "n": 10}),
        ],
        "sql_mode" => vec![json!({"b": 1, "a": 2})],
        "stream_sample" => {
            vec![json!({"timestamp": 1_760_003_600_000i64, "service_name": "cart", "body": "ok"})]
        }
        _ => vec![],
    })
}

pub const FULLTEXT_DDL: &str = "CREATE TABLE IF NOT EXISTS `opentelemetry_logs` (\n  `timestamp` TIMESTAMP(9) NOT NULL,\n  `body` STRING NULL FULLTEXT INDEX WITH(analyzer = 'English'),\n  TIME INDEX (`timestamp`)\n)";
pub const PLAIN_DDL: &str = "CREATE TABLE IF NOT EXISTS `opentelemetry_logs` (\n  `timestamp` TIMESTAMP(9) NOT NULL,\n  `body` STRING NULL,\n  TIME INDEX (`timestamp`)\n)";

pub fn log_row(ts_ns: &str, level: &str, body: &str) -> Value {
    json!({"timestamp": 1_793_399_269_857i64, "ts_ns": ts_ns, "severity_text": level, "severity_number": 17, "body": body,
           "trace_id": "f1ebd7ef1a8ecefd2ce38517da7e7234", "span_id": "e3586378d5b65d18", "scope_name": "demo",
           "log_attributes": {"http.method": "POST"}, "resource_attributes": {"service.name": "payment"}})
}

pub fn app_with(cfg: Config, reader: Arc<dyn Reader>) -> Router {
    router(AppState::new(cfg, reader).expect("valid config"))
}

pub fn app(reader: Arc<dyn Reader>) -> Router {
    app_with(Config::default(), reader)
}

pub struct Resp {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
}

impl Resp {
    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or_else(|_| panic!("not JSON: {}", self.text()))
    }
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
    pub fn header(&self, name: &str) -> &str {
        self.headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
    }
}

pub async fn send(app: &Router, method: Method, uri: &str) -> Resp {
    let r = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let (parts, body) = r.into_parts();
    Resp {
        status: parts.status,
        headers: parts.headers,
        body: body.collect().await.unwrap().to_bytes().to_vec(),
    }
}

pub async fn get(app: &Router, uri: &str) -> Resp {
    send(app, Method::GET, uri).await
}

pub const NOW_MS: i64 = 1_760_003_600_000;
