//! AC0.13.3 – AC0.13.5: the result cache with single-flight, `/metrics`, graceful shutdown, pool
//! limits and the read-only account check.
mod common;
use async_trait::async_trait;
use axum::{Router, http::Method, http::StatusCode};
use common::*;
use grepui::{
    alerts::engine::Engine,
    api::{AppState, router},
    auth::Role,
    cache::{Cached, ResultCache},
    config::Config,
    error::ReadError,
    metrics::Metrics,
    probe::{self, Access},
    query::{Q, Reader, SqlxReader, Writer},
    workload::{CTX, Ctx},
};
use serde_json::{Value, json};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicI64, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    sync::Notify,
};

const BASE: &str = "/api/v1/orgs/default";
const LOGS: &str = "/api/v1/orgs/default/logs?from=1000&to=2000";

/// Counts the reads that reach it; every read takes `delay`.
struct Counting {
    calls: AtomicUsize,
    delay: Duration,
    rows: Mutex<Vec<Value>>,
    fail: Mutex<bool>,
}

impl Counting {
    fn new(delay_ms: u64) -> Arc<Counting> {
        Arc::new(Counting {
            calls: AtomicUsize::new(0),
            delay: Duration::from_millis(delay_ms),
            rows: Mutex::new(vec![json!({"n": 1})]),
            fail: Mutex::new(false),
        })
    }
    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl Reader for Counting {
    async fn fetch(&self, _q: &Q) -> Result<Vec<Value>, ReadError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        tokio::time::sleep(self.delay).await;
        if *self.fail.lock().unwrap() {
            return Err(ReadError::Db("boom".into()));
        }
        Ok(self.rows.lock().unwrap().clone())
    }
}

fn mk_cache(ttl_ms: u64, entries: usize, bytes: usize) -> (Arc<ResultCache>, Arc<Metrics>) {
    let m = Arc::new(Metrics::default());
    (
        Arc::new(ResultCache::new(
            Duration::from_millis(ttl_ms),
            entries,
            bytes,
            m.clone(),
        )),
        m,
    )
}

fn ctx(org: &str, role: Role) -> Ctx {
    Ctx {
        user: "u".into(),
        org: org.into(),
        role,
    }
}

fn q(n: i64) -> Q {
    Q::new("read", "SELECT ? AS n", vec![grepui::query::Bind::Int(n)])
}

async fn read(c: &Cached, who: Ctx, q: Q) -> Vec<Value> {
    CTX.scope(who, async { c.fetch(&q).await.unwrap() }).await
}

// ---- AC0.13.3 --------------------------------------------------------------------------------

#[tokio::test]
async fn ac_0_13_3_single_flight() {
    let inner = Counting::new(150);
    let (cache, m) = mk_cache(5_000, 100, 1 << 20);
    let c = Arc::new(Cached::new(inner.clone(), cache, "primary"));
    let jobs: Vec<_> = (0..6)
        .map(|_| {
            let c = c.clone();
            tokio::spawn(async move { read(&c, ctx("default", Role::Viewer), q(1)).await })
        })
        .collect();
    let mut all = vec![];
    for j in jobs {
        all.push(j.await.unwrap());
    }
    // Six identical concurrent reads hit the database once and all get the answer.
    assert_eq!(inner.calls(), 1);
    assert!(all.iter().all(|r| *r == vec![json!({"n": 1})]));
    let text = m.render();
    assert!(text.contains("grepui_cache_joined_total 5"), "{text}");
    assert!(text.contains("grepui_cache_misses_total 1"), "{text}");
    // A failure is shared by the ones that waited, and is not kept for the next read.
    let inner = Counting::new(100);
    *inner.fail.lock().unwrap() = true;
    let (cache, _) = mk_cache(5_000, 100, 1 << 20);
    let c = Arc::new(Cached::new(inner.clone(), cache.clone(), "primary"));
    let jobs: Vec<_> = (0..3)
        .map(|_| {
            let c = c.clone();
            tokio::spawn(async move {
                CTX.scope(ctx("default", Role::Viewer), async { c.fetch(&q(2)).await })
                    .await
            })
        })
        .collect();
    for j in jobs {
        assert!(matches!(j.await.unwrap(), Err(ReadError::Db(_))));
    }
    assert_eq!(inner.calls(), 1);
    assert!(cache.is_empty(), "errors are not cached");
    *inner.fail.lock().unwrap() = false;
    assert_eq!(read(&c, ctx("default", Role::Viewer), q(2)).await.len(), 1);
    assert_eq!(inner.calls(), 2);
}

#[tokio::test]
async fn ac_0_13_3_ttl_expiry() {
    let inner = Counting::new(0);
    let (cache, m) = mk_cache(200, 100, 1 << 20);
    let c = Cached::new(inner.clone(), cache.clone(), "primary");
    read(&c, ctx("default", Role::Viewer), q(1)).await;
    read(&c, ctx("default", Role::Viewer), q(1)).await;
    assert_eq!(inner.calls(), 1, "the second read is served from the cache");
    assert!(m.render().contains("grepui_cache_hits_total 1"));
    tokio::time::sleep(Duration::from_millis(260)).await;
    read(&c, ctx("default", Role::Viewer), q(1)).await;
    assert_eq!(inner.calls(), 2, "an expired result is read again");
    // Reads with no caller (the alert engine), grepui's own tables and a zero TTL are never cached.
    let bypass = Cached::new(inner.clone(), cache, "primary");
    let n = inner.calls();
    bypass.fetch(&q(1)).await.unwrap();
    bypass.fetch(&q(1)).await.unwrap();
    assert_eq!(inner.calls(), n + 2, "no caller, no cache");
    let own = Q::new("view_list", "SELECT 1 FROM `grepui`.`grepui_views`", vec![]);
    for _ in 0..2 {
        CTX.scope(ctx("default", Role::Viewer), async {
            bypass.fetch(&own).await.unwrap()
        })
        .await;
    }
    assert_eq!(inner.calls(), n + 4);
    let (off, _) = mk_cache(0, 100, 1 << 20);
    let c = Cached::new(inner.clone(), off, "primary");
    let n = inner.calls();
    read(&c, ctx("default", Role::Viewer), q(1)).await;
    read(&c, ctx("default", Role::Viewer), q(1)).await;
    assert_eq!(inner.calls(), n + 2, "a zero TTL turns the cache off");
}

#[tokio::test]
async fn ac_0_13_3_key_includes_role() {
    let inner = Counting::new(0);
    let (cache, _) = mk_cache(5_000, 100, 1 << 20);
    let c = Cached::new(inner.clone(), cache.clone(), "primary");
    read(&c, ctx("default", Role::Viewer), q(1)).await;
    read(&c, ctx("default", Role::Admin), q(1)).await;
    assert_eq!(inner.calls(), 2, "a viewer's result is not an admin's");
    // Same role, same org, same statement and binds: shared; any difference is a new entry.
    read(&c, ctx("default", Role::Viewer), q(1)).await;
    read(&c, ctx("default", Role::Admin), q(1)).await;
    assert_eq!(inner.calls(), 2);
    read(&c, ctx("other", Role::Viewer), q(1)).await;
    assert_eq!(inner.calls(), 3, "another org");
    read(&c, ctx("default", Role::Viewer), q(2)).await;
    assert_eq!(inner.calls(), 4, "other binds");
    let other_sql = Q::new("read", "SELECT ? AS m", vec![grepui::query::Bind::Int(1)]);
    read(&c, ctx("default", Role::Viewer), other_sql).await;
    assert_eq!(inner.calls(), 5, "another statement");
    // The type of a bind matters: the string "1" is not the number 1.
    let s = Q::new(
        "read",
        "SELECT ? AS n",
        vec![grepui::query::Bind::Str("1".into())],
    );
    read(&c, ctx("default", Role::Viewer), s).await;
    assert_eq!(inner.calls(), 6);
    // Another source is another namespace.
    let twin = Cached::new(inner.clone(), cache, "eu");
    read(&twin, ctx("default", Role::Viewer), q(1)).await;
    assert_eq!(inner.calls(), 7);
}

#[tokio::test]
async fn ac_0_13_3_bounded() {
    let inner = Counting::new(0);
    // At most three entries: ten different reads leave three, the oldest gone first.
    let (cache3, m) = mk_cache(60_000, 3, 1 << 20);
    let c = Cached::new(inner.clone(), cache3.clone(), "primary");
    for i in 0..10 {
        read(&c, ctx("default", Role::Viewer), q(i)).await;
    }
    assert_eq!(cache3.len(), 3);
    assert!(
        m.render().contains("grepui_cache_evictions_total 7"),
        "{}",
        m.render()
    );
    let n = inner.calls();
    read(&c, ctx("default", Role::Viewer), q(9)).await;
    assert_eq!(inner.calls(), n, "the newest is still cached");
    read(&c, ctx("default", Role::Viewer), q(0)).await;
    assert_eq!(inner.calls(), n + 1, "the oldest was dropped");
    // Memory: results are measured, the total stays under the cap, and a result that would
    // take a big share is not kept at all.
    *inner.rows.lock().unwrap() = vec![json!({"text": "x".repeat(2_000)})];
    let (small, _) = mk_cache(60_000, 1000, 10_000);
    let c = Cached::new(inner.clone(), small.clone(), "primary");
    for i in 0..20 {
        read(&c, ctx("default", Role::Viewer), q(i)).await;
        assert!(small.bytes() <= 10_000, "{} bytes", small.bytes());
    }
    *inner.rows.lock().unwrap() = vec![json!({"text": "y".repeat(50_000)})];
    let before = small.len();
    read(&c, ctx("default", Role::Viewer), q(99)).await;
    assert_eq!(
        small.len(),
        before,
        "a result over an eighth of the cap is not cached"
    );
}

// ---- AC0.13.4 --------------------------------------------------------------------------------

fn counter(text: &str, name: &str) -> f64 {
    text.lines()
        .find(|l| l.starts_with(name))
        .and_then(|l| l.rsplit(' ').next())
        .and_then(|v| v.parse().ok())
        .unwrap_or(-1.0)
}

#[tokio::test]
async fn ac_0_13_4_metrics_text() {
    let a = app(Fake::new(sample_db));
    get(&a, LOGS).await;
    let r = get(&a, "/metrics").await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(
        r.header("content-type")
            .starts_with("text/plain; version=0.0.4")
    );
    let t = r.text();
    for want in [
        "# TYPE grepui_build_info gauge",
        "# TYPE grepui_http_requests_total counter",
        "# TYPE grepui_db_query_duration_seconds histogram",
        "grepui_db_query_duration_seconds_bucket{le=\"0.005\"}",
        "grepui_db_query_duration_seconds_bucket{le=\"+Inf\"}",
        "grepui_db_query_duration_seconds_count",
        "grepui_cache_hits_total",
        "grepui_cache_misses_total",
        "grepui_alert_evaluations_total{result=\"ok\"}",
        "grepui_uptime_seconds",
    ] {
        assert!(t.contains(want), "missing {want}:\n{t}");
    }
    // Routes are labelled by template, never by the raw path, so ids do not multiply series.
    assert!(
        t.contains("route=\"/api/v1/orgs/{org}/logs\",code=\"200\""),
        "{t}"
    );
    assert!(!t.contains("from=1000"));
    // Histogram buckets are cumulative.
    let b: Vec<f64> = t
        .lines()
        .filter(|l| l.starts_with("grepui_db_query_duration_seconds_bucket"))
        .filter_map(|l| l.rsplit(' ').next()?.parse().ok())
        .collect();
    assert!(b.windows(2).all(|p| p[0] <= p[1]), "{b:?}");
}

#[tokio::test]
async fn ac_0_13_4_counters_move() {
    let store = MemStore::new();
    *store.script.lock().unwrap() = Some(Box::new(|q| {
        Ok(match q.name {
            "sql_mode" | "alert_query" => vec![json!({"n": 10})],
            _ => sample_db(q)?,
        })
    }));
    let clock = Arc::new(AtomicI64::new(1_793_400_000_000));
    let rec = Arc::new(Rec::default());
    let (a, state) = alert_app(&store, &clock, &rec);
    let t0 = state.metrics.render();
    let (reqs0, db0) = (
        counter(
            &t0,
            "grepui_http_requests_total{route=\"/api/v1/orgs/{org}/logs\",code=\"200\"}",
        ),
        counter(&t0, "grepui_db_query_duration_seconds_count"),
    );
    assert!(reqs0 < 0.0, "no logs request yet");
    for _ in 0..3 {
        assert_eq!(get(&a, LOGS).await.status, StatusCode::OK);
    }
    assert_eq!(
        get(&a, "/api/v1/orgs/default/nothing").await.status,
        StatusCode::NOT_FOUND
    );
    let t1 = state.metrics.render();
    assert_eq!(
        counter(
            &t1,
            "grepui_http_requests_total{route=\"/api/v1/orgs/{org}/logs\",code=\"200\"}"
        ),
        3.0
    );
    // A path no route matches is not a label (that would let a scanner grow the series).
    assert!(!t1.contains("nothing"), "{t1}");
    assert!(counter(&t1, "grepui_db_query_duration_seconds_count") > db0);
    // An alert evaluation moves its counter.
    let rule = json!({
        "name": "n", "kind": "scheduled",
        "query": {"kind": "sql", "text": "SELECT count(*) AS n FROM opentelemetry_logs WHERE timestamp >= $__from AND timestamp < $__to"},
        "condition": {"op": ">", "threshold": 5.0}, "schedule": {"every_s": 60}, "lookback_s": 300,
    });
    let r = send_json(&a, Method::POST, &format!("{BASE}/alerts"), &rule).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let before = counter(
        &state.metrics.render(),
        "grepui_alert_evaluations_total{result=\"ok\"}",
    );
    Engine::new(state.clone(), rec.clone())
        .with_settle(Duration::ZERO)
        .tick(state.org("default").unwrap())
        .await
        .unwrap();
    let after = counter(
        &state.metrics.render(),
        "grepui_alert_evaluations_total{result=\"ok\"}",
    );
    assert_eq!(after, before + 1.0);
    // The cache counters move with a configured cache.
    let c = Config {
        cache_ttl_secs: 30,
        ..Config::default()
    };
    let (a2, s2) = app_cfg(
        &MemStore::new(),
        &Arc::new(AtomicI64::new(1_793_400_000_000)),
        c,
    );
    get(&a2, LOGS).await;
    get(&a2, LOGS).await;
    let t = s2.metrics.render();
    assert!(counter(&t, "grepui_cache_hits_total") >= 1.0, "{t}");
}

#[tokio::test]
async fn ac_0_13_4_metrics_access() {
    let store = MemStore::new();
    let clock = Arc::new(AtomicI64::new(1_793_400_000_000));
    let token = "scrape-token-0123456789";
    let (a, _) = app_cfg(
        &store,
        &clock,
        Config {
            no_auth: false,
            metrics_token: Some(token.into()),
            ..Config::default()
        },
    );
    // Nobody anonymous, and a wrong token is no better.
    assert_eq!(get(&a, "/metrics").await.status, StatusCode::UNAUTHORIZED);
    for bad in ["Bearer nope", "Bearer scrape-token-0123456780", "Basic abc"] {
        let r = send_with(&a, Method::GET, "/metrics", &[("authorization", bad)], None).await;
        assert!(
            r.status == StatusCode::UNAUTHORIZED || r.status == StatusCode::FORBIDDEN,
            "{bad}: {:?}",
            r.status
        );
    }
    // The scraper's token works for /metrics and for nothing else.
    let auth = format!("Bearer {token}");
    let r = send_with(
        &a,
        Method::GET,
        "/metrics",
        &[("authorization", &auth)],
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.text().contains("grepui_build_info"));
    let r = send_with(&a, Method::GET, LOGS, &[("authorization", &auth)], None).await;
    assert!(r.status == StatusCode::UNAUTHORIZED || r.status == StatusCode::FORBIDDEN);
    // A platform admin's session works too; the token is not required.
    let r = send_with(
        &a,
        Method::POST,
        "/api/v1/auth/setup",
        &[],
        Some(&json!({"username": "root", "password": "correct horse battery"})),
    )
    .await;
    let cookie = r.cookie();
    let r = send_with(&a, Method::GET, "/metrics", &[("cookie", &cookie)], None).await;
    assert_eq!(r.status, StatusCode::OK);
    // Short tokens are refused at startup.
    assert!(
        Config {
            metrics_token: Some("short".into()),
            ..Config::default()
        }
        .validate()
        .is_err()
    );
}

// ---- AC0.13.5 --------------------------------------------------------------------------------

/// A reader whose log search waits for a gate.
struct Gate {
    open: Notify,
    started: AtomicUsize,
    hold: bool,
}

#[async_trait]
impl Reader for Gate {
    async fn fetch(&self, q: &Q) -> Result<Vec<Value>, ReadError> {
        if q.name == "logs_search" {
            self.started.fetch_add(1, Ordering::SeqCst);
            if self.hold {
                self.open.notified().await;
            }
            return Ok(vec![json!({"timestamp": 1500})]);
        }
        sample_db(q)
    }
}

async fn http_get(addr: std::net::SocketAddr, path: &str) -> String {
    let mut s = TcpStream::connect(addr).await.unwrap();
    s.write_all(format!("GET {path} HTTP/1.1\r\nHost: t\r\nConnection: close\r\n\r\n").as_bytes())
        .await
        .unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).await.unwrap();
    out
}

async fn serve(
    gate: Arc<Gate>,
    grace_ms: u64,
) -> (
    std::net::SocketAddr,
    tokio::sync::oneshot::Sender<()>,
    tokio::task::JoinHandle<anyhow::Result<()>>,
) {
    let state = AppState::new(Config::default(), gate).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let h = tokio::spawn(grepui::server::run(
        listener,
        router(state),
        async move {
            let _ = rx.await;
        },
        Duration::from_millis(grace_ms),
    ));
    (addr, tx, h)
}

#[tokio::test]
async fn ac_0_13_5_graceful_shutdown() {
    // A request that is running when the stop comes still gets its answer.
    let gate = Arc::new(Gate {
        open: Notify::new(),
        started: AtomicUsize::new(0),
        hold: true,
    });
    let (addr, stop, server) = serve(gate.clone(), 5_000).await;
    let req = tokio::spawn(async move { http_get(addr, LOGS).await });
    for _ in 0..200 {
        if gate.started.load(Ordering::SeqCst) > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        gate.started.load(Ordering::SeqCst),
        1,
        "the request is in flight"
    );
    stop.send(()).unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(
        !server.is_finished(),
        "the server waits for the running request"
    );
    gate.open.notify_waiters();
    let resp = req.await.unwrap();
    assert!(resp.starts_with("HTTP/1.1 200"), "{resp}");
    assert!(resp.contains("1500"));
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("the server stops once the request is done")
        .unwrap()
        .unwrap();
    // A request that never ends does not hold the process forever: the grace period closes it.
    let gate = Arc::new(Gate {
        open: Notify::new(),
        started: AtomicUsize::new(0),
        hold: true,
    });
    let (addr, stop, server) = serve(gate.clone(), 300).await;
    let _stuck = tokio::spawn(async move { http_get(addr, LOGS).await });
    for _ in 0..200 {
        if gate.started.load(Ordering::SeqCst) > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    stop.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("the grace period ends the wait")
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn ac_0_13_5_pool_limits() {
    // The limit is a flag, checked at startup, and reaches the pool.
    assert_eq!(Config::default().pool_max_connections, 8);
    for bad in [0, 101] {
        let e = Config {
            pool_max_connections: bad,
            ..Config::default()
        }
        .validate()
        .unwrap_err();
        assert!(e.to_string().contains("--pool-max-connections"), "{e}");
    }
    assert!(
        Config {
            pool_max_connections: 100,
            ..Config::default()
        }
        .validate()
        .is_ok()
    );
    let url = "mysql://root@127.0.0.1:4002/public";
    let r = SqlxReader::lazy_with(url, Duration::from_secs(5), 3).unwrap();
    assert_eq!(r.max_connections(), 3);
    assert_eq!(
        SqlxReader::lazy(url, Duration::from_secs(5))
            .unwrap()
            .max_connections(),
        8
    );
    // The other new limits are checked too.
    for c in [
        Config {
            cache_max_mb: 0,
            ..Config::default()
        },
        Config {
            cache_max_entries: 0,
            ..Config::default()
        },
        Config {
            cache_ttl_secs: 7200,
            ..Config::default()
        },
        Config {
            shutdown_grace_secs: 601,
            ..Config::default()
        },
    ] {
        assert!(c.validate().is_err());
    }
}

/// A writer that records DDL and refuses it on request.
struct Ddl {
    seen: Mutex<Vec<String>>,
    refuse_create: bool,
    refuse_drop: bool,
}

#[async_trait]
impl Writer for Ddl {
    async fn exec(&self, _q: &Q) -> Result<u64, ReadError> {
        Err(ReadError::Db("no".into()))
    }
    async fn ddl(&self, sql: &str) -> Result<(), ReadError> {
        self.seen.lock().unwrap().push(sql.to_string());
        let refuse = (sql.starts_with("CREATE") && self.refuse_create)
            || (sql.starts_with("DROP") && self.refuse_drop);
        if refuse {
            Err(ReadError::Db("permission denied for user".into()))
        } else {
            Ok(())
        }
    }
}

#[tokio::test]
async fn ac_0_13_5_warns_if_writable() {
    // An account that can create tables is reported, and the probe table is removed again.
    let w = Ddl {
        seen: Mutex::new(vec![]),
        refuse_create: false,
        refuse_drop: false,
    };
    assert_eq!(
        probe::check_read_only(&w, "demo").await.unwrap(),
        Access::CanWrite
    );
    let seen = w.seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 2);
    assert!(
        seen[0].starts_with("CREATE TABLE IF NOT EXISTS `demo`.`grepui_write_probe`"),
        "{seen:?}"
    );
    assert_eq!(seen[1], "DROP TABLE IF EXISTS `demo`.`grepui_write_probe`");
    // A read-only account is refused at the create and nothing else is attempted.
    let w = Ddl {
        seen: Mutex::new(vec![]),
        refuse_create: true,
        refuse_drop: false,
    };
    assert_eq!(
        probe::check_read_only(&w, "demo").await.unwrap(),
        Access::ReadOnly
    );
    assert_eq!(w.seen.lock().unwrap().len(), 1);
    // A probe table that cannot be removed is named so it can be dropped by hand.
    let w = Ddl {
        seen: Mutex::new(vec![]),
        refuse_create: false,
        refuse_drop: true,
    };
    match probe::check_read_only(&w, "demo").await.unwrap() {
        Access::Left(m) => assert!(
            m.contains("grepui_write_probe") && m.contains("drop it by hand"),
            "{m}"
        ),
        other => panic!("{other:?}"),
    }
    // The only thing that varies is a validated database name.
    let w = Ddl {
        seen: Mutex::new(vec![]),
        refuse_create: false,
        refuse_drop: false,
    };
    assert!(
        probe::check_read_only(&w, "demo`; DROP TABLE x; --")
            .await
            .is_err()
    );
    assert!(w.seen.lock().unwrap().is_empty());
    assert!(
        Config {
            read_only_user: true,
            ..Config::default()
        }
        .validate()
        .is_ok()
    );
    let _: Option<Router> = None;
}
