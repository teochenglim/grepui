//! AC0.6.1 – AC0.6.2: PromQL through TQL EVAL with caps, the metric and label lists, SQL on metric
//! tables and the shareable page spec.
mod common;
use axum::{Router, http::StatusCode};
use common::*;
use grepui::{
    api::{AppState, router},
    config::Config,
    playground::{http, send, send_metrics, synth},
    query::{
        SqlxReader, Window,
        metrics::{self, Grid, Spec},
    },
};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};

const BASE: &str = "/api/v1/orgs/default";
const W: &str = "from=1793398000000&to=1793402000000";

fn win(span_ms: i64) -> Window {
    Window {
        from_ms: 1_793_398_000_000,
        to_ms: 1_793_398_000_000 + span_ms,
        limit: 100,
    }
}

fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

// ---- AC0.6.1 TQL with caps ----------------------------------------------------------

#[test]
fn ac_0_6_1_tql_built_with_caps() {
    let g = Grid::range(win(3_600_000), Some(60));
    let q = metrics::tql("  sum by (job) (rate(up[5m])) ", g);
    assert_eq!(
        q.sql,
        "/* q:metrics_query */ TQL EVAL (1793398000, 1793401600, '60s') sum by (job) (rate(up[5m]))"
    );
    assert!(
        q.binds.is_empty(),
        "the grid is integers we format; the expression is checked"
    );
    let i = Grid::instant(1_793_398_123_456);
    assert_eq!(
        metrics::tql("up", i).sql,
        "/* q:metrics_query */ TQL EVAL (1793398123, 1793398123, '1s') up"
    );
    // The checker accepts ordinary PromQL, including regex matchers and offsets.
    for ok in [
        "up",
        "rate(http_requests_total{job=~\"a.*\", code!=\"500\"}[5m]) / 2",
        "histogram_quantile(0.95, sum by (le) (rate(x_bucket[5m] offset 1h)))",
        "label_replace(up, \"x\", \"$1\", \"job\", \"(.*)\")",
        "up{job=\"a\\\\.b\"}",
    ] {
        metrics::check(ok).unwrap_or_else(|e| panic!("{ok}: {}", e.1));
    }
}

#[test]
fn ac_0_6_1_promql_rejects_other_databases_and_stacking() {
    for bad in [
        "",
        "   ",
        "up{__schema__=\"public\"}",
        "up{__database__=\"public\"}",
        "up{ __SCHEMA__ = \"public\" }",
        "up{\"__schema__\"=\"public\"}",
        "up{\"__sch\\u0065ma__\"=\"public\"}",
        "up{\"__sch\\x65ma__\"=\"public\"}",
        "up{'__sch\\145ma__'=\"public\"}",
        "up{`__database__`=\"x\"}",
        "up{__sch\\u0065ma__=\"public\"}",
        "up; SELECT 1",
        "up # comment",
        "up{job=\"a\\q\"}",
        "up{job=\"unclosed}",
        "up\u{0}",
        &"a".repeat(metrics::MAX_EXPR_LEN + 1),
    ] {
        assert!(metrics::check(bad).is_err(), "should reject {bad:?}");
    }
}

#[test]
fn ac_0_6_1_step_and_window_capped() {
    // One hour at the default: the step floor keeps it under 1000 points.
    let g = Grid::range(win(3_600_000), None);
    assert_eq!(g.step_s, 4);
    assert!((g.end_s - g.start_s) / g.step_s <= metrics::MAX_POINTS);
    // A month: never per-second points, however small a step is asked for.
    let month = win(31 * 86_400_000);
    for asked in [None, Some(0), Some(1), Some(-9)] {
        let g = Grid::range(month, asked);
        assert!(
            (g.end_s - g.start_s) / g.step_s <= metrics::MAX_POINTS,
            "{asked:?}: {g:?}"
        );
    }
    // A step larger than the floor is honoured up to a day.
    assert_eq!(Grid::range(win(3_600_000), Some(300)).step_s, 300);
    assert_eq!(
        Grid::range(win(3_600_000), Some(10_000_000)).step_s,
        metrics::MAX_STEP_S
    );
    // Window limits come from the shared parser: a 32 day window is refused.
    assert!(Window::parse(Some(0), Some(32 * 86_400_000), None).is_err());
}

#[tokio::test]
async fn ac_0_6_1_query_endpoint() {
    let f = Fake::new(sample_db);
    let a = app(f.clone());
    let q = format!("expr={}&{W}&step=60", enc("process_cpu_utilization"));
    let r = get(&a, &format!("{BASE}/metrics/query?{q}")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    assert_eq!(j["data"]["step_s"], 60);
    let series = j["data"]["series"].as_array().unwrap();
    assert_eq!(series.len(), 2, "grouped by label set");
    assert_eq!(series[0]["labels"], json!({"service_name": "cart"}));
    assert_eq!(
        series[0]["points"],
        json!([[1_793_399_100_000i64, 1.5], [1_793_399_160_000i64, 2.5]])
    );
    assert!(
        j["meta"]["sql"]
            .as_str()
            .unwrap()
            .contains("TQL EVAL (1793398000, 1793402000, '60s')")
    );
    // Instant query, no window needed.
    let r = get(
        &a,
        &format!("{BASE}/metrics/query?expr=up&time=1793399100000"),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["data"]["instant"], true);
    // Rejected before anything reaches the database.
    let before = f.seen_names().len();
    for bad in [
        format!("expr={}&{W}", enc("up{__schema__=\"x\"}")),
        format!("expr=&{W}"),
        "expr=up".to_string(),
        "expr=up&time=-1".to_string(),
    ] {
        let r = get(&a, &format!("{BASE}/metrics/query?{bad}")).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{bad}");
    }
    assert_eq!(f.seen_names().len(), before);
}

#[test]
fn ac_0_6_1_series_from_rows() {
    // Column roles are told apart by type, whatever the names are.
    let rows = vec![
        json!({"job": "x", "ts": 1000, "prom_rate(ts_range,val,ts,Int64(30000))": 0.5}),
        json!({"job": "x", "ts": 2000, "prom_rate(ts_range,val,ts,Int64(30000))": 1.0}),
        json!({"job": null, "ts": 1000, "prom_rate(ts_range,val,ts,Int64(30000))": 2.0}),
    ];
    let s = metrics::to_series(&rows).unwrap();
    assert_eq!(s.len(), 2);
    assert_eq!(s[0]["labels"], json!({"job": "x"}));
    assert_eq!(s[1]["labels"], json!({}), "a null label is no label");
    assert_eq!(s[0]["points"], json!([[1000, 0.5], [2000, 1.0]]));
    assert!(metrics::to_series(&[]).unwrap().is_empty());
    assert!(metrics::to_series(&[json!({"a": "x"})]).is_err());
}

#[tokio::test]
#[ignore = "needs GreptimeDB: make db-up && make test-db"]
async fn ac_0_6_1_tql_live() {
    let l = live().await;
    // Sample times are multiples of 15 s; start the grid on one so every step lands on a sample.
    let end_s = (l.now_s / 60) * 60;
    let (from_ms, to_ms) = ((end_s - 1800) * 1000, end_s * 1000);
    let uri = |expr: &str, extra: &str| {
        format!(
            "{BASE}/metrics/query?expr={}&from={from_ms}&to={to_ms}{extra}",
            enc(expr)
        )
    };
    // A counter, exactly: every point is the generator's own value at that second.
    let r = get(
        &l.app,
        &uri(
            "http_server_request_count_total{service_name=\"payment\",http_status_class=\"5xx\"}",
            "&step=60",
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let series = r.json()["data"]["series"].as_array().unwrap().clone();
    assert_eq!(series.len(), 1, "{series:?}");
    assert_eq!(series[0]["labels"]["service_name"], "payment");
    let pts = series[0]["points"].as_array().unwrap();
    assert!(pts.len() >= 25, "{} points", pts.len());
    for p in pts {
        let t_s = (p[0].as_i64().unwrap() / 1000) as u64;
        assert_eq!(
            p[1].as_f64().unwrap(),
            synth::counter_at("payment", true, t_s),
            "t={t_s}"
        );
    }
    // A gauge, as an instant query at a sample time.
    let t_s = (end_s - 300) as u64;
    let r = get(
        &l.app,
        &format!(
            "{BASE}/metrics/query?expr={}&time={}",
            enc("process_cpu_utilization{service_name=\"cart\"}"),
            t_s * 1000
        ),
    )
    .await;
    let s = r.json()["data"]["series"].as_array().unwrap().clone();
    assert_eq!(
        s[0]["points"][0][1].as_f64().unwrap(),
        synth::gauge_at("cart", t_s)
    );
    // Aggregation and rate: sum over services of the 5xx counter rate equals the generator's rate.
    let r = get(
        &l.app,
        &uri(
            "sum by (http_status_class) (rate(http_server_request_count_total[2m]))",
            "&step=300",
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let series = r.json()["data"]["series"].as_array().unwrap().clone();
    assert_eq!(series.len(), 2);
    for s in &series {
        let class = s["labels"]["http_status_class"].as_str().unwrap();
        for p in s["points"].as_array().unwrap() {
            let t = (p[0].as_i64().unwrap() / 1000) as u64;
            let want: f64 = synth::SERVICES
                .iter()
                .map(|v| {
                    (synth::counter_at(v, class == "5xx", t)
                        - synth::counter_at(v, class == "5xx", t - 120))
                        / 120.0
                })
                .sum();
            // rate() extrapolates to the window edges; within 5% of the plain slope.
            assert!(
                (p[1].as_f64().unwrap() - want).abs() / want < 0.05,
                "{class} t={t}: {} vs {want}",
                p[1]
            );
        }
    }
    // Database-crossing matchers never reach the engine, and a bad metric is a clean 200 / 400.
    let r = get(
        &l.app,
        &uri("process_cpu_utilization{__schema__=\"public\"}", ""),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = get(&l.app, &uri("no_such_metric", "")).await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.json()["data"]["series"].as_array().unwrap().is_empty());
    let r = get(&l.app, &uri("sum by (", "")).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    l.drop().await;
}

// ---- AC0.6.2 lists, SQL and the share URL -------------------------------------------

#[tokio::test]
async fn ac_0_6_2_metric_names_list() {
    let a = app(Fake::new(sample_db));
    let r = get(&a, &format!("{BASE}/metrics")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(
        r.json()["data"],
        json!([{"name": "http_server_request_count_total", "kind": "counter"}, {"name": "process_cpu_utilization", "kind": "gauge"}])
    );
    assert!(
        r.json()["meta"]["sql"]
            .as_str()
            .unwrap()
            .contains("engine = 'metric'")
    );
    // Labels are the tag columns of the metric's table.
    let r = get(
        &a,
        &format!("{BASE}/metrics/process_cpu_utilization/labels"),
    )
    .await;
    assert_eq!(r.json()["data"], json!(["service_name"]));
    let r = get(&a, &format!("{BASE}/metrics/bad%20name/labels")).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let none = Fake::new(|q| {
        if q.name == "stream_schema" {
            Ok(vec![])
        } else {
            sample_db(q)
        }
    });
    assert_eq!(
        get(&app(none), &format!("{BASE}/metrics/ghost/labels"))
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(metrics::kind_of("physical_metric_table=true"), "unknown");
}

#[test]
fn ac_0_6_2_share_url_roundtrip() {
    let spec = Spec {
        chart: "bar".into(),
        kind: "promql".into(),
        stream: "http_server_request_count_total".into(),
        query:
            "sum by (job) (rate(http_server_request_count_total{code=~\"5..\"}[5m])) & \"x\" 100%"
                .into(),
        range: "6h".into(),
    };
    let qs = spec.to_query();
    assert!(
        !qs.contains(' ') && !qs.contains('"') && !qs.contains('{'),
        "{qs}"
    );
    assert_eq!(Spec::from_query(&qs).unwrap(), spec);
    // SQL, non-ASCII and the default spec survive too.
    let sql = Spec {
        chart: "table".into(),
        kind: "sql".into(),
        query: "SELECT 'µ' FROM t WHERE a=1&b=2".into(),
        ..Spec::default()
    };
    assert_eq!(Spec::from_query(&sql.to_query()).unwrap(), sql);
    assert_eq!(
        Spec::from_query(&Spec::default().to_query()).unwrap(),
        Spec::default()
    );
    assert_eq!(Spec::from_query("").unwrap(), Spec::default());
    // A mistyped or hostile link fails loudly instead of showing something else.
    for bad in [
        "chart=pie",
        "kind=mongo",
        "range=9y",
        "stream=a%20b",
        "zz=1",
        "q=%ZZ",
        "q=%FF",
    ] {
        assert!(Spec::from_query(bad).is_err(), "{bad}");
    }
}

#[tokio::test]
async fn ac_0_6_2_views_keep_metric_specs() {
    // A saved view of kind metrics keeps the spec keys, so a view is a stored share link.
    let store = MemStore::new();
    let clock = Arc::new(std::sync::atomic::AtomicI64::new(NOW_MS));
    let a = app_with_store(&store, &clock);
    let body = json!({"name": "CPU", "kind": "metrics", "spec": {"chart": "line", "kind": "promql", "stream": "process_cpu_utilization", "q": "process_cpu_utilization", "range": "1h"}});
    let r = send_json(
        &a,
        axum::http::Method::POST,
        &format!("{BASE}/views"),
        &body,
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    assert_eq!(
        r.json()["data"]["spec"]["stream"],
        "process_cpu_utilization"
    );
    assert_eq!(r.json()["data"]["kind"], "metrics");
}

#[tokio::test]
#[ignore = "needs GreptimeDB: make db-up && make test-db"]
async fn ac_0_6_2_metric_sql_live() {
    let l = live().await;
    let sql = "SELECT service_name, count(*) AS n FROM process_cpu_utilization GROUP BY service_name ORDER BY service_name";
    let r = get(
        &l.app,
        &format!("{BASE}/sql?sql={}&{}", enc(sql), Live::window()),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let rows = r.json()["data"]["rows"].as_array().unwrap().clone();
    assert_eq!(rows.len(), synth::SERVICES.len());
    let per = l
        .points
        .iter()
        .filter(|p| p.name == synth::GAUGE && p.service == "cart")
        .count() as i64;
    assert!(per > 100);
    for row in &rows {
        assert_eq!(row["n"].as_i64().unwrap(), per, "{row}");
    }
    // Names and labels from the live catalog.
    let r = get(&l.app, &format!("{BASE}/metrics")).await;
    let names: Vec<Value> = r.json()["data"].as_array().unwrap().clone();
    assert_eq!(
        names,
        [
            json!({"name": "http_server_request_count_total", "kind": "counter"}),
            json!({"name": "process_cpu_utilization", "kind": "gauge"})
        ]
    );
    let r = get(
        &l.app,
        &format!("{BASE}/metrics/http_server_request_count_total/labels"),
    )
    .await;
    let labels = r.json()["data"].clone();
    assert!(
        labels
            .as_array()
            .unwrap()
            .contains(&json!("http_status_class"))
    );
    assert!(labels.as_array().unwrap().contains(&json!("service_name")));
    // TQL stays out of SQL mode.
    let r = get(
        &l.app,
        &format!(
            "{BASE}/sql?sql={}&{}",
            enc("TQL EVAL (0, 10, '5s') up"),
            Live::window()
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    l.drop().await;
}

// ---- live fixture ---------------------------------------------------------------------

struct Live {
    db: String,
    app: Router,
    points: Vec<synth::MetricPoint>,
    now_s: i64,
}

async fn live() -> Live {
    let base = std::env::var("GREPUI_TEST_DB")
        .expect("set GREPUI_TEST_DB=mysql://root@127.0.0.1:4002/public");
    static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let db = format!(
        "demo_it_mt_{}_{}",
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
    let points = synth::generate_metrics(now_ns - 3_600 * 1_000_000_000, now_ns);
    send_metrics("http://127.0.0.1:4000", &db, &points)
        .await
        .expect("seed metrics");
    // Traces make the database look like a normal org (and prove the lists skip them).
    let batch = synth::generate(3, 5, now_ns, 10);
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
    Live {
        db,
        app,
        points,
        now_s: (now_ns / 1_000_000_000) as i64,
    }
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
