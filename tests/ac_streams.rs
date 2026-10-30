//! AC0.1.10 – AC0.1.11: Streams (R-T1) and the Connect page (R-I1).
mod common;
use axum::http::StatusCode;
use common::*;
use grepui::{
    api::{connect, streams::classify},
    config::Config,
    org::Orgs,
    query::{Reader, SqlxReader, build},
};
use serde_json::json;
use std::time::Duration;

// ---- AC0.1.10 streams -------------------------------------------------------

#[tokio::test]
async fn ac_0_1_10_streams_shape() {
    let a = app(Fake::new(sample_db));
    let j = get(&a, "/api/v1/orgs/default/streams").await.json();
    let d = j["data"].as_array().unwrap();
    assert_eq!(d.len(), 4);
    let kind = |n: &str| d.iter().find(|r| r["name"] == n).unwrap()["kind"].clone();
    assert_eq!(kind("opentelemetry_logs"), "logs");
    assert_eq!(kind("opentelemetry_traces"), "traces");
    assert_eq!(kind("opentelemetry_traces_services"), "traces_aux");
    assert_eq!(kind("orders"), "table");
    assert_eq!(d[0]["rows_estimate"], 1499);
    assert_eq!(d[0]["engine"], "mito");

    let org = Orgs::from_config(&Config::default()).unwrap();
    let org = org.resolve("default").unwrap();
    assert_eq!(
        classify(org, "anything", "greptime.semantic.signal_type=trace"),
        "traces"
    );
    assert_eq!(
        classify(org, "custom_logs", "greptime.semantic.signal_type=log"),
        "logs"
    );
    assert_eq!(classify(org, "opentelemetry_logs_x", ""), "logs_aux");
}

#[tokio::test]
async fn ac_0_1_10_schema_roles() {
    let a = app(Fake::new(sample_db));
    let j = get(&a, "/api/v1/orgs/default/streams/opentelemetry_logs")
        .await
        .json();
    assert_eq!(j["data"]["time_index"], "timestamp");
    let cols = j["data"]["columns"].as_array().unwrap();
    let role = |n: &str| cols.iter().find(|c| c["name"] == n).unwrap()["role"].clone();
    assert_eq!(role("timestamp"), "time_index");
    assert_eq!(role("service_name"), "tag");
    assert_eq!(role("body"), "field");
    assert_eq!(cols[0]["type"], "timestamp(9)");

    let stats = get(&a, "/api/v1/orgs/default/streams/opentelemetry_logs/stats")
        .await
        .json();
    assert_eq!(
        stats["data"],
        json!({"rows": 1499, "oldest": 1_760_000_000_000i64, "newest": 1_760_003_600_000i64})
    );

    let empty = app(Fake::new(|_| Ok(vec![])));
    assert_eq!(
        get(&empty, "/api/v1/orgs/default/streams/ghost")
            .await
            .status,
        StatusCode::NOT_FOUND
    );
}

/// Live: needs `make db-up` and `python3 scripts/seed.py` (set GREPUI_TEST_DB).
#[tokio::test]
#[ignore = "needs GreptimeDB: make db-up && python3 scripts/seed.py && make test-db"]
async fn ac_0_1_10_live_streams_live() {
    let url = std::env::var("GREPUI_TEST_DB")
        .expect("set GREPUI_TEST_DB=mysql://root@127.0.0.1:4002/public");
    let reader = std::sync::Arc::new(SqlxReader::lazy(&url, Duration::from_secs(20)).unwrap());
    // Every builder must be accepted by the real database with real bound parameters.
    for q in [build::health(), build::streams_list("public")] {
        reader
            .fetch(&q)
            .await
            .unwrap_or_else(|e| panic!("{}: {e}", q.name));
    }
    let a = app_with(
        Config {
            greptime_mysql: url,
            ..Config::default()
        },
        reader,
    );

    let list = get(&a, "/api/v1/orgs/default/streams").await.json();
    let d = list["data"].as_array().unwrap();
    let kind = |n: &str| d.iter().find(|r| r["name"] == n).map(|r| r["kind"].clone());
    assert_eq!(
        kind("opentelemetry_traces"),
        Some(json!("traces")),
        "{list}"
    );
    assert_eq!(kind("opentelemetry_logs"), Some(json!("logs")));

    let sch = get(&a, "/api/v1/orgs/default/streams/opentelemetry_traces")
        .await
        .json();
    assert_eq!(sch["data"]["time_index"], "timestamp", "{sch}");
    let roles: Vec<_> = sch["data"]["columns"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            (
                c["name"].as_str().unwrap().to_string(),
                c["role"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert!(
        roles.contains(&("service_name".into(), "tag".into())),
        "{roles:?}"
    );

    let st = get(&a, "/api/v1/orgs/default/streams/opentelemetry_logs/stats")
        .await
        .json();
    assert!(st["data"]["rows"].as_i64().unwrap() > 0, "{st}");
    assert!(st["data"]["newest"].as_i64().unwrap() >= st["data"]["oldest"].as_i64().unwrap());

    let now = chrono::Utc::now().timestamp_millis();
    let url = format!(
        "/api/v1/orgs/default/streams/opentelemetry_logs/sample?from={}&to={}&limit=5",
        now - 30 * 86_400_000,
        now + 3_600_000
    );
    let s = get(&a, &url).await;
    assert_eq!(s.status, StatusCode::OK, "{}", s.text());
    let rows = s.json()["data"].as_array().unwrap().clone();
    assert!(!rows.is_empty() && rows.len() <= 5);
    assert!(
        rows[0]["timestamp"].is_i64(),
        "timestamps are epoch ms: {}",
        rows[0]
    );
    assert!(
        rows[0]["log_attributes"].is_object(),
        "JSON columns come back as objects: {}",
        rows[0]
    );

    // Window with no data must be empty, proving the bound timestamps filter.
    let none = get(
        &a,
        "/api/v1/orgs/default/streams/opentelemetry_logs/sample?from=1000&to=2000",
    )
    .await
    .json();
    assert_eq!(none["data"].as_array().unwrap().len(), 0, "{none}");
}

// ---- AC0.1.11 connect -------------------------------------------------------

async fn connect_json(cfg: Config) -> serde_json::Value {
    get(
        &app_with(cfg, Fake::new(sample_db)),
        "/api/v1/orgs/default/connect",
    )
    .await
    .json()
}

#[tokio::test]
async fn ac_0_1_11_connect_uses_configured_address() {
    let cfg = Config {
        greptime_http_public: "https://db.example.com:4000/".into(),
        greptime_mysql: "mysql://root@h:4002/tenant_a".into(),
        ..Config::default()
    };
    let j = connect_json(cfg).await;
    let d = &j["data"];
    assert_eq!(d["http_base"], "https://db.example.com:4000");
    let traces = d["endpoints"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["signal"] == "traces")
        .unwrap();
    assert_eq!(
        traces["url"],
        "https://db.example.com:4000/v1/otlp/v1/traces"
    );
    assert_eq!(traces["headers"]["x-greptime-db-name"], "tenant_a");
    assert!(
        d["snippets"]["otel_collector"]
            .as_str()
            .unwrap()
            .contains("endpoint: https://db.example.com:4000/v1/otlp")
    );
    assert_eq!(j["meta"]["source"], "config");
}

#[tokio::test]
async fn ac_0_1_11_connect_mentions_protobuf() {
    let j = connect_json(Config::default()).await;
    let notes = j["data"]["notes"].as_array().unwrap();
    assert!(
        notes
            .iter()
            .any(|n| n.as_str().unwrap().contains("protobuf only"))
    );
    assert!(
        notes
            .iter()
            .any(|n| n.as_str().unwrap().contains("greptime_trace_v1"))
    );
    for e in j["data"]["endpoints"].as_array().unwrap() {
        assert_eq!(e["content_type"], "application/x-protobuf");
    }
    assert!(connect::notes().len() >= 3);
}

#[tokio::test]
async fn ac_0_1_11_connect_snippets_complete() {
    let j = connect_json(Config::default()).await;
    let s = &j["data"]["snippets"];
    for k in ["otel_collector", "sdk_env", "prometheus", "check_tables"] {
        let v = s[k].as_str().unwrap_or_default();
        assert!(
            v.len() > 20 && v.contains("http://127.0.0.1:4000") && v.contains("public"),
            "{k}: {v}"
        );
    }
    assert!(
        s["sdk_env"]
            .as_str()
            .unwrap()
            .contains("OTEL_EXPORTER_OTLP_PROTOCOL=http/protobuf")
    );
    let signals: Vec<_> = j["data"]["endpoints"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["signal"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        signals,
        ["traces", "logs", "metrics", "prometheus_remote_write"]
    );
}
