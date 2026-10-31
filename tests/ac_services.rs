//! AC0.5.1 – AC0.5.4: RED per service, series and operations, dependencies and the service map,
//! the catalog and its annotations.
mod common;
use axum::{Router, http::Method, http::StatusCode};
use common::*;
use grepui::{
    api::{AppState, router},
    config::Config,
    layout::{self, EdgeIn},
    playground::{http, send, synth},
    query::{Bind, SqlxReader, Window, services},
};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, atomic::AtomicI64},
    time::Duration,
};

const BASE: &str = "/api/v1/orgs/default";
const W: &str = "from=1793398000000&to=1793402000000";

fn win() -> Window {
    Window {
        from_ms: 1_793_398_000_000,
        to_ms: 1_793_402_000_000,
        limit: 100,
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

// ---- AC0.5.1 RED ------------------------------------------------------------------

#[test]
fn ac_0_5_1_red_shape() {
    let q = services::services("public", "opentelemetry_traces", None, win()).unwrap();
    assert!(
        q.sql.starts_with("/* q:services_red */ SELECT "),
        "{}",
        q.sql
    );
    // RED is over entry spans only; each aggregate is computed once (planner rule).
    assert!(
        q.sql
            .contains("`span_kind` IN ('SPAN_KIND_SERVER', 'SPAN_KIND_CONSUMER')")
    );
    assert_eq!(q.sql.matches("approx_percentile_cont(").count(), 1);
    assert_eq!(q.sql.matches("AS `requests`").count(), 1);
    assert!(q.sql.contains("GROUP BY `service_name`"));
    assert!(!q.sql.contains("JOIN"), "RED needs no join");
    assert_eq!(q.binds.len(), 2, "only the window is bound");
    let one =
        services::services("public", "opentelemetry_traces", Some("it's; DROP"), win()).unwrap();
    assert!(
        !one.sql.contains("DROP"),
        "the name is bound, never formatted"
    );
    assert_eq!(one.binds.last(), Some(&Bind::Str("it's; DROP".into())));
}

#[tokio::test]
async fn ac_0_5_1_list_adds_rate_and_error_pct() {
    let a = app(Fake::new(sample_db));
    let r = get(&a, &format!("{BASE}/services?{W}")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    let fe = &j["data"][0];
    assert_eq!(fe["service_name"], "frontend");
    assert!((fe["error_pct"].as_f64().unwrap() - 10.0).abs() < 1e-9);
    // 30 requests over 4000 s.
    assert!((fe["rate_per_s"].as_f64().unwrap() - 30.0 / 4000.0).abs() < 1e-9);
    // A service with no entry spans has no rate and no division by zero.
    assert_eq!(j["data"][1]["error_pct"].as_f64().unwrap(), 0.0);
    assert!(
        j["meta"]["sql"]
            .as_str()
            .unwrap()
            .contains("q:services_red")
    );
    let bad = get(&a, &format!("{BASE}/services")).await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
#[ignore = "needs GreptimeDB: make db-up && make test-db"]
async fn ac_0_5_1_red_matches_seed_live() {
    let l = live().await;
    let r = get(&l.app, &format!("{BASE}/services?{}", Live::window())).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let rows = r.json()["data"].as_array().unwrap().clone();
    let mut want: HashMap<&str, Vec<&synth::SpanRec>> = HashMap::new();
    for s in &l.batch.spans {
        want.entry(s.service).or_default().push(s);
    }
    assert_eq!(rows.len(), want.len(), "every service listed");
    for row in &rows {
        let name = row["service_name"].as_str().unwrap();
        let spans = &want[name];
        let entry: Vec<_> = spans
            .iter()
            .filter(|s| s.kind == synth::KIND_SERVER)
            .collect();
        assert_eq!(
            row["spans"].as_i64().unwrap() as usize,
            spans.len(),
            "{name}"
        );
        assert_eq!(
            row["requests"].as_i64().unwrap() as usize,
            entry.len(),
            "{name}"
        );
        assert_eq!(
            row["errors"].as_i64().unwrap() as usize,
            entry.iter().filter(|s| s.error).count(),
            "{name}"
        );
        if entry.is_empty() {
            assert!(
                row["avg_us"].is_null(),
                "{name}: no entry spans, no average"
            );
            continue;
        }
        let mut d: Vec<f64> = entry
            .iter()
            .map(|s| (s.end_ns - s.start_ns) as f64 / 1000.0)
            .collect();
        let avg = d.iter().sum::<f64>() / d.len() as f64;
        assert!(
            (row["avg_us"].as_f64().unwrap() - avg).abs() < 1.0,
            "{name} avg"
        );
        d.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let exact = d[((d.len() as f64 * 0.95).ceil() as usize).clamp(1, d.len()) - 1];
        let p95 = row["p95_us"].as_f64().unwrap();
        // approx_percentile_cont is a sketch: close to, not exactly, the nearest-rank value.
        assert!(
            (p95 - exact).abs() / exact < 0.10,
            "{name}: p95 {p95} vs exact {exact}"
        );
    }
    l.drop().await;
}

// ---- AC0.5.2 series and operations ------------------------------------------------

#[test]
fn ac_0_5_2_series_bucketed() {
    // The bucket comes from the fixed set: a 1 h window is 1-minute buckets, 31 days is 12 hours.
    for (span, bucket) in [(3_600_000i64, 60_000i64), (31 * 86_400_000, 43_200_000)] {
        let w = Window {
            from_ms: 1_000_000_000_000,
            to_ms: 1_000_000_000_000 + span,
            limit: 10,
        };
        let q = services::series("public", "t", "cart", w).unwrap();
        assert!(
            q.sql.contains(&format!("* {bucket} AS `bucket_ms`")),
            "{span}: {}",
            q.sql
        );
        assert_eq!(q.sql.matches("GROUP BY 1").count(), 1);
        assert_eq!(q.binds.last(), Some(&Bind::Str("cart".into())));
    }
}

#[tokio::test]
async fn ac_0_5_2_operations_breakdown() {
    let a = app(Fake::new(sample_db));
    let r = get(&a, &format!("{BASE}/services/frontend?{W}")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    assert_eq!(j["data"]["service"], "frontend");
    assert_eq!(j["data"]["summary"]["requests"], 30);
    assert_eq!(j["data"]["series"].as_array().unwrap().len(), 2);
    assert_eq!(j["data"]["operations"][0]["operation"], "GET /");
    assert!(j["data"]["bucket_ms"].as_i64().unwrap() > 0);
    assert!(j["data"]["annotation"].is_null());
    // Operations are entry spans of the one service, bound by name.
    let q = services::operations("public", "t", "frontend", win()).unwrap();
    assert!(q.sql.contains("GROUP BY `span_name`") && q.sql.contains("SPAN_KIND_SERVER"));
    // An unknown service is a 404, not an empty page.
    let none = Fake::new(|q| {
        if q.name == "services_red" {
            Ok(vec![])
        } else {
            sample_db(q)
        }
    });
    let r = get(&app(none), &format!("{BASE}/services/ghost?{W}")).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = get(&a, &format!("{BASE}/services/%20?{W}")).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
}

// ---- AC0.5.3 dependencies and the map ---------------------------------------------

#[test]
fn ac_0_5_3_dependency_query_shape() {
    let q = services::dependencies("public", "t", false, win()).unwrap();
    assert!(q.sql.contains("JOIN") && q.sql.contains("c.`parent_span_id` = p.`span_id`"));
    assert!(q.sql.contains("c.`trace_id` = p.`trace_id`"));
    assert!(
        q.sql.contains("p.`service_name` <> c.`service_name`"),
        "no self loops by default"
    );
    assert_eq!(q.binds.len(), 4, "both sides are windowed");
    let all = services::dependencies("public", "t", true, win()).unwrap();
    assert!(!all.sql.contains("<>"));
    assert!(!q.sql.contains(';'));
}

fn edge(a: &str, b: &str, calls: i64, errors: i64) -> EdgeIn {
    EdgeIn {
        from: a.into(),
        to: b.into(),
        calls,
        errors,
        avg_us: 1000.0,
    }
}

#[test]
fn ac_0_5_3_graph_nodes_edges() {
    let g = layout::graph(
        &[
            edge("frontend", "cart", 10, 0),
            edge("frontend", "checkout", 8, 0),
            edge("checkout", "payment", 8, 2),
            edge("cart", "cart", 3, 0),
        ],
        false,
    );
    let nodes = g["nodes"].as_array().unwrap();
    let layer = |n: &str| {
        nodes.iter().find(|x| x["id"] == n).unwrap()["layer"]
            .as_i64()
            .unwrap()
    };
    assert_eq!(nodes.len(), 4);
    assert_eq!(
        (
            layer("frontend"),
            layer("cart"),
            layer("checkout"),
            layer("payment")
        ),
        (0, 1, 1, 2)
    );
    let edges = g["edges"].as_array().unwrap();
    assert_eq!(edges.len(), 3, "the self loop is dropped by default");
    assert!(edges.iter().all(|e| e["from"] != e["to"]));
    let pay = edges.iter().find(|e| e["to"] == "payment").unwrap();
    assert!((pay["error_pct"].as_f64().unwrap() - 25.0).abs() < 1e-9);
    // Nodes never share a position.
    let pos: HashSet<(i64, i64)> = nodes
        .iter()
        .map(|n| (n["x"].as_i64().unwrap(), n["y"].as_i64().unwrap()))
        .collect();
    assert_eq!(pos.len(), 4);
    // Self loops come back when asked for; a cycle still lays out and is flagged.
    let g = layout::graph(
        &[
            edge("a", "b", 1, 0),
            edge("b", "a", 1, 0),
            edge("a", "a", 1, 0),
        ],
        true,
    );
    assert_eq!(g["edges"].as_array().unwrap().len(), 3);
    assert_eq!(
        g["edges"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["back"] == true)
            .count(),
        1
    );
    assert_eq!(g["nodes"].as_array().unwrap().len(), 2);
    assert_eq!(
        layout::graph(&[], false)["nodes"].as_array().unwrap().len(),
        0
    );
}

#[tokio::test]
async fn ac_0_5_3_dependencies_endpoint() {
    let a = app(Fake::new(sample_db));
    let r = get(&a, &format!("{BASE}/dependencies?{W}")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    assert_eq!(j["data"]["nodes"].as_array().unwrap().len(), 4);
    assert_eq!(j["data"]["edges"].as_array().unwrap().len(), 3);
    assert!(j["data"]["width"].as_i64().unwrap() > 0);
}

#[tokio::test]
#[ignore = "needs GreptimeDB: make db-up && make test-db"]
async fn ac_0_5_3_dependencies_live() {
    let l = live().await;
    let r = get(&l.app, &format!("{BASE}/dependencies?{}", Live::window())).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    // The independent answer: pair every span with its parent from the generator's own records.
    let mut by_id: HashMap<(String, String), &synth::SpanRec> = HashMap::new();
    for s in &l.batch.spans {
        by_id.insert((hex(&s.trace_id), hex(&s.span_id)), s);
    }
    let mut want: HashMap<(String, String), (i64, i64)> = HashMap::new();
    for s in &l.batch.spans {
        let Some(p) = s.parent else { continue };
        let parent = by_id[&(hex(&s.trace_id), hex(&p))];
        if parent.service != s.service {
            let e = want
                .entry((parent.service.to_string(), s.service.to_string()))
                .or_default();
            e.0 += 1;
            e.1 += i64::from(s.error);
        }
    }
    assert!(!want.is_empty());
    let edges = r.json()["data"]["edges"].as_array().unwrap().clone();
    let got: HashMap<(String, String), (i64, i64)> = edges
        .iter()
        .map(|e| {
            (
                (
                    e["from"].as_str().unwrap().into(),
                    e["to"].as_str().unwrap().into(),
                ),
                (e["calls"].as_i64().unwrap(), e["errors"].as_i64().unwrap()),
            )
        })
        .collect();
    assert_eq!(
        got, want,
        "the join agrees with the generator, edge by edge"
    );
    // Run it again: v1.2.1 returned different rows each time.
    let again = get(&l.app, &format!("{BASE}/dependencies?{}", Live::window())).await;
    assert_eq!(again.json()["data"]["edges"], r.json()["data"]["edges"]);
    l.drop().await;
}

// ---- AC0.5.4 catalog and annotations ----------------------------------------------

#[tokio::test]
async fn ac_0_5_4_catalog_lists_services() {
    let store = MemStore::new();
    let clock = Arc::new(AtomicI64::new(NOW_MS));
    let a = app_with_store(&store, &clock);
    let note =
        json!({"description": "Retired API", "owner": "legacy", "tier": "low", "runbook": ""});
    let r = send_json(
        &a,
        Method::PUT,
        &format!("{BASE}/services/legacy/annotation"),
        &note,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    send_json(&a, Method::PUT, &format!("{BASE}/services/frontend/annotation"), &json!({"description": "Edge", "owner": "web", "tier": "critical", "runbook": "https://example.com/rb"})).await;
    let r = get(&a, &format!("{BASE}/catalog?{W}")).await;
    let rows = r.json()["data"].as_array().unwrap().clone();
    let names: Vec<_> = rows
        .iter()
        .map(|x| x["service_name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        ["frontend", "postgres", "legacy"],
        "seen services first, then noted ones"
    );
    assert_eq!(rows[0]["annotation"]["owner"], "web");
    assert_eq!(rows[0]["first_seen_ms"], 1_793_399_000_000i64);
    assert_eq!(rows[0]["last_seen_ms"], 1_793_399_900_000i64);
    assert_eq!(rows[0]["requests"], 30);
    assert!(rows[1]["annotation"].is_null());
    assert_eq!(rows[2]["seen"], false);
    // The note appears on the service page too.
    let d = get(&a, &format!("{BASE}/services/frontend?{W}"))
        .await
        .json();
    assert_eq!(d["data"]["annotation"]["tier"], "critical");
}

#[tokio::test]
async fn ac_0_5_4_annotations_roundtrip() {
    let store = MemStore::new();
    let clock = Arc::new(AtomicI64::new(NOW_MS));
    let a = app_with_store(&store, &clock);
    let uri = format!("{BASE}/services/cart/annotation");
    let r = send_json(&a, Method::PUT, &uri, &json!({"description": "Cart API", "owner": "shop", "tier": "high", "runbook": "https://example.com/cart"})).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["data"]["updated_by"], "local");
    assert_eq!(r.json()["data"]["updated_ms"], NOW_MS);
    // Audited-ready: who and when are stored, and the write is the one metadata insert.
    let w = store.writes.lock().unwrap().last().unwrap().clone();
    assert_eq!(w.name, "service_note_put");
    assert!(
        w.sql
            .contains("INSERT INTO `grepui`.`grepui_services` (`org_id`")
    );
    assert!(
        w.binds.contains(&Bind::Str("default".into()))
            && w.binds.contains(&Bind::Str("local".into()))
    );
    // Update in place: same key, new text, still one note.
    send_json(
        &a,
        Method::PUT,
        &uri,
        &json!({"description": "Cart API v2", "owner": "shop", "tier": "high", "runbook": ""}),
    )
    .await;
    assert_eq!(store.notes.lock().unwrap().len(), 1);
    let cat = get(&a, &format!("{BASE}/catalog?{W}")).await.json();
    let names: Vec<_> = cat["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["service_name"].as_str().unwrap().to_string())
        .collect();
    assert!(names.contains(&"cart".to_string()));
    // Validation: tier, runbook scheme, length.
    for body in [
        json!({"tier": "urgent"}),
        json!({"runbook": "javascript:alert(1)"}),
        json!({"runbook": "https://a b"}),
        json!({"description": "x".repeat(501)}),
        json!({"owner": "bad\u{0}name"}),
    ] {
        let r = send_json(&a, Method::PUT, &uri, &body).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{body}");
    }
    // Clear is a soft delete.
    let r = send_json(&a, Method::DELETE, &uri, &json!({})).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let cat = get(&a, &format!("{BASE}/catalog?{W}")).await.json();
    assert!(
        !cat["data"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x["service_name"] == "cart")
    );
    // Another org's notes are not visible: the org is part of the key.
    assert!(
        store
            .notes
            .lock()
            .unwrap()
            .keys()
            .all(|(o, _)| o == "default")
    );
}

// ---- live fixture -----------------------------------------------------------------

struct Live {
    db: String,
    app: Router,
    batch: synth::Batch,
}

async fn live() -> Live {
    let base = std::env::var("GREPUI_TEST_DB")
        .expect("set GREPUI_TEST_DB=mysql://root@127.0.0.1:4002/public");
    static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let db = format!(
        "demo_it_sv_{}_{}",
        std::process::id(),
        N.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    );
    let form = [("content-type", "application/x-www-form-urlencoded")];
    http::post(
        "http://127.0.0.1:4000",
        "/v1/sql",
        &form,
        format!("sql=CREATE DATABASE IF NOT EXISTS {db}").as_bytes(),
    )
    .await
    .unwrap();
    let now_ns = chrono::Utc::now().timestamp_nanos_opt().unwrap() as u64;
    let batch = synth::generate(7, 60, now_ns, 60);
    send(
        "http://127.0.0.1:4000",
        &db,
        "opentelemetry_traces",
        "opentelemetry_logs",
        &batch,
    )
    .await
    .expect("seed");
    let url = base.replace("/public", &format!("/{db}"));
    let r = Arc::new(SqlxReader::lazy(&url, Duration::from_secs(20)).unwrap());
    let app = router(
        AppState::new(
            Config {
                greptime_mysql: url,
                ..Config::default()
            },
            r,
        )
        .unwrap(),
    );
    Live { db, app, batch }
}

impl Live {
    async fn drop(self) {
        let form = [("content-type", "application/x-www-form-urlencoded")];
        let _ = http::post(
            "http://127.0.0.1:4000",
            "/v1/sql",
            &form,
            format!("sql=DROP DATABASE IF EXISTS {}", self.db).as_bytes(),
        )
        .await;
    }
    fn window() -> String {
        let now = chrono::Utc::now().timestamp_millis();
        format!("from={}&to={}", now - 3 * 3_600_000, now + 3_600_000)
    }
}

#[allow(dead_code)]
fn unused(_: Value) {}
