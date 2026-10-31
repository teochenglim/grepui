//! AC0.7.1 – AC0.7.4: the rollup planner and its source, rollup management from templates, and
//! retention. Everything that writes is admin-only and generated from fixed templates.
mod common;
use axum::{Router, http::Method, http::StatusCode};
use common::*;
use grepui::{
    admin::{self, Plan, Stream, Ttl},
    api::{AppState, router},
    config::Config,
    org::Org,
    playground::{http, send as seed, synth},
    query::{
        SqlxReader, Window,
        services::{self, Source},
    },
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{Arc, atomic::AtomicI64},
    time::Duration,
};

const BASE: &str = "/api/v1/orgs/default";
const NOW: i64 = 1_793_400_000_000;
const DAY: i64 = 86_400_000;

fn org() -> Org {
    Org {
        name: "default".into(),
        database: "public".into(),
        traces_table: "opentelemetry_traces".into(),
        logs_table: "opentelemetry_logs".into(),
    }
}

fn window(days: i64) -> String {
    format!("from={}&to={}", NOW - days * DAY, NOW)
}

/// A reader that says whether the rollup sink exists and records which statements ran.
fn planner_app(sink_exists: bool) -> (Router, Arc<Fake>) {
    let f = Fake::new(move |q| {
        if q.name == "stream_schema"
            && q.binds
                .get(1)
                .is_some_and(|b| b.show().ends_with("_red_1m"))
        {
            return Ok(if sink_exists { sample_db(q)? } else { vec![] });
        }
        if q.name == "services_red_rollup" {
            return Ok(vec![
                json!({"service_name": "frontend", "first_seen_ms": 1, "last_seen_ms": 2, "spans": 600, "requests": 600, "errors": 6, "avg_us": 1500.5, "p95_us": null, "max_us": 9000}),
            ]);
        }
        sample_db(q)
    });
    (app(f.clone()), f)
}

// ---- AC0.7.1 planner ------------------------------------------------------------------

#[test]
fn ac_0_7_1_planner_picks_rollup() {
    assert_eq!(services::plan(DAY + 1, true), Source::Rollup);
    assert_eq!(services::plan(30 * DAY, true), Source::Rollup);
    // Short windows stay on raw spans whether or not a rollup exists.
    assert_eq!(services::plan(DAY, true), Source::Raw);
    assert_eq!(services::plan(3_600_000, true), Source::Raw);
    let w = Window {
        from_ms: NOW - 2 * DAY,
        to_ms: NOW,
        limit: 10,
    };
    let q = services::services_rollup("public", "opentelemetry_traces_red_1m", None, w).unwrap();
    assert!(
        q.sql
            .contains("FROM `public`.`opentelemetry_traces_red_1m`")
            && q.sql.contains("GROUP BY `service_name`")
    );
    assert!(!q.sql.contains("JOIN"));
    // The average is a float division (integer division would truncate microseconds).
    assert!(
        q.sql
            .contains("CAST(sum(`sum_ns`) AS DOUBLE) / sum(`requests`)")
    );
    assert_eq!(q.binds.len(), 2);
    let one = services::series_rollup("public", "s", "cart", w).unwrap();
    assert_eq!(one.binds.last().unwrap().show(), "cart");
}

#[tokio::test]
async fn ac_0_7_1_falls_back_with_warning() {
    let (a, f) = planner_app(false);
    let r = get(&a, &format!("{BASE}/services?{}", window(7))).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    assert_eq!(j["meta"]["source"], "raw");
    assert!(
        j["meta"]["note"].as_str().unwrap().contains("no rollup"),
        "{}",
        j["meta"]
    );
    assert!(f.seen_names().contains(&"services_red"));
    assert!(!f.seen_names().contains(&"services_red_rollup"));
    // A short window raises no warning.
    let r = get(
        &a,
        &format!(
            "{BASE}/services?{}",
            window(0).replace(
                &format!("from={}", NOW),
                &format!("from={}", NOW - 3_600_000)
            )
        ),
    )
    .await;
    assert!(r.json()["meta"]["note"].is_null());
}

#[tokio::test]
async fn ac_0_7_1_planner_uses_rollup_endpoint() {
    let (a, f) = planner_app(true);
    let r = get(&a, &format!("{BASE}/services?{}", window(7))).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(f.seen_names().contains(&"services_red_rollup"));
    let j = r.json();
    assert_eq!(j["meta"]["source"], "rollup_1m");
    assert_eq!(j["data"][0]["requests"], 600);
    // The service page reads its series from the rollup too; operations stay raw.
    let r = get(&a, &format!("{BASE}/services/frontend?{}", window(7))).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let names = f.seen_names();
    assert!(names.contains(&"service_series_rollup") && names.contains(&"service_operations"));
}

#[tokio::test]
#[ignore = "needs GreptimeDB: make db-up && make test-db"]
async fn ac_0_7_1_rollup_live() {
    let l = live().await;
    let now = chrono::Utc::now().timestamp_millis();
    let w = format!("from={}&to={}", now - 48 * 3_600_000, now);
    // No rollup yet: raw with a warning.
    let r = get(&l.app, &format!("{BASE}/services?{w}")).await;
    assert_eq!(r.json()["meta"]["source"], "raw");
    assert!(
        r.json()["meta"]["note"]
            .as_str()
            .unwrap()
            .contains("no rollup")
    );
    let r = send(&l.app, Method::POST, &format!("{BASE}/rollups/traces_red")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = send(
        &l.app,
        Method::POST,
        &format!(
            "{BASE}/rollups/traces_red/backfill?from={}&to={}",
            now - 5 * 3_600_000,
            now - 3_600_000
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = get(&l.app, &format!("{BASE}/services?{w}")).await;
    let j = r.json();
    assert_eq!(j["meta"]["source"], "rollup_1m", "{}", j["meta"]);
    let rows = j["data"].as_array().unwrap();
    let want = l.expected();
    assert_eq!(
        rows.len(),
        want.len(),
        "one row per service with entry spans (postgres has none)"
    );
    for row in rows {
        let name = row["service_name"].as_str().unwrap();
        let (req, err, sum_ns, max_ns) = want[name];
        assert_eq!(row["requests"].as_i64().unwrap(), req, "{name}");
        assert_eq!(row["errors"].as_i64().unwrap(), err, "{name}");
        assert!(
            (row["avg_us"].as_f64().unwrap() - sum_ns as f64 / req as f64 / 1000.0).abs() < 0.5,
            "{name}"
        );
        assert_eq!(row["max_us"].as_i64().unwrap(), max_ns / 1000, "{name}");
        assert!(row["p95_us"].is_null());
    }
    // A window inside the raw limit is still answered from raw spans.
    let r = get(
        &l.app,
        &format!("{BASE}/services?from={}&to={}", now - 6 * 3_600_000, now),
    )
    .await;
    assert_eq!(r.json()["meta"]["source"], "raw");
    l.drop().await;
}

// ---- AC0.7.2 source and precision -----------------------------------------------------

#[tokio::test]
async fn ac_0_7_2_source_reported() {
    let (a, _) = planner_app(true);
    for (days, source) in [(0, "raw"), (2, "rollup_1m"), (30, "rollup_1m")] {
        let w = if days == 0 {
            format!("from={}&to={}", NOW - 3_600_000, NOW)
        } else {
            window(days)
        };
        let r = get(&a, &format!("{BASE}/services?{w}")).await;
        assert_eq!(r.json()["meta"]["source"], source, "{days} days");
        let c = get(&a, &format!("{BASE}/catalog?{w}")).await;
        assert_eq!(c.json()["meta"]["source"], source, "catalog, {days} days");
    }
    assert_eq!(services::SOURCE_ROLLUP, "rollup_1m");
}

#[tokio::test]
async fn ac_0_7_2_no_p95_from_rollup() {
    let (a, _) = planner_app(true);
    let r = get(&a, &format!("{BASE}/services?{}", window(7))).await;
    let j = r.json();
    assert!(
        j["data"][0]["p95_us"].is_null(),
        "a rollup cannot rebuild p95"
    );
    assert!(
        j["meta"]["note"].as_str().unwrap().contains("p95"),
        "and says why: {}",
        j["meta"]
    );
    // The rollup SQL never claims a percentile.
    let w = Window {
        from_ms: NOW - 2 * DAY,
        to_ms: NOW,
        limit: 10,
    };
    let q = services::services_rollup("public", "s", None, w).unwrap();
    assert!(
        q.sql.contains("CAST(NULL AS DOUBLE) AS `p95_us`") && !q.sql.contains("approx_percentile")
    );
}

// ---- AC0.7.3 rollup management --------------------------------------------------------

#[test]
fn ac_0_7_3_templates_only() {
    assert!(Plan::for_org(&org(), "traces_red").is_ok());
    assert!(Plan::for_org(&org(), "anything_else").is_err());
    assert!(Plan::for_org(&org(), "traces_red; DROP").is_err());
    let p = Plan::for_org(&org(), "traces_red").unwrap();
    // Every statement the module generates passes its own gate; nothing else does.
    for sql in p.create().into_iter().chain(p.drop()) {
        admin::check(&sql, 0, &org()).unwrap_or_else(|e| panic!("{sql}: {}", e.1));
    }
    let q = p.backfill(0, 60_000);
    let body = q.sql.strip_prefix("/* q:rollup_backfill */ ").unwrap();
    admin::check(body, 2, &org()).unwrap();
    assert!(
        admin::check(body, 3, &org()).is_err(),
        "the binds are part of the shape"
    );
    for bad in [
        "DROP TABLE `public`.`opentelemetry_traces`",
        "DROP TABLE `public`.`grepui_views`",
        "DELETE FROM `public`.`opentelemetry_traces`",
        "CREATE FLOW x SINK TO y AS SELECT 1",
        "ALTER TABLE `public`.`orders` SET 'ttl' = '1d'",
        "ALTER TABLE `public`.`opentelemetry_traces` SET 'ttl' = '1d'; DROP TABLE x",
        "ALTER TABLE `public`.`opentelemetry_traces` SET 'ttl' = '1 day'",
        "INSERT INTO `public`.`opentelemetry_traces` SELECT 1",
        "",
    ] {
        assert!(
            admin::check(bad, 0, &org()).is_err(),
            "should refuse {bad:?}"
        );
    }
    // The statement kinds are a closed, pinned list (R-F4).
    assert_eq!(admin::ADMIN_WRITES.len(), 10);
    assert_eq!(admin::TEMPLATES, ["traces_red"]);
    // The flow reads only entry spans and names its sink explicitly.
    let flow = &p.create()[1];
    assert!(
        flow.contains("SINK TO `public`.`opentelemetry_traces_red_1m`")
            && flow.contains("SPAN_KIND_SERVER")
    );
}

#[tokio::test]
async fn ac_0_7_3_rollup_admin_only() {
    let store = MemStore::new();
    let clock = Arc::new(AtomicI64::new(NOW));
    let off = app_admin(&store, &clock, false);
    let on = app_admin(&store, &clock, true);
    for (m, uri) in [
        (Method::POST, format!("{BASE}/rollups/traces_red")),
        (
            Method::POST,
            format!("{BASE}/rollups/traces_red/backfill?{}", window(1)),
        ),
        (Method::DELETE, format!("{BASE}/rollups/traces_red")),
    ] {
        let r = send(&off, m.clone(), &uri).await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{m} {uri}: {}", r.text());
        assert!(
            r.json()["error"]
                .as_str()
                .unwrap()
                .contains("--admin-writes")
        );
    }
    assert!(
        store.ddl.lock().unwrap().is_empty() && store.write_count() == 0,
        "nothing ran"
    );
    // With the flag on, the same calls run exactly the template statements.
    assert_eq!(
        send(&on, Method::POST, &format!("{BASE}/rollups/traces_red"))
            .await
            .status,
        StatusCode::OK
    );
    let ran = store.ddl.lock().unwrap().clone();
    assert_eq!(ran.len(), 2);
    assert!(
        ran[0].starts_with("CREATE TABLE IF NOT EXISTS `public`.`opentelemetry_traces_red_1m`")
    );
    assert!(ran[1].starts_with("CREATE FLOW IF NOT EXISTS `grepui_public_red_1m`"));
    assert_eq!(
        send(&on, Method::DELETE, &format!("{BASE}/rollups/traces_red"))
            .await
            .status,
        StatusCode::OK
    );
    assert_eq!(store.ddl.lock().unwrap().len(), 4);
    // Unknown templates and bad routes.
    assert_eq!(
        send(&on, Method::POST, &format!("{BASE}/rollups/nope"))
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    // Reads need no flag.
    assert_eq!(
        get(&off, &format!("{BASE}/rollups")).await.status,
        StatusCode::OK
    );
    assert_eq!(
        get(&off, &format!("{BASE}/rollups")).await.json()["data"]["admin_writes"],
        false
    );
}

#[test]
fn ac_0_7_3_backfill_chunked() {
    let h = 3_600_000;
    // Whole minutes only: both ends are rounded down.
    let c = admin::chunks(10 * 60_000 + 7, 20 * 60_000 + 59_999).unwrap();
    assert_eq!(c, vec![(10 * 60_000, 20 * 60_000)]);
    // A three day range is three day-sized chunks that meet exactly.
    let c = admin::chunks(0, 3 * DAY).unwrap();
    assert_eq!(c, vec![(0, DAY), (DAY, 2 * DAY), (2 * DAY, 3 * DAY)]);
    // Chunks never overlap and cover every minute.
    let c = admin::chunks(5 * 60_000, 2 * DAY + 5 * h + 60_000).unwrap();
    assert!(c.windows(2).all(|p| p[0].1 == p[1].0));
    assert!(
        c.iter()
            .all(|(a, b)| b - a <= DAY && a % 60_000 == 0 && b % 60_000 == 0)
    );
    // Limits: under a minute, backwards, and over 31 days are refused.
    assert!(admin::chunks(0, 59_000).is_err());
    assert!(admin::chunks(DAY, 0).is_err());
    assert!(admin::chunks(0, 32 * DAY).is_err());
    assert_eq!(admin::chunks(0, 31 * DAY).unwrap().len(), 31);
}

#[tokio::test]
async fn ac_0_7_3_backfill_endpoint_runs_one_insert_per_chunk() {
    let store = MemStore::new();
    let clock = Arc::new(AtomicI64::new(NOW));
    let a = app_admin(&store, &clock, true);
    let r = send(
        &a,
        Method::POST,
        &format!(
            "{BASE}/rollups/traces_red/backfill?from={}&to={}",
            NOW - 3 * DAY,
            NOW
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    assert_eq!(j["data"]["chunks"], 3, "three whole days");
    assert_eq!(store.write_count(), 3);
    for w in store.writes.lock().unwrap().iter() {
        assert_eq!(w.name, "rollup_backfill");
        assert!(
            w.sql
                .contains("INSERT INTO `public`.`opentelemetry_traces_red_1m`")
                && w.binds.len() == 2
        );
    }
    assert_eq!(j["data"]["rows"], 15, "the rows of every chunk are summed");
    let r = send(
        &a,
        Method::POST,
        &format!(
            "{BASE}/rollups/traces_red/backfill?from={}&to={}",
            NOW - 40 * DAY,
            NOW
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = send(
        &a,
        Method::POST,
        &format!("{BASE}/rollups/traces_red/backfill"),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
#[ignore = "needs GreptimeDB: make db-up && make test-db"]
async fn ac_0_7_3_rollup_create_live() {
    let l = live().await;
    let now = chrono::Utc::now().timestamp_millis();
    let list = |l: &Live| {
        let app = l.app.clone();
        async move { get(&app, &format!("{BASE}/rollups")).await.json()["data"]["templates"][0].clone() }
    };
    let t = list(&l).await;
    assert_eq!(
        (t["sink_exists"].clone(), t["flow_exists"].clone()),
        (json!(false), json!(false))
    );
    for _ in 0..2 {
        let r = send(&l.app, Method::POST, &format!("{BASE}/rollups/traces_red")).await;
        assert_eq!(
            r.status,
            StatusCode::OK,
            "create is idempotent: {}",
            r.text()
        );
    }
    let t = list(&l).await;
    assert_eq!(
        (t["sink_exists"].clone(), t["flow_exists"].clone()),
        (json!(true), json!(true)),
        "{t}"
    );
    // Chunked backfill over three days, run twice: the second run changes nothing.
    let uri = format!(
        "{BASE}/rollups/traces_red/backfill?from={}&to={}",
        now - 3 * DAY,
        now - 3_600_000
    );
    let r = send(&l.app, Method::POST, &uri).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(r.json()["data"]["chunks"].as_i64().unwrap() >= 3);
    let first = list(&l).await["rows"].clone();
    send(&l.app, Method::POST, &uri).await;
    assert_eq!(
        list(&l).await["rows"],
        first,
        "re-running a backfill is idempotent"
    );
    assert!(first.as_i64().unwrap() > 0);
    // The sink holds exactly the generator's counts.
    let sql = format!(
        "{BASE}/sql?sql={}&from={}&to={}",
        enc("SELECT sum(requests) AS r, sum(errors) AS e FROM opentelemetry_traces_red_1m"),
        now - DAY,
        now + DAY
    );
    let rows = get(&l.app, &sql).await.json()["data"]["rows"][0].clone();
    let want = l.expected();
    assert_eq!(
        rows["r"].as_i64().unwrap(),
        want.values().map(|v| v.0).sum::<i64>()
    );
    assert_eq!(
        rows["e"].as_i64().unwrap(),
        want.values().map(|v| v.1).sum::<i64>()
    );
    // The flow keeps the sink current: new spans appear without a backfill.
    let new = synth::generate(21, 20, (now as u64) * 1_000_000, 1);
    seed(
        "http://127.0.0.1:4000",
        &l.db,
        "opentelemetry_traces",
        "opentelemetry_logs",
        &new,
    )
    .await
    .unwrap();
    let want_new: i64 = new
        .spans
        .iter()
        .filter(|s| s.kind == synth::KIND_SERVER)
        .count() as i64;
    let form = [("content-type", "application/x-www-form-urlencoded")];
    let mut got = 0;
    for _ in 0..30 {
        let _ = http::post(
            "http://127.0.0.1:4000",
            "/v1/sql",
            &form,
            format!("sql=ADMIN FLUSH_FLOW('grepui_{}_red_1m')", l.db).as_bytes(),
        )
        .await;
        let q = format!(
            "{BASE}/sql?sql={}&from={}&to={}",
            enc(&format!(
                "SELECT sum(requests) AS r FROM opentelemetry_traces_red_1m WHERE time_window >= '{}'",
                grepui::query::ts_string(now - 2 * 3_600_000)
            )),
            now - DAY,
            now + DAY
        );
        got = get(&l.app, &q).await.json()["data"]["rows"][0]["r"]
            .as_i64()
            .unwrap_or(0);
        if got == want_new {
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    assert_eq!(got, want_new, "the flow rolled up the new spans");
    // Drop removes both.
    let r = send(
        &l.app,
        Method::DELETE,
        &format!("{BASE}/rollups/traces_red"),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let t = list(&l).await;
    assert_eq!(
        (t["sink_exists"].clone(), t["flow_exists"].clone()),
        (json!(false), json!(false)),
        "{t}"
    );
    l.drop().await;
}

// ---- AC0.7.4 retention ----------------------------------------------------------------

fn retention_app(admin: bool) -> (Router, Arc<MemStore>) {
    let store = MemStore::new();
    let clock = Arc::new(AtomicI64::new(NOW));
    (app_admin(&store, &clock, admin), store)
}

#[tokio::test]
async fn ac_0_7_4_ttl_preview() {
    let f = Fake::new(|q| match q.name {
        "retention_counts" => Ok(vec![json!({"total": 1000, "older": 400})]),
        "retention_options" => Ok(vec![
            json!({"table_name": "opentelemetry_traces", "create_options": "ttl=7days append_mode=true"}),
            json!({"table_name": "opentelemetry_logs", "create_options": "append_mode=true"}),
            json!({"table_name": "opentelemetry_traces_red_1m", "create_options": "ttl=2months 29days 2h 52m 48s"}),
        ]),
        _ => sample_db(q),
    });
    let a = app(f.clone());
    let j = get(&a, &format!("{BASE}/retention")).await.json();
    let s = j["data"]["streams"].as_array().unwrap();
    assert_eq!(s.len(), 3);
    assert_eq!(
        (
            s[0]["ttl"].as_str().unwrap(),
            s[0]["ttl_secs"].as_i64().unwrap()
        ),
        ("7days", 604_800)
    );
    assert_eq!(s[1]["ttl"], "none");
    assert!(s[1]["ttl_secs"].is_null());
    assert_eq!(s[2]["ttl"], "2months 29days 2h 52m 48s");
    // The preview counts rows older than now - ttl and changes nothing.
    let r = get(&a, &format!("{BASE}/retention/traces/preview?ttl=7d")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let p = r.json();
    assert_eq!(
        (
            p["data"]["total"].clone(),
            p["data"]["would_expire"].clone(),
            p["data"]["would_keep"].clone()
        ),
        (json!(1000), json!(400), json!(600))
    );
    let q = f
        .seen
        .lock()
        .unwrap()
        .iter()
        .find(|q| q.name == "retention_counts")
        .cloned()
        .unwrap();
    let cutoff = chrono::NaiveDateTime::parse_from_str(&q.binds[0].show(), "%Y-%m-%d %H:%M:%S%.3f")
        .unwrap()
        .and_utc()
        .timestamp_millis();
    assert!(
        (cutoff - (system_now() - 7 * DAY)).abs() < 20_000,
        "cutoff = now - ttl"
    );
    assert!(
        q.sql
            .starts_with("/* q:retention_counts */ SELECT count(*)")
    );
    // forever expires nothing.
    let p = get(&a, &format!("{BASE}/retention/logs/preview?ttl=forever"))
        .await
        .json();
    assert_eq!(p["data"]["would_expire"], 0);
    // The sample table is on the list of streams only by role, never by request text.
    assert_eq!(
        get(&a, &format!("{BASE}/retention/orders/preview?ttl=7d"))
            .await
            .status,
        StatusCode::BAD_REQUEST
    );
    // Advice: raw kept longer than the rollup.
    assert!(j["data"]["advice"].is_null());
}

fn system_now() -> i64 {
    // The app under test uses the system clock (`app`), within a few seconds of this.
    (chrono::Utc::now().timestamp_millis() / 10_000) * 10_000
}

#[test]
fn ac_0_7_4_ttl_duration_validated() {
    for (raw, text, secs) in [
        ("7d", "7d", Some(604_800)),
        ("48h", "48h", Some(172_800)),
        (" 2w ", "2w", Some(1_209_600)),
        ("3650d", "3650d", Some(315_360_000)),
        ("forever", "forever", None),
    ] {
        let t = Ttl::parse(raw).unwrap();
        assert_eq!((t.text(), t.secs()), (text, secs), "{raw}");
    }
    for bad in [
        "", "d", "7", "7 d", "7days", "0d", "30m", "3651d", "99999d", "-1d", "1.5d", "7d; DROP",
        "7d'", "'7d", "７d", "1y", "FOREVER", "∞", "0", "1h2m",
    ] {
        assert!(Ttl::parse(bad).is_err(), "should refuse {bad:?}");
    }
    // What the statement contains is only the re-rendered duration.
    let t = Ttl::parse(" 12h ").unwrap();
    assert_eq!(
        admin::ttl_ddl(&org(), Stream::Logs, &t).unwrap(),
        "ALTER TABLE `public`.`opentelemetry_logs` SET 'ttl' = '12h'"
    );
    assert!(Stream::parse("orders").is_err());
    // GreptimeDB's echo of a TTL, as it comes back in create_options.
    assert_eq!(
        admin::ttl_secs_from_options("append_mode=true ttl=7days"),
        Some(604_800)
    );
    assert_eq!(
        admin::ttl_secs_from_options("ttl=2months 29days 2h 52m 48s"),
        Some(7_776_000)
    );
    assert_eq!(admin::ttl_secs_from_options("ttl=forever"), None);
    assert_eq!(admin::ttl_secs_from_options("append_mode=true"), None);
    assert_eq!(
        admin::ttl_text_from_options("a=b ttl=2months 29days c=d").as_deref(),
        Some("2months 29days")
    );
}

#[tokio::test]
async fn ac_0_7_4_ttl_admin_only() {
    let (off, store) = retention_app(false);
    let r = send_json(
        &off,
        Method::PUT,
        &format!("{BASE}/retention/traces"),
        &json!({"ttl": "7d"}),
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    assert!(store.ddl.lock().unwrap().is_empty());
    let (on, store) = retention_app(true);
    let r = send_json(
        &on,
        Method::PUT,
        &format!("{BASE}/retention/traces"),
        &json!({"ttl": "7d"}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(
        store.ddl.lock().unwrap().as_slice(),
        ["ALTER TABLE `public`.`opentelemetry_traces` SET 'ttl' = '7d'"]
    );
    assert_eq!(
        r.json()["meta"]["sql"],
        "ALTER TABLE `public`.`opentelemetry_traces` SET 'ttl' = '7d'"
    );
    // Bad input never reaches the writer, with the flag on.
    for body in [
        json!({"ttl": "7d; DROP TABLE x"}),
        json!({"ttl": "yesterday"}),
        json!({"ttl": ""}),
        json!({}),
    ] {
        let r = send_json(&on, Method::PUT, &format!("{BASE}/retention/traces"), &body).await;
        assert!(
            r.status == StatusCode::BAD_REQUEST || r.status == StatusCode::UNPROCESSABLE_ENTITY,
            "{body}: {}",
            r.status
        );
    }
    let r = send_json(
        &on,
        Method::PUT,
        &format!("{BASE}/retention/mysql_users"),
        &json!({"ttl": "7d"}),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        store.ddl.lock().unwrap().len(),
        1,
        "only the one valid change ran"
    );
}

#[tokio::test]
#[ignore = "needs GreptimeDB: make db-up && make test-db"]
async fn ac_0_7_4_ttl_live() {
    let l = live().await;
    let now = chrono::Utc::now().timestamp_millis();
    // `live()` wrote old spans (2 h back); add current ones so a 1 h TTL splits the table.
    let new = synth::generate(31, 15, (now as u64) * 1_000_000, 1);
    seed(
        "http://127.0.0.1:4000",
        &l.db,
        "opentelemetry_traces",
        "opentelemetry_logs",
        &new,
    )
    .await
    .unwrap();
    let old_n = l.batch.spans.len() as i64;
    let new_n = new.spans.len() as i64;
    let p = get(&l.app, &format!("{BASE}/retention/traces/preview?ttl=1h"))
        .await
        .json();
    assert_eq!(p["data"]["total"].as_i64().unwrap(), old_n + new_n);
    assert_eq!(
        p["data"]["would_expire"].as_i64().unwrap(),
        old_n,
        "exactly the old spans"
    );
    assert_eq!(p["data"]["would_keep"].as_i64().unwrap(), new_n);
    let p = get(&l.app, &format!("{BASE}/retention/traces/preview?ttl=4h"))
        .await
        .json();
    assert_eq!(p["data"]["would_expire"], 0);
    // Set, read back, change, forever.
    let put = |ttl: &str| {
        let app = l.app.clone();
        let ttl = ttl.to_string();
        async move {
            send_json(
                &app,
                Method::PUT,
                &format!("{BASE}/retention/traces"),
                &json!({"ttl": ttl}),
            )
            .await
        }
    };
    let read = |stream: &'static str| {
        let app = l.app.clone();
        async move {
            let j = get(&app, &format!("{BASE}/retention")).await.json();
            j["data"]["streams"]
                .as_array()
                .unwrap()
                .iter()
                .find(|s| s["stream"] == stream)
                .unwrap()
                .clone()
        }
    };
    let r = put("7d").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(read("traces").await["ttl_secs"], 604_800);
    assert_eq!(
        read("logs").await["ttl_secs"],
        Value::Null,
        "other streams are untouched"
    );
    assert_eq!(put("48h").await.status, StatusCode::OK);
    assert_eq!(read("traces").await["ttl_secs"], 172_800);
    assert_eq!(put("forever").await.status, StatusCode::OK);
    let s = read("traces").await;
    assert_eq!(
        (s["ttl"].as_str().unwrap(), s["ttl_secs"].clone()),
        ("forever", Value::Null)
    );
    assert_eq!(put("0d").await.status, StatusCode::BAD_REQUEST);
    // The data is still all there: setting a TTL does not delete synchronously.
    let sql = format!(
        "{BASE}/sql?sql={}&from={}&to={}",
        enc("SELECT count(*) AS n FROM opentelemetry_traces"),
        now - DAY,
        now + DAY
    );
    assert_eq!(
        get(&l.app, &sql).await.json()["data"]["rows"][0]["n"]
            .as_i64()
            .unwrap(),
        old_n + new_n
    );
    l.drop().await;
}

// ---- live fixture ---------------------------------------------------------------------

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
        "demo_it_ro_{}_{}",
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
    // Spans from 2 h ago, 60 minutes spread: they sit inside the backfill range (5 h to 1 h back).
    let at = (chrono::Utc::now().timestamp_nanos_opt().unwrap() as u64) - 2 * 3_600 * 1_000_000_000;
    let batch = synth::generate(5, 60, at, 60);
    seed(
        "http://127.0.0.1:4000",
        &db,
        "opentelemetry_traces",
        "opentelemetry_logs",
        &batch,
    )
    .await
    .expect("seed");
    let url = base.replace("/public", &format!("/{db}"));
    let r = Arc::new(SqlxReader::lazy(&url, Duration::from_secs(30)).unwrap());
    let app = router(
        AppState::with_parts(
            Config {
                greptime_mysql: url,
                admin_writes: true,
                ..Config::default()
            },
            r.clone(),
            r,
            grepui::api::system_clock(),
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
            format!("sql=DROP FLOW IF EXISTS grepui_{}_red_1m", self.db).as_bytes(),
        )
        .await;
        let _ = http::post(
            "http://127.0.0.1:4000",
            "/v1/sql",
            &form,
            format!("sql=DROP DATABASE IF EXISTS {}", self.db).as_bytes(),
        )
        .await;
    }
    /// Entry spans per service: (requests, errors, sum of duration ns, max duration ns).
    fn expected(&self) -> HashMap<&'static str, (i64, i64, i64, i64)> {
        let mut m: HashMap<&'static str, (i64, i64, i64, i64)> = HashMap::new();
        for s in self
            .batch
            .spans
            .iter()
            .filter(|s| s.kind == synth::KIND_SERVER)
        {
            let d = (s.end_ns - s.start_ns) as i64;
            let e = m.entry(s.service).or_default();
            e.0 += 1;
            e.1 += i64::from(s.error);
            e.2 += d;
            e.3 = e.3.max(d);
        }
        m
    }
}

#[allow(dead_code)]
fn unused(_: &AppState) {}
