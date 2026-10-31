//! AC0.2.1 – AC0.2.8: log search, grammar, histogram, fields, record and search around,
//! SQL mode, fulltext, export.
mod common;
use axum::http::StatusCode;
use common::*;
use grepui::{
    api::logs::{export_csv, export_ndjson},
    config::Config,
    query::{
        Bind, MAX_LIMIT, SqlxReader, Window,
        filter::{FtMode, LogFilter, Op, parse},
        logs, sqlmode,
    },
};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};

const BASE: &str = "/api/v1/orgs/default";
const W: &str = "from=1000&to=2000";

fn win() -> Window {
    Window {
        from_ms: 1_760_000_000_000,
        to_ms: 1_760_000_600_000,
        limit: 50,
    }
}

fn filter(q: &str) -> LogFilter {
    LogFilter::new(Some(q), None, FtMode::Term).unwrap()
}

// ---- AC0.2.1 filters -> bound SQL -------------------------------------------

#[test]
fn ac_0_2_1_filters_build_bound_sql() {
    let f = LogFilter::new(
        Some("level=error scope~cart service.name!=x timeout"),
        Some("warn"),
        FtMode::Term,
    )
    .unwrap();
    let q = logs::search("public", "opentelemetry_logs", &f, win(), 100).unwrap();
    // Window first, then level, conditions in order, then free text.
    let binds: Vec<String> = q.binds.iter().map(Bind::show).collect();
    assert_eq!(binds[0], "2025-10-09 08:53:20.000");
    assert_eq!(binds[2], "WARN");
    assert!(binds.contains(&"ERROR".to_string()) || binds.contains(&"error".to_string()));
    assert!(binds.contains(&"%cart%".to_string()));
    assert_eq!(binds.last().unwrap(), "timeout");
    for v in ["error", "cart", "timeout", "WARN", "2025-"] {
        assert!(!q.sql.contains(v), "{v} leaked into SQL text: {}", q.sql);
    }
    assert_eq!(q.sql.matches('?').count(), q.binds.len());
    assert!(
        q.sql
            .contains("ORDER BY `timestamp` DESC LIMIT 50 OFFSET 100")
    );
    assert!(q.sql.contains("matches_term(`body`, ?)"));
    // Offsets are clamped.
    let q = logs::search("public", "t", &f, win(), 10_000_000).unwrap();
    assert!(
        q.sql.ends_with(&format!("OFFSET {}", logs::MAX_OFFSET)),
        "{}",
        q.sql
    );
}

#[test]
fn ac_0_2_1_attribute_keys_bound() {
    let q = logs::search("public", "t", &filter("http.method=GET"), win(), 0).unwrap();
    assert!(
        q.sql.contains("json_get_string(`log_attributes`, ?) = ?"),
        "{}",
        q.sql
    );
    assert!(q.binds.contains(&Bind::Str("[\"http.method\"]".into())));
    assert!(!q.sql.contains("http.method"));

    // A hostile key stays inside a bound JSON path, escaped; it never reaches SQL text.
    // (The filter grammar refuses quotes in keys; the top-values field path accepts any key.)
    let evil = r#"x") OR 1=1 --"#;
    let col = logs::top_value_column(&format!("log_attributes.{evil}")).unwrap();
    let q = logs::top_values("public", "t", &col, &filter(""), win(), 10).unwrap();
    assert!(
        !q.sql.contains("OR 1=1") && !q.sql.contains("--") && !q.sql.contains(evil),
        "{}",
        q.sql
    );
    assert_eq!(q.binds[0], Bind::Str(r#"["x\") OR 1=1 --"]"#.into()));
    assert!(
        logs::search(
            "public",
            "t",
            &filter(&format!("resource.{evil}=v")),
            win(),
            0
        )
        .unwrap()
        .sql
        .matches("--")
        .count()
            == 0
    );
    assert_eq!(logs::json_path(r#"a"b\c"#), r#"["a\"b\\c"]"#);

    // Ne and Like shapes.
    let q = logs::search("public", "t", &filter("a!=b c~d"), win(), 0).unwrap();
    assert!(
        q.sql
            .contains("COALESCE(json_get_string(`log_attributes`, ?), '') != ?"),
        "{}",
        q.sql
    );
    assert!(
        q.sql
            .contains("json_get_string(`log_attributes`, ?) LIKE ?")
    );
}

#[tokio::test]
async fn ac_0_2_1_search_through_http_binds_everything() {
    let f = Fake::new(sample_db);
    let a = app(f.clone());
    let r = get(
        &a,
        &format!("{BASE}/logs?{W}&level=error&q=http.method%3DGET%20slow&limit=9&offset=5"),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["meta"]["query"], "logs_search");
    assert_eq!(r.json()["data"][0]["ts_ns"], "1793399269857388008");
    let q = f
        .seen
        .lock()
        .unwrap()
        .iter()
        .find(|q| q.name == "logs_search")
        .unwrap()
        .clone();
    assert!(q.sql.contains("LIMIT 9 OFFSET 5"), "{}", q.sql);
    assert!(q.binds.contains(&Bind::Str("ERROR".into())));
    // Window, level and bad filters are validated before the database is touched.
    assert_eq!(
        get(&a, &format!("{BASE}/logs?q=a%3D")).await.status,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        get(&a, &format!("{BASE}/logs?{W}&level=er%27ror"))
            .await
            .status,
        StatusCode::BAD_REQUEST
    );
}

fn live_reader() -> Option<(Arc<SqlxReader>, String)> {
    let url = std::env::var("GREPUI_TEST_DB").ok()?;
    Some((
        Arc::new(SqlxReader::lazy(&url, Duration::from_secs(20)).unwrap()),
        url,
    ))
}

fn live_app() -> axum::Router {
    let (r, url) = live_reader().expect("set GREPUI_TEST_DB=mysql://root@127.0.0.1:4002/public");
    app_with(
        Config {
            greptime_mysql: url,
            ..Config::default()
        },
        r,
    )
}

fn live_window() -> String {
    let now = chrono::Utc::now().timestamp_millis();
    format!("from={}&to={}", now - 30 * 86_400_000, now + 3_600_000)
}

#[tokio::test]
#[ignore = "needs GreptimeDB: make db-up && make seed && make test-db"]
async fn ac_0_2_1_search_live() {
    let a = live_app();
    let w = live_window();
    let all = get(&a, &format!("{BASE}/logs?{w}&limit=200")).await;
    assert_eq!(all.status, StatusCode::OK, "{}", all.text());
    let rows = all.json()["data"].as_array().unwrap().clone();
    assert!(rows.len() > 20, "seed the demo data first: make seed");
    let r0 = &rows[0];
    assert!(
        r0["ts_ns"]
            .as_str()
            .unwrap()
            .bytes()
            .all(|b| b.is_ascii_digit()),
        "ns timestamps are strings: {r0}"
    );
    assert!(r0["timestamp"].is_i64() && r0["log_attributes"].is_object());

    // level
    let errs = get(&a, &format!("{BASE}/logs?{w}&level=error&limit=200"))
        .await
        .json();
    let errs = errs["data"].as_array().unwrap();
    assert!(!errs.is_empty() && errs.iter().all(|r| r["severity_text"] == "ERROR"));
    // equality on a dotted attribute key
    let post = get(
        &a,
        &format!("{BASE}/logs?{w}&q=http.method%3DPOST&limit=200"),
    )
    .await
    .json();
    let post = post["data"].as_array().unwrap();
    assert!(
        !post.is_empty()
            && post
                .iter()
                .all(|r| r["log_attributes"]["http.method"] == "POST"),
        "{post:?}"
    );
    // != excludes them
    let not_post = get(
        &a,
        &format!("{BASE}/logs?{w}&q=http.method!%3DPOST&limit=200"),
    )
    .await
    .json();
    assert!(
        not_post["data"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["log_attributes"]["http.method"] != "POST")
    );
    // ~ substring on a resource attribute
    let svc = get(
        &a,
        &format!("{BASE}/logs?{w}&q=resource.service.name~pay&limit=200"),
    )
    .await
    .json();
    let svc = svc["data"].as_array().unwrap();
    assert!(
        !svc.is_empty()
            && svc.iter().all(|r| r["resource_attributes"]["service.name"]
                .as_str()
                .unwrap()
                .contains("pay"))
    );
    // paging: offset moves the window and keeps order
    let p1 = get(&a, &format!("{BASE}/logs?{w}&limit=5&offset=0"))
        .await
        .json();
    let p2 = get(&a, &format!("{BASE}/logs?{w}&limit=5&offset=5"))
        .await
        .json();
    assert_ne!(p1["data"][0]["ts_ns"], p2["data"][0]["ts_ns"]);
    assert!(p1["data"][4]["ts_ns"].as_str().unwrap() >= p2["data"][0]["ts_ns"].as_str().unwrap());
    // injection attempts are just values
    let evil = get(
        &a,
        &format!("{BASE}/logs?{w}&q=http.method%3D%22x%27%20OR%20%271%27%3D%271%22"),
    )
    .await;
    assert_eq!(evil.status, StatusCode::OK);
    assert_eq!(evil.json()["data"].as_array().unwrap().len(), 0);
}

// ---- AC0.2.2 grammar ---------------------------------------------------------

#[test]
fn ac_0_2_2_parse_conditions() {
    let p = parse("service=checkout status!=ok http.route~cart a-b.c=1").unwrap();
    let c: Vec<_> = p
        .conds
        .iter()
        .map(|c| (c.key.as_str(), c.op, c.value.as_str()))
        .collect();
    assert_eq!(
        c,
        [
            ("service", Op::Eq, "checkout"),
            ("status", Op::Ne, "ok"),
            ("http.route", Op::Like, "cart"),
            ("a-b.c", Op::Eq, "1")
        ]
    );
    assert!(p.terms.is_empty());
    assert_eq!(parse("").unwrap(), Default::default());
    assert_eq!(parse("   ").unwrap(), Default::default());
    // Deterministic.
    assert_eq!(parse("a=b c~d x").unwrap(), parse("a=b c~d x").unwrap());
}

#[test]
fn ac_0_2_2_parse_quotes_and_terms() {
    let p = parse(r#""slow query" key="a b" 'x y' latency>5 plain k='it\'s'"#).unwrap();
    assert_eq!(p.terms, ["slow query", "x y", "latency>5", "plain"]);
    assert_eq!(
        (p.conds[0].key.as_str(), p.conds[0].value.as_str()),
        ("key", "a b")
    );
    assert_eq!(p.conds[1].value, "it's");
    for bad in ["key=", "key!=", r#"a="open"#, "'open", "k~"] {
        assert!(parse(bad).is_err(), "{bad}");
    }
    let many = (0..21)
        .map(|i| format!("a{i}=1"))
        .collect::<Vec<_>>()
        .join(" ");
    assert!(parse(&many).is_err());
    assert!(parse(&"a".repeat(201)).is_err());
    assert!(parse(&format!("k={}", "v".repeat(201))).is_err());
    assert!(parse(&"a=1 ".repeat(20)).is_ok());
}

// ---- AC0.2.3 histogram -------------------------------------------------------

#[test]
fn ac_0_2_3_bucket_choice() {
    const M: i64 = 60_000;
    for (window, want) in [
        (15 * M, 10_000),
        (60 * M, 60_000),
        (24 * 60 * M, 1_800_000),
        (7 * 24 * 60 * M, 10_800_000),
        (31 * 24 * 60 * M, 43_200_000),
    ] {
        let (ms, interval) = logs::bucket_for(window);
        assert_eq!(ms, want, "window {window}");
        assert!(
            logs::BUCKETS
                .iter()
                .any(|(m, i)| *m == ms && *i == interval)
        );
        assert!(window / ms <= 90);
    }
    // The bucket size in SQL is always one of the fixed literals (no user-controlled number).
    let q = logs::histogram("public", "t", &filter("level=x"), win()).unwrap();
    assert!(
        logs::BUCKETS.iter().any(|(ms, _)| q
            .sql
            .contains(&format!("/ {}) * {ms} AS bucket_ms", ms * 1_000_000))),
        "{}",
        q.sql
    );
    assert!(!q.sql.contains("INTERVAL"), "{}", q.sql);
    assert!(q.sql.contains("GROUP BY 1 ORDER BY 1"));
}

#[tokio::test]
async fn ac_0_2_3_histogram_response_totals() {
    let a = app(Fake::new(sample_db));
    let j = get(&a, &format!("{BASE}/logs/histogram?from=0&to=3600000"))
        .await
        .json();
    assert_eq!(j["data"]["bucket_ms"], 60_000);
    assert_eq!(j["data"]["total"], 239);
    assert_eq!(
        j["data"]["buckets"][1],
        json!({"t": 1_793_399_400_000i64, "count": 194})
    );
    assert_eq!(j["meta"]["query"], "logs_histogram");
    assert_eq!(
        get(&a, &format!("{BASE}/logs/histogram")).await.status,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
#[ignore = "needs GreptimeDB: make db-up && make seed && make test-db"]
async fn ac_0_2_3_histogram_sums_live() {
    let (reader, _) = live_reader().expect("GREPUI_TEST_DB");
    let a = live_app();
    let now = chrono::Utc::now().timestamp_millis();
    let (from, to) = (now - 30 * 86_400_000, now + 3_600_000);
    let h = get(&a, &format!("{BASE}/logs/histogram?from={from}&to={to}"))
        .await
        .json();
    let total = h["data"]["total"].as_i64().unwrap();
    let sum: i64 = h["data"]["buckets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["count"].as_i64().unwrap())
        .sum();
    assert_eq!(sum, total);
    // Direct count with the same window must agree.
    let q = grepui::query::Q::new(
        "t",
        "SELECT CAST(count(*) AS BIGINT) AS n FROM `public`.`opentelemetry_logs` WHERE `timestamp` >= ? AND `timestamp` < ?",
        vec![
            Bind::Str(grepui::query::ts_string(from)),
            Bind::Str(grepui::query::ts_string(to)),
        ],
    );
    let n = grepui::query::Reader::fetch(reader.as_ref(), &q)
        .await
        .unwrap()[0]["n"]
        .as_i64()
        .unwrap();
    assert_eq!(total, n);
    assert!(total > 20);
    // Buckets are aligned to the bucket size, and a filter can only shrink the total.
    let step = h["data"]["bucket_ms"].as_i64().unwrap();
    assert!(
        h["data"]["buckets"]
            .as_array()
            .unwrap()
            .iter()
            .all(|b| b["t"].as_i64().unwrap() % step == 0)
    );
    let e = get(
        &a,
        &format!("{BASE}/logs/histogram?from={from}&to={to}&level=error"),
    )
    .await
    .json();
    let et = e["data"]["total"].as_i64().unwrap();
    assert!(et > 0 && et < total);
}

// ---- AC0.2.4 fields ----------------------------------------------------------

#[tokio::test]
async fn ac_0_2_4_fields_with_types() {
    let a = app(Fake::new(sample_db));
    let j = get(&a, &format!("{BASE}/logs/fields")).await.json();
    let d = j["data"].as_array().unwrap();
    let f = |n: &str| d.iter().find(|c| c["name"] == n).unwrap().clone();
    assert_eq!(f("timestamp")["role"], "time_index");
    assert_eq!(f("timestamp")["type"], "timestamp(9)");
    assert_eq!(f("service_name")["role"], "tag");
    assert_eq!(f("body")["type"], "string");
    let empty = app(Fake::new(|_| Ok(vec![])));
    assert_eq!(
        get(&empty, &format!("{BASE}/logs/fields")).await.status,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn ac_0_2_4_quick_mode_subset() {
    let a = app(Fake::new(sample_db));
    let j = get(&a, &format!("{BASE}/logs/fields")).await.json();
    let quick: Vec<&str> = j["data"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["quick"] == true)
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert_eq!(quick, ["timestamp", "body"]);
    assert!(
        logs::QUICK_FIELDS.contains(&"severity_text")
            && logs::QUICK_FIELDS.iter().all(|f| !f.contains("attributes"))
    );
}

#[tokio::test]
async fn ac_0_2_4_top_values_allowlist() {
    let f = Fake::new(sample_db);
    let a = app(f.clone());
    for ok_field in [
        "severity_text",
        "scope_name",
        "resource_attributes.service.name",
        "log_attributes.http.method",
    ] {
        let r = get(&a, &format!("{BASE}/logs/fields/{ok_field}/values?{W}")).await;
        assert_eq!(r.status, StatusCode::OK, "{ok_field}: {}", r.text());
        assert_eq!(r.json()["data"][0], json!({"value": "INFO", "count": 90}));
    }
    let before = f.seen_names().len();
    for bad in [
        "body",
        "trace_id",
        "timestamp",
        "log_attributes.",
        "resource_attributes.",
        "x%27y",
        "scope_attributes.k",
    ] {
        let r = get(&a, &format!("{BASE}/logs/fields/{bad}/values?{W}")).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{bad}");
    }
    assert_eq!(
        f.seen_names().len(),
        before,
        "rejected fields must not reach the database"
    );
    // The limit is capped and the attribute key is a bound path.
    get(
        &a,
        &format!("{BASE}/logs/fields/log_attributes.http.method/values?{W}&limit=9999"),
    )
    .await;
    let seen = f.seen.lock().unwrap();
    let q = seen
        .iter()
        .rev()
        .find(|q| q.name == "logs_top_values")
        .unwrap();
    assert!(
        q.sql.contains(&format!("LIMIT {}", logs::TOP_VALUES_MAX)),
        "{}",
        q.sql
    );
    assert_eq!(q.binds[0], Bind::Str("[\"http.method\"]".into()));
}

// ---- AC0.2.5 record and search around ---------------------------------------

#[tokio::test]
async fn ac_0_2_5_source_detail_by_key() {
    let f = Fake::new(sample_db);
    let a = app(f.clone());
    let r = get(&a, &format!("{BASE}/logs/record?ts_ns=1793399269857388008")).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json()["data"][0]["ts_ns"], "1793399269857388008");
    let q = f
        .seen
        .lock()
        .unwrap()
        .iter()
        .find(|q| q.name == "logs_record")
        .unwrap()
        .clone();
    assert!(q.binds.contains(&Bind::Int(1_793_399_269_857_388_008)));
    assert_eq!(
        q.binds[0],
        Bind::Str(grepui::query::ts_string(1_793_399_269_856)),
        "the window is derived from the key"
    );
    assert!(q.sql.contains("LIMIT 10"));
    for bad in [
        "",
        "abc",
        "12x",
        "1.5",
        "-1",
        "99999999999999999999",
        "1%27%3B",
    ] {
        assert_eq!(
            get(&a, &format!("{BASE}/logs/record?ts_ns={bad}"))
                .await
                .status,
            StatusCode::BAD_REQUEST,
            "{bad}"
        );
    }
    assert_eq!(
        get(&a, &format!("{BASE}/logs/record")).await.status,
        StatusCode::BAD_REQUEST
    );
    let none = app(Fake::new(|q| {
        Ok(if q.name == "logs_record" {
            vec![]
        } else {
            sample_db(q)?
        })
    }));
    assert_eq!(
        get(&none, &format!("{BASE}/logs/record?ts_ns=5"))
            .await
            .status,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn ac_0_2_5_search_around_window() {
    let f = Fake::new(sample_db);
    let a = app(f.clone());
    // Filters in the query string are ignored: search around never applies them.
    let r = get(
        &a,
        &format!("{BASE}/logs/around?ts_ns=1793399269857388008&n=10&q=level%3Derror&level=ERROR"),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["data"]["pivot_ns"], "1793399269857388008");
    let q = f
        .seen
        .lock()
        .unwrap()
        .iter()
        .find(|q| q.name == "logs_around")
        .unwrap()
        .clone();
    assert!(
        q.sql.contains("UNION ALL")
            && q.sql.contains("LIMIT 5")
            && !q.sql.contains("severity_text` ="),
        "{}",
        q.sql
    );
    assert_eq!(q.binds.len(), 4);
    assert_eq!(q.binds[1], Bind::Int(1_793_399_269_857_388_008));
    // n is clamped to 2..=200 and split before/after.
    let q = logs::around("public", "t", 5_000_000_000, 100, 100).unwrap();
    assert!(q.sql.matches("LIMIT 100").count() == 2);
    get(&a, &format!("{BASE}/logs/around?ts_ns=5&n=100000")).await;
    get(&a, &format!("{BASE}/logs/around?ts_ns=5&n=0")).await;
    let around: Vec<_> = f
        .seen
        .lock()
        .unwrap()
        .iter()
        .filter(|q| q.name == "logs_around")
        .cloned()
        .collect();
    assert_eq!(around.len(), 3);
    // n=100000 is clamped to 200 (100 + 100); n=0 is raised to 2 (1 + 1).
    assert_eq!(
        around[1].sql.matches("LIMIT 100)").count(),
        2,
        "{}",
        around[1].sql
    );
    assert_eq!(
        around[2].sql.matches("LIMIT 1)").count(),
        2,
        "{}",
        around[2].sql
    );
    assert_eq!(
        get(&a, &format!("{BASE}/logs/around")).await.status,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
#[ignore = "needs GreptimeDB: make db-up && make seed && make test-db"]
async fn ac_0_2_5_search_around_live() {
    let a = live_app();
    let w = live_window();
    let list = get(&a, &format!("{BASE}/logs?{w}&limit=40")).await.json();
    let pivot = list["data"][20].clone();
    let ns = pivot["ts_ns"].as_str().unwrap();
    let rec = get(&a, &format!("{BASE}/logs/record?ts_ns={ns}")).await;
    assert_eq!(rec.status, StatusCode::OK, "{}", rec.text());
    assert!(
        rec.json()["data"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["body"] == pivot["body"])
    );

    let r = get(&a, &format!("{BASE}/logs/around?ts_ns={ns}&n=10")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let rows = r.json()["data"]["rows"].as_array().unwrap().clone();
    assert_eq!(rows.len(), 10);
    let tss: Vec<u128> = rows
        .iter()
        .map(|r| r["ts_ns"].as_str().unwrap().parse().unwrap())
        .collect();
    assert!(
        tss.windows(2).all(|w| w[0] <= w[1]),
        "ascending storage order"
    );
    let p: u128 = ns.parse().unwrap();
    assert_eq!(tss.iter().filter(|t| **t < p).count(), 5);
    assert!(tss.iter().filter(|t| **t >= p).count() == 5 && tss.contains(&p));
}

// ---- AC0.2.6 SQL mode --------------------------------------------------------

const DB: &str = "public";

#[test]
fn ac_0_2_6_sql_mode_accepts_reads() {
    for ok in [
        "SELECT 1",
        "select * from opentelemetry_logs where body like '%kill%' limit 5", // 'kill' inside a string is fine
        "SELECT * FROM public.opentelemetry_logs LIMIT 3",
        "SELECT * FROM greptime.public.opentelemetry_logs LIMIT 3",
        "WITH e AS (SELECT * FROM opentelemetry_logs WHERE severity_text = 'ERROR') SELECT count(*) FROM e",
        "SELECT a FROM t1 UNION ALL SELECT a FROM t2",
        "SELECT * FROM (SELECT 1 AS x) s",
        "SELECT 1;",
        "SHOW TABLES",
        "SHOW CREATE TABLE opentelemetry_logs",
        "SHOW COLUMNS FROM opentelemetry_logs",
        "DESC TABLE opentelemetry_logs",
        "EXPLAIN SELECT * FROM opentelemetry_logs",
        "SHOW TABLES FROM public",
        "SHOW TABLES IN public",
        "SHOW CREATE TABLE public.opentelemetry_logs",
        "SHOW COLUMNS FROM opentelemetry_logs",
        "DESC public.opentelemetry_logs",
    ] {
        assert_eq!(sqlmode::check(ok, DB), Ok(()), "{ok}");
    }
    let m = sqlmode::expand_macros(
        "SELECT * FROM t WHERE ts >= $__from AND ts < $__to",
        1_760_000_000_000,
        1_760_000_060_000,
    );
    assert!(
        m.contains("'2025-10-09 08:53:20.000'")
            && m.contains("'2025-10-09 08:54:20.000'")
            && !m.contains("$__")
    );
    assert_eq!(sqlmode::check(&m, DB), Ok(()));
}

#[test]
fn ac_0_2_6_sql_mode_rejects_writes() {
    for bad in [
        "INSERT INTO t VALUES (1)",
        "UPDATE t SET a = 1",
        "DELETE FROM t",
        "DROP TABLE t",
        "CREATE TABLE x (a INT)",
        "ALTER TABLE t ADD COLUMN c INT",
        "TRUNCATE TABLE t",
        "COPY t TO '/tmp/x'",
        "ADMIN flush_table('t')",
        "SELECT flush_table('t')",
        "SELECT * FROM t INTO OUTFILE '/tmp/x'",
        "SELECT * FROM t FOR UPDATE",
        "SET autocommit = 0",
        "USE other",
        "KILL 1",
        "SHOW DATABASES",
        "SELECT * FROM other_db.t",
        "SELECT * FROM information_schema.tables",
        "SELECT * FROM other.public.t",
        "SELECT * FROM t WHERE x IN (SELECT y FROM secret_db.u)",
        "WITH x AS (SELECT 1) INSERT INTO t SELECT * FROM x",
        "EXPLAIN DELETE FROM t",
        "TQL DELETE x",
        // TQL is refused outright: `__schema__` / `__database__` select another database.
        "TQL EVAL (0, 10, '1s') up",
        "tql explain (0, 10, '1s') rate(x[5m])",
        "TQL EVAL (0, 10, '1s') m{__schema__=\"other\"}",
        "  tql eval (0,1,'1s') m{__database__=\"other\"}",
        "tql eval (0,1,'1s') {\"__sch\\u0065ma__\"=\"other\"}",
        // Other-database SHOW/DESC variants (found by the security review).
        "SHOW TABLES FROM other",
        "SHOW TABLES IN other",
        "SHOW FULL TABLES FROM other",
        "SHOW TABLES FROM greptime.other",
        "SHOW COLUMNS FROM t FROM other",
        "SHOW COLUMNS FROM other.t",
        "SHOW CREATE TABLE other.t",
        "SHOW CREATE TABLE greptime.other.t",
        "SHOW CREATE VIEW other.v",
        "DESC other.t",
        "DESC TABLE other.t",
        "DESCRIBE other.t",
        "SHOW CREATE TABLE information_schema.tables",
        "",
        "   ",
        "not sql at all",
    ] {
        assert!(sqlmode::check(bad, DB).is_err(), "must reject: {bad}");
    }
    assert!(
        sqlmode::check(
            &format!("SELECT '{}'", "x".repeat(sqlmode::MAX_SQL_LEN)),
            DB
        )
        .is_err(),
        "length cap"
    );
}

#[test]
fn ac_0_2_6_sql_mode_rejects_multistatement() {
    for bad in [
        "SELECT 1; DROP TABLE t",
        "SELECT 1; SELECT 2",
        "SELECT 1;;SELECT 2",
        "SELECT 1; -- x\nDELETE FROM t",
        "TQL EVAL (0,1,'1s') up; DROP TABLE t",
    ] {
        assert!(sqlmode::check(bad, DB).is_err(), "must reject: {bad}");
    }
    // Comments cannot hide a second statement or change what is parsed.
    assert!(sqlmode::check("SELECT 1 /* ; DROP TABLE t */", DB).is_ok());
    assert!(sqlmode::check("/* SELECT */ DELETE FROM t", DB).is_err());
    assert!(sqlmode::check("SELECT 1 -- ; DROP TABLE t", DB).is_ok());
}

#[tokio::test]
async fn ac_0_2_6_sql_mode_caps_applied() {
    let big = Fake::new(|q| {
        Ok(if q.name == "sql_mode" {
            (0..5000).map(|i| json!({"i": i})).collect()
        } else {
            vec![]
        })
    });
    let a = app(big);
    let enc = "SELECT%20i%20FROM%20t";
    // Window is required.
    assert_eq!(
        get(&a, &format!("{BASE}/sql?sql={enc}")).await.status,
        StatusCode::BAD_REQUEST
    );
    // Rows are truncated to the limit and the response says so.
    let j = get(&a, &format!("{BASE}/sql?sql={enc}&{W}&limit=7"))
        .await
        .json();
    assert_eq!(j["data"]["rows"].as_array().unwrap().len(), 7);
    assert_eq!(j["data"]["truncated"], true);
    assert_eq!(j["meta"]["query"], "sql_mode");
    let j = get(&a, &format!("{BASE}/sql?sql={enc}&{W}&limit=999999"))
        .await
        .json();
    assert_eq!(
        j["data"]["rows"].as_array().unwrap().len(),
        MAX_LIMIT as usize
    );
    // Rejections are 400 with a reason, and nothing reaches the database.
    let f = Fake::new(sample_db);
    let a = app(f.clone());
    for bad in [
        "DROP%20TABLE%20t",
        "SELECT%201%3B%20SELECT%202",
        "SELECT%20*%20FROM%20other.t",
        "SHOW%20DATABASES",
        "SHOW%20TABLES%20FROM%20other",
        "SHOW%20CREATE%20TABLE%20other.t",
        "TQL%20EVAL%20(0%2C1%2C%271s%27)%20m",
    ] {
        let r = get(&a, &format!("{BASE}/sql?sql={bad}&{W}")).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{bad}");
        assert!(r.json()["error"].is_string());
    }
    assert!(f.seen_names().is_empty());
    // Column order follows the query, not the alphabet.
    let j = get(&a, &format!("{BASE}/sql?sql=SELECT%20b%2C%20a&{W}"))
        .await
        .json();
    assert_eq!(j["data"]["columns"], json!(["b", "a"]));
    // Timeout is a 504.
    let slow = app(Fake::new(|_| Err(grepui::error::ReadError::Timeout)));
    assert_eq!(
        get(&slow, &format!("{BASE}/sql?sql={enc}&{W}"))
            .await
            .status,
        StatusCode::GATEWAY_TIMEOUT
    );
}

// ---- AC0.2.7 fulltext --------------------------------------------------------

fn ddl_app(ddl: &'static str, f: &Arc<Fake>) -> axum::Router {
    let f2 = f.clone();
    let _ = f2;
    app(Fake::new(move |q| {
        Ok(if q.name == "logs_ddl" {
            vec![json!({"Create Table": ddl})]
        } else {
            sample_db(q)?
        })
    }))
}

#[tokio::test]
async fn ac_0_2_7_fulltext_term_match() {
    let f = Fake::new(sample_db);
    let a = app(f.clone());
    let j = get(&a, &format!("{BASE}/logs?{W}&q=timeout")).await.json();
    assert!(
        j["meta"].get("note").is_none(),
        "no note when the index is used: {}",
        j["meta"]
    );
    let q = f
        .seen
        .lock()
        .unwrap()
        .iter()
        .find(|q| q.name == "logs_search")
        .unwrap()
        .clone();
    assert!(
        q.sql.contains("matches_term(`body`, ?)") && !q.sql.contains("LIKE"),
        "{}",
        q.sql
    );
    assert_eq!(q.binds.last().unwrap(), &Bind::Str("timeout".into()));
    assert!(logs::has_fulltext(FULLTEXT_DDL) && !logs::has_fulltext(PLAIN_DDL));
    // HTTP quotes identifiers with double quotes; the MySQL protocol uses backticks.
    assert!(logs::has_fulltext(
        "CREATE TABLE \"l\" (\n  \"body\" STRING NULL FULLTEXT INDEX WITH(a = 'b'),\n)"
    ));
    assert!(!logs::has_fulltext(
        "CREATE TABLE `l` (\n  `body_x` STRING NULL FULLTEXT INDEX,\n  `body` STRING NULL\n)"
    ));
    // A filter-only search does not even ask about the index.
    let f2 = Fake::new(sample_db);
    get(&app(f2.clone()), &format!("{BASE}/logs?{W}&level=error")).await;
    assert!(!f2.seen_names().contains(&"logs_ddl"));
}

#[tokio::test]
async fn ac_0_2_7_like_fallback_flagged() {
    let f = Fake::new(sample_db);
    let a = ddl_app(PLAIN_DDL, &f);
    let j = get(&a, &format!("{BASE}/logs?{W}&q=50%25_x%5C"))
        .await
        .json();
    assert!(
        j["meta"]["note"].as_str().unwrap().contains("LIKE"),
        "{}",
        j["meta"]
    );
    assert!(j["meta"]["sql"].as_str().unwrap().contains("`body` LIKE ?"));
    // LIKE metacharacters in the user's text are escaped (GreptimeDB supports only the backslash escape).
    assert_eq!(
        j["meta"]["binds"].as_array().unwrap().last().unwrap(),
        r"%50\%\_x\\%"
    );
    assert_eq!(
        grepui::query::filter::like_escape(r"a%b_c\d"),
        r"a\%b\_c\\d"
    );
    // The histogram and top values flag it too.
    let h = get(&a, &format!("{BASE}/logs/histogram?{W}&q=x"))
        .await
        .json();
    assert!(h["meta"]["note"].is_string());
}

#[tokio::test]
async fn ac_0_2_7_index_probe_is_cached() {
    let f = Fake::new(sample_db);
    let a = app(f.clone());
    for _ in 0..3 {
        get(&a, &format!("{BASE}/logs?{W}&q=timeout")).await;
    }
    assert_eq!(
        f.seen_names().iter().filter(|n| **n == "logs_ddl").count(),
        1
    );
}

#[tokio::test]
#[ignore = "needs GreptimeDB: make db-up && make seed && make test-db"]
async fn ac_0_2_7_fulltext_live() {
    let a = live_app();
    let w = live_window();
    let j = get(&a, &format!("{BASE}/logs?{w}&q=failed&limit=100"))
        .await
        .json();
    assert!(
        j["meta"].get("note").is_none(),
        "OTLP logs table has a fulltext index on body: {}",
        j["meta"]
    );
    assert!(j["meta"]["sql"].as_str().unwrap().contains("matches_term"));
    let rows = j["data"].as_array().unwrap();
    assert!(
        !rows.is_empty()
            && rows.iter().all(|r| r["body"]
                .as_str()
                .unwrap()
                .to_lowercase()
                .contains("failed"))
    );
    // Two terms are ANDed.
    let both = get(&a, &format!("{BASE}/logs?{w}&q=failed%20timeout&limit=100"))
        .await
        .json();
    assert!(
        both["data"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["body"].as_str().unwrap().contains("timeout"))
    );
    // A term that is absent matches nothing.
    let none = get(&a, &format!("{BASE}/logs?{w}&q=zzzznotthere"))
        .await
        .json();
    assert_eq!(none["data"].as_array().unwrap().len(), 0);
}

// ---- AC0.2.8 export ----------------------------------------------------------

#[test]
fn ac_0_2_8_export_csv_escaping() {
    let mut r = log_row("1", "ERROR", "a,b \"quoted\"\nnext line");
    r["trace_id"] = json!("=HYPERLINK(\"http://x\")");
    r["span_id"] = json!("+cmd");
    r["scope_name"] = Value::Null;
    r["timestamp"] = json!(1_760_000_000_123i64);
    let csv = export_csv(&[r]);
    let mut lines = csv.splitn(2, '\n');
    assert_eq!(
        lines.next().unwrap(),
        "timestamp,ts_ns,severity_text,severity_number,body,trace_id,span_id,scope_name,log_attributes,resource_attributes"
    );
    let body = lines.next().unwrap();
    assert!(
        body.starts_with("2025-10-09T08:53:20.123Z,1,ERROR,17,\"a,b \"\"quoted\"\"\nnext line\","),
        "{body}"
    );
    assert!(
        body.contains("\"'=HYPERLINK(\"\"http://x\"\")\""),
        "formulas are defused: {body}"
    );
    assert!(body.contains(",'+cmd,"), "{body}");
    assert!(
        body.contains(",,\"{\"\"http.method\"\":\"\"POST\"\"}\""),
        "JSON cells are quoted JSON: {body}"
    );
}

#[test]
fn ac_0_2_8_export_ndjson() {
    let rows = vec![
        log_row("1", "INFO", "one"),
        log_row("2", "ERROR", "two\nlines"),
    ];
    let out = export_ndjson(&rows);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 2, "newlines inside values stay escaped");
    for (l, r) in lines.iter().zip(&rows) {
        assert_eq!(&serde_json::from_str::<Value>(l).unwrap(), r);
    }
    assert!(out.ends_with('\n'));
    assert_eq!(export_ndjson(&[]), "");
}

#[tokio::test]
async fn ac_0_2_8_export_capped() {
    let f = Fake::new(sample_db);
    let a = app(f.clone());
    let r = get(
        &a,
        &format!("{BASE}/logs/export?{W}&limit=99999999&format=csv"),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.header("content-type"), "text/csv; charset=utf-8");
    assert_eq!(
        r.header("content-disposition"),
        "attachment; filename=\"logs.csv\""
    );
    assert!(r.text().starts_with("timestamp,ts_ns,"));
    let q = f
        .seen
        .lock()
        .unwrap()
        .iter()
        .find(|q| q.name == "logs_search")
        .unwrap()
        .clone();
    assert!(
        q.sql
            .contains(&format!("LIMIT {} OFFSET 0", logs::EXPORT_MAX)),
        "{}",
        q.sql
    );
    let n = get(&a, &format!("{BASE}/logs/export?{W}&format=ndjson")).await;
    assert_eq!(n.header("content-type"), "application/x-ndjson");
    assert_eq!(n.header("x-grepui-rows"), "1");
    assert_eq!(
        get(&a, &format!("{BASE}/logs/export?{W}&format=xml"))
            .await
            .status,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        get(&a, &format!("{BASE}/logs/export")).await.status,
        StatusCode::BAD_REQUEST
    );
}
