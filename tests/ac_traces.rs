//! AC0.4.1 – AC0.4.6: trace list, histogram, scatter, detail (waterfall), flame graph, span
//! panel and the trace-to-logs pivot.
mod common;
use axum::{Router, http::StatusCode};
use common::*;
use grepui::{
    api::{AppState, NoWriter, router},
    config::Config,
    layout::{self, SpanIn, span_from_row},
    playground::{http, send, synth},
    query::{
        Bind, SqlxReader, Window,
        filter::parse,
        traces::{self, TCol, TraceFilter},
    },
};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Duration,
};

const BASE: &str = "/api/v1/orgs/default";
const W: &str = "from=1793398000000&to=1793402000000";
const NOW: i64 = 1_793_400_000_000;

/// The traces table's columns, flat attribute columns included (as GreptimeDB creates them).
fn columns() -> Vec<String> {
    [
        "timestamp",
        "timestamp_end",
        "duration_nano",
        "parent_span_id",
        "trace_id",
        "span_id",
        "span_kind",
        "span_name",
        "span_status_code",
        "span_status_message",
        "scope_name",
        "service_name",
        "span_attributes.http.route",
        "span_attributes.http.status_code",
        "resource_attributes.service.version",
        "span_events",
        "span_links",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

fn trace_fake() -> Arc<Fake> {
    Fake::new(|q| {
        if q.name == "stream_schema" {
            return Ok(columns()
                .into_iter()
                .map(|c| json!({"column_name": c, "data_type": "string", "semantic_type": "FIELD"}))
                .collect());
        }
        sample_db(q)
    })
}

fn app_at(f: Arc<Fake>, now: i64) -> Router {
    router(
        AppState::with_parts(
            Config::default(),
            f,
            Arc::new(NoWriter),
            Arc::new(move || now),
        )
        .unwrap(),
    )
}

fn win() -> Window {
    Window {
        from_ms: 1_793_398_000_000,
        to_ms: 1_793_402_000_000,
        limit: 50,
    }
}

fn filter(q: &str) -> TraceFilter {
    TraceFilter {
        conds: traces::resolve(&parse(q).unwrap(), &columns()).unwrap(),
        ..Default::default()
    }
}

fn spans_of(rows: Vec<Value>) -> Vec<SpanIn> {
    rows.iter().filter_map(span_from_row).collect()
}

// ---- AC0.4.1 list --------------------------------------------------------------

#[tokio::test]
async fn ac_0_4_1_list_shape() {
    let f = trace_fake();
    let a = app_at(f.clone(), NOW);
    let r = get(&a, &format!("{BASE}/traces?{W}&limit=25&offset=50")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    assert_eq!(j["meta"]["query"], "traces_list");
    let first = &j["data"][0];
    for k in [
        "trace_id",
        "start_ms",
        "duration_us",
        "spans",
        "services",
        "errors",
        "root_service",
        "root_operation",
    ] {
        assert!(!first[k].is_null(), "{k}: {first}");
    }
    assert_eq!(
        (
            first["root_service"].as_str(),
            first["spans"].as_i64(),
            first["errors"].as_i64()
        ),
        (Some("frontend"), Some(5), Some(1))
    );
    let q = f
        .seen
        .lock()
        .unwrap()
        .iter()
        .find(|q| q.name == "traces_list")
        .unwrap()
        .clone();
    // One row per trace: grouped by trace_id; the root is the span without a parent.
    assert!(
        q.sql.contains("GROUP BY `trace_id`") && q.sql.contains("count(DISTINCT `service_name`)"),
        "{}",
        q.sql
    );
    assert!(
        q.sql
            .contains("`parent_span_id` IS NULL OR `parent_span_id` = ''"),
        "{}",
        q.sql
    );
    assert!(
        q.sql.contains("ORDER BY start_ms DESC LIMIT 25 OFFSET 50"),
        "{}",
        q.sql
    );
    // No filter: no subquery, no HAVING; just the window.
    assert!(
        !q.sql.contains("IN (SELECT") && !q.sql.contains("HAVING"),
        "{}",
        q.sql
    );
    assert_eq!(q.binds.len(), 2);
    // Offsets are capped; a window is required.
    get(&a, &format!("{BASE}/traces?{W}&offset=99999999")).await;
    assert!(
        f.seen
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .sql
            .ends_with(&format!("OFFSET {}", traces::MAX_OFFSET))
    );
    assert_eq!(
        get(&a, &format!("{BASE}/traces")).await.status,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn ac_0_4_1_filters_bound() {
    let f = trace_fake();
    let a = app_at(f.clone(), NOW);
    let evil = "x' OR '1'='1";
    let url = format!(
        "{BASE}/traces?{W}&service={}&errors=true&min_ms=5&q={}",
        "x%27%20OR%20%271%27%3D%271",
        "http.route%3DGET%20status%3Derror%20resource.service.version~1.4%20slow"
    );
    let r = get(&a, &url).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let q = f
        .seen
        .lock()
        .unwrap()
        .iter()
        .find(|q| q.name == "traces_list")
        .unwrap()
        .clone();
    let binds: Vec<String> = q.binds.iter().map(Bind::show).collect();
    // Placeholders follow the text: the match predicate in the select list (service, conditions in
    // order, free text), then the window, then the minimum duration in the outer WHERE.
    assert_eq!(binds[0], evil, "the service is a bound value");
    assert_eq!(&binds[1..4], ["GET", "STATUS_CODE_ERROR", "%1.4%"]);
    assert_eq!(binds[4], "%slow%");
    assert_eq!(
        &binds[5..7],
        [
            grepui::query::ts_string(1_793_398_000_000),
            grepui::query::ts_string(1_793_402_000_000)
        ]
    );
    assert_eq!(
        q.binds.last().unwrap(),
        &Bind::Int(5_000_000),
        "min_ms in nanoseconds"
    );
    for leak in [evil, "GET", "slow", "1.4", "STATUS_CODE_ERROR\""] {
        assert!(
            !q.sql.contains(leak),
            "{leak} leaked into SQL text: {}",
            q.sql
        );
    }
    // A trace matches when one of its spans does: a conditional count in the same aggregate. (No
    // IN-subquery, EXISTS or join: GreptimeDB v1.2.1 returns wrong, nondeterministic rows for them.)
    assert!(
        q.sql.contains(
            "sum(CASE WHEN `service_name` = ? AND `span_status_code` = 'STATUS_CODE_ERROR' AND "
        ),
        "{}",
        q.sql
    );
    assert!(
        q.sql.contains("THEN 1 ELSE 0 END) AS `matches`"),
        "{}",
        q.sql
    );
    assert!(
        q.sql
            .contains("AS `g` WHERE `matches` > 0 AND (`last_ns` - `first_ns`) >= ?"),
        "{}",
        q.sql
    );
    assert!(
        !q.sql.contains(" IN (SELECT") && !q.sql.contains("EXISTS") && !q.sql.contains(" JOIN "),
        "{}",
        q.sql
    );
    assert!(
        q.sql.contains("`service_name` = ?")
            && q.sql.contains("`span_status_code` = 'STATUS_CODE_ERROR'"),
        "{}",
        q.sql
    );
    assert!(
        q.sql
            .contains("CAST(`span_attributes.http.route` AS STRING) = ?"),
        "attribute columns compare as text: {}",
        q.sql
    );
    assert!(
        q.sql
            .contains("CAST(`resource_attributes.service.version` AS STRING) LIKE ?"),
        "{}",
        q.sql
    );
    assert!(q.sql.contains("`span_name` LIKE ?"), "{}", q.sql);
    assert_eq!(q.sql.matches('?').count(), q.binds.len());

    // Keys are matched against the table's real columns before they are used in SQL.
    let before = f.seen_names().len();
    for bad in ["no.such.key=1", "http.status=1", "resource.nothing=x"] {
        let r = get(&a, &format!("{BASE}/traces?{W}&q={bad}")).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{bad}");
        assert!(
            r.json()["error"]
                .as_str()
                .unwrap()
                .contains("unknown attribute"),
            "{bad}"
        );
    }
    assert_eq!(
        f.seen_names()
            .iter()
            .filter(|n| **n == "traces_list")
            .count(),
        1,
        "rejected filters never ran a list query ({before})"
    );
    for bad in ["errors=maybe", "min_ms=abc"] {
        assert_eq!(
            get(&a, &format!("{BASE}/traces?{W}&{bad}")).await.status,
            StatusCode::BAD_REQUEST,
            "{bad}"
        );
    }
    // Resolution is pure: shorthands and aliases.
    let c = filter("service=cart operation~Charge kind=server status=ok trace=abc scope=demo span=1 attr.http.route=x").conds;
    let cols: Vec<_> = c.iter().map(|c| c.col.clone()).collect();
    assert_eq!(cols[0], TCol::Plain("service_name"));
    assert_eq!(cols[1], TCol::Plain("span_name"));
    assert_eq!(c[2].value, "SPAN_KIND_SERVER");
    assert_eq!(c[3].value, "STATUS_CODE_OK");
    assert_eq!(cols[7], TCol::Attr("span_attributes.http.route".into()));
    assert_eq!(
        filter("status=weird").conds[0].value,
        "weird",
        "unknown values pass through as a plain bound value"
    );
    // Negative and huge min_ms are clamped.
    let q = traces::list(
        "public",
        "t",
        &TraceFilter {
            min_ms: Some(-5),
            ..Default::default()
        },
        win(),
        0,
    )
    .unwrap();
    assert!(!q.sql.contains("`last_ns` - `first_ns`) >= ?"), "{}", q.sql);
    // Each aggregate appears once (the planner rejects duplicates).
    assert_eq!(
        q.sql.matches("min(CAST(`timestamp` AS BIGINT))").count(),
        1,
        "{}",
        q.sql
    );
    assert_eq!(
        q.sql
            .matches("max(CAST(`timestamp_end` AS BIGINT))")
            .count(),
        1
    );
}

// ---- live fixture: a throwaway database built from the deterministic generator -------

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

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
        "demo_it_tr_{}_{}",
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
    let batch = synth::generate(5, 40, now_ns, 60);
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
    /// Spans of each trace, by hex trace id.
    fn by_trace(&self) -> HashMap<String, Vec<&synth::SpanRec>> {
        let mut m: HashMap<String, Vec<&synth::SpanRec>> = HashMap::new();
        for s in &self.batch.spans {
            m.entry(hex(&s.trace_id)).or_default().push(s);
        }
        m
    }
}

fn ids(rows: &Value) -> HashSet<String> {
    rows["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["trace_id"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
#[ignore = "needs GreptimeDB: make db-up && make test-db"]
async fn ac_0_4_1_one_row_per_trace_live() {
    let l = live().await;
    let w = Live::window();
    let list = get(&l.app, &format!("{BASE}/traces?{w}&limit=1000")).await;
    assert_eq!(list.status, StatusCode::OK, "{}", list.text());
    let rows = list.json()["data"].as_array().unwrap().clone();
    let expected = l.by_trace();
    assert_eq!(rows.len(), 40, "one row per trace");
    assert_eq!(
        ids(&list.json()),
        expected.keys().cloned().collect::<HashSet<_>>()
    );
    for r in &rows {
        let spans = &expected[r["trace_id"].as_str().unwrap()];
        let root = spans.iter().find(|s| s.parent.is_none()).unwrap();
        let (start, end) = (
            spans.iter().map(|s| s.start_ns).min().unwrap(),
            spans.iter().map(|s| s.end_ns).max().unwrap(),
        );
        assert_eq!(r["spans"].as_i64().unwrap() as usize, spans.len());
        assert_eq!(
            r["services"].as_i64().unwrap() as usize,
            spans
                .iter()
                .map(|s| s.service)
                .collect::<HashSet<_>>()
                .len()
        );
        assert_eq!(
            r["errors"].as_i64().unwrap() as usize,
            spans.iter().filter(|s| s.error).count()
        );
        assert_eq!(r["root_service"], "frontend");
        assert_eq!(r["root_operation"], root.name);
        assert_eq!(
            r["duration_us"].as_i64().unwrap(),
            ((end - start) / 1000) as i64,
            "duration is exact to the microsecond"
        );
        assert_eq!(r["start_ms"].as_i64().unwrap(), (start / 1_000_000) as i64);
    }
    assert!(
        rows.windows(2)
            .all(|p| p[0]["start_ms"].as_i64() >= p[1]["start_ms"].as_i64()),
        "newest first"
    );

    // Filters against what was generated.
    let want = |pred: &dyn Fn(&[&synth::SpanRec]) -> bool| -> HashSet<String> {
        expected
            .iter()
            .filter(|(_, s)| pred(s))
            .map(|(k, _)| k.clone())
            .collect()
    };
    let cart = want(&|s| s.iter().any(|x| x.service == "cart"));
    assert!(!cart.is_empty() && cart.len() < 40);
    assert_eq!(
        ids(&get(
            &l.app,
            &format!("{BASE}/traces?{w}&limit=1000&service=cart")
        )
        .await
        .json()),
        cart
    );
    let err = want(&|s| s.iter().any(|x| x.error));
    assert!(!err.is_empty() && err.len() < 40);
    assert_eq!(
        ids(
            &get(&l.app, &format!("{BASE}/traces?{w}&limit=1000&errors=true"))
                .await
                .json()
        ),
        err
    );
    assert_eq!(
        ids(&get(
            &l.app,
            &format!("{BASE}/traces?{w}&limit=1000&q=status%3Derror")
        )
        .await
        .json()),
        err
    );
    let charge = want(&|s| s.iter().any(|x| x.name == "Charge"));
    assert_eq!(
        ids(
            &get(&l.app, &format!("{BASE}/traces?{w}&limit=1000&q=Charge"))
                .await
                .json()
        ),
        charge
    );
    assert_eq!(
        ids(&get(
            &l.app,
            &format!("{BASE}/traces?{w}&limit=1000&q=http.route%3DCharge")
        )
        .await
        .json()),
        charge,
        "flat attribute column"
    );
    assert_eq!(
        ids(&get(
            &l.app,
            &format!("{BASE}/traces?{w}&limit=1000&q=resource.service.version%3D1.4.2")
        )
        .await
        .json())
        .len(),
        40
    );
    let mut durs: Vec<i64> = expected
        .values()
        .map(|s| {
            ((s.iter().map(|x| x.end_ns).max().unwrap()
                - s.iter().map(|x| x.start_ns).min().unwrap())
                / 1_000_000) as i64
        })
        .collect();
    durs.sort();
    let min_ms = durs[20].max(1);
    let slow = want(&|s| {
        (s.iter().map(|x| x.end_ns).max().unwrap() - s.iter().map(|x| x.start_ns).min().unwrap())
            as i64
            >= min_ms * 1_000_000
    });
    assert_eq!(
        ids(&get(
            &l.app,
            &format!("{BASE}/traces?{w}&limit=1000&min_ms={min_ms}")
        )
        .await
        .json()),
        slow
    );
    // Filters must all match the SAME span. Only payment spans fail in the generated data, so
    // "a cart span that errored" matches nothing, while "a payment span that errored" matches
    // every trace with an error. (Matching trace-level sets would give a non-empty intersection.)
    let cart_and_error = want(&|s| s.iter().any(|x| x.service == "cart" && x.error));
    assert!(cart_and_error.is_empty());
    assert!(
        !cart.intersection(&err).collect::<Vec<_>>().is_empty(),
        "the trace-level sets do overlap"
    );
    assert!(
        ids(&get(
            &l.app,
            &format!("{BASE}/traces?{w}&limit=1000&service=cart&errors=true")
        )
        .await
        .json())
        .is_empty(),
        "same-span semantics"
    );
    let pay_err = want(&|s| s.iter().any(|x| x.service == "payment" && x.error));
    assert_eq!(pay_err, err);
    assert_eq!(
        ids(&get(
            &l.app,
            &format!("{BASE}/traces?{w}&limit=1000&service=payment&errors=true")
        )
        .await
        .json()),
        pay_err
    );
    // Different spans can still be combined with duration: slow traces that also have a cart span.
    let slow_cart = want(&|s| {
        s.iter().any(|x| x.service == "cart")
            && (s.iter().map(|x| x.end_ns).max().unwrap()
                - s.iter().map(|x| x.start_ns).min().unwrap()) as i64
                >= min_ms * 1_000_000
    });
    assert_eq!(
        ids(&get(
            &l.app,
            &format!("{BASE}/traces?{w}&limit=1000&service=cart&min_ms={min_ms}")
        )
        .await
        .json()),
        slow_cart
    );
    // An injection attempt is only a value.
    assert!(
        ids(&get(
            &l.app,
            &format!("{BASE}/traces?{w}&service=x%27%20OR%20%271%27%3D%271")
        )
        .await
        .json())
        .is_empty()
    );
    // Paging is disjoint and complete.
    let p1 = ids(
        &get(&l.app, &format!("{BASE}/traces?{w}&limit=15&offset=0"))
            .await
            .json(),
    );
    let p2 = ids(
        &get(&l.app, &format!("{BASE}/traces?{w}&limit=15&offset=15"))
            .await
            .json(),
    );
    let p3 = ids(
        &get(&l.app, &format!("{BASE}/traces?{w}&limit=15&offset=30"))
            .await
            .json(),
    );
    assert_eq!((p1.len(), p2.len(), p3.len()), (15, 15, 10));
    assert!(p1.is_disjoint(&p2) && p2.is_disjoint(&p3));
    l.drop().await;
}

// ---- AC0.4.2 histogram and scatter ------------------------------------------------

#[tokio::test]
async fn ac_0_4_2_histogram_matches_filters() {
    // The histogram uses exactly the list's span predicate: same text, same binds.
    let f = filter("http.route=GET status=error");
    let f = TraceFilter {
        service: Some("cart".into()),
        errors: true,
        ..f
    };
    let list = traces::list("public", "t", &f, win(), 0).unwrap();
    let hist = traces::histogram("public", "t", &f, win()).unwrap();
    // list binds: [predicate..., window(2)]; histogram binds: [window(2), predicate...].
    let n = list.binds.len() - 2;
    assert_eq!(&list.binds[..n], &hist.binds[2..], "same predicate binds");
    let pred = list
        .sql
        .split("sum(CASE WHEN ")
        .nth(2)
        .unwrap()
        .split(" THEN 1 ELSE 0 END) AS `matches`")
        .next()
        .unwrap();
    assert!(
        hist.sql.contains(pred),
        "the same predicate text: {pred}\n{}",
        hist.sql
    );
    // Each trace is counted once, in the bucket of its earliest matching span.
    assert!(
        hist.sql.contains("min(`timestamp`) AS `first_span`")
            && hist
                .sql
                .contains("GROUP BY `trace_id`) AS `s` GROUP BY 1 ORDER BY 1"),
        "{}",
        hist.sql
    );
    assert!(
        hist.sql.contains("/ 60000000000) * 60000 AS bucket_ms"),
        "{}",
        hist.sql
    );
    assert!(!hist.sql.contains("INTERVAL"));
    // Over HTTP.
    let a = app_at(trace_fake(), NOW);
    let j = get(&a, &format!("{BASE}/traces/histogram?{W}&service=cart"))
        .await
        .json();
    assert_eq!(j["data"]["total"], 42);
    assert_eq!(
        j["data"]["bucket_ms"], 60_000,
        "a 4000 s window gives 66 one-minute buckets"
    );
    assert_eq!(
        j["data"]["buckets"][0],
        json!({"t": 1_793_399_100_000i64, "count": 12})
    );
    assert_eq!(j["meta"]["query"], "traces_histogram");
    assert_eq!(
        get(&a, &format!("{BASE}/traces/histogram")).await.status,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn ac_0_4_2_scatter_capped() {
    let f = Fake::new(|q| {
        if q.name == "traces_scatter" {
            return Ok((0..500).map(|i| json!({"trace_id": format!("{i:032x}"), "start_ms": 1_793_399_000_000i64 + i, "duration_us": 100 + i, "spans": 3, "errors": 0})).collect());
        }
        sample_db(q)
    });
    let a = app_at(f.clone(), NOW);
    let j = get(
        &a,
        &format!("{BASE}/traces/scatter?{W}&limit=5&service=cart"),
    )
    .await
    .json();
    assert_eq!(j["data"]["points"].as_array().unwrap().len(), 500);
    assert_eq!(
        (j["data"]["capped"].clone(), j["data"]["max"].clone()),
        (json!(true), json!(500))
    );
    let q = f
        .seen
        .lock()
        .unwrap()
        .iter()
        .find(|q| q.name == "traces_scatter")
        .unwrap()
        .clone();
    assert!(
        q.sql.contains("LIMIT 500 OFFSET 0"),
        "the cap does not follow the limit parameter: {}",
        q.sql
    );
    assert!(
        q.binds.iter().any(|b| b == &Bind::Str("cart".into())),
        "scatter shares the filters"
    );
    let few = app_at(trace_fake(), NOW);
    let j = get(&few, &format!("{BASE}/traces/scatter?{W}"))
        .await
        .json();
    assert_eq!(
        (
            j["data"]["points"].as_array().unwrap().len(),
            j["data"]["capped"].clone()
        ),
        (2, json!(false))
    );
}

// ---- AC0.4.3 detail / waterfall ------------------------------------------------------

#[tokio::test]
async fn ac_0_4_3_trace_id_validated() {
    let f = trace_fake();
    let a = app_at(f.clone(), NOW);
    for bad in [
        "xyz",
        "g".repeat(32).as_str(),
        "abc",
        &"a".repeat(33),
        "a-b-c-d-e-f-a-b-c-d",
        "0x0123456789abcdef",
        "%27%3B",
        "aaaaaaaaaaaaaaa%20a",
    ] {
        for tail in ["", "/flame", "/logs"] {
            let r = get(&a, &format!("{BASE}/traces/{bad}{tail}")).await;
            assert_eq!(
                r.status,
                StatusCode::BAD_REQUEST,
                "{bad}{tail}: {}",
                r.text()
            );
        }
    }
    assert!(
        f.seen_names().is_empty(),
        "invalid ids never reach the database"
    );
    // Upper-case hex is accepted and bound lower-cased; a 16-digit id is accepted too.
    let r = get(&a, &format!("{BASE}/traces/{}", TID.to_uppercase())).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(
        f.seen.lock().unwrap().last().unwrap().binds[0],
        Bind::Str(TID.into())
    );
    assert_eq!(
        get(&a, &format!("{BASE}/traces/0123456789abcdef"))
            .await
            .status,
        StatusCode::OK
    );
    // No spans in the window: 404 with advice; window params validated.
    let none = app_at(Fake::new(|_| Ok(vec![])), NOW);
    let r = get(&none, &format!("{BASE}/traces/{TID}")).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert!(r.json()["error"].as_str().unwrap().contains("widen"));
    assert_eq!(
        get(&a, &format!("{BASE}/traces/{TID}?from=5")).await.status,
        StatusCode::BAD_REQUEST
    );
    // Without a window the last week is searched, bound as timestamps.
    let w = traces::default_window(NOW);
    assert_eq!(w.to_ms - w.from_ms, 7 * 86_400_000 + 3_600_000);
    let q = traces::spans("public", "t", TID, w).unwrap();
    assert!(
        q.sql.contains("`trace_id` = ?")
            && q.sql.contains(&format!("LIMIT {}", traces::SPANS_MAX + 1)),
        "{}",
        q.sql
    );
    assert!(
        q.sql
            .contains("CAST(CAST(`timestamp` AS BIGINT) AS VARCHAR) AS start_ns"),
        "nanoseconds travel as strings: {}",
        q.sql
    );
}

#[test]
fn ac_0_4_3_spans_ordered() {
    // Input in arbitrary (here reversed, then rotated) order; output is depth-first, siblings by start.
    let mut rows = sample_trace();
    rows.reverse();
    rows.rotate_left(2);
    let wf = layout::waterfall(&spans_of(rows));
    let order: Vec<&str> = wf
        .rows
        .iter()
        .map(|r| r["span_id"].as_str().unwrap())
        .collect();
    assert_eq!(
        order,
        [
            "a000000000000001",
            "a000000000000002",
            "a000000000000003",
            "a000000000000004",
            "a000000000000005",
            "a000000000000006",
            "a000000000000007"
        ]
    );
    // Every parent precedes its children.
    let pos: HashMap<&str, usize> = order.iter().enumerate().map(|(i, s)| (*s, i)).collect();
    for r in &wf.rows {
        if let Some(p) = r["parent_id"].as_str() {
            assert!(pos[p] < pos[r["span_id"].as_str().unwrap()]);
        }
    }
    // Siblings that start together are ordered by id (stable); a partial trace (missing parent) keeps its subtree.
    let mut partial = sample_trace();
    partial.remove(0); // no root: cart and checkout become roots
    let wf = layout::waterfall(&spans_of(partial));
    let depths: Vec<_> = wf
        .rows
        .iter()
        .map(|r| {
            (
                r["span_id"].as_str().unwrap().to_string(),
                r["depth"].as_i64().unwrap(),
            )
        })
        .collect();
    assert_eq!(depths[0], ("a000000000000002".into(), 0), "cart is a root");
    assert_eq!(
        depths[1],
        ("a000000000000003".into(), 1),
        "postgres stays under cart"
    );
    assert_eq!(
        depths[2],
        ("a000000000000004".into(), 0),
        "checkout is a root"
    );
    assert_eq!(depths[3], ("a000000000000005".into(), 1));
    // A parent cycle cannot hang or drop spans.
    let mut cyc = vec![
        span_row(
            "c000000000000001",
            Some("c000000000000002"),
            "a",
            "x",
            0,
            5,
            "STATUS_CODE_OK",
        ),
        span_row(
            "c000000000000002",
            Some("c000000000000001"),
            "b",
            "y",
            1,
            4,
            "STATUS_CODE_OK",
        ),
    ];
    cyc.push(span_row(
        "c000000000000003",
        Some("c000000000000003"),
        "c",
        "self",
        2,
        3,
        "STATUS_CODE_OK",
    ));
    let wf = layout::waterfall(&spans_of(cyc.clone()));
    assert_eq!(wf.rows.len(), 3, "every span appears once");
    assert!(layout::flame(&spans_of(cyc)).is_object());
    assert!(layout::waterfall(&[]).rows.is_empty());
    assert_eq!(layout::flame(&[])["total_us"], 0);
}

#[test]
fn ac_0_4_3_waterfall_depth_offsets() {
    let wf = layout::waterfall(&spans_of(sample_trace()));
    let row = |id: &str| wf.rows.iter().find(|r| r["span_id"] == id).unwrap().clone();
    // (id, depth, offset_us, duration_us)
    for (id, depth, off, dur) in [
        ("a000000000000001", 0, 0, 100_000),
        ("a000000000000002", 1, 5_000, 35_000),
        ("a000000000000003", 2, 10_000, 20_000),
        ("a000000000000004", 1, 45_000, 50_000),
        ("a000000000000005", 2, 50_000, 20_000),
        ("a000000000000006", 2, 72_000, 18_000),
        ("a000000000000007", 2, 91_000, 3_000),
    ] {
        let r = row(id);
        assert_eq!(
            (
                r["depth"].as_i64(),
                r["offset_us"].as_i64(),
                r["duration_us"].as_i64()
            ),
            (Some(depth), Some(off), Some(dur)),
            "{id}"
        );
    }
    assert_eq!((wf.end_ns - wf.start_ns) / 1000, 100_000);
    assert_eq!(row("a000000000000005")["error"], true);
    assert_eq!(row("a000000000000006")["error"], false);
    assert_eq!(row("a000000000000003")["service"], "postgres");
    assert_eq!(
        row("a000000000000001")["start_ns"],
        T_NS.to_string(),
        "exact nanoseconds, as a string"
    );
    // Sub-millisecond precision survives (a 1.5 µs offset is 1 µs, not 0 and not 2).
    let mut fine = sample_trace();
    fine[1]["start_ns"] = json!((T_NS + 1_500).to_string());
    fine[1]["end_ns"] = json!((T_NS + 4_999).to_string());
    let wf = layout::waterfall(&spans_of(fine));
    let r = wf
        .rows
        .iter()
        .find(|r| r["span_id"] == "a000000000000002")
        .unwrap();
    assert_eq!(
        (r["offset_us"].as_i64(), r["duration_us"].as_i64()),
        (Some(1), Some(3))
    );
    // An end before the start never gives a negative duration; unusable rows are skipped.
    let mut weird = sample_trace();
    weird[2]["end_ns"] = json!((T_NS).to_string());
    assert_eq!(
        layout::waterfall(&spans_of(weird))
            .rows
            .iter()
            .find(|r| r["span_id"] == "a000000000000003")
            .unwrap()["duration_us"],
        0
    );
    let mut junk = sample_trace();
    junk[1]["start_ns"] = json!("not a number");
    junk[2]["span_id"] = json!("");
    assert_eq!(spans_of(junk).len(), 5);
}

#[tokio::test]
async fn ac_0_4_3_detail_response_shape() {
    let mut rows = sample_trace();
    rows.reverse();
    let a = app_at(
        Fake::new(move |q| {
            Ok(if q.name == "trace_spans" {
                rows.clone()
            } else {
                sample_db(q)?
            })
        }),
        NOW,
    );
    let j = get(&a, &format!("{BASE}/traces/{TID}")).await.json();
    let t = &j["data"]["trace"];
    assert_eq!(t["trace_id"], TID);
    assert_eq!(
        (
            t["spans"].as_i64(),
            t["errors"].as_i64(),
            t["duration_us"].as_i64(),
            t["truncated"].clone()
        ),
        (Some(7), Some(1), Some(100_000), json!(false))
    );
    assert_eq!(
        t["services"],
        json!([
            "cart",
            "checkout",
            "frontend",
            "inventory",
            "payment",
            "postgres"
        ])
    );
    assert_eq!(t["start_ms"], T_NS / 1_000_000);
    assert_eq!(j["data"]["spans"][0]["span_id"], "a000000000000001");
    assert_eq!(j["meta"]["query"], "trace_spans");
    // More spans than the cap: truncated, and the extra row is not returned.
    let many: Vec<Value> = (0..traces::SPANS_MAX + 1)
        .map(|i| {
            span_row(
                &format!("{:016x}", i + 1),
                None,
                "s",
                "op",
                i as i64,
                i as i64 + 1,
                "STATUS_CODE_OK",
            )
        })
        .collect();
    let a = app_at(
        Fake::new(move |q| {
            Ok(if q.name == "trace_spans" {
                many.clone()
            } else {
                sample_db(q)?
            })
        }),
        NOW,
    );
    let j = get(&a, &format!("{BASE}/traces/{TID}")).await.json();
    assert_eq!(j["data"]["trace"]["truncated"], true);
    assert_eq!(j["data"]["trace"]["spans"], traces::SPANS_MAX);
}

#[tokio::test]
#[ignore = "needs GreptimeDB: make db-up && make test-db"]
async fn ac_0_4_3_detail_live() {
    let l = live().await;
    let by = l.by_trace();
    let (tid, spans) = by
        .iter()
        .max_by_key(|(k, v)| (v.len(), (*k).clone()))
        .unwrap();
    let w = Live::window();
    let r = get(&l.app, &format!("{BASE}/traces/{tid}?{w}")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    let rows = j["data"]["spans"].as_array().unwrap();
    assert_eq!(rows.len(), spans.len());
    let start = spans.iter().map(|s| s.start_ns).min().unwrap() as i64;
    let by_id: HashMap<String, &&synth::SpanRec> =
        spans.iter().map(|s| (hex(&s.span_id), s)).collect();
    let mut depth_of: HashMap<String, i64> = HashMap::new();
    for r in rows {
        let id = r["span_id"].as_str().unwrap();
        let s = by_id[id];
        assert_eq!(r["service"], s.service);
        assert_eq!(r["name"], s.name);
        assert_eq!(
            r["offset_us"].as_i64().unwrap(),
            (s.start_ns as i64 - start) / 1000,
            "{id} offset"
        );
        assert_eq!(
            r["duration_us"].as_i64().unwrap(),
            (s.end_ns - s.start_ns) as i64 / 1000,
            "{id} duration"
        );
        assert_eq!(r["error"].as_bool().unwrap(), s.error);
        assert_eq!(r["start_ns"].as_str().unwrap(), s.start_ns.to_string());
        let d = s.parent.map(|p| depth_of[&hex(&p)] + 1).unwrap_or(0);
        assert_eq!(
            r["depth"].as_i64().unwrap(),
            d,
            "{id} depth (parents come first)"
        );
        depth_of.insert(id.to_string(), d);
        // Flat attribute columns come back as attributes.
        assert_eq!(r["attributes"]["http.route"], s.name);
        assert_eq!(
            r["attributes"]["http.status_code"],
            if s.error { 500 } else { 200 }
        );
        assert_eq!(r["resource"]["service.version"], "1.4.2");
    }
    assert_eq!(rows[0]["depth"], 0);
    assert_eq!(
        j["data"]["trace"]["spans"].as_u64().unwrap() as usize,
        spans.len()
    );
    // Flame graph over the same trace.
    let f = get(&l.app, &format!("{BASE}/traces/{tid}/flame?{w}"))
        .await
        .json();
    let root = &f["data"]["root"];
    assert_eq!(
        root["name"],
        by_id.values().find(|s| s.parent.is_none()).unwrap().name
    );
    assert_eq!(
        root["total_us"].as_i64().unwrap(),
        j["data"]["trace"]["duration_us"].as_i64().unwrap()
    );
    fn count(n: &Value) -> u64 {
        n["count"].as_u64().unwrap()
            + n["children"]
                .as_array()
                .unwrap()
                .iter()
                .map(count)
                .sum::<u64>()
    }
    assert_eq!(
        count(root) as usize,
        spans.len(),
        "merging keeps every span counted"
    );
    // An unknown trace is a 404, a trace outside the window too.
    assert_eq!(
        get(&l.app, &format!("{BASE}/traces/{}?{w}", "f".repeat(32)))
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        get(&l.app, &format!("{BASE}/traces/{tid}?from=1000&to=2000"))
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    l.drop().await;
}

// ---- AC0.4.4 flame graph -----------------------------------------------------------------

#[test]
fn ac_0_4_4_flame_merges_paths() {
    let f = layout::flame(&spans_of(sample_trace()));
    assert_eq!(
        (
            f["name"].as_str(),
            f["service"].as_str(),
            f["count"].as_i64()
        ),
        (Some("GET /"), Some("frontend"), Some(1))
    );
    // Children are ordered by total time: checkout (50 ms) before cart (35 ms).
    let kids = f["children"].as_array().unwrap();
    assert_eq!(
        kids.iter()
            .map(|k| k["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["PlaceOrder", "GET /cart"]
    );
    // The two payment Charge spans under PlaceOrder are one node.
    let checkout = &kids[0];
    let sub = checkout["children"].as_array().unwrap();
    assert_eq!(sub.len(), 2, "Charge x2 merged, plus Reserve");
    let charge = &sub[0];
    assert_eq!(
        (
            charge["name"].as_str(),
            charge["service"].as_str(),
            charge["count"].as_i64(),
            charge["errors"].as_i64()
        ),
        (Some("Charge"), Some("payment"), Some(2), Some(1))
    );
    assert_eq!(sub[1]["name"], "Reserve");
    // Same operation under a different parent is a different node.
    let mut rows = sample_trace();
    rows.push(span_row(
        "a000000000000008",
        Some("a000000000000002"),
        "payment",
        "Charge",
        31,
        35,
        "STATUS_CODE_OK",
    )); // Charge under cart
    let f = layout::flame(&spans_of(rows));
    let cart = f["children"]
        .as_array()
        .unwrap()
        .iter()
        .find(|k| k["name"] == "GET /cart")
        .unwrap();
    assert!(
        cart["children"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["name"] == "Charge"),
        "a separate Charge node under cart"
    );
    let checkout = f["children"]
        .as_array()
        .unwrap()
        .iter()
        .find(|k| k["name"] == "PlaceOrder")
        .unwrap();
    assert_eq!(
        checkout["children"][0]["count"], 2,
        "and the one under checkout is unchanged"
    );
    // Several roots (a partial trace) hang off a synthetic (trace) root.
    let mut partial = sample_trace();
    partial.remove(0);
    let f = layout::flame(&spans_of(partial));
    assert_eq!(
        (f["name"].as_str(), f["service"].as_str()),
        (Some("(trace)"), Some(""))
    );
    assert_eq!(f["children"].as_array().unwrap().len(), 2);
    assert_eq!(
        f["total_us"], 90_000,
        "from the first start (5 ms) to the last end (95 ms)"
    );
}

#[tokio::test]
async fn ac_0_4_4_flame_self_total_times() {
    let f = layout::flame(&spans_of(sample_trace()));
    let find = |n: &Value, name: &str| -> Value {
        n["children"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == name)
            .unwrap()
            .clone()
    };
    assert_eq!(
        (f["total_us"].as_i64(), f["self_us"].as_i64()),
        (Some(100_000), Some(15_000)),
        "100 - (50 + 35)"
    );
    let checkout = find(&f, "PlaceOrder");
    assert_eq!(
        (checkout["total_us"].as_i64(), checkout["self_us"].as_i64()),
        (Some(50_000), Some(9_000)),
        "50 - (38 + 3)"
    );
    let charge = find(&checkout, "Charge");
    assert_eq!(
        (charge["total_us"].as_i64(), charge["self_us"].as_i64()),
        (Some(38_000), Some(38_000)),
        "20 + 18, a leaf"
    );
    let cart = find(&f, "GET /cart");
    assert_eq!(
        (cart["total_us"].as_i64(), cart["self_us"].as_i64()),
        (Some(35_000), Some(15_000)),
        "35 - 20"
    );
    assert_eq!(find(&cart, "SELECT carts")["total_us"], 20_000);
    // Self + children's total == total at every node (when children do not overlap).
    fn check(n: &Value) {
        let kids: i64 = n["children"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["total_us"].as_i64().unwrap())
            .sum();
        assert_eq!(
            n["self_us"].as_i64().unwrap() + kids,
            n["total_us"].as_i64().unwrap(),
            "{}",
            n["name"]
        );
        n["children"].as_array().unwrap().iter().for_each(check);
    }
    check(&f);
    // Overlapping (parallel) children can exceed the parent: self time is clamped at 0, never negative.
    let par = vec![
        span_row(
            "b000000000000001",
            None,
            "p",
            "parent",
            0,
            10,
            "STATUS_CODE_OK",
        ),
        span_row(
            "b000000000000002",
            Some("b000000000000001"),
            "c",
            "a",
            0,
            8,
            "STATUS_CODE_OK",
        ),
        span_row(
            "b000000000000003",
            Some("b000000000000001"),
            "c",
            "b",
            0,
            8,
            "STATUS_CODE_OK",
        ),
    ];
    let f = layout::flame(&spans_of(par));
    assert_eq!(
        (f["total_us"].as_i64(), f["self_us"].as_i64()),
        (Some(10_000), Some(0))
    );
    // Over HTTP.
    let a = app_at(
        Fake::new(|q| {
            Ok(if q.name == "trace_spans" {
                sample_trace()
            } else {
                sample_db(q)?
            })
        }),
        NOW,
    );
    let j = get(&a, &format!("{BASE}/traces/{TID}/flame")).await.json();
    assert_eq!(j["data"]["root"]["total_us"], 100_000);
    assert_eq!(j["data"]["truncated"], false);
}

// ---- AC0.4.5 span panel -----------------------------------------------------------------------

#[test]
fn ac_0_4_5_span_attributes_flat_columns() {
    let mut rows = sample_trace();
    rows[0]["span_attributes.db.statement"] = Value::Null; // a column this span did not set
    rows[0]["resource_attributes.k8s.pod.name"] = json!("frontend-7d9");
    rows[0]["span_attributes.retry"] = json!(true);
    let wf = layout::waterfall(&spans_of(rows));
    let r = &wf.rows[0];
    assert_eq!(
        r["attributes"],
        json!({"http.route": "GET /", "http.status_code": 200, "retry": true}),
        "prefix stripped, types kept, nulls omitted"
    );
    assert_eq!(
        r["resource"],
        json!({"service.version": "1.4.2", "k8s.pod.name": "frontend-7d9"})
    );
    // Dotted keys stay as one key; an error span carries its status code attribute.
    let err = wf.rows.iter().find(|r| r["error"] == true).unwrap();
    assert_eq!(err["attributes"]["http.status_code"], 500);
    // Core columns are not mistaken for attributes.
    assert!(
        r["attributes"].get("span_name").is_none() && r["attributes"].get("service_name").is_none()
    );
    // A span with no attribute columns at all has empty maps.
    let bare = vec![
        json!({"start_ns": "1000", "end_ns": "2000", "span_id": "d000000000000001", "span_name": "x", "service_name": "s"}),
    ];
    let wf = layout::waterfall(&spans_of(bare));
    assert_eq!(
        (
            wf.rows[0]["attributes"].clone(),
            wf.rows[0]["resource"].clone()
        ),
        (json!({}), json!({}))
    );
}

#[test]
fn ac_0_4_5_span_events_links() {
    let events = json!([{"name": "exception", "time": "2026-10-31T00:00:00Z", "attributes": {"exception.message": "upstream timeout"}}]);
    let links =
        json!([{"trace_id": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", "span_id": "e000000000000001"}]);
    let mut rows = sample_trace();
    rows[4]["span_events"] = events.clone();
    rows[4]["span_links"] = links.clone();
    let wf = layout::waterfall(&spans_of(rows));
    let r = wf
        .rows
        .iter()
        .find(|r| r["span_id"] == "a000000000000005")
        .unwrap();
    assert_eq!(r["events"], events);
    assert_eq!(r["links"], links);
    // Empty arrays and nulls are left out, so the panel only shows what exists.
    let plain = wf
        .rows
        .iter()
        .find(|r| r["span_id"] == "a000000000000001")
        .unwrap();
    assert!(plain.get("events").is_none() && plain.get("links").is_none());
    let mut rows = sample_trace();
    rows[0]["span_events"] = Value::Null;
    rows[0]["span_links"] = json!({});
    assert!(
        layout::waterfall(&spans_of(rows)).rows[0]
            .get("events")
            .is_none()
    );
    // The status message and scope travel with the span.
    let mut rows = sample_trace();
    rows[4]["span_status_message"] = json!("upstream timeout");
    let wf = layout::waterfall(&spans_of(rows));
    let r = wf
        .rows
        .iter()
        .find(|r| r["span_id"] == "a000000000000005")
        .unwrap();
    assert_eq!(
        (
            r["message"].as_str(),
            r["scope"].as_str(),
            r["kind"].as_str()
        ),
        (
            Some("upstream timeout"),
            Some("demo"),
            Some("SPAN_KIND_SERVER")
        )
    );
}

// ---- AC0.4.6 trace <-> logs ---------------------------------------------------------------------

#[tokio::test]
async fn ac_0_4_6_logs_by_trace() {
    let f = trace_fake();
    let a = app_at(f.clone(), NOW);
    let r = get(&a, &format!("{BASE}/traces/{TID}/logs?{W}")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["meta"]["query"], "trace_logs");
    assert_eq!(
        r.json()["data"][0]["trace_id"],
        "f1ebd7ef1a8ecefd2ce38517da7e7234"
    );
    let q = f
        .seen
        .lock()
        .unwrap()
        .iter()
        .find(|q| q.name == "trace_logs")
        .unwrap()
        .clone();
    assert_eq!(q.binds[0], Bind::Str(TID.into()));
    assert!(
        q.sql.contains("`trace_id` = ?")
            && !q.sql.contains(TID)
            && q.sql.contains("ORDER BY `timestamp` ASC LIMIT 500"),
        "{}",
        q.sql
    );
    assert!(!q.sql.contains("`span_id` = ?"));
    // One span of the trace.
    get(
        &a,
        &format!("{BASE}/traces/{TID}/logs?{W}&span=A000000000000005"),
    )
    .await;
    let q = f
        .seen
        .lock()
        .unwrap()
        .iter()
        .rev()
        .find(|q| q.name == "trace_logs")
        .unwrap()
        .clone();
    assert!(
        q.sql.contains("`span_id` = ?")
            && q.binds.last().unwrap() == &Bind::Str("a000000000000005".into()),
        "{}",
        q.sql
    );
    for bad in ["zz", "a000000000000005%27", "1234", "a0000000000000050"] {
        assert_eq!(
            get(&a, &format!("{BASE}/traces/{TID}/logs?span={bad}"))
                .await
                .status,
            StatusCode::BAD_REQUEST,
            "{bad}"
        );
    }
    // The limit is capped; with no window the last week is used.
    get(&a, &format!("{BASE}/traces/{TID}/logs?limit=999999")).await;
    assert!(
        f.seen
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .sql
            .contains("LIMIT 1000")
    );
    let q = f.seen.lock().unwrap().last().unwrap().clone();
    assert_eq!(
        q.binds[1],
        Bind::Str(grepui::query::ts_string(NOW - 7 * 86_400_000))
    );
}

#[tokio::test]
async fn ac_0_4_6_log_rows_link_trace() {
    // Log rows carry the ids the trace page needs, and the UI turns them into links.
    let a = app_at(trace_fake(), NOW);
    let rows = get(&a, &format!("{BASE}/logs?{W}")).await.json();
    let row = &rows["data"][0];
    assert_eq!(row["trace_id"].as_str().unwrap().len(), 32);
    assert_eq!(row["span_id"].as_str().unwrap().len(), 16);
    let q = grepui::query::logs::COLS;
    assert!(q.contains("`trace_id`") && q.contains("`span_id`"));
    // A log's trace id opens a trace; the trace list also finds it by `trace=<id>`.
    let fl = filter(&format!("trace={}", row["trace_id"].as_str().unwrap()));
    assert_eq!(fl.conds[0].col, TCol::Plain("trace_id"));
    let detail = get(
        &a,
        &format!("{BASE}/traces/{}", row["trace_id"].as_str().unwrap()),
    )
    .await;
    assert_eq!(
        detail.status,
        StatusCode::OK,
        "a log's trace_id is a valid trace id"
    );
    // UI wiring: the record detail links to the trace page, and the trace page embeds its logs.
    let rec = std::fs::read_to_string("web/static/js/components/recorddetail.js").unwrap();
    assert!(
        rec.contains("href('traces', r.trace_id)"),
        "log record links to its trace"
    );
    let page = std::fs::read_to_string("web/static/js/pages/trace-detail.js").unwrap();
    assert!(
        page.contains("traces/${") && page.contains("/logs"),
        "trace page loads its logs"
    );
}

#[tokio::test]
#[ignore = "needs GreptimeDB: make db-up && make test-db"]
async fn ac_0_4_6_trace_logs_live() {
    let l = live().await;
    let by = l.by_trace();
    let (tid, spans) = by
        .iter()
        .max_by_key(|(k, v)| (v.len(), (*k).clone()))
        .unwrap();
    let w = Live::window();
    let r = get(&l.app, &format!("{BASE}/traces/{tid}/logs?{w}")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let logs = r.json()["data"].as_array().unwrap().clone();
    assert_eq!(logs.len(), spans.len(), "one log per span");
    assert!(logs.iter().all(|x| x["trace_id"] == tid.as_str()));
    assert!(
        logs.windows(2)
            .all(|p| p[0]["ts_ns"].as_str().unwrap().parse::<u128>().unwrap()
                <= p[1]["ts_ns"].as_str().unwrap().parse::<u128>().unwrap()),
        "oldest first"
    );
    // One span: exactly its log, with the message the generator wrote.
    let s = spans.iter().find(|s| s.error).or(spans.first()).unwrap();
    let sid = hex(&s.span_id);
    let one = get(&l.app, &format!("{BASE}/traces/{tid}/logs?{w}&span={sid}"))
        .await
        .json();
    assert_eq!(one["data"].as_array().unwrap().len(), 1);
    assert_eq!(one["data"][0]["span_id"], sid);
    assert!(
        one["data"][0]["body"]
            .as_str()
            .unwrap()
            .starts_with(&s.name)
    );
    // The reverse pivot: the log search finds the same lines by trace id.
    let via = get(
        &l.app,
        &format!("{BASE}/logs?{w}&q=trace%3D{tid}&limit=100"),
    )
    .await
    .json();
    assert_eq!(via["data"].as_array().unwrap().len(), spans.len());
    // Another trace's logs do not leak in.
    let other = by.keys().find(|k| *k != tid).unwrap();
    let ol = get(&l.app, &format!("{BASE}/traces/{other}/logs?{w}"))
        .await
        .json();
    assert!(
        ol["data"]
            .as_array()
            .unwrap()
            .iter()
            .all(|x| x["trace_id"] == other.as_str())
    );
    l.drop().await;
}
