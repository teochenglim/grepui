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

/// LLM applications (R-AI2): OpenInference and OTel GenAI spans go to a separate **trace v2**
/// table. Verified with the real OpenTelemetry SDK and the OpenInference OpenAI instrumentor
/// (ARCH 08, ADR 0015).
pub fn llm(base: &str, db: &str, org: &str) -> Value {
    // Org names may hold `-`; a table name may not, so it becomes `_`.
    let table = format!("{}_llm_traces", org.replace('-', "_"));
    let python = format!(
        "# pip install openinference-instrumentation-openai opentelemetry-sdk opentelemetry-exporter-otlp-proto-http\n\
from opentelemetry import trace\n\
from opentelemetry.sdk.resources import Resource\n\
from opentelemetry.sdk.trace import TracerProvider\n\
from opentelemetry.sdk.trace.export import BatchSpanProcessor\n\
from opentelemetry.exporter.otlp.proto.http.trace_exporter import OTLPSpanExporter\n\
from openinference.instrumentation.openai import OpenAIInstrumentor\n\
\n\
provider = TracerProvider(resource=Resource.create({{\n\
    \"service.name\": \"my-llm-app\",\n\
    \"openinference.project.name\": \"my-project\",   # grepui reads this as the project\n\
}}))\n\
provider.add_span_processor(BatchSpanProcessor(OTLPSpanExporter(\n\
    endpoint=\"{base}/v1/otlp/v1/traces\",\n\
    headers={{\n\
        \"x-greptime-db-name\": \"{db}\",\n\
        \"x-greptime-pipeline-name\": \"greptime_trace_v2\",\n\
        \"x-greptime-trace-table-name\": \"{table}\",\n\
    }},\n\
)))\n\
trace.set_tracer_provider(provider)\n\
OpenAIInstrumentor().instrument(tracer_provider=provider)\n"
    );
    let env = format!(
        "export OTEL_EXPORTER_OTLP_TRACES_ENDPOINT={base}/v1/otlp/v1/traces\nexport OTEL_EXPORTER_OTLP_TRACES_PROTOCOL=http/protobuf\nexport OTEL_EXPORTER_OTLP_TRACES_HEADERS=\"x-greptime-db-name={db},x-greptime-pipeline-name=greptime_trace_v2,x-greptime-trace-table-name={table}\"\nexport OTEL_RESOURCE_ATTRIBUTES=\"openinference.project.name=my-project\"\n"
    );
    let collector = format!(
        "exporters:\n  otlphttp/greptime-llm:\n    traces_endpoint: {base}/v1/otlp/v1/traces\n    encoding: proto\n    headers:\n      x-greptime-db-name: {db}\n      x-greptime-pipeline-name: greptime_trace_v2\n      x-greptime-trace-table-name: {table}\nservice:\n  pipelines:\n    traces/llm: {{ receivers: [otlp], exporters: [otlphttp/greptime-llm] }}\n"
    );
    json!({
        "table": table,
        "headers": {"x-greptime-db-name": db, "x-greptime-pipeline-name": "greptime_trace_v2", "x-greptime-trace-table-name": table},
        "python": python, "sdk_env": env, "otel_collector": collector,
        "notes": [
            "Use the greptime_trace_v2 pipeline (GreptimeDB v1.3.0 or newer). It stores attributes as JSON in three columns instead of one column per attribute, so long conversations and tool calls do not widen the table.",
            "Send LLM spans to their own table: a v2 write to a trace v1 table is rejected, and existing v1 tables are not migrated.",
            format!("An org reads one traces table from its own database. Give LLM traces their own org (a platform admin creates it with its database and traces table {table}), or start grepui with --traces-table {table} for the default org. grepui detects the layout by itself."),
            "grepui reads the project from the resource attribute openinference.project.name. Both OpenInference (llm.*, openinference.span.kind) and OTel GenAI (gen_ai.*) attributes are stored as sent.",
            "Prompts and completions are user data and can be large. Keep content capture on only where you may store it, set a TTL on the table, and use redaction rules; values over 32 KiB are cut in the trace view.",
            "GreptimeDB's JSON2 columns shred at most 100 paths into columns automatically; everything else is still stored and read.",
        ],
    })
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
        "llm": llm(&base, &org.database, &org.name),
    });
    // No database query is involved: the response is derived from configuration (`source: config`).
    let meta = Meta {
        query: "connect".into(),
        sql: String::new(),
        binds: vec![],
        ms: 0,
        rows: 0,
        source: "config",
        note: None,
    };
    Ok(ok(data, meta))
}
