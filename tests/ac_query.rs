//! AC0.1.6 – AC0.1.9: SQL safety, window and limit caps, error handling, Show SQL meta.
mod common;
use axum::http::StatusCode;
use common::*;
use grepui::{
    error::ReadError,
    query::{self, Bind, MAX_LIMIT, MAX_WINDOW_MS, Window, build, ident, qualified},
};

// ---- AC0.1.6 identifiers and binds ------------------------------------------

#[test]
fn ac_0_1_6_ident_validation() {
    for ok in ["opentelemetry_traces", "T1", "_x", "a_b_c"] {
        assert!(ident(ok).is_ok(), "{ok}");
    }
    for bad in [
        "",
        "a b",
        "a;b",
        "a`b",
        "a'b",
        "a-b",
        "a.b",
        "t) UNION SELECT 1",
        "ünï",
        &"a".repeat(129),
    ] {
        assert!(ident(bad).is_err(), "{bad}");
        assert!(qualified("public", bad).is_err(), "{bad}");
        assert!(qualified(bad, "t").is_err(), "{bad}");
        assert!(build::stream_stats("public", bad, "ts").is_err(), "{bad}");
        assert!(build::stream_stats("public", "t", bad).is_err(), "{bad}");
    }
}

#[test]
fn ac_0_1_6_values_are_bound() {
    let evil = "x' OR '1'='1";
    let q = build::stream_schema(evil, evil);
    assert!(
        !q.sql.contains(evil) && !q.sql.contains("OR '1'"),
        "{}",
        q.sql
    );
    assert_eq!(q.sql.matches('?').count(), 2);
    assert_eq!(
        q.binds,
        vec![Bind::Str(evil.into()), Bind::Str(evil.into())]
    );

    let w = Window {
        from_ms: 1_760_000_000_000,
        to_ms: 1_760_000_060_000,
        limit: 5,
    };
    let q = build::stream_sample("public", "t", "ts", w).unwrap();
    assert_eq!(q.sql.matches('?').count(), 2);
    assert_eq!(
        q.binds,
        vec![
            Bind::Str("2025-10-09 08:53:20.000".into()),
            Bind::Str("2025-10-09 08:54:20.000".into())
        ]
    );
    assert!(
        !q.sql.contains("2025-"),
        "timestamps must be binds: {}",
        q.sql
    );
}

#[tokio::test]
async fn ac_0_1_6_hostile_stream_name_never_reaches_the_database() {
    let f = Fake::new(sample_db);
    let a = app(f.clone());
    for bad in ["t;DROP%20TABLE%20x", "a%27b", "a%20b", "x%60y"] {
        let r = get(&a, &format!("/api/v1/orgs/default/streams/{bad}")).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{bad}");
    }
    assert!(f.seen_names().is_empty());
}

// ---- AC0.1.7 window and limit -----------------------------------------------

#[tokio::test]
async fn ac_0_1_7_window_required() {
    let a = app(Fake::new(sample_db));
    for q in [
        "",
        "?from=1000",
        "?to=2000",
        "?from=2000&to=1000",
        "?from=1000&to=1000",
        "?from=-5&to=10",
    ] {
        let r = get(&a, &format!("/api/v1/orgs/default/streams/t/sample{q}")).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{q}");
        assert!(r.json()["error"].is_string());
    }
}

#[tokio::test]
async fn ac_0_1_7_window_capped() {
    let a = app(Fake::new(sample_db));
    let ok = get(
        &a,
        &format!(
            "/api/v1/orgs/default/streams/t/sample?from=1000&to={}",
            1000 + MAX_WINDOW_MS
        ),
    )
    .await;
    assert_eq!(ok.status, StatusCode::OK);
    let over = get(
        &a,
        &format!(
            "/api/v1/orgs/default/streams/t/sample?from=1000&to={}",
            1001 + MAX_WINDOW_MS
        ),
    )
    .await;
    assert_eq!(over.status, StatusCode::BAD_REQUEST);
    assert!(over.json()["error"].as_str().unwrap().contains("31 days"));
}

#[tokio::test]
async fn ac_0_1_7_limit_clamped() {
    assert_eq!(
        Window::parse(Some(0), Some(10), Some(100_000))
            .unwrap()
            .limit,
        MAX_LIMIT
    );
    assert_eq!(Window::parse(Some(0), Some(10), Some(0)).unwrap().limit, 1);
    assert_eq!(
        Window::parse(Some(0), Some(10), None).unwrap().limit,
        query::DEFAULT_LIMIT
    );
    let f = Fake::new(sample_db);
    let a = app(f.clone());
    get(
        &a,
        "/api/v1/orgs/default/streams/t/sample?from=1000&to=2000&limit=99999999",
    )
    .await;
    let seen = f.seen.lock().unwrap();
    assert!(
        seen.last()
            .unwrap()
            .sql
            .ends_with(&format!("LIMIT {MAX_LIMIT}")),
        "{}",
        seen.last().unwrap().sql
    );
}

// ---- AC0.1.8 errors ---------------------------------------------------------

#[tokio::test]
async fn ac_0_1_8_timeout_is_504() {
    let a = app(Fake::new(|_| Err(ReadError::Timeout)));
    let r = get(&a, "/api/v1/orgs/default/streams").await;
    assert_eq!(r.status, StatusCode::GATEWAY_TIMEOUT);
    assert!(r.header("content-type").starts_with("application/json"));
    assert!(r.json()["error"].as_str().unwrap().contains("too long"));
}

#[tokio::test]
async fn ac_0_1_8_db_error_is_sanitised() {
    let secret =
        "mysql://root:hunter2@10.1.2.3:4002/public: connection refused; SELECT * FROM grepui_users";
    let a = app(Fake::new(move |_| Err(ReadError::Db(secret.into()))));
    let r = get(&a, "/api/v1/orgs/default/streams").await;
    assert_eq!(r.status, StatusCode::BAD_GATEWAY);
    let body = r.text();
    for leak in ["hunter2", "10.1.2.3", "mysql://", "SELECT", "grepui_users"] {
        assert!(!body.contains(leak), "leaked {leak}: {body}");
    }
    assert!(r.json()["error"].is_string());
}

// ---- AC0.1.9 meta -----------------------------------------------------------

#[tokio::test]
async fn ac_0_1_9_meta_present() {
    let a = app(Fake::new(sample_db));
    for path in [
        "/api/v1/orgs/default/streams",
        "/api/v1/orgs/default/streams/t",
        "/api/v1/orgs/default/streams/t/stats",
        "/api/v1/orgs/default/streams/t/sample?from=1000&to=2000",
    ] {
        let j = get(&a, path).await.json();
        let m = &j["meta"];
        assert!(m["query"].is_string() && m["sql"].is_string(), "{path}");
        assert!(
            m["ms"].is_u64() && m["rows"].is_u64() && m["binds"].is_array(),
            "{path}"
        );
        assert_eq!(m["source"], "raw", "{path}");
        assert!(j.get("data").is_some(), "{path}");
    }
}

#[tokio::test]
async fn ac_0_1_9_meta_query_matches_tag() {
    let a = app(Fake::new(sample_db));
    let j = get(&a, "/api/v1/orgs/default/streams").await.json();
    assert_eq!(j["meta"]["query"], "streams_list");
    assert!(
        j["meta"]["sql"]
            .as_str()
            .unwrap()
            .starts_with("/* q:streams_list */")
    );
    assert_eq!(j["meta"]["rows"], 4);
    assert_eq!(j["meta"]["binds"][0], "public");
    let j = get(&a, "/api/v1/orgs/default/streams/t/stats").await.json();
    assert_eq!(j["meta"]["query"], "stream_stats");
}
