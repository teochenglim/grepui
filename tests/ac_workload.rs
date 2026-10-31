//! AC0.12.1 – AC0.12.5: running queries and cancel, work group quotas, statistics, and sources.
mod common;
use async_trait::async_trait;
use axum::{Router, http::Method, http::StatusCode};
use common::*;
use grepui::{
    api::{AppState, Shared, router},
    config::Config,
    error::ReadError,
    query::{Q, Reader},
};
use serde_json::{Value, json};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicI64, AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::sync::Notify;

const T0: i64 = 1_793_400_000_000;
const PW: &str = "correct horse battery";
const LOGS: &str = "/api/v1/orgs/default/logs?from=1000&to=2000";

/// A reader whose log search can be held open, and which knows what is "running" the way
/// GreptimeDB's process list does. Everything else is the in-memory store.
struct Slow {
    store: Arc<MemStore>,
    hold: Mutex<bool>,
    gate: Notify,
    inflight: Mutex<Vec<(u64, String)>>,
    killed: Mutex<Vec<String>>,
    next: AtomicU64,
    rows: Mutex<Vec<Value>>,
    delay_ms: Mutex<u64>,
    fail: Mutex<bool>,
}

struct InFlight<'a>(&'a Slow, u64);
impl Drop for InFlight<'_> {
    fn drop(&mut self) {
        self.0
            .inflight
            .lock()
            .unwrap()
            .retain(|(i, _)| *i != self.1);
    }
}

impl Slow {
    fn new(store: &Arc<MemStore>) -> Arc<Slow> {
        Arc::new(Slow {
            store: store.clone(),
            hold: Mutex::new(false),
            gate: Notify::new(),
            inflight: Mutex::new(vec![]),
            killed: Mutex::new(vec![]),
            next: AtomicU64::new(0),
            rows: Mutex::new(vec![json!({"timestamp": 1500})]),
            delay_ms: Mutex::new(0),
            fail: Mutex::new(false),
        })
    }
    fn hold(&self, on: bool) {
        *self.hold.lock().unwrap() = on;
        if !on {
            self.gate.notify_waiters();
        }
    }
    async fn wait_inflight(&self, n: usize) {
        for _ in 0..200 {
            if self.inflight.lock().unwrap().len() >= n {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("the held query never started");
    }
}

#[async_trait]
impl Reader for Slow {
    async fn fetch(&self, q: &Q) -> Result<Vec<Value>, ReadError> {
        match q.name {
            "process_list" => Ok(self
                .inflight
                .lock()
                .unwrap()
                .iter()
                .map(|(i, sql)| {
                    json!({"id": format!("gdb/{i}"), "db": "d", "query": sql.to_uppercase(),
                           "start_ms": T0, "elapsed_ms": 5})
                })
                .collect()),
            "logs_search" | "traces_list" => {
                if *self.fail.lock().unwrap() {
                    return Err(ReadError::Db("boom at 10.0.0.1".into()));
                }
                let id = self.next.fetch_add(1, Ordering::SeqCst) + 1;
                self.inflight.lock().unwrap().push((id, q.sql.clone()));
                let _g = InFlight(self, id);
                if *self.hold.lock().unwrap() {
                    self.gate.notified().await;
                }
                let d = *self.delay_ms.lock().unwrap();
                if d > 0 {
                    tokio::time::sleep(Duration::from_millis(d)).await;
                }
                Ok(self.rows.lock().unwrap().clone())
            }
            _ => self.store.fetch(q).await,
        }
    }
    async fn kill(&self, id: &str) -> Result<(), ReadError> {
        self.killed.lock().unwrap().push(id.into());
        Ok(())
    }
}

/// A plain source that answers with fixed rows, or fails.
struct Src {
    rows: Vec<Value>,
    fail: bool,
}
#[async_trait]
impl Reader for Src {
    async fn fetch(&self, q: &Q) -> Result<Vec<Value>, ReadError> {
        match q.name {
            "health" if self.fail => Err(ReadError::Db("down".into())),
            "health" => Ok(vec![json!({"ok": 1})]),
            _ if self.fail => Err(ReadError::Db("secret detail".into())),
            _ => Ok(self.rows.clone()),
        }
    }
}

struct W {
    app: Router,
    state: Shared,
    slow: Arc<Slow>,
    root: String,
}

async fn world() -> W {
    let store = MemStore::new();
    let slow = Slow::new(&store);
    let clock = Arc::new(AtomicI64::new(T0));
    let c = clock.clone();
    let state = AppState::with_parts(
        Config {
            no_auth: false,
            ..Config::default()
        },
        slow.clone(),
        store.clone(),
        Arc::new(move || c.load(Ordering::SeqCst)),
    )
    .unwrap();
    let app = router(state.clone());
    let r = send_with(
        &app,
        Method::POST,
        "/api/v1/auth/setup",
        &[],
        Some(&json!({"username": "root", "password": PW})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let root = r.cookie();
    W {
        app,
        state,
        slow,
        root,
    }
}

impl W {
    async fn call(&self, m: Method, uri: &str, who: &str, body: Option<Value>) -> Resp {
        let h = if who.is_empty() {
            vec![]
        } else {
            vec![("cookie", who)]
        };
        send_with(&self.app, m, uri, &h, body.as_ref()).await
    }
    async fn get(&self, uri: &str, who: &str) -> Resp {
        self.call(Method::GET, uri, who, None).await
    }
    async fn put(&self, uri: &str, who: &str, body: Value) -> Resp {
        self.call(Method::PUT, uri, who, Some(body)).await
    }
    async fn user(&self, name: &str, role: &str) -> String {
        let r = self
            .call(
                Method::POST,
                "/api/v1/platform/users",
                &self.root,
                Some(json!({"username": name, "password": PW, "orgs": {"default": role}})),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
        let l = self
            .call(
                Method::POST,
                "/api/v1/auth/login",
                "",
                Some(json!({"username": name, "password": PW})),
            )
            .await;
        l.cookie()
    }
    /// A request that stays open until released.
    fn spawn_get(&self, uri: &'static str, who: &str) -> tokio::task::JoinHandle<Resp> {
        let (app, who) = (self.app.clone(), who.to_string());
        tokio::spawn(
            async move { send_with(&app, Method::GET, uri, &[("cookie", &who)], None).await },
        )
    }
}

// ---- AC0.12.1 --------------------------------------------------------------------------------

#[tokio::test]
async fn ac_0_12_1_process_list_attribution() {
    let w = world().await;
    let alice = w.user("alice", "editor").await;
    let bob = w.user("bob", "editor").await;
    w.slow.hold(true);
    let h = w.spawn_get(LOGS, &alice);
    w.slow.wait_inflight(1).await;
    // The owner sees it, with the GreptimeDB process it matches.
    let r = w.get("/api/v1/orgs/default/queries", &alice).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let run = &r.json()["data"]["running"];
    assert_eq!(run.as_array().unwrap().len(), 1, "{run}");
    assert_eq!(run[0]["user"], "alice");
    assert_eq!(run[0]["tag"], "logs_search");
    assert_eq!(run[0]["greptime_id"], "gdb/1");
    assert!(run[0]["sql"].is_null(), "statement text is for admins");
    // Someone else in the org sees none of it.
    let r = w.get("/api/v1/orgs/default/queries", &bob).await;
    assert_eq!(r.json()["data"]["running"].as_array().unwrap().len(), 0);
    assert!(r.json()["data"].get("other").is_none());
    // A platform admin sees the statement and the rows grepui did not start.
    let r = w.get("/api/v1/orgs/default/queries", &w.root).await;
    let d = &r.json()["data"];
    assert_eq!(d["running"].as_array().unwrap().len(), 1);
    assert!(
        d["running"][0]["sql"]
            .as_str()
            .unwrap()
            .contains("q:logs_search")
    );
    assert!(d["other"].is_array());
    w.slow.hold(false);
    assert_eq!(h.await.unwrap().status, StatusCode::OK);
    // Once it ends it is no longer listed.
    let r = w.get("/api/v1/orgs/default/queries", &alice).await;
    assert_eq!(r.json()["data"]["running"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn ac_0_12_1_cancel_requires_permission() {
    let w = world().await;
    let alice = w.user("alice", "editor").await;
    let bob = w.user("bob", "editor").await;
    let vera = w.user("vera", "viewer").await;
    w.slow.hold(true);
    let h = w.spawn_get(LOGS, &alice);
    w.slow.wait_inflight(1).await;
    let id = w.get("/api/v1/orgs/default/queries", &alice).await.json()["data"]["running"][0]["id"]
        .as_u64()
        .unwrap();
    let url = format!("/api/v1/orgs/default/queries/{id}/cancel");
    // A viewer may not cancel; another editor may not cancel someone else's.
    for who in [&vera, &bob, &String::new()] {
        let r = w.call(Method::POST, &url, who, None).await;
        assert!(
            r.status == StatusCode::FORBIDDEN || r.status == StatusCode::UNAUTHORIZED,
            "{:?}",
            r.status
        );
    }
    assert!(w.slow.killed.lock().unwrap().is_empty());
    // The owner can; the request ends at once and the database is asked to stop too.
    let r = w.call(Method::POST, &url, &alice, None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["data"]["cancelled"], true);
    assert_eq!(r.json()["data"]["killed"], true);
    let done = h.await.unwrap();
    assert_eq!(done.status, StatusCode::TOO_MANY_REQUESTS);
    assert!(done.json()["error"].as_str().unwrap().contains("cancelled"));
    assert_eq!(*w.slow.killed.lock().unwrap(), vec!["gdb/1".to_string()]);
    // An admin may cancel anyone's.
    let h = w.spawn_get(LOGS, &bob);
    w.slow.wait_inflight(1).await;
    let id =
        w.get("/api/v1/orgs/default/queries", &w.root).await.json()["data"]["running"][0]["id"]
            .as_u64()
            .unwrap();
    let r = w
        .call(
            Method::POST,
            &format!("/api/v1/orgs/default/queries/{id}/cancel"),
            &w.root,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(h.await.unwrap().status, StatusCode::TOO_MANY_REQUESTS);
    // Cancelling is audited, and an unknown id is a 404.
    w.state.audit.flush(&w.state).await;
    let r = w
        .call(
            Method::POST,
            "/api/v1/orgs/default/queries/999999/cancel",
            &w.root,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
#[ignore = "needs GreptimeDB (make db-up)"]
async fn ac_0_12_1_cancel_live() {
    use grepui::{
        auth::Role,
        query::SqlxReader,
        workload::{CTX, Ctx},
    };
    let base = std::env::var("GREPUI_TEST_DB")
        .expect("set GREPUI_TEST_DB=mysql://root@127.0.0.1:4002/public");
    let db = format!("demo_it_cancel_{}", std::process::id());
    let form = [("content-type", "application/x-www-form-urlencoded")];
    grepui::http::post(
        "http://127.0.0.1:4000",
        "/v1/sql",
        &form,
        format!("sql=CREATE DATABASE IF NOT EXISTS {db}").as_bytes(),
    )
    .await
    .unwrap();
    let batch = grepui::playground::synth::generate(
        13,
        40,
        chrono::Utc::now().timestamp_nanos_opt().unwrap() as u64,
        30,
    );
    grepui::playground::send(
        "http://127.0.0.1:4000",
        &db,
        "opentelemetry_traces",
        "opentelemetry_logs",
        &batch,
    )
    .await
    .expect("seed");
    let url = base.replace("/public", &format!("/{db}"));
    let r = Arc::new(SqlxReader::lazy(&url, Duration::from_secs(60)).unwrap());
    let state = AppState::with_parts(
        Config {
            greptime_mysql: url,
            ..Config::default()
        },
        r.clone(),
        r,
        grepui::api::system_clock(),
    )
    .unwrap();
    // A statement that would run for a long time: the log table crossed with itself.
    let q = Q::new(
        "slow_test",
        format!(
            "SELECT count(*) AS n FROM `{db}`.`opentelemetry_logs` a CROSS JOIN `{db}`.`opentelemetry_logs` b \
             CROSS JOIN `{db}`.`opentelemetry_logs` c CROSS JOIN `{db}`.`opentelemetry_logs` d \
             WHERE a.body <> b.body"
        ),
        vec![],
    );
    let ctx = Ctx {
        user: "live".into(),
        org: "default".into(),
        role: Role::Editor,
    };
    let s2 = state.clone();
    let t = std::time::Instant::now();
    let job = tokio::spawn(CTX.scope(ctx, async move { s2.reader.fetch(&q).await }));
    // Wait for it to be listed, then cancel it.
    let mut id = None;
    for _ in 0..100 {
        if let Some((v, _)) = state.workload.running().first() {
            id = Some(v.id);
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    id.expect("the statement never showed up as running");
    // GreptimeDB lists the same statement, and its text matches ours once normalised: that is
    // how a running query is tied to its process.
    let (_, norm) = state.workload.running().remove(0);
    let procs = state
        .reader
        .fetch(&grepui::query::build::process_list())
        .await
        .unwrap();
    let mine = procs
        .iter()
        .find(|p| grepui::workload::normalize(p["query"].as_str().unwrap_or_default()) == norm)
        .unwrap_or_else(|| panic!("no process matches the statement: {procs:?}"));
    let pid = mine["id"].as_str().unwrap().to_string();
    // A real KILL stops the work in the database.
    state.reader.kill(&pid).await.expect("kill");
    let res = job.await.unwrap();
    assert!(res.is_err(), "the killed statement returned rows");
    assert!(state.workload.running().is_empty());
    let after = state
        .reader
        .fetch(&grepui::query::build::process_list())
        .await
        .unwrap();
    assert!(
        after.iter().all(|p| p["id"] != pid.as_str()),
        "the process is gone from the list"
    );
    // Cancelling on our side ends the request at once, whatever the database is doing.
    let q2 = Q::new(
        "slow_test",
        format!(
            "SELECT count(*) AS n FROM `{db}`.`opentelemetry_logs` a CROSS JOIN `{db}`.`opentelemetry_logs` b \
             CROSS JOIN `{db}`.`opentelemetry_logs` c CROSS JOIN `{db}`.`opentelemetry_logs` d \
             WHERE a.body <> b.body"
        ),
        vec![],
    );
    let ctx2 = Ctx {
        user: "live".into(),
        org: "default".into(),
        role: Role::Editor,
    };
    let s3 = state.clone();
    let job = tokio::spawn(CTX.scope(ctx2, async move { s3.reader.fetch(&q2).await }));
    let mut id = None;
    for _ in 0..100 {
        if let Some((v, _)) = state.workload.running().first() {
            id = Some(v.id);
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let id = id.expect("the second statement never showed up as running");
    assert_eq!(state.workload.cancel("default", id, None), Ok(true));
    let res = job.await.unwrap();
    assert!(
        matches!(&res, Err(ReadError::Quota(m)) if m.contains("cancelled")),
        "{res:?}"
    );
    assert!(t.elapsed() < Duration::from_secs(30), "cancel was prompt");
    assert!(state.workload.running().is_empty());
    // And a process id that GreptimeDB does not know is refused, not executed.
    assert!(state.reader.kill("1.2.3.4:5/99999").await.is_err());
    assert!(state.reader.kill("1'; DROP TABLE x; --").await.is_err());
}

// ---- AC0.12.2 --------------------------------------------------------------------------------

#[tokio::test]
async fn ac_0_12_2_quota_concurrency() {
    let w = world().await;
    let alice = w.user("alice", "viewer").await;
    let bob = w.user("bob", "viewer").await;
    let r = w
        .put(
            "/api/v1/orgs/default/workgroups/viewer",
            &w.root,
            json!({"max_concurrent": 1}),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    w.slow.hold(true);
    let h = w.spawn_get(LOGS, &alice);
    w.slow.wait_inflight(1).await;
    // Her second query is stopped before it reaches the database.
    let before = w.slow.next.load(Ordering::SeqCst);
    let r = w.get(LOGS, &alice).await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS);
    assert!(
        r.json()["error"]
            .as_str()
            .unwrap()
            .contains("max_concurrent (1)")
    );
    assert_eq!(w.slow.next.load(Ordering::SeqCst), before, "never ran");
    // Another person has their own allowance.
    let h2 = w.spawn_get(LOGS, &bob);
    w.slow.wait_inflight(2).await;
    w.slow.hold(false);
    assert_eq!(h.await.unwrap().status, StatusCode::OK);
    assert_eq!(h2.await.unwrap().status, StatusCode::OK);
    // Once the first has finished, she can run again; removing the limit lifts it.
    assert_eq!(w.get(LOGS, &alice).await.status, StatusCode::OK);
    let r = w
        .call(
            Method::DELETE,
            "/api/v1/orgs/default/workgroups/viewer",
            &w.root,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
}

#[tokio::test]
async fn ac_0_12_2_quota_window_rows() {
    let w = world().await;
    let vera = w.user("vera", "viewer").await;
    let r = w
        .put(
            "/api/v1/orgs/default/workgroups/viewer",
            &w.root,
            json!({"max_window_hours": 1, "max_rows": 2}),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    // 30 minutes is inside the window, 2 hours is not (and nothing reached the database).
    let ok = format!(
        "/api/v1/orgs/default/logs?from={}&to={}",
        T0 - 1_800_000,
        T0
    );
    let long = format!(
        "/api/v1/orgs/default/logs?from={}&to={}",
        T0 - 7_200_000,
        T0
    );
    assert_eq!(w.get(&ok, &vera).await.status, StatusCode::OK);
    let n = w.slow.next.load(Ordering::SeqCst);
    let r = w.get(&long, &vera).await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS);
    assert!(
        r.json()["error"]
            .as_str()
            .unwrap()
            .contains("max_window_hours (1)")
    );
    assert_eq!(w.slow.next.load(Ordering::SeqCst), n);
    // Admins have their own (unlimited) group.
    assert_eq!(w.get(&long, &w.root).await.status, StatusCode::OK);
    // More rows than the limit is an error that names it; at the limit is fine.
    *w.slow.rows.lock().unwrap() = (0..3).map(|i| json!({"timestamp": i})).collect();
    let r = w.get(&ok, &vera).await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS);
    assert!(r.json()["error"].as_str().unwrap().contains("max_rows (2)"));
    *w.slow.rows.lock().unwrap() = (0..2).map(|i| json!({"timestamp": i})).collect();
    assert_eq!(w.get(&ok, &vera).await.status, StatusCode::OK);
}

#[tokio::test]
async fn ac_0_12_2_quota_error_names_limit() {
    let w = world().await;
    let vera = w.user("vera", "viewer").await;
    // Setting limits is for admins, and bad values are refused.
    let url = "/api/v1/orgs/default/workgroups/viewer";
    assert_eq!(
        w.put(url, &vera, json!({"max_rows": 5})).await.status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        w.put(url, &w.root, json!({"timeout_secs": 100000}))
            .await
            .status,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        w.put("/api/v1/orgs/default/workgroups/boss", &w.root, json!({}))
            .await
            .status,
        StatusCode::BAD_REQUEST
    );
    // A timeout stops the query and says which limit did it.
    w.put(url, &w.root, json!({"timeout_secs": 1})).await;
    w.slow.hold(true);
    let t = std::time::Instant::now();
    let r = w.get(LOGS, &vera).await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS);
    let e = r.json()["error"].as_str().unwrap().to_string();
    assert!(
        e.contains("timeout_secs (1)") && e.contains("viewer"),
        "{e}"
    );
    assert!(t.elapsed() < Duration::from_secs(5));
    w.slow.hold(false);
    // The limits are listed for an admin.
    let r = w.get("/api/v1/orgs/default/workgroups", &w.root).await;
    let d = r.json();
    assert_eq!(d["data"][0]["role"], "viewer");
    assert_eq!(d["data"][0]["limits"]["timeout_secs"], 1);
    assert_eq!(
        w.get("/api/v1/orgs/default/workgroups", &vera).await.status,
        StatusCode::FORBIDDEN
    );
}

// ---- AC0.12.3 --------------------------------------------------------------------------------

#[tokio::test]
async fn ac_0_12_3_stats_slowest() {
    let w = world().await;
    let alice = w.user("alice", "viewer").await;
    let bob = w.user("bob", "viewer").await;
    // Three fast log searches by alice, one slow one by bob, and a few other tags.
    for _ in 0..3 {
        assert_eq!(w.get(LOGS, &alice).await.status, StatusCode::OK);
    }
    *w.slow.delay_ms.lock().unwrap() = 120;
    assert_eq!(w.get(LOGS, &bob).await.status, StatusCode::OK);
    let r = w.get("/api/v1/orgs/default/queries/stats", &w.root).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let d = &r.json()["data"];
    // Slowest first, per user and tag.
    assert_eq!(d["slowest"][0]["user"], "bob");
    assert_eq!(d["slowest"][0]["tag"], "logs_search");
    assert!(d["slowest"][0]["max_ms"].as_u64().unwrap() >= 100);
    // Most frequent: alice's three searches lead.
    assert_eq!(d["most_frequent"][0]["user"], "alice");
    assert_eq!(d["most_frequent"][0]["count"], 3);
    // Narrowing to one person.
    let r = w
        .get("/api/v1/orgs/default/queries/stats?user=bob", &w.root)
        .await;
    let rows = r.json()["data"]["most_frequent"]
        .as_array()
        .unwrap()
        .clone();
    assert!(rows.iter().all(|x| x["user"] == "bob"));
    // Failed runs are counted as errors.
    *w.slow.fail.lock().unwrap() = true;
    assert_eq!(w.get(LOGS, &alice).await.status, StatusCode::BAD_GATEWAY);
    let r = w.get("/api/v1/orgs/default/queries/stats", &w.root).await;
    let errs: u64 = r.json()["data"]["most_frequent"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["errors"].as_u64().unwrap())
        .sum();
    assert_eq!(errs, 1);
}

#[tokio::test]
async fn ac_0_12_3_stats_admin_only() {
    let w = world().await;
    let vera = w.user("vera", "viewer").await;
    let ed = w.user("ed", "editor").await;
    let url = "/api/v1/orgs/default/queries/stats";
    assert_eq!(w.get(url, &vera).await.status, StatusCode::FORBIDDEN);
    assert_eq!(w.get(url, &ed).await.status, StatusCode::FORBIDDEN);
    assert_eq!(w.get(url, "").await.status, StatusCode::UNAUTHORIZED);
    assert_eq!(w.get(url, &w.root).await.status, StatusCode::OK);
    // Statistics are per org: another org's callers see only their own.
    let r = w.get("/api/v1/orgs/nowhere/queries/stats", &w.root).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

// ---- AC0.12.4 --------------------------------------------------------------------------------

fn add_source(w: &W, name: &str, rows: Vec<Value>, fail: bool) {
    w.state.add_source(name, Arc::new(Src { rows, fail }));
}

#[tokio::test]
async fn ac_0_12_4_fanout_merge_order() {
    let w = world().await;
    *w.slow.rows.lock().unwrap() = vec![json!({"timestamp": 300}), json!({"timestamp": 100})];
    add_source(
        &w,
        "eu",
        vec![json!({"timestamp": 200}), json!({"timestamp": 50})],
        false,
    );
    // Without `sources` nothing changes: one source, no `sources` block.
    let r = w.get(LOGS, &w.root).await;
    assert!(r.json().get("sources").is_none());
    let r = w.get(&format!("{LOGS}&sources=all"), &w.root).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    let ts: Vec<i64> = j["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["timestamp"].as_i64().unwrap())
        .collect();
    assert_eq!(ts, vec![300, 200, 100, 50]);
    let from: Vec<&str> = j["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["_source"].as_str().unwrap())
        .collect();
    assert_eq!(from, vec!["primary", "eu", "primary", "eu"]);
    assert_eq!(j["sources"].as_array().unwrap().len(), 2);
    assert_eq!(j["meta"]["rows"], 4);
    // The row limit applies to the merged list.
    let r = w
        .get(&format!("{LOGS}&sources=primary,eu&limit=3"), &w.root)
        .await;
    assert_eq!(r.json()["data"].as_array().unwrap().len(), 3);
    // Named sources, unknown names and paging.
    let r = w.get(&format!("{LOGS}&sources=eu"), &w.root).await;
    assert_eq!(r.json()["data"].as_array().unwrap().len(), 2);
    assert_eq!(
        w.get(&format!("{LOGS}&sources=mars"), &w.root).await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        w.get(&format!("{LOGS}&sources=all&offset=10"), &w.root)
            .await
            .status,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn ac_0_12_4_partial_failure_reported() {
    let w = world().await;
    *w.slow.rows.lock().unwrap() = vec![json!({"timestamp": 300})];
    add_source(&w, "bad", vec![], true);
    let r = w.get(&format!("{LOGS}&sources=all"), &w.root).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    assert_eq!(j["data"].as_array().unwrap().len(), 1);
    let bad = j["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == "bad")
        .unwrap()
        .clone();
    assert_eq!(bad["ok"], false);
    // The error is generic: nothing from the database leaks.
    assert_eq!(bad["error"], "the source returned an error");
    assert!(!r.text().contains("secret detail"));
    // Traces fan out the same way.
    *w.slow.rows.lock().unwrap() = vec![json!({"start_ms": 10})];
    let r = w
        .get(
            "/api/v1/orgs/default/traces?from=1000&to=2000&sources=all",
            &w.root,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["sources"].as_array().unwrap().len(), 2);
    // Only when every source fails is the read itself an error.
    let r = w.get(&format!("{LOGS}&sources=bad"), &w.root).await;
    assert_eq!(r.status, StatusCode::BAD_GATEWAY);
}

#[tokio::test]
#[ignore = "needs GreptimeDB (make db-up)"]
async fn ac_0_12_4_federation_live() {
    use grepui::query::SqlxReader;
    let base = std::env::var("GREPUI_TEST_DB")
        .expect("set GREPUI_TEST_DB=mysql://root@127.0.0.1:4002/public");
    let db = format!("demo_it_fed_{}", std::process::id());
    let form = [("content-type", "application/x-www-form-urlencoded")];
    grepui::http::post(
        "http://127.0.0.1:4000",
        "/v1/sql",
        &form,
        format!("sql=CREATE DATABASE IF NOT EXISTS {db}").as_bytes(),
    )
    .await
    .unwrap();
    let now = chrono::Utc::now().timestamp_nanos_opt().unwrap() as u64;
    let batch = grepui::playground::synth::generate(13, 40, now, 30);
    grepui::playground::send(
        "http://127.0.0.1:4000",
        &db,
        "opentelemetry_traces",
        "opentelemetry_logs",
        &batch,
    )
    .await
    .expect("seed");
    let url = base.replace("/public", &format!("/{db}"));
    let mk = || Arc::new(SqlxReader::lazy(&url, Duration::from_secs(30)).unwrap());
    let state = AppState::with_parts(
        Config {
            greptime_mysql: url.clone(),
            ..Config::default()
        },
        mk(),
        mk(),
        grepui::api::system_clock(),
    )
    .unwrap();
    // The second source is the same data again, so the merge has an independent answer:
    // twice the rows of one source, newest first, half from each.
    state.add_source("twin", mk());
    let app = router(state);
    let to = chrono::Utc::now().timestamp_millis() + 60_000;
    let from = to - 3_600_000;
    let one = send_with(
        &app,
        Method::GET,
        &format!("/api/v1/orgs/default/logs?from={from}&to={to}&limit=20"),
        &[],
        None,
    )
    .await;
    assert_eq!(one.status, StatusCode::OK, "{}", one.text());
    let single = one.json()["data"].as_array().unwrap().len();
    assert!(single > 0, "the seed produced logs");
    let both = send_with(
        &app,
        Method::GET,
        &format!("/api/v1/orgs/default/logs?from={from}&to={to}&limit=1000&sources=all"),
        &[],
        None,
    )
    .await;
    assert_eq!(both.status, StatusCode::OK, "{}", both.text());
    let j = both.json();
    let rows = j["data"].as_array().unwrap();
    let all_one = send_with(
        &app,
        Method::GET,
        &format!("/api/v1/orgs/default/logs?from={from}&to={to}&limit=1000"),
        &[],
        None,
    )
    .await;
    let n1 = all_one.json()["data"].as_array().unwrap().len();
    assert_eq!(rows.len(), n1 * 2);
    let ts: Vec<i64> = rows
        .iter()
        .map(|r| r["timestamp"].as_i64().unwrap())
        .collect();
    assert!(ts.windows(2).all(|p| p[0] >= p[1]), "newest first");
    assert_eq!(rows.iter().filter(|r| r["_source"] == "twin").count(), n1);
}

// ---- AC0.12.5 --------------------------------------------------------------------------------

#[tokio::test]
async fn ac_0_12_5_source_rbac() {
    let w = world().await;
    let vera = w.user("vera", "viewer").await;
    let ed = w.user("ed", "editor").await;
    add_source(&w, "eu", vec![json!({"timestamp": 1})], false);
    // Only a platform admin changes who may use a source.
    let url = "/api/v1/platform/sources/eu";
    assert_eq!(
        w.put(url, &ed, json!({"min_role": "editor"})).await.status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        w.put(url, &w.root, json!({"min_role": "admin!"}))
            .await
            .status,
        StatusCode::BAD_REQUEST
    );
    let r = w.put(url, &w.root, json!({"min_role": "editor"})).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    // A viewer is refused by name, left out of `all`, and does not see it in the selector.
    let r = w.get(&format!("{LOGS}&sources=eu"), &vera).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let r = w.get(&format!("{LOGS}&sources=all"), &vera).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json()["sources"].as_array().unwrap().len(), 1);
    let names = |r: Resp| -> Vec<String> {
        r.json()["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["name"].as_str().unwrap().to_string())
            .collect()
    };
    assert_eq!(
        names(w.get("/api/v1/orgs/default/sources", &vera).await),
        vec!["primary"]
    );
    // An editor has it.
    let r = w.get(&format!("{LOGS}&sources=eu"), &ed).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(
        names(w.get("/api/v1/orgs/default/sources", &ed).await),
        vec!["primary", "eu"]
    );
    // The setting is in the store, not only in memory.
    let rows = w.state.sources.state("eu");
    assert_eq!(rows.min_role, grepui::auth::Role::Editor);
}

#[tokio::test]
async fn ac_0_12_5_source_health() {
    let w = world().await;
    add_source(&w, "eu", vec![], false);
    add_source(&w, "down", vec![], true);
    let r = w.get("/api/v1/platform/sources", &w.root).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let list = r.json()["data"].as_array().unwrap().clone();
    let by = |n: &str| list.iter().find(|s| s["name"] == n).unwrap().clone();
    assert_eq!(by("primary")["health"]["ok"], true);
    assert_eq!(by("eu")["health"]["ok"], true);
    assert_eq!(by("down")["health"]["ok"], false);
    assert_eq!(by("down")["health"]["error"], "the source did not answer");
    // The selector shows health to anyone who may use the source.
    let r = w.get("/api/v1/orgs/default/sources", &w.root).await;
    let d = r.json()["data"].as_array().unwrap().clone();
    assert_eq!(d.len(), 3);
    assert!(
        d.iter()
            .any(|s| s["name"] == "down" && s["health"]["ok"] == false)
    );
    // The platform list is for platform admins only.
    let vera = w.user("vera", "viewer").await;
    assert_eq!(
        w.get("/api/v1/platform/sources", &vera).await.status,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn ac_0_12_5_source_disable() {
    let w = world().await;
    let ed = w.user("ed", "editor").await;
    *w.slow.rows.lock().unwrap() = vec![json!({"timestamp": 9})];
    add_source(&w, "eu", vec![json!({"timestamp": 8})], false);
    let both = format!("{LOGS}&sources=all");
    let n = |r: Resp| r.json()["data"].as_array().unwrap().len();
    assert_eq!(n(w.get(&both, &ed).await), 2);
    // Disabled: effective on the next request, with no restart.
    let url = "/api/v1/platform/sources/eu";
    let r = w.put(url, &w.root, json!({"enabled": false})).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(n(w.get(&both, &ed).await), 1);
    assert_eq!(
        w.get(&format!("{LOGS}&sources=eu"), &ed).await.status,
        StatusCode::FORBIDDEN
    );
    // Back on again.
    w.put(url, &w.root, json!({"enabled": true})).await;
    assert_eq!(n(w.get(&both, &ed).await), 2);
    // The primary cannot be turned off, and an unknown source is a 404.
    assert_eq!(
        w.put(
            "/api/v1/platform/sources/primary",
            &w.root,
            json!({"enabled": false})
        )
        .await
        .status,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        w.put(
            "/api/v1/platform/sources/mars",
            &w.root,
            json!({"enabled": false})
        )
        .await
        .status,
        StatusCode::NOT_FOUND
    );
    // A reload from the store (what another replica does) keeps the same answer.
    w.put(url, &w.root, json!({"enabled": false})).await;
    grepui::api::workload::reload(&w.state).await;
    assert_eq!(n(w.get(&both, &ed).await), 1);
}
