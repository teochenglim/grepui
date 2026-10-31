//! AC0.13.2: the RUM view reads GrepRUM's tables with bound queries, says plainly when they are
//! not installed, and gives exact numbers against real data.
mod common;
use axum::http::StatusCode;
use common::*;
use grepui::{
    api::{AppState, router},
    config::Config,
    playground::http,
    query::{Bind, SqlxReader, rum},
};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};

const BASE: &str = "/api/v1/orgs/default";
const W: &str = "from=1793398000000&to=1793402000000";

fn schema(cols: &[&str]) -> Vec<Value> {
    cols.iter()
        .map(|c| json!({"column_name": c, "data_type": "string", "semantic_type": "FIELD"}))
        .collect()
}

fn rum_db(installed: bool) -> Arc<Fake> {
    Fake::new(move |q| {
        Ok(match q.name {
            "stream_schema" if installed => schema(rum::REQUIRED),
            "stream_schema" => vec![],
            "rum_sites" => vec![json!({"site_id": "shop"}), json!({"site_id": "docs"})],
            "rum_overview" => vec![
                json!({"pageviews": 120, "errors": 4, "vitals": 30, "sessions": 40, "visitors": 33}),
            ],
            "rum_series" => vec![
                json!({"bucket_ms": 1_793_398_000_000i64, "pageviews": 60, "errors": 1}),
                json!({"bucket_ms": 1_793_398_060_000i64, "pageviews": 60, "errors": 3}),
            ],
            "rum_pages" => vec![json!({"path": "/", "views": 80, "sessions": 30})],
            "rum_vitals" => vec![
                json!({"samples": 30, "p50": 120.0, "p75": 260.0, "p95": 640.0, "good": 18, "needs_improvement": 8, "poor": 4}),
            ],
            "rum_vitals_pages" => vec![
                json!({"path": "/cart", "samples": 9, "p50": 300.0, "p75": 480.0, "p95": 700.0}),
            ],
            "rum_errors" => vec![
                json!({"message": "TypeError: x is undefined", "occurrences": 3, "sessions": 2, "first_ms": 1, "last_ms": 2, "stack": "at f (a.js:1)", "path": "/cart"}),
            ],
            "rum_sessions" => vec![
                json!({"session_id": "s1", "visitor_id": "v1", "started_ms": 5, "last_ms": 9, "events": 4, "pageviews": 2, "errors": 1, "last_path": "/cart"}),
            ],
            _ => sample_db(q)?,
        })
    })
}

#[tokio::test]
async fn ac_0_13_2_rum_queries_bound() {
    let fake = rum_db(true);
    let a = app(fake.clone());
    let hostile = "shop'%20OR%201=1%3B%20DROP%20TABLE%20events%3B--";
    let mut seen_all = vec![];
    for path in [
        "rum/status",
        "rum/sites",
        "rum/overview",
        "rum/vitals",
        "rum/errors",
        "rum/sessions",
    ] {
        let r = get(&a, &format!("{BASE}/{path}?{W}&site={hostile}")).await;
        assert_eq!(r.status, StatusCode::OK, "{path}: {}", r.text());
        seen_all.push((path, r.json()));
    }
    // Every RUM statement is tagged, has one `?` per bind, and the site only ever travels as a bind.
    let seen = fake.seen.lock().unwrap().clone();
    let rum_qs: Vec<_> = seen.iter().filter(|q| q.name.starts_with("rum_")).collect();
    for name in [
        "rum_overview",
        "rum_series",
        "rum_pages",
        "rum_vitals",
        "rum_vitals_pages",
        "rum_errors",
        "rum_sessions",
    ] {
        let q = rum_qs
            .iter()
            .find(|q| q.name == name)
            .unwrap_or_else(|| panic!("{name} never ran"));
        assert!(q.sql.starts_with(&format!("/* q:{name} */ ")));
        assert_eq!(q.sql.matches('?').count(), q.binds.len(), "{name}");
        assert!(
            !q.sql.contains("DROP") && !q.sql.contains("OR 1=1"),
            "{}",
            q.sql
        );
        assert!(
            q.binds
                .iter()
                .any(|b| matches!(b, Bind::Str(s) if s.contains("DROP TABLE"))),
            "{name}: the site is a bind"
        );
        assert!(q.sql.contains("`default`.`events`") || q.sql.contains(".`events`"));
    }
    // The answers have the documented shape.
    let get_json = |p: &str| seen_all.iter().find(|(n, _)| *n == p).unwrap().1.clone();
    let ov = get_json("rum/overview");
    assert_eq!(ov["data"]["totals"]["pageviews"], 120);
    assert_eq!(ov["data"]["series"].as_array().unwrap().len(), 2);
    assert_eq!(ov["data"]["pages"][0]["path"], "/");
    let v = get_json("rum/vitals");
    assert_eq!(v["data"]["metric"], "INP");
    assert_eq!(v["data"]["good_ms"], 200);
    assert_eq!(v["data"]["overall"]["p75"], 260.0);
    assert_eq!(v["data"]["pages"][0]["path"], "/cart");
    assert_eq!(get_json("rum/errors")["data"][0]["occurrences"], 3);
    assert_eq!(get_json("rum/sessions")["data"][0]["session_id"], "s1");
    assert_eq!(get_json("rum/sites")["data"], json!(["shop", "docs"]));
    // Bad input is refused before any statement.
    let r = get(&a, &format!("{BASE}/rum/overview?from=5&to=2")).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let long = "x".repeat(200);
    let r = get(&a, &format!("{BASE}/rum/overview?{W}&site={long}")).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn ac_0_13_2_rum_absent_state() {
    // No table at all: a plain "not installed", and nothing else is run.
    let fake = rum_db(false);
    let a = app(fake.clone());
    let r = get(&a, &format!("{BASE}/rum/status")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    assert_eq!(j["data"]["installed"], false);
    assert!(j["data"]["missing"].as_array().unwrap().len() >= 7);
    for p in ["overview", "vitals", "errors", "sessions", "sites"] {
        let r = get(&a, &format!("{BASE}/rum/{p}?{W}")).await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{p}");
        let e = r.json()["error"].as_str().unwrap().to_string();
        assert!(e.contains("not installed") && e.contains("GrepRUM"), "{e}");
    }
    assert!(
        fake.seen_names().iter().all(|n| *n == "stream_schema"),
        "no RUM statement ran: {:?}",
        fake.seen_names()
    );
    // A table called `events` that is not GrepRUM's is not taken for it: the missing columns are named.
    let other = Fake::new(|q| {
        Ok(match q.name {
            "stream_schema" => schema(&["ts", "site_id", "event_name"]),
            _ => sample_db(q)?,
        })
    });
    let a = app(other);
    let j = get(&a, &format!("{BASE}/rum/status")).await.json();
    assert_eq!(j["data"]["installed"], false);
    let miss: Vec<&str> = j["data"]["missing"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(
        miss.contains(&"props") && miss.contains(&"session_id") && !miss.contains(&"ts"),
        "{miss:?}"
    );
    // With the tables there, the state flips.
    let a = app(rum_db(true));
    let j = get(&a, &format!("{BASE}/rum/status")).await.json();
    assert_eq!(j["data"]["installed"], true);
    assert_eq!(j["data"]["missing"], json!([]));
}

/// A throwaway database with GrepRUM's `events` table and rows whose numbers are known.
#[tokio::test]
#[ignore = "needs GreptimeDB (make db-up)"]
async fn ac_0_13_2_rum_live() {
    let base = std::env::var("GREPUI_TEST_DB")
        .expect("set GREPUI_TEST_DB=mysql://root@127.0.0.1:4002/public");
    let db = format!("demo_it_rum_{}", std::process::id());
    let form = [("content-type", "application/x-www-form-urlencoded")];
    let sql = |db: &str, s: String| {
        let db = db.to_string();
        async move {
            let r = http::post(
                "http://127.0.0.1:4000",
                &format!("/v1/sql?db={db}"),
                &form,
                format!(
                    "sql={}",
                    url::form_urlencoded::byte_serialize(s.as_bytes()).collect::<String>()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
            let t = r.1.clone();
            assert!(!t.contains("\"error\""), "{s}: {t}");
        }
    };
    sql("public", format!("CREATE DATABASE IF NOT EXISTS {db}")).await;
    sql(
        &db,
        "CREATE TABLE IF NOT EXISTS events (ts TIMESTAMP(3) NOT NULL, site_id STRING, event_name STRING, \
         visitor_id STRING, session_id STRING, `path` STRING, props STRING, browser STRING, device STRING, \
         country STRING, TIME INDEX (ts), PRIMARY KEY (site_id, event_name)) WITH (append_mode = 'true')"
            .into(),
    )
    .await;
    let t0 = chrono::Utc::now().timestamp_millis() - 600_000;
    // Known data: 5 page views over 2 sessions and 2 visitors, 4 INP samples
    // (100, 200, 300, 600 ms), and 3 errors of two messages.
    let mut rows = vec![];
    let mut add = |dt: i64, ev: &str, vis: &str, ses: &str, path: &str, props: &str| {
        rows.push(format!(
            "({}, 'shop', '{ev}', '{vis}', '{ses}', '{path}', '{}')",
            t0 + dt,
            props.replace('\'', "''")
        ));
    };
    for (i, (vis, ses, path)) in [
        ("v1", "s1", "/"),
        ("v1", "s1", "/cart"),
        ("v1", "s1", "/cart"),
        ("v2", "s2", "/"),
        ("v2", "s2", "/pay"),
    ]
    .iter()
    .enumerate()
    {
        add(i as i64 * 1000, "$pageview", vis, ses, path, "{}");
    }
    for (i, v) in [100, 200, 300, 600].iter().enumerate() {
        let (vis, ses, path) = if i < 2 {
            ("v1", "s1", "/cart")
        } else {
            ("v2", "s2", "/pay")
        };
        add(
            10_000 + i as i64,
            "$web_vital",
            vis,
            ses,
            path,
            &format!("{{\"name\":\"INP\",\"value\":{v}}}"),
        );
    }
    add(
        20_000,
        "$web_vital",
        "v1",
        "s1",
        "/",
        "{\"name\":\"LCP\",\"value\":9999}",
    );
    for (i, m) in ["TypeError: a", "TypeError: a", "RangeError: b"]
        .iter()
        .enumerate()
    {
        add(
            30_000 + i as i64,
            "$error",
            "v1",
            "s1",
            "/cart",
            &format!("{{\"message\":\"{m}\",\"stack\":\"at x\"}}"),
        );
    }
    sql(
        &db,
        format!(
            "INSERT INTO events (ts, site_id, event_name, visitor_id, session_id, `path`, props) VALUES {}",
            rows.join(", ")
        ),
    )
    .await;
    let url = base.replace("/public", &format!("/{db}"));
    let r = Arc::new(SqlxReader::lazy(&url, Duration::from_secs(30)).unwrap());
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
    let a = router(state);
    let (from, to) = (t0 - 60_000, t0 + 120_000);
    let q = format!("from={from}&to={to}");
    let j = get(&a, &format!("{BASE}/rum/status")).await.json();
    assert_eq!(j["data"]["installed"], true, "{j}");
    let ov = get(&a, &format!("{BASE}/rum/overview?{q}&site=shop"))
        .await
        .json();
    let t = &ov["data"]["totals"];
    assert_eq!(
        (
            t["pageviews"].as_i64(),
            t["errors"].as_i64(),
            t["vitals"].as_i64(),
            t["sessions"].as_i64(),
            t["visitors"].as_i64()
        ),
        (Some(5), Some(3), Some(5), Some(2), Some(2)),
        "{ov}"
    );
    let pages = ov["data"]["pages"].as_array().unwrap();
    assert_eq!(pages[0]["path"], "/");
    assert_eq!(
        (pages[0]["views"].as_i64(), pages[1]["views"].as_i64()),
        (Some(2), Some(2))
    );
    let series: i64 = ov["data"]["series"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["pageviews"].as_i64().unwrap())
        .sum();
    assert_eq!(series, 5);
    // Only INP is read: the LCP row is ignored. 100 and 200 are good, 300 needs work, 600 is poor.
    let v = get(&a, &format!("{BASE}/rum/vitals?{q}&site=shop"))
        .await
        .json();
    let o = &v["data"]["overall"];
    assert_eq!(
        (
            o["samples"].as_i64(),
            o["good"].as_i64(),
            o["needs_improvement"].as_i64(),
            o["poor"].as_i64()
        ),
        (Some(4), Some(2), Some(1), Some(1)),
        "{v}"
    );
    let p75 = o["p75"].as_f64().unwrap();
    assert!((300.0..=600.0).contains(&p75), "p75 {p75}");
    assert_eq!(v["data"]["pages"][0]["path"], "/pay");
    let e = get(&a, &format!("{BASE}/rum/errors?{q}&site=shop"))
        .await
        .json();
    assert_eq!(e["data"][0]["message"], "TypeError: a");
    assert_eq!(e["data"][0]["occurrences"], 2);
    assert_eq!(e["data"][1]["message"], "RangeError: b");
    assert_eq!(e["data"][0]["stack"], "at x");
    let s = get(&a, &format!("{BASE}/rum/sessions?{q}&site=shop"))
        .await
        .json();
    let ses = s["data"].as_array().unwrap();
    assert_eq!(ses.len(), 2);
    let s1 = ses.iter().find(|x| x["session_id"] == "s1").unwrap();
    assert_eq!(
        (s1["pageviews"].as_i64(), s1["errors"].as_i64()),
        (Some(3), Some(3))
    );
    // Another site has nothing.
    let none = get(&a, &format!("{BASE}/rum/overview?{q}&site=other"))
        .await
        .json();
    assert_eq!(none["data"]["totals"]["pageviews"], 0);
    let sites = get(&a, &format!("{BASE}/rum/sites?{q}")).await.json();
    assert_eq!(sites["data"], json!(["shop"]));
}
