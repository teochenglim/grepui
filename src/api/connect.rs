//! Connect page data (R-I1): where and how to send telemetry, built from the configured address.
//! grepui never receives telemetry; collectors and SDKs write to GreptimeDB directly.

use super::{Shared, ok};
use crate::{error::ApiError, query::Meta};
use axum::{
    extract::{Path, State},
    response::Response,
};
use serde_json::{Value, json};

pub fn endpoints(base: &str, db: &str, traces_table: &str, logs_table: &str) -> Vec<Value> {
    let h = |pairs: &[(&str, &str)]| -> Value {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), json!(v)))
            .collect::<serde_json::Map<_, _>>()
            .into()
    };
    vec![
        json!({"signal": "traces", "method": "POST", "url": format!("{base}/v1/otlp/v1/traces"), "content_type": "application/x-protobuf",
               "headers": h(&[("x-greptime-db-name", db), ("x-greptime-pipeline-name", "greptime_trace_v1"), ("x-greptime-trace-table-name", traces_table)])}),
        json!({"signal": "logs", "method": "POST", "url": format!("{base}/v1/otlp/v1/logs"), "content_type": "application/x-protobuf",
               "headers": h(&[("x-greptime-db-name", db), ("x-greptime-log-table-name", logs_table)])}),
        json!({"signal": "metrics", "method": "POST", "url": format!("{base}/v1/otlp/v1/metrics"), "content_type": "application/x-protobuf",
               "headers": h(&[("x-greptime-db-name", db)])}),
        json!({"signal": "prometheus_remote_write", "method": "POST", "url": format!("{base}/v1/prometheus/write?db={db}"), "content_type": "application/x-protobuf", "headers": h(&[])}),
    ]
}

pub fn snippets(base: &str, db: &str, traces_table: &str, logs_table: &str) -> Value {
    let collector = format!(
        "exporters:\n  otlphttp/greptime:\n    endpoint: {base}/v1/otlp\n    encoding: proto\n    headers:\n      x-greptime-db-name: {db}\n      x-greptime-pipeline-name: greptime_trace_v1\n      x-greptime-trace-table-name: {traces_table}\n      x-greptime-log-table-name: {logs_table}\nservice:\n  pipelines:\n    traces:  {{ receivers: [otlp], exporters: [otlphttp/greptime] }}\n    logs:    {{ receivers: [otlp], exporters: [otlphttp/greptime] }}\n    metrics: {{ receivers: [otlp], exporters: [otlphttp/greptime] }}\n"
    );
    let env = format!(
        "export OTEL_EXPORTER_OTLP_ENDPOINT={base}/v1/otlp\nexport OTEL_EXPORTER_OTLP_PROTOCOL=http/protobuf\nexport OTEL_EXPORTER_OTLP_HEADERS=\"x-greptime-db-name={db},x-greptime-pipeline-name=greptime_trace_v1\"\n"
    );
    let prom = format!("remote_write:\n  - url: {base}/v1/prometheus/write?db={db}\n");
    let check =
        format!("curl -s -X POST '{base}/v1/sql?db={db}' --data-urlencode 'sql=SHOW TABLES'");
    json!({"otel_collector": collector, "sdk_env": env, "prometheus": prom, "check_tables": check})
}

pub fn notes() -> Vec<&'static str> {
    vec![
        "GreptimeDB's OTLP endpoint accepts protobuf only (application/x-protobuf). JSON bodies are rejected with HTTP 400.",
        "Send traces with the greptime_trace_v1 pipeline header; GreptimeDB then creates the opentelemetry_traces table and its _services and _operations tables.",
        "grepui does not receive telemetry. Point collectors and SDKs at GreptimeDB; grepui reads what arrives.",
    ]
}

pub async fn get(State(s): State<Shared>, Path(org): Path<String>) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let base = s.cfg.greptime_http_public.trim_end_matches('/').to_string();
    let data = json!({
        "org": org.name,
        "database": org.database,
        "http_base": base,
        "endpoints": endpoints(&base, &org.database, &org.traces_table, &org.logs_table),
        "snippets": snippets(&base, &org.database, &org.traces_table, &org.logs_table),
        "notes": notes(),
    });
    // No database query is involved: the response is derived from configuration (`source: config`).
    let meta = Meta {
        query: "connect".into(),
        sql: String::new(),
        binds: vec![],
        ms: 0,
        rows: 0,
        source: "config",
    };
    Ok(ok(data, meta))
}
