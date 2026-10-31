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
    query::{Bind, Q, Reader, Writer},
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicI64, Ordering},
    },
};
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
        "view_get" | "view_list" => vec![
            json!({"view_id": "0123456789abcdef", "owner": "local", "name": "errors", "kind": "logs", "spec": "{\"level\":\"ERROR\"}", "created_ms": 1000, "updated_ms": 2000}),
        ],
        "history_list" => vec![json!({"kind": "logs", "spec": "{\"q\":\"a=b\"}", "at_ms": 5000})],
        "ingest_freshness" => vec![
            json!({"stream": "logs", "newest": 1_793_399_269_857i64}),
            json!({"stream": "traces", "newest": null}),
        ],
        "ingest_rate" => vec![
            json!({"bucket_ms": 1_793_399_100_000i64, "n": 60}),
            json!({"bucket_ms": 1_793_399_160_000i64, "n": 120}),
        ],
        "ingest_services" => {
            vec![json!({"service_name": "cart", "last_seen": 1_793_399_269_857i64, "spans": 10})]
        }
        "stream_info" => vec![
            json!({"table_name": "opentelemetry_logs", "engine": "mito", "table_rows": 2016, "data_length": 48051, "index_length": 386, "create_options": "greptime.semantic.signal_type=log append_mode=true"}),
        ],
        "stream_ddl" => vec![json!({"Table": "opentelemetry_logs", "Create Table": FULLTEXT_DDL})],
        "traces_list" | "traces_scatter" => vec![
            json!({"trace_id": TID, "start_ms": 1_793_399_384_857i64, "duration_us": 9_801, "spans": 5, "services": 4, "errors": 1, "root_service": "frontend", "root_operation": "GET /"}),
            json!({"trace_id": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", "start_ms": 1_793_399_300_000i64, "duration_us": 2_000, "spans": 1, "services": 1, "errors": 0, "root_service": "cart", "root_operation": "GET /cart"}),
        ],
        "traces_histogram" => vec![
            json!({"bucket_ms": 1_793_399_100_000i64, "n": 12}),
            json!({"bucket_ms": 1_793_399_160_000i64, "n": 30}),
        ],
        "traces_services" => vec![
            json!({"service_name": "cart"}),
            json!({"service_name": "frontend"}),
        ],
        "services_red" => vec![
            json!({"service_name": "frontend", "first_seen_ms": 1_793_399_000_000i64, "last_seen_ms": 1_793_399_900_000i64, "spans": 30, "requests": 30, "errors": 3, "avg_us": 26_000.5, "p95_us": 57_000.0}),
            json!({"service_name": "postgres", "first_seen_ms": 1_793_399_000_000i64, "last_seen_ms": 1_793_399_900_000i64, "spans": 20, "requests": 0, "errors": 0, "avg_us": null, "p95_us": null}),
        ],
        "service_series" => vec![
            json!({"bucket_ms": 1_793_399_100_000i64, "requests": 10, "errors": 1, "avg_us": 20_000.0, "p95_us": 50_000.0}),
            json!({"bucket_ms": 1_793_399_400_000i64, "requests": 20, "errors": 2, "avg_us": 30_000.0, "p95_us": 60_000.0}),
        ],
        "service_operations" => vec![
            json!({"operation": "GET /", "requests": 20, "errors": 2, "avg_us": 20_000.0, "p95_us": 50_000.0}),
            json!({"operation": "POST /checkout", "requests": 10, "errors": 1, "avg_us": 40_000.0, "p95_us": 80_000.0}),
        ],
        "service_dependencies" => vec![
            json!({"caller": "frontend", "callee": "cart", "calls": 233, "errors": 0, "avg_us": 23_000.0}),
            json!({"caller": "frontend", "callee": "checkout", "calls": 240, "errors": 0, "avg_us": 26_000.0}),
            json!({"caller": "checkout", "callee": "payment", "calls": 197, "errors": 32, "avg_us": 18_000.0}),
        ],
        "metrics_names" => vec![
            json!({"table_name": "http_server_request_count_total", "create_options": "greptime.semantic.metric.type=counter on_physical_table=greptime_physical_table"}),
            json!({"table_name": "process_cpu_utilization", "create_options": "greptime.semantic.metric.type=gauge"}),
        ],
        "metrics_query" => vec![
            json!({"greptime_timestamp": 1_793_399_100_000i64, "greptime_value": 1.5, "service_name": "cart"}),
            json!({"greptime_timestamp": 1_793_399_160_000i64, "greptime_value": 2.5, "service_name": "cart"}),
            json!({"greptime_timestamp": 1_793_399_100_000i64, "greptime_value": 9.0, "service_name": "payment"}),
        ],
        "trace_spans" => sample_trace(),
        "trace_logs" => vec![log_row("1793399384857990822", "ERROR", "Charge failed")],
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

pub const TID: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
pub const T_NS: i64 = 1_793_399_000_000_000_000;

/// One `trace_spans` row.
#[allow(clippy::too_many_arguments)]
pub fn span_row(
    id: &str,
    parent: Option<&str>,
    service: &str,
    name: &str,
    start_off_ms: i64,
    end_off_ms: i64,
    status: &str,
) -> Value {
    let (s, e) = (
        T_NS + start_off_ms * 1_000_000,
        T_NS + end_off_ms * 1_000_000,
    );
    json!({
        "timestamp": s / 1_000_000, "start_ns": s.to_string(), "end_ns": e.to_string(), "trace_id": TID, "span_id": id,
        "parent_span_id": parent, "service_name": service, "span_name": name, "span_kind": "SPAN_KIND_SERVER",
        "span_status_code": status, "span_status_message": "", "duration_nano": (e - s), "scope_name": "demo",
        "span_attributes.http.route": name, "span_attributes.http.status_code": if status == "STATUS_CODE_ERROR" { 500 } else { 200 },
        "resource_attributes.service.version": "1.4.2", "span_events": [], "span_links": [],
    })
}

/// frontend 0-100ms
///   cart 5-40 -> postgres 10-30
///   checkout 45-95 -> Charge 50-70 (error), Charge 72-90, Reserve 91-94
pub fn sample_trace() -> Vec<Value> {
    let ok = "STATUS_CODE_OK";
    vec![
        span_row("a000000000000001", None, "frontend", "GET /", 0, 100, ok),
        span_row(
            "a000000000000002",
            Some("a000000000000001"),
            "cart",
            "GET /cart",
            5,
            40,
            ok,
        ),
        span_row(
            "a000000000000003",
            Some("a000000000000002"),
            "postgres",
            "SELECT carts",
            10,
            30,
            ok,
        ),
        span_row(
            "a000000000000004",
            Some("a000000000000001"),
            "checkout",
            "PlaceOrder",
            45,
            95,
            ok,
        ),
        span_row(
            "a000000000000005",
            Some("a000000000000004"),
            "payment",
            "Charge",
            50,
            70,
            "STATUS_CODE_ERROR",
        ),
        span_row(
            "a000000000000006",
            Some("a000000000000004"),
            "payment",
            "Charge",
            72,
            90,
            ok,
        ),
        span_row(
            "a000000000000007",
            Some("a000000000000004"),
            "inventory",
            "Reserve",
            91,
            94,
            ok,
        ),
    ]
}

pub const NOW_MS: i64 = 1_760_003_600_000;

// ---- in-memory state store ----------------------------------------------------

/// Behaves like the `grepui_*` tables for the queries the store builds: upsert by key, soft
/// delete, org scoping, history retention and ordering. Reads of anything else go to `sample_db`.
#[derive(Default)]
pub struct MemStore {
    pub views: Mutex<HashMap<(String, String), Vec<Bind>>>,
    pub history: Mutex<Vec<Vec<Bind>>>,
    pub notes: Mutex<HashMap<(String, String), Vec<Bind>>>,
    pub version: Mutex<Option<i64>>,
    pub ddl: Mutex<Vec<String>>,
    pub writes: Mutex<Vec<Q>>,
    pub fail_missing: Mutex<bool>,
}

fn s_of(b: &Bind) -> String {
    b.show()
}
fn i_of(b: &Bind) -> i64 {
    if let Bind::Int(i) = b { *i } else { 0 }
}
fn parse_ts(s: &str) -> i64 {
    chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S%.3f")
        .map(|d| d.and_utc().timestamp_millis())
        .unwrap_or(0)
}

impl MemStore {
    pub fn new() -> Arc<MemStore> {
        Arc::new(MemStore::default())
    }
    fn view_json(b: &[Bind]) -> Value {
        json!({"view_id": s_of(&b[1]), "owner": s_of(&b[2]), "name": s_of(&b[3]), "kind": s_of(&b[4]), "spec": s_of(&b[5]), "created_ms": i_of(&b[6]), "updated_ms": i_of(&b[7])})
    }
    pub fn write_count(&self) -> usize {
        self.writes.lock().unwrap().len()
    }
}

#[async_trait]
impl Reader for MemStore {
    async fn fetch(&self, q: &Q) -> Result<Vec<Value>, ReadError> {
        if *self.fail_missing.lock().unwrap() && q.sql.contains("grepui_") {
            return Err(ReadError::Db(
                "Table not found: greptime.grepui.grepui_views".into(),
            ));
        }
        Ok(match q.name {
            "schema_get" => self
                .version
                .lock()
                .unwrap()
                .map(|v| vec![json!({"version": v})])
                .unwrap_or_default(),
            "view_get" => {
                let (org, id) = (s_of(&q.binds[0]), s_of(&q.binds[1]));
                self.views
                    .lock()
                    .unwrap()
                    .get(&(org, id))
                    .filter(|b| b[8] == Bind::Bool(false))
                    .map(|b| vec![Self::view_json(b)])
                    .unwrap_or_default()
            }
            "view_list" => {
                let org = s_of(&q.binds[0]);
                let kind = q.binds.get(1).map(s_of);
                let mut rows: Vec<_> = self
                    .views
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|((o, _), b)| {
                        *o == org
                            && b[8] == Bind::Bool(false)
                            && kind.as_ref().is_none_or(|k| s_of(&b[4]) == *k)
                    })
                    .map(|(_, b)| (i_of(&b[7]), Self::view_json(b)))
                    .collect();
                rows.sort_by_key(|(u, _)| std::cmp::Reverse(*u));
                rows.into_iter().map(|(_, v)| v).collect()
            }
            "service_notes" | "service_note_get" => {
                let org = s_of(&q.binds[0]);
                let only = q.binds.get(1).map(s_of);
                let mut rows: Vec<(String, Value)> = self
                    .notes
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|((o, sv), b)| {
                        *o == org && b[8] == Bind::Bool(false) && only.as_ref().is_none_or(|x| x == sv)
                    })
                    .map(|((_, sv), b)| (sv.clone(), json!({"service": sv, "description": s_of(&b[2]), "owner": s_of(&b[3]), "tier": s_of(&b[4]), "runbook": s_of(&b[5]), "updated_by": s_of(&b[6]), "updated_ms": i_of(&b[7])})))
                    .collect();
                rows.sort_by(|a, b| a.0.cmp(&b.0));
                rows.into_iter().map(|(_, v)| v).collect()
            }
            "history_list" => {
                let (org, user) = (s_of(&q.binds[0]), s_of(&q.binds[1]));
                let has_kind = q.binds.len() == 4;
                let kind = has_kind.then(|| s_of(&q.binds[2]));
                let since = parse_ts(&s_of(q.binds.last().unwrap()));
                let limit: usize = q
                    .sql
                    .rsplit("LIMIT ")
                    .next()
                    .and_then(|n| n.trim().parse().ok())
                    .unwrap_or(100);
                let mut rows: Vec<(i64, Value)> = self.history.lock().unwrap().iter().filter(|b| s_of(&b[0]) == org && s_of(&b[1]) == user && kind.as_ref().is_none_or(|k| s_of(&b[2]) == *k) && parse_ts(&s_of(&b[4])) >= since).map(|b| (parse_ts(&s_of(&b[4])), json!({"kind": s_of(&b[2]), "spec": s_of(&b[3]), "at_ms": parse_ts(&s_of(&b[4]))}))).collect();
                rows.sort_by_key(|(t, _)| std::cmp::Reverse(*t));
                rows.into_iter().take(limit).map(|(_, v)| v).collect()
            }
            _ => sample_db(q)?,
        })
    }
}

#[async_trait]
impl Writer for MemStore {
    async fn exec(&self, q: &Q) -> Result<u64, ReadError> {
        self.writes.lock().unwrap().push(q.clone());
        match q.name {
            "view_put" => {
                self.views
                    .lock()
                    .unwrap()
                    .insert((s_of(&q.binds[0]), s_of(&q.binds[1])), q.binds.clone());
            }
            "service_note_put" => {
                self.notes
                    .lock()
                    .unwrap()
                    .insert((s_of(&q.binds[0]), s_of(&q.binds[1])), q.binds.clone());
            }
            "history_add" => self.history.lock().unwrap().push(q.binds.clone()),
            "schema_put" => *self.version.lock().unwrap() = Some(i_of(&q.binds[1])),
            other => return Err(ReadError::Db(format!("unexpected write {other}"))),
        }
        Ok(1)
    }
    async fn ddl(&self, sql: &str) -> Result<(), ReadError> {
        self.ddl.lock().unwrap().push(sql.to_string());
        Ok(())
    }
}

/// An app wired to a [`MemStore`] and a clock the test controls.
pub fn app_with_store(store: &Arc<MemStore>, clock: &Arc<AtomicI64>) -> Router {
    let c = clock.clone();
    router(
        AppState::with_parts(
            Config::default(),
            store.clone(),
            store.clone(),
            Arc::new(move || c.load(Ordering::SeqCst)),
        )
        .expect("state"),
    )
}

pub async fn send_json(app: &Router, method: Method, uri: &str, body: &Value) -> Resp {
    let r = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
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
