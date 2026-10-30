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
        "stream_sample" => {
            vec![json!({"timestamp": 1_760_003_600_000i64, "service_name": "cart", "body": "ok"})]
        }
        _ => vec![],
    })
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
