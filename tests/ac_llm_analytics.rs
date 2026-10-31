//! AC0.16.1 to AC0.16.3: LLM analytics, sessions and users, and alerts on LLM metrics. Live tests
//! send a generated OpenInference batch to a real trace v2 table and compare every number with
//! the ones computed from the batch; offline tests script the database.
mod common;
use axum::{Router, http::Method, http::StatusCode};
use common::*;
use grepui::{
    alerts::engine::Engine,
    api::{AppState, Shared, router},
    auth::{Access, Role, access_for},
    config::Config,
    playground::{self, http, synth},
    query::{
        Q, SqlxReader, Window,
        llm::{self as qllm, By},
        traces::TraceFilter,
    },
    store,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    sync::{
        Arc,
        atomic::{AtomicI64, Ordering},
    },
    time::Duration,
};

const BASE: &str = "/api/v1/orgs/default";
const W: &str = "from=1793398000000&to=1793402000000";

fn json_cols() -> Vec<Value> {
    [
        "timestamp",
        "trace_id",
        "span_attributes",
        "resource_attributes",
    ]
    .iter()
    .map(|c| json!({"column_name": c, "data_type": "string", "semantic_type": "FIELD"}))
    .collect()
}

fn flat_cols() -> Vec<Value> {
    ["timestamp", "trace_id", "span_attributes.http.route"]
        .iter()
        .map(|c| json!({"column_name": c, "data_type": "string", "semantic_type": "FIELD"}))
        .collect()
}

/// An app whose database answers the analytics statements from `rows`.
fn scripted(cols: Vec<Value>, rows: HashMap<&'static str, Vec<Value>>) -> (Router, Arc<MemStore>) {
    let store = MemStore::new();
    *store.script.lock().unwrap() = Some(Box::new(move |q: &Q| {
        Ok(match q.name {
            "stream_schema" => cols.clone(),
            n if rows.contains_key(n) => rows[n].clone(),
            _ => sample_db(q)?,
        })
    }));
    let clock = Arc::new(AtomicI64::new(1_793_399_000_000));
    (app_admin(&store, &clock, true), store)
}

fn win() -> Window {
    Window {
        from_ms: 1_000,
        to_ms: 9_000,
        limit: 50,
    }
}

// ---- AC0.16.1 ---------------------------------------------------------------------------------

#[test]
fn ac_0_16_1_paths_are_binds() {
    let f = TraceFilter::default();
    let evil = qllm::breakdown(
        "db",
        "t",
        By::Model,
        &f,
        Some("m'; DROP TABLE t; --"),
        win(),
    )
    .unwrap();
    let all: Vec<Q> = vec![
        evil,
        qllm::breakdown("db", "t", By::Provider, &f, None, win()).unwrap(),
        qllm::breakdown("db", "t", By::Kind, &f, None, win()).unwrap(),
        qllm::breakdown("db", "t", By::Tool, &f, None, win()).unwrap(),
        qllm::totals("db", "t", &f, None, win()).unwrap(),
        qllm::series("db", "t", &f, None, win()).unwrap(),
        qllm::sessions("db", "t", &f, win(), 0).unwrap(),
        qllm::users("db", "t", &f, win(), 0).unwrap(),
        qllm::session_turns("db", "t", "s'; DROP", win()).unwrap(),
    ];
    for q in &all {
        assert!(q.sql.starts_with("/* q:llm_"), "{}", q.sql);
        assert_eq!(q.sql.matches('?').count(), q.binds.len(), "{}", q.sql);
        for word in [
            "llm.",
            "openinference",
            "gen_ai",
            "session.id",
            "user.id",
            "tool.name",
            "DROP",
        ] {
            assert!(
                !q.sql.contains(word),
                "{word} in the statement text: {}",
                q.sql
            );
        }
        assert!(q.sql.contains("json_get_"), "{}", q.sql);
    }
    // The values travel as binds too.
    assert!(
        all[0]
            .binds
            .iter()
            .any(|b| b.show() == "m'; DROP TABLE t; --")
    );
    assert!(all[8].binds.iter().any(|b| b.show() == "s'; DROP"));
    // A request filter narrows the spans, with its own binds after the window.
    let filt = TraceFilter {
        service: Some("llm-app".into()),
        ..Default::default()
    };
    let q = qllm::totals("db", "t", &filt, None, win()).unwrap();
    assert!(q.sql.contains("`service_name` = ?") && q.binds.iter().any(|b| b.show() == "llm-app"));
}

#[tokio::test]
async fn ac_0_16_1_flat_table_refused() {
    let (app, store) = scripted(flat_cols(), HashMap::new());
    for p in ["overview", "breakdown", "sessions", "users", "sessions/s1"] {
        let r = get(&app, &format!("{BASE}/llm/{p}?{W}")).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{p}: {}", r.text());
        assert!(r.text().contains("trace v2"), "{}", r.text());
    }
    assert!(
        !store
            .seen
            .lock()
            .unwrap()
            .iter()
            .any(|q| q.name.starts_with("llm_")),
        "no analytics statement ran against a flat table"
    );
}

#[tokio::test]
async fn ac_0_16_1_window_capped() {
    let (app, _) = scripted(json_cols(), HashMap::new());
    // 60 days is over the 31-day cap; reversed ranges are refused too.
    let r = get(
        &app,
        &format!("{BASE}/llm/overview?from=1700000000000&to=1705184000000"),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    let r = get(
        &app,
        &format!("{BASE}/llm/breakdown?from=1793402000000&to=1793398000000"),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = get(&app, &format!("{BASE}/llm/breakdown?{W}&by=bogus")).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = get(&app, &format!("{BASE}/llm/breakdown?{W}&model=a%0Ab")).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn ac_0_16_1_price_estimate_in_overview() {
    // gpt-4o reported its cost on some calls; 1000 prompt and 500 completion tokens had none.
    let model_row = json!({"grp": "gpt-4o", "calls": 4, "errors": 1, "p50_ns": 1e8, "p95_ns": 3e8, "p99_ns": 4e8, "avg_ns": 2e8,
        "prompt_tokens": 3000, "completion_tokens": 900, "cost": 0.5, "uncosted_prompt": 1000, "uncosted_completion": 500});
    let other = json!({"grp": "mystery", "calls": 1, "errors": 0, "p50_ns": 1e8, "p95_ns": 1e8, "p99_ns": 1e8, "avg_ns": 1e8,
        "prompt_tokens": 10, "completion_tokens": 5, "cost": 0.0, "uncosted_prompt": 10, "uncosted_completion": 5});
    let mut rows = HashMap::new();
    rows.insert("llm_breakdown", vec![model_row.clone(), other]);
    rows.insert("llm_totals", vec![json!({"calls": 5, "errors": 1, "p50_ns": 1e8, "p95_ns": 3e8, "p99_ns": 4e8, "avg_ns": 2e8,
        "prompt_tokens": 3010, "completion_tokens": 905, "cost": 0.5, "uncosted_prompt": 1010, "uncosted_completion": 505})]);
    rows.insert(
        "llm_series",
        vec![
            json!({"bucket_ms": 1793399000000_i64, "calls": 5, "errors": 1, "p95_ns": 3e8,
        "prompt_tokens": 3010, "completion_tokens": 905, "cost": 0.5}),
        ],
    );
    let (app, _) = scripted(json_cols(), rows);
    let send = |m: &str, p: f64, c: f64| {
        let app = app.clone();
        let body = json!({"model": m, "prompt_per_1m": p, "completion_per_1m": c});
        async move { send_json(&app, Method::PUT, &format!("{BASE}/llm/prices"), &body).await }
    };
    assert_eq!(send("gpt-4o", 2.0, 4.0).await.status, StatusCode::OK);
    let d = get(&app, &format!("{BASE}/llm/overview?{W}")).await.json()["data"].clone();
    // 1000 * 2/1M + 500 * 4/1M = 0.004 estimated on top of the 0.5 reported; the unpriced model adds nothing.
    let t = &d["totals"];
    assert!((t["cost"]["reported"].as_f64().unwrap() - 0.5).abs() < 1e-12);
    assert!(
        (t["cost"]["estimated"].as_f64().unwrap() - 0.004).abs() < 1e-12,
        "{t}"
    );
    assert!((t["cost"]["total"].as_f64().unwrap() - 0.504).abs() < 1e-12);
    assert_eq!(t["total_tokens"], 3915);
    assert_eq!(t["calls"], 5);
    assert_eq!(t["error_rate"], 20.0);
    assert_eq!(t["p95_ms"], 300.0);
    assert_eq!(d["models"][0]["grp"], "gpt-4o");
    assert_eq!(
        d["models"][1]["cost"]["estimated"], 0.0,
        "no price, no estimate"
    );
    assert_eq!(d["series"][0]["calls"], 5);
    // Per-model breakdown estimates by model; the other groupings only report.
    let b = get(&app, &format!("{BASE}/llm/breakdown?{W}&by=model"))
        .await
        .json()["data"]
        .clone();
    assert!((b[0]["cost"]["estimated"].as_f64().unwrap() - 0.004).abs() < 1e-12);
    let b = get(&app, &format!("{BASE}/llm/breakdown?{W}&by=provider"))
        .await
        .json()["data"]
        .clone();
    assert_eq!(b[0]["cost"]["estimated"], 0.0);
}

/// A throwaway database with a trace v2 table holding a generated OpenInference batch, a migrated
/// state store, and an app and alert engine over them.
struct Live {
    app: Router,
    state: Shared,
    engine: Engine,
    rec: Arc<Rec>,
    batch: synth::Batch,
    clock: Arc<AtomicI64>,
}

async fn live(seed: u64, traces: usize) -> Live {
    let base = std::env::var("GREPUI_TEST_DB")
        .expect("set GREPUI_TEST_DB=mysql://root@127.0.0.1:4002/public");
    let (db, meta) = (
        format!("demo_it_llma_{}_{seed}", std::process::id()),
        format!("grepui_it_llma_{}_{seed}", std::process::id()),
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
    let now = chrono::Utc::now().timestamp_nanos_opt().unwrap() as u64;
    let batch = synth::generate_llm(seed, traces, now, 30);
    playground::send_llm_traces(
        "http://127.0.0.1:4000",
        &db,
        "llm_traces",
        synth::LLM_PROJECT,
        &batch,
    )
    .await
    .expect("seed v2");
    let url = base.replace("/public", &format!("/{db}"));
    let r = Arc::new(SqlxReader::lazy(&url, Duration::from_secs(30)).unwrap());
    store::migrate(r.as_ref(), r.as_ref(), &meta)
        .await
        .expect("migrate");
    let clock = Arc::new(AtomicI64::new(chrono::Utc::now().timestamp_millis()));
    let rec = Arc::new(Rec::default());
    let c = clock.clone();
    let state = AppState::with_sender(
        Config {
            greptime_mysql: url,
            meta_database: meta,
            traces_table: "llm_traces".into(),
            admin_writes: true,
            ..Config::default()
        },
        r.clone(),
        r,
        Arc::new(move || c.load(Ordering::SeqCst)),
        rec.clone(),
    )
    .unwrap();
    let engine = Engine::new(state.clone(), rec.clone()).with_settle(Duration::ZERO);
    Live {
        app: router(state.clone()),
        state,
        engine,
        rec,
        batch,
        clock,
    }
}

fn win_q() -> String {
    let to = chrono::Utc::now().timestamp_millis() + 600_000;
    format!("from={}&to={to}&limit=1000", to - 7_200_000)
}

fn sattr(s: &synth::SpanRec, k: &str) -> Option<String> {
    s.attrs
        .iter()
        .find(|(n, _)| n == k)
        .and_then(|(_, v)| match v {
            synth::AttrVal::Str(x) => Some(x.clone()),
            _ => None,
        })
}

fn iattr(s: &synth::SpanRec, k: &str) -> i64 {
    s.attrs
        .iter()
        .find(|(n, _)| n == k)
        .map_or(0, |(_, v)| match v {
            synth::AttrVal::Int(i) => *i,
            _ => 0,
        })
}

fn fattr(s: &synth::SpanRec, k: &str) -> f64 {
    s.attrs
        .iter()
        .find(|(n, _)| n == k)
        .map_or(0.0, |(_, v)| match v {
            synth::AttrVal::Float(f) => *f,
            _ => 0.0,
        })
}

fn is_llm(s: &synth::SpanRec) -> bool {
    sattr(s, "openinference.span.kind").as_deref() == Some("LLM")
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

#[derive(Default, Debug)]
struct Agg {
    calls: i64,
    errors: i64,
    prompt: i64,
    completion: i64,
    cost: f64,
    durs: Vec<f64>,
}

fn agg_by(
    batch: &synth::Batch,
    pred: impl Fn(&synth::SpanRec) -> bool,
    key: impl Fn(&synth::SpanRec) -> String,
) -> BTreeMap<String, Agg> {
    let mut m: BTreeMap<String, Agg> = BTreeMap::new();
    for s in batch.spans.iter().filter(|s| pred(s)) {
        let a = m.entry(key(s)).or_default();
        a.calls += 1;
        a.errors += i64::from(s.error);
        if is_llm(s) {
            a.prompt += iattr(s, "llm.token_count.prompt");
            a.completion += iattr(s, "llm.token_count.completion");
            a.cost += fattr(s, "llm.cost.total");
        }
        a.durs.push((s.end_ns - s.start_ns) as f64 / 1e6);
    }
    m
}

fn check_rows(rows: &Value, want: &BTreeMap<String, Agg>, what: &str) {
    let rows = rows.as_array().unwrap();
    assert_eq!(rows.len(), want.len(), "{what}: {rows:?} vs {want:?}");
    for r in rows {
        let g = r["grp"].as_str().unwrap();
        let a = want
            .get(g)
            .unwrap_or_else(|| panic!("{what}: unexpected group {g}"));
        assert_eq!(r["calls"], a.calls, "{what} {g} calls");
        assert_eq!(r["errors"], a.errors, "{what} {g} errors");
        assert_eq!(r["prompt_tokens"], a.prompt, "{what} {g} prompt tokens");
        assert_eq!(
            r["completion_tokens"], a.completion,
            "{what} {g} completion tokens"
        );
        assert!(
            (r["cost"]["reported"].as_f64().unwrap() - a.cost).abs() < 1e-9,
            "{what} {g} cost"
        );
        // Percentiles are approximate: they must sit between the true minimum and maximum, in order.
        let (lo, hi) = (
            a.durs.iter().cloned().fold(f64::MAX, f64::min),
            a.durs.iter().cloned().fold(0.0, f64::max),
        );
        let (p50, p95, p99) = (
            r["p50_ms"].as_f64().unwrap(),
            r["p95_ms"].as_f64().unwrap(),
            r["p99_ms"].as_f64().unwrap(),
        );
        assert!(
            lo - 0.01 <= p50 && p50 <= p95 && p95 <= p99 && p99 <= hi + 0.01,
            "{what} {g}: {lo} {p50} {p95} {p99} {hi}"
        );
    }
}

#[tokio::test]
#[ignore = "needs GreptimeDB (make db-up)"]
async fn ac_0_16_1_breakdown_matches_batch_live() {
    let l = live(33, 40).await;
    let q = win_q();
    let by = |by: &'static str| {
        let (app, q) = (l.app.clone(), q.clone());
        async move {
            let r = get(&app, &format!("{BASE}/llm/breakdown?{q}&by={by}")).await;
            assert_eq!(r.status, StatusCode::OK, "{by}: {}", r.text());
            r.json()["data"].clone()
        }
    };
    check_rows(
        &by("model").await,
        &agg_by(&l.batch, is_llm, |s| sattr(s, "llm.model_name").unwrap()),
        "model",
    );
    check_rows(
        &by("provider").await,
        &agg_by(&l.batch, is_llm, |s| sattr(s, "llm.provider").unwrap()),
        "provider",
    );
    check_rows(
        &by("kind").await,
        &agg_by(
            &l.batch,
            |s| sattr(s, "openinference.span.kind").is_some(),
            |s| sattr(s, "openinference.span.kind").unwrap(),
        ),
        "kind",
    );
    check_rows(
        &by("tool").await,
        &agg_by(
            &l.batch,
            |s| sattr(s, "openinference.span.kind").as_deref() == Some("TOOL"),
            |s| sattr(s, "tool.name").unwrap(),
        ),
        "tool",
    );
    // One model only, through the model filter and through the filter bar.
    let gpt = agg_by(
        &l.batch,
        |s| is_llm(s) && sattr(s, "llm.model_name").as_deref() == Some("gpt-4o"),
        |s| sattr(s, "llm.model_name").unwrap(),
    );
    let r = get(
        &l.app,
        &format!("{BASE}/llm/breakdown?{q}&by=model&model=gpt-4o"),
    )
    .await;
    check_rows(&r.json()["data"], &gpt, "model filter");
    let r = get(
        &l.app,
        &format!("{BASE}/llm/breakdown?{q}&by=model&q=llm.model_name%3Dgpt-4o"),
    )
    .await;
    check_rows(&r.json()["data"], &gpt, "filter bar");
}

#[tokio::test]
#[ignore = "needs GreptimeDB (make db-up)"]
async fn ac_0_16_1_overview_totals_live() {
    let l = live(34, 40).await;
    let q = win_q();
    let r = get(&l.app, &format!("{BASE}/llm/overview?{q}")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let d = r.json()["data"].clone();
    let all = agg_by(&l.batch, is_llm, |_| "all".into());
    let a = &all["all"];
    let t = &d["totals"];
    assert_eq!(t["calls"], a.calls);
    assert_eq!(t["errors"], a.errors);
    assert_eq!(t["prompt_tokens"], a.prompt);
    assert_eq!(t["completion_tokens"], a.completion);
    assert_eq!(t["total_tokens"], a.prompt + a.completion);
    assert!((t["cost"]["total"].as_f64().unwrap() - a.cost).abs() < 1e-9);
    assert!(
        (t["error_rate"].as_f64().unwrap() - a.errors as f64 * 100.0 / a.calls as f64).abs() < 1e-9
    );
    // The series adds up to the totals.
    let pts = d["series"].as_array().unwrap();
    assert_eq!(
        pts.iter()
            .map(|p| p["calls"].as_i64().unwrap())
            .sum::<i64>(),
        a.calls
    );
    assert_eq!(
        pts.iter()
            .map(|p| p["prompt_tokens"].as_i64().unwrap())
            .sum::<i64>(),
        a.prompt
    );
    // A price table never touches calls that reported their own cost.
    let r = send_json(
        &l.app,
        Method::PUT,
        &format!("{BASE}/llm/prices"),
        &json!({"model": "gpt-4o", "prompt_per_1m": 100.0, "completion_per_1m": 100.0}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let d2 = get(&l.app, &format!("{BASE}/llm/overview?{q}"))
        .await
        .json()["data"]
        .clone();
    assert_eq!(d2["totals"]["cost"]["estimated"], 0.0);
    assert!((d2["totals"]["cost"]["total"].as_f64().unwrap() - a.cost).abs() < 1e-9);
}

// ---- AC0.16.2 ---------------------------------------------------------------------------------

fn sessions_of(batch: &synth::Batch) -> BTreeMap<String, (BTreeSet<String>, i64, i64, i64)> {
    // session -> (traces, spans carrying it, model calls, prompt tokens)
    let mut m: BTreeMap<String, (BTreeSet<String>, i64, i64, i64)> = BTreeMap::new();
    for s in &batch.spans {
        let Some(sid) = sattr(s, "session.id") else {
            continue;
        };
        let e = m.entry(sid).or_default();
        e.0.insert(hex(&s.trace_id));
        e.1 += 1;
        if is_llm(s) {
            e.2 += 1;
            e.3 += iattr(s, "llm.token_count.prompt");
        }
    }
    m
}

#[tokio::test]
#[ignore = "needs GreptimeDB (make db-up)"]
async fn ac_0_16_2_sessions_match_batch_live() {
    let l = live(35, 45).await;
    let r = get(&l.app, &format!("{BASE}/llm/sessions?{}", win_q())).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let rows = r.json()["data"].as_array().unwrap().clone();
    let want = sessions_of(&l.batch);
    assert_eq!(rows.len(), want.len());
    for r in &rows {
        let (traces, spans, calls, prompt) = &want[r["session_id"].as_str().unwrap()];
        assert_eq!(r["traces"], traces.len(), "{r}");
        assert_eq!(r["spans"], *spans, "{r}");
        assert_eq!(r["llm_calls"], *calls, "{r}");
        assert_eq!(r["prompt_tokens"], *prompt, "{r}");
        assert!(r["last_ms"].as_i64().unwrap() >= r["first_ms"].as_i64().unwrap());
        assert!(r["user"].is_string());
    }
    // Most recent first.
    let last: Vec<i64> = rows
        .iter()
        .map(|r| r["last_ms"].as_i64().unwrap())
        .collect();
    assert!(last.windows(2).all(|w| w[0] >= w[1]), "{last:?}");
}

#[tokio::test]
#[ignore = "needs GreptimeDB (make db-up)"]
async fn ac_0_16_2_session_turns_in_order_live() {
    let l = live(36, 45).await;
    let want = sessions_of(&l.batch);
    let (sid, (traces, ..)) = want.iter().max_by_key(|(_, v)| v.0.len()).unwrap();
    assert!(traces.len() >= 2, "the batch has a session with two turns");
    let r = get(&l.app, &format!("{BASE}/llm/sessions/{sid}?{}", win_q())).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let d = r.json()["data"].clone();
    let turns = d["turns"].as_array().unwrap();
    assert_eq!(turns.len(), traces.len());
    let starts: Vec<i64> = turns
        .iter()
        .map(|t| t["start_ms"].as_i64().unwrap())
        .collect();
    assert!(
        starts.windows(2).all(|w| w[0] <= w[1]),
        "oldest first: {starts:?}"
    );
    for t in turns {
        let tid = t["trace_id"].as_str().unwrap();
        // Each turn's input and output are the root span's, equal to what the batch holds.
        let root = l
            .batch
            .spans
            .iter()
            .find(|s| hex(&s.trace_id) == tid && s.parent.is_none())
            .unwrap();
        assert_eq!(t["input.value"], sattr(root, "input.value").unwrap(), "{t}");
        assert_eq!(t["output.value"], sattr(root, "output.value").unwrap());
        assert_eq!(t["llm_calls"], 1);
    }
    // An unknown session is a clear 404.
    let r = get(&l.app, &format!("{BASE}/llm/sessions/nope?{}", win_q())).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
#[ignore = "needs GreptimeDB (make db-up)"]
async fn ac_0_16_2_users_match_batch_live() {
    let l = live(37, 45).await;
    let r = get(&l.app, &format!("{BASE}/llm/users?{}", win_q())).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let rows = r.json()["data"].as_array().unwrap().clone();
    let mut want: BTreeMap<String, (BTreeSet<String>, BTreeSet<String>, i64)> = BTreeMap::new();
    for s in &l.batch.spans {
        let Some(u) = sattr(s, "user.id") else {
            continue;
        };
        let e = want.entry(u).or_default();
        e.0.insert(hex(&s.trace_id));
        if let Some(sid) = sattr(s, "session.id") {
            e.1.insert(sid);
        }
        e.2 += i64::from(is_llm(s));
    }
    assert_eq!(rows.len(), want.len());
    for r in &rows {
        let (traces, sessions, calls) = &want[r["user_id"].as_str().unwrap()];
        assert_eq!(r["traces"], traces.len(), "{r}");
        assert_eq!(r["sessions"], sessions.len(), "{r}");
        assert_eq!(r["llm_calls"], *calls, "{r}");
    }
}

#[tokio::test]
async fn ac_0_16_2_genai_conversation_id() {
    // A session is `session.id` or, failing that, `gen_ai.conversation.id`: both paths are bound,
    // in that order, in the grouping and in the restriction.
    let q = qllm::sessions("db", "t", &TraceFilter::default(), win(), 0).unwrap();
    let paths: Vec<String> = q
        .binds
        .iter()
        .map(|b| b.show())
        .filter(|s| s.starts_with("[\""))
        .collect();
    let first = paths.iter().position(|p| p == "[\"session.id\"]").unwrap();
    assert_eq!(paths[first + 1], "[\"gen_ai.conversation.id\"]");
    assert!(
        paths
            .iter()
            .filter(|p| *p == "[\"gen_ai.conversation.id\"]")
            .count()
            >= 2
    );
    assert!(q.sql.contains(
        "coalesce(json_get_string(`span_attributes`, ?), json_get_string(`span_attributes`, ?))"
    ));
    // And a scripted session row from a GenAI-only app reads like any other.
    let mut rows = HashMap::new();
    rows.insert("llm_sessions", vec![json!({"session_id": "conv-1", "traces": 2, "spans": 6, "llm_calls": 2, "errors": 0,
        "prompt_tokens": 20, "completion_tokens": 10, "cost": 0.1, "uncosted_prompt": 0, "uncosted_completion": 0,
        "first_ms": 1, "last_ms": 9, "users": 1, "user": "u"})]);
    let (app, _) = scripted(json_cols(), rows);
    let d = get(&app, &format!("{BASE}/llm/sessions?{W}")).await.json()["data"].clone();
    assert_eq!(d[0]["session_id"], "conv-1");
    assert_eq!(d[0]["total_tokens"], 30);
}

#[tokio::test]
async fn ac_0_16_2_session_turns_masked_for_viewers() {
    // A turn carries a snippet of the prompt and the answer: below admin they are masked like any
    // other content (R-AI8), while ids, counts and times stay.
    let mut rows = HashMap::new();
    rows.insert("llm_session_turns", vec![json!({"trace_id": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "start_ms": 1, "duration_us": 5, "spans": 3,
        "errors": 0, "llm_calls": 1, "prompt_tokens": 7, "completion_tokens": 3, "cost": 0.1, "uncosted_prompt": 0, "uncosted_completion": 0,
        "input": "my card number is 4111", "output": "thanks, noted"})]);
    let store = MemStore::new();
    let cols = json_cols();
    *store.script.lock().unwrap() = Some(Box::new(move |q: &Q| {
        Ok(match q.name {
            "stream_schema" => cols.clone(),
            "llm_session_turns" => rows["llm_session_turns"].clone(),
            _ => sample_db(q)?,
        })
    }));
    let clock = Arc::new(AtomicI64::new(1_793_399_000_000));
    for (admin, sees) in [(false, false), (true, true)] {
        let app = app_admin(&store, &clock, admin);
        let r = get(&app, &format!("{BASE}/llm/sessions/s1?{W}")).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
        let t = r.text();
        assert_eq!(
            t.contains("4111") && t.contains("thanks, noted"),
            sees,
            "admin={admin}: {t}"
        );
        assert!(
            t.contains("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa") && t.contains("\"total_tokens\":10"),
            "{t}"
        );
    }
}

#[test]
fn ac_0_16_2_new_routes_classified() {
    for (path, level) in [
        ("overview", Role::Viewer),
        ("breakdown", Role::Viewer),
        ("sessions", Role::Viewer),
        ("sessions/{id}", Role::Viewer),
        ("users", Role::Viewer),
    ] {
        let t = format!("/api/v1/orgs/{{org}}/llm/{path}");
        assert!(grepui::api::ROUTES.contains(&t.as_str()), "{t}");
        assert_eq!(access_for(&Method::GET, &t), Access::Org(level), "{t}");
        assert_eq!(access_for(&Method::POST, &t), Access::Deny, "{t}");
    }
}

// ---- AC0.16.3 ---------------------------------------------------------------------------------

fn llm_rule(
    name: &str,
    metric: &str,
    per_model: bool,
    op: &str,
    threshold: f64,
    dest: &str,
) -> Value {
    json!({
        "name": name, "kind": "scheduled",
        "query": {"kind": "llm", "llm": {"metric": metric, "per_model": per_model}},
        "condition": {"op": op, "threshold": threshold},
        "schedule": {"every_s": 60}, "lookback_s": 7200,
        "dedup": {"fields": if per_model { json!(["model"]) } else { json!([]) }, "window_s": 3600},
        "destinations": [dest],
    })
}

async fn webhook(app: &Router) -> String {
    let r = send_json(
        app,
        Method::POST,
        &format!("{BASE}/destinations"),
        &json!({"name": "hook", "kind": "webhook", "url": "http://127.0.0.1:9/hook"}),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    r.json()["data"]["id"].as_str().unwrap().to_string()
}

async fn rule(app: &Router, body: Value) -> String {
    let r = send_json(app, Method::POST, &format!("{BASE}/alerts"), &body).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    r.json()["data"]["id"].as_str().unwrap().to_string()
}

#[tokio::test]
#[ignore = "needs GreptimeDB (make db-up)"]
async fn ac_0_16_3_cost_condition_fires_live() {
    let l = live(38, 40).await;
    let total: f64 = l
        .batch
        .spans
        .iter()
        .filter(|s| is_llm(s))
        .map(|s| fattr(s, "llm.cost.total"))
        .sum();
    let tokens: i64 = l
        .batch
        .spans
        .iter()
        .filter(|s| is_llm(s))
        .map(|s| iattr(s, "llm.token_count.prompt") + iattr(s, "llm.token_count.completion"))
        .sum();
    let org = l.state.org("default").unwrap();
    let dest = webhook(&l.app).await;
    // Spend is over half of what was spent: fires once, with the value the batch adds up to.
    let id = rule(
        &l.app,
        llm_rule("spend high", "cost", false, ">", total / 2.0, &dest),
    )
    .await;
    // Spend is over twice what was spent: never fires.
    let quiet = rule(
        &l.app,
        llm_rule("spend absurd", "cost", false, ">", total * 2.0, &dest),
    )
    .await;
    // Tokens, in total.
    let tok = rule(
        &l.app,
        llm_rule("tokens high", "tokens", false, ">=", tokens as f64, &dest),
    )
    .await;
    let fired = l.engine.tick(org).await.unwrap();
    assert!(
        fired.contains(&id) && fired.contains(&quiet) && fired.contains(&tok),
        "{fired:?}"
    );
    let sent = l.rec.sent.lock().unwrap().clone();
    let by_name = |n: &str| {
        sent.iter()
            .filter(|(_, m)| m.payload["alert"]["name"] == n)
            .collect::<Vec<_>>()
    };
    assert_eq!(by_name("spend high").len(), 1);
    assert!(
        (by_name("spend high")[0].1.payload["value"]
            .as_f64()
            .unwrap()
            - total)
            .abs()
            < 1e-9
    );
    assert_eq!(by_name("spend absurd").len(), 0, "{sent:?}");
    assert_eq!(
        by_name("tokens high")[0].1.payload["value"]
            .as_f64()
            .unwrap(),
        tokens as f64
    );
    // Not due again straight away: once per slot.
    let before = l.rec.count();
    assert!(l.engine.tick(org).await.unwrap().is_empty());
    assert_eq!(l.rec.count(), before);
    let _ = &l.clock;
}

#[tokio::test]
#[ignore = "needs GreptimeDB (make db-up)"]
async fn ac_0_16_3_error_rate_per_model_live() {
    let l = live(39, 60).await;
    let org = l.state.org("default").unwrap();
    let want = agg_by(&l.batch, is_llm, |s| sattr(s, "llm.model_name").unwrap());
    let with_errors: BTreeSet<String> = want
        .iter()
        .filter(|(_, a)| a.errors > 0)
        .map(|(m, _)| m.clone())
        .collect();
    assert!(
        !with_errors.is_empty() && with_errors.len() < want.len() + 1,
        "the batch has failing calls"
    );
    let dest = webhook(&l.app).await;
    id_unused(
        rule(
            &l.app,
            llm_rule("model failing", "error_rate", true, ">", 0.0, &dest),
        )
        .await,
    );
    l.engine.tick(org).await.unwrap();
    let sent = l.rec.sent.lock().unwrap().clone();
    let got: BTreeSet<String> = sent
        .iter()
        .map(|(_, m)| m.payload["labels"]["model"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        got, with_errors,
        "one alert per model that had a failing call"
    );
    for (_, m) in &sent {
        let model = m.payload["labels"]["model"].as_str().unwrap();
        let a = &want[model];
        assert!(
            (m.payload["value"].as_f64().unwrap() - a.errors as f64 * 100.0 / a.calls as f64).abs()
                < 1e-9,
            "{model}"
        );
    }
}

fn id_unused(_: String) {}

#[tokio::test]
async fn ac_0_16_3_preview_writes_nothing() {
    let mut rows = HashMap::new();
    rows.insert("llm_breakdown", vec![json!({"grp": "gpt-4o", "calls": 10, "errors": 2, "p50_ns": 1e8, "p95_ns": 9e8, "p99_ns": 9e8, "avg_ns": 2e8,
        "prompt_tokens": 100, "completion_tokens": 50, "cost": 1.5, "uncosted_prompt": 0, "uncosted_completion": 0})]);
    let (app, store) = scripted(json_cols(), rows);
    let rule = llm_rule("preview cost", "cost", false, ">", 1.0, "none");
    let rule = json!({"name": rule["name"], "kind": "scheduled", "query": rule["query"], "condition": rule["condition"], "schedule": rule["schedule"], "lookback_s": 300});
    let writes = store.write_count();
    let enc: String = rule
        .to_string()
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect();
    let r = get(&app, &format!("{BASE}/alerts/preview?rule={enc}")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let d = &r.json()["data"];
    assert_eq!(d["state"], "firing");
    assert_eq!(d["would_notify"], false);
    assert_eq!(d["samples"][0]["value"], 1.5);
    assert_eq!(
        store.write_count(),
        writes,
        "a preview is a GET and writes nothing"
    );
    // The statement it ran is the analytics statement.
    assert!(
        r.json()["meta"]["sql"]
            .as_str()
            .unwrap()
            .contains("llm_breakdown")
    );
}

#[tokio::test]
async fn ac_0_16_3_condition_validation() {
    let (app, _) = scripted(json_cols(), HashMap::new());
    let post = |body: Value| {
        let app = app.clone();
        async move { send_json(&app, Method::POST, &format!("{BASE}/alerts"), &body).await }
    };
    let ok_rule = llm_rule("ok", "error_rate", true, ">", 5.0, "x");
    let mut good = ok_rule.clone();
    good["destinations"] = json!([]);
    assert_eq!(post(good.clone()).await.status, StatusCode::CREATED);
    for (what, mutate) in [
        (
            "unknown metric",
            json!({"query": {"kind": "llm", "llm": {"metric": "vibes"}}}),
        ),
        ("no settings", json!({"query": {"kind": "llm"}})),
        (
            "with text",
            json!({"query": {"kind": "llm", "text": "select 1", "llm": {"metric": "cost"}}}),
        ),
        (
            "bad model",
            json!({"query": {"kind": "llm", "llm": {"metric": "cost", "model": "a\nb"}}}),
        ),
        (
            "stray field",
            json!({"query": {"kind": "llm", "llm": {"metric": "cost", "table": "x"}}}),
        ),
        ("anomaly", json!({"kind": "anomaly", "condition": null})),
        (
            "sql with llm",
            json!({"query": {"kind": "sql", "text": "select 1", "llm": {"metric": "cost"}}}),
        ),
    ] {
        let mut b = good.clone();
        b["name"] = json!(what);
        for (k, v) in mutate.as_object().unwrap() {
            b[k] = v.clone();
        }
        let r = post(b).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{what}: {}", r.text());
    }
}
