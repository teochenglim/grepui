//! AC0.3.1 – AC0.3.6: the state store (migrate, write gate), saved views, org isolation,
//! history, stream details, data health.
mod common;
use axum::http::{Method, StatusCode};
use common::*;
use grepui::{
    api::{
        ROUTES, WRITE_ROUTES,
        ingest::state_of,
        streams::{DdlInfo, parse_ddl},
    },
    config::Config,
    error::ReadError,
    query::{Bind, Q, Reader, SqlxReader, WRITE_TARGETS, Writer},
    server::startup_decision,
    store::{self, SCHEMA_VERSION, SchemaState, build},
};
use serde_json::{Value, json};
use std::{
    sync::{
        Arc,
        atomic::{AtomicI64, Ordering},
    },
    time::Duration,
};

const BASE: &str = "/api/v1/orgs/default";
const T0: i64 = 1_793_400_000_000;

fn clock(ms: i64) -> Arc<AtomicI64> {
    Arc::new(AtomicI64::new(ms))
}

// ---- AC0.3.1 migrate and the write gate --------------------------------------

#[test]
fn ac_0_3_1_write_allowlist_is_metadata_only() {
    assert_eq!(
        WRITE_TARGETS,
        ["grepui_schema", "grepui_views", "grepui_history"]
    );
    // The migration creates exactly those tables, all in the metadata database.
    let sql = store::migration_statements("grepui").unwrap();
    let created: Vec<&str> = sql
        .iter()
        .filter_map(|s| s.strip_prefix("CREATE TABLE IF NOT EXISTS `grepui`.`"))
        .map(|r| r.split('`').next().unwrap())
        .collect();
    assert_eq!(created, WRITE_TARGETS);
    assert!(sql[0].starts_with("CREATE DATABASE IF NOT EXISTS `grepui`"));
    assert!(
        sql.iter()
            .all(|s| !s.contains("DROP") && !s.contains("DELETE") && !s.contains("ALTER"))
    );
    assert!(
        store::TENANT_TABLES
            .iter()
            .all(|t| WRITE_TARGETS.contains(t))
    );
}

#[test]
fn ac_0_3_1_write_guard_blocks_everything_else() {
    // Everything the store builds passes the gate.
    let v = build::ViewRow {
        view_id: "0123456789abcdef".into(),
        owner: "local".into(),
        name: "n".into(),
        kind: "logs".into(),
        spec: "{}".into(),
        created_ms: 1,
        updated_ms: 1,
        deleted: false,
    };
    for q in [
        build::view_put("grepui", "default", &v).unwrap(),
        build::history_add("grepui", "default", "local", "logs", "{}", 5).unwrap(),
        build::schema_put("grepui", 1).unwrap(),
    ] {
        store::check_write(&q, "grepui").unwrap_or_else(|e| panic!("{}: {e}", q.name));
    }
    // Nothing else does.
    let bad = |name: &'static str, sql: &str| Q {
        name,
        sql: format!("/* q:{name} */ {sql}"),
        binds: vec![],
    };
    for q in [
        bad(
            "x",
            "INSERT INTO `grepui`.`opentelemetry_logs` (`a`) VALUES (?)",
        ),
        bad("x", "INSERT INTO `public`.`grepui_views` (`a`) VALUES (?)"),
        bad(
            "x",
            "INSERT INTO `grepui`.`grepui_views_evil` (`a`) VALUES (?)",
        ),
        bad("x", "DELETE FROM `grepui`.`grepui_views` WHERE 1=1"),
        bad("x", "UPDATE `grepui`.`grepui_views` SET `deleted` = true"),
        bad("x", "DROP TABLE `grepui`.`grepui_views`"),
        bad(
            "x",
            "INSERT INTO `grepui`.`grepui_views` (`a`) VALUES (?); DROP TABLE t",
        ),
        bad("x", "SELECT 1"),
        Q {
            name: "x",
            sql: "INSERT INTO `grepui`.`grepui_views` (`a`) VALUES (?)".into(),
            binds: vec![],
        }, // missing tag
    ] {
        assert!(
            store::check_write(&q, "grepui").is_err(),
            "must refuse: {}",
            q.sql
        );
    }
    // DDL: only the fixed migration statements.
    for s in store::migration_statements("grepui").unwrap() {
        store::check_ddl(&s, "grepui").unwrap();
    }
    for s in [
        "DROP TABLE x",
        "CREATE TABLE evil (a INT)",
        "CREATE DATABASE IF NOT EXISTS `grepui`; DROP DATABASE public",
        "",
    ] {
        assert!(store::check_ddl(s, "grepui").is_err(), "{s}");
    }
    assert!(store::migration_statements("bad name").is_err());
}

#[tokio::test]
async fn ac_0_3_1_write_routes_are_declared_and_only_those() {
    let store = MemStore::new();
    let a = app_with_store(&store, &clock(T0));
    // Declared routes accept their method and nothing else.
    for (_, route) in WRITE_ROUTES {
        assert!(ROUTES.contains(route), "{route} is also a GET route");
        for m in [Method::POST, Method::PUT, Method::PATCH, Method::DELETE] {
            if WRITE_ROUTES
                .iter()
                .any(|(wm, wp)| wp == route && *wm == m.as_str())
            {
                continue; // declared: accepted (and validated) by its handler
            }
            let r = send(&a, m.clone(), &fill(route)).await;
            assert_eq!(
                r.status,
                StatusCode::METHOD_NOT_ALLOWED,
                "{m} {route} is not declared"
            );
        }
    }
    // A write needs JSON: forms are refused, so a cross-site form post cannot create state.
    let form = axum::http::Request::builder()
        .method("POST")
        .uri(format!("{BASE}/views"))
        .header("content-type", "application/x-www-form-urlencoded")
        .body(axum::body::Body::from("name=x&kind=logs"))
        .unwrap();
    let r = tower::ServiceExt::oneshot(a.clone(), form).await.unwrap();
    assert_eq!(r.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    // Oversized bodies are refused before parsing.
    let big = json!({"name": "x", "kind": "logs", "spec": {"q": "a".repeat(100_000)}});
    assert_eq!(
        send_json(&a, Method::POST, &format!("{BASE}/views"), &big)
            .await
            .status,
        StatusCode::PAYLOAD_TOO_LARGE
    );
    assert_eq!(
        store.write_count(),
        0,
        "nothing was written by the rejected requests"
    );
}

fn fill(route: &str) -> String {
    route
        .replace("{org}", "default")
        .replace("{name}", "t")
        .replace("{field}", "severity_text")
        .replace("{id}", "0123456789abcdef")
        .replace("{trace_id}", "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
}

#[tokio::test]
async fn ac_0_3_1_refuses_newer_schema() {
    let store = MemStore::new();
    // Missing: the table does not exist yet.
    *store.fail_missing.lock().unwrap() = true;
    let missing = store::schema_state(store.as_ref(), "grepui").await;
    assert_eq!(missing.as_ref().unwrap(), &SchemaState::Missing);
    assert!(
        startup_decision(&missing)
            .unwrap()
            .unwrap()
            .contains("grepui migrate")
    );
    *store.fail_missing.lock().unwrap() = false;
    // Current, older, newer.
    *store.version.lock().unwrap() = Some(SCHEMA_VERSION);
    let cur = store::schema_state(store.as_ref(), "grepui").await;
    assert_eq!(cur.as_ref().unwrap(), &SchemaState::Current);
    assert_eq!(startup_decision(&cur).unwrap(), None);
    *store.version.lock().unwrap() = Some(SCHEMA_VERSION - 1);
    let old = store::schema_state(store.as_ref(), "grepui").await;
    assert_eq!(
        old.as_ref().unwrap(),
        &SchemaState::Older(SCHEMA_VERSION - 1)
    );
    assert!(
        startup_decision(&old).unwrap().is_some(),
        "older: warn and point at migrate"
    );
    *store.version.lock().unwrap() = Some(SCHEMA_VERSION + 5);
    let newer = store::schema_state(store.as_ref(), "grepui").await;
    assert_eq!(
        newer.as_ref().unwrap(),
        &SchemaState::Newer(SCHEMA_VERSION + 5)
    );
    assert!(
        startup_decision(&newer)
            .unwrap_err()
            .to_string()
            .contains("upgrade grepui")
    );
    // The database being down is not fatal: /healthz must still come up.
    let down: anyhow::Result<SchemaState> = Err(anyhow::anyhow!("connection refused"));
    assert!(
        startup_decision(&down)
            .unwrap()
            .unwrap()
            .contains("could not check")
    );
    // And migrate itself refuses to touch a newer schema.
    let e = store::migrate(store.as_ref(), store.as_ref(), "grepui")
        .await
        .unwrap_err()
        .to_string();
    assert!(e.contains("newer than this binary"), "{e}");
    assert!(
        store.ddl.lock().unwrap().is_empty(),
        "no DDL ran against the newer schema"
    );
}

#[tokio::test]
async fn ac_0_3_1_migrate_runs_every_statement_once_and_stamps_the_version() {
    let store = MemStore::new();
    let done = store::migrate(store.as_ref(), store.as_ref(), "grepui")
        .await
        .unwrap();
    assert_eq!(done.len(), 4);
    assert_eq!(store.ddl.lock().unwrap().len(), 4);
    assert_eq!(*store.version.lock().unwrap(), Some(SCHEMA_VERSION));
    // Running it again is the same operation (idempotent by IF NOT EXISTS + upsert).
    store::migrate(store.as_ref(), store.as_ref(), "grepui")
        .await
        .unwrap();
    assert_eq!(*store.version.lock().unwrap(), Some(SCHEMA_VERSION));
}

fn live_url() -> String {
    std::env::var("GREPUI_TEST_DB").expect("set GREPUI_TEST_DB=mysql://root@127.0.0.1:4002/public")
}

async fn drop_db(db: &str) {
    let _ = grepui::playground::http::post(
        "http://127.0.0.1:4000",
        "/v1/sql",
        &[("content-type", "application/x-www-form-urlencoded")],
        format!("sql=DROP DATABASE IF EXISTS {db}").as_bytes(),
    )
    .await;
}

#[tokio::test]
#[ignore = "needs GreptimeDB: make db-up && make test-db"]
async fn ac_0_3_1_migrate_idempotent_live() {
    let db = format!("grepui_it_{}", std::process::id());
    let r = SqlxReader::lazy(&live_url(), Duration::from_secs(20)).unwrap();
    assert_eq!(
        store::schema_state(&r, &db).await.unwrap(),
        SchemaState::Missing,
        "a fresh database is Missing, not an error"
    );
    store::migrate(&r, &r, &db).await.expect("first migrate");
    assert_eq!(
        store::schema_state(&r, &db).await.unwrap(),
        SchemaState::Current
    );
    store::migrate(&r, &r, &db)
        .await
        .expect("second migrate is a no-op");
    assert_eq!(
        store::schema_state(&r, &db).await.unwrap(),
        SchemaState::Current
    );
    let tables = r.fetch(&Q::new("t", format!("SELECT table_name FROM information_schema.tables WHERE table_schema = '{db}' ORDER BY 1"), vec![])).await.unwrap();
    let names: Vec<&str> = tables
        .iter()
        .map(|t| t["table_name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["grepui_history", "grepui_schema", "grepui_views"]);
    // The version is one row, overwritten in place.
    let v = r.fetch(&build::schema_get(&db).unwrap()).await.unwrap();
    assert_eq!(v.len(), 1);
    drop_db(&db).await;
}

// ---- AC0.3.2 saved views ------------------------------------------------------

#[tokio::test]
async fn ac_0_3_2_view_roundtrip() {
    let store = MemStore::new();
    let a = app_with_store(&store, &clock(T0));
    let body = json!({"name": "Payment errors", "kind": "logs", "spec": {"q": "resource.service.name=payment", "level": "ERROR", "range": "6h"}});
    let r = send_json(&a, Method::POST, &format!("{BASE}/views"), &body).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let created = r.json()["data"].clone();
    let id = created["id"].as_str().unwrap().to_string();
    assert!(
        id.len() == 16 && id.bytes().all(|b| b.is_ascii_hexdigit()),
        "{id}"
    );
    assert_eq!(r.json()["meta"]["query"], "view_put");
    assert_eq!(created["owner"], "local");
    assert_eq!(
        (
            created["created_ms"].as_i64(),
            created["updated_ms"].as_i64()
        ),
        (Some(T0), Some(T0))
    );

    // Query, window and level come back exactly, by id and in the list.
    let got = get(&a, &format!("{BASE}/views/{id}")).await.json()["data"].clone();
    assert_eq!(got["name"], "Payment errors");
    assert_eq!(got["kind"], "logs");
    assert_eq!(got["spec"], body["spec"]);
    let list = get(&a, &format!("{BASE}/views?kind=logs")).await.json();
    assert_eq!(list["data"].as_array().unwrap().len(), 1);
    assert_eq!(list["data"][0]["id"], id);
    assert_eq!(
        get(&a, &format!("{BASE}/views?kind=sql")).await.json()["data"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    // The write is a single bound INSERT into the metadata table.
    let w = store.writes.lock().unwrap()[0].clone();
    assert!(
        w.sql.contains("INSERT INTO `grepui`.`grepui_views`") && !w.sql.contains("Payment errors"),
        "{}",
        w.sql
    );
    assert_eq!(w.binds[0], Bind::Str("default".into()));
}

#[tokio::test]
async fn ac_0_3_2_view_rename_delete() {
    let store = MemStore::new();
    let now = clock(T0);
    let a = app_with_store(&store, &now);
    let id = send_json(
        &a,
        Method::POST,
        &format!("{BASE}/views"),
        &json!({"name": "old", "kind": "sql", "spec": {"sql": "SELECT 1"}}),
    )
    .await
    .json()["data"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    now.store(T0 + 60_000, Ordering::SeqCst);
    let r = send_json(
        &a,
        Method::PUT,
        &format!("{BASE}/views/{id}"),
        &json!({"name": "new name"}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let v = get(&a, &format!("{BASE}/views/{id}")).await.json()["data"].clone();
    assert_eq!(v["name"], "new name");
    assert_eq!(
        v["spec"],
        json!({"sql": "SELECT 1"}),
        "renaming keeps the spec"
    );
    assert_eq!(
        (v["created_ms"].as_i64(), v["updated_ms"].as_i64()),
        (Some(T0), Some(T0 + 60_000))
    );
    // The same key was overwritten: still one view.
    assert_eq!(
        get(&a, &format!("{BASE}/views")).await.json()["data"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    // Replace the spec.
    send_json(
        &a,
        Method::PUT,
        &format!("{BASE}/views/{id}"),
        &json!({"spec": {"sql": "SELECT 2"}}),
    )
    .await;
    assert_eq!(
        get(&a, &format!("{BASE}/views/{id}")).await.json()["data"]["spec"],
        json!({"sql": "SELECT 2"})
    );

    let d = send(&a, Method::DELETE, &format!("{BASE}/views/{id}")).await;
    assert_eq!(d.status, StatusCode::OK);
    assert_eq!(d.json()["data"], json!({"deleted": true, "id": id}));
    assert_eq!(
        get(&a, &format!("{BASE}/views/{id}")).await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        get(&a, &format!("{BASE}/views")).await.json()["data"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    assert_eq!(
        send(&a, Method::DELETE, &format!("{BASE}/views/{id}"))
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        send_json(
            &a,
            Method::PUT,
            &format!("{BASE}/views/{id}"),
            &json!({"name": "x"})
        )
        .await
        .status,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn ac_0_3_2_view_validation() {
    let store = MemStore::new();
    let a = app_with_store(&store, &clock(T0));
    let post = |b: Value| {
        let a = a.clone();
        async move {
            send_json(&a, Method::POST, &format!("{BASE}/views"), &b)
                .await
                .status
        }
    };
    let ok_spec = json!({"q": "a=b"});
    assert_eq!(
        post(json!({"name": "n", "kind": "logs", "spec": ok_spec})).await,
        StatusCode::CREATED
    );
    let before = store.write_count();
    for bad in [
        json!({"name": "n", "kind": "traces", "spec": ok_spec}),
        json!({"name": "", "kind": "logs", "spec": ok_spec}),
        json!({"name": "   ", "kind": "logs", "spec": ok_spec}),
        json!({"name": "x".repeat(101), "kind": "logs", "spec": ok_spec}),
        json!({"name": "bad\u{0007}name", "kind": "logs", "spec": ok_spec}),
        json!({"name": "n", "kind": "logs", "spec": {"q": 5}}),
        json!({"name": "n", "kind": "logs", "spec": {"evil": "x"}}),
        json!({"name": "n", "kind": "logs", "spec": {"q": "x".repeat(8_001)}}),
        json!({"name": "n", "kind": "logs", "spec": "not an object"}),
        json!({"name": "n", "kind": "logs", "spec": ["q"]}),
    ] {
        let s = post(bad.clone()).await;
        assert!(
            s == StatusCode::BAD_REQUEST || s == StatusCode::UNPROCESSABLE_ENTITY,
            "{bad} -> {s}"
        );
    }
    assert_eq!(
        post(json!({"name": "n"})).await,
        StatusCode::UNPROCESSABLE_ENTITY,
        "missing fields"
    );
    for bad_id in [
        "zz",
        "0123456789ABCDEF",
        "0123456789abcde",
        "0123456789abcdeg",
    ] {
        assert_eq!(
            get(&a, &format!("{BASE}/views/{bad_id}")).await.status,
            StatusCode::BAD_REQUEST,
            "{bad_id}"
        );
    }
    assert_eq!(
        get(&a, &format!("{BASE}/views?kind=evil")).await.status,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        send_json(
            &a,
            Method::POST,
            "/api/v1/orgs/nope/views",
            &json!({"name": "n", "kind": "logs", "spec": {}})
        )
        .await
        .status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        store.write_count(),
        before,
        "no rejected request wrote anything"
    );

    // No state store configured, or not migrated: a clear 503, not a 500/502.
    let ro = app(Fake::new(sample_db));
    let r = send_json(
        &ro,
        Method::POST,
        &format!("{BASE}/views"),
        &json!({"name": "n", "kind": "logs", "spec": {}}),
    )
    .await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(
        r.json()["error"]
            .as_str()
            .unwrap()
            .contains("grepui migrate")
    );
    *store.fail_missing.lock().unwrap() = true;
    let r = get(&a, &format!("{BASE}/views")).await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(
        r.json()["error"]
            .as_str()
            .unwrap()
            .contains("grepui migrate")
    );
}

fn live_app(db: &str) -> (axum::Router, Arc<SqlxReader>) {
    let url = live_url();
    let r = Arc::new(SqlxReader::lazy(&url, Duration::from_secs(20)).unwrap());
    let cfg = Config {
        greptime_mysql: url,
        meta_database: db.into(),
        ..Config::default()
    };
    let app = router(
        AppState::with_parts(cfg, r.clone(), r.clone(), grepui::api::system_clock()).unwrap(),
    );
    (app, r)
}
use grepui::api::{AppState, router};

#[tokio::test]
#[ignore = "needs GreptimeDB: make db-up && make test-db"]
async fn ac_0_3_2_view_share_by_id_live() {
    let db = format!("grepui_it_sv_{}", std::process::id());
    let (a, r) = live_app(&db);
    store::migrate(r.as_ref(), r.as_ref(), &db).await.unwrap();
    let spec = json!({"q": "http.method=GET \"upstream timeout\"", "level": "ERROR", "range": "24h", "mode": "search"});
    let c = send_json(
        &a,
        Method::POST,
        &format!("{BASE}/views"),
        &json!({"name": "Shared", "kind": "logs", "spec": spec}),
    )
    .await;
    assert_eq!(c.status, StatusCode::CREATED, "{}", c.text());
    let id = c.json()["data"]["id"].as_str().unwrap().to_string();

    // A different app instance (as if another replica, or after a restart) resolves the same id.
    let (b, _) = live_app(&db);
    let g = get(&b, &format!("{BASE}/views/{id}")).await;
    assert_eq!(g.status, StatusCode::OK, "{}", g.text());
    assert_eq!(g.json()["data"]["spec"], spec, "quotes and spaces survive");
    assert_eq!(g.json()["data"]["name"], "Shared");
    // Rename: the upsert overwrites the same row.
    assert_eq!(
        send_json(
            &b,
            Method::PUT,
            &format!("{BASE}/views/{id}"),
            &json!({"name": "Renamed"})
        )
        .await
        .status,
        StatusCode::OK
    );
    assert_eq!(
        get(&a, &format!("{BASE}/views/{id}")).await.json()["data"]["name"],
        "Renamed"
    );
    assert_eq!(
        get(&a, &format!("{BASE}/views")).await.json()["data"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    // Delete hides it everywhere.
    assert_eq!(
        send(&b, Method::DELETE, &format!("{BASE}/views/{id}"))
            .await
            .status,
        StatusCode::OK
    );
    assert_eq!(
        get(&a, &format!("{BASE}/views/{id}")).await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        get(&a, &format!("{BASE}/views")).await.json()["data"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    drop_db(&db).await;
}

// ---- AC0.3.3 org isolation ----------------------------------------------------

#[test]
fn ac_0_3_3_builders_require_org() {
    let v = build::ViewRow {
        view_id: "0123456789abcdef".into(),
        owner: "local".into(),
        name: "n".into(),
        kind: "logs".into(),
        spec: "{}".into(),
        created_ms: 1,
        updated_ms: 1,
        deleted: false,
    };
    type Builder = Box<dyn Fn(&str) -> anyhow::Result<Q>>;
    let builders: Vec<(&str, Builder)> = vec![
        (
            "view_list",
            Box::new(|o| build::view_list("grepui", o, None)),
        ),
        (
            "view_list_kind",
            Box::new(|o| build::view_list("grepui", o, Some("logs"))),
        ),
        (
            "view_get",
            Box::new(|o| build::view_get("grepui", o, "0123456789abcdef")),
        ),
        (
            "view_put",
            Box::new(move |o| build::view_put("grepui", o, &v)),
        ),
        (
            "history_add",
            Box::new(|o| build::history_add("grepui", o, "local", "logs", "{}", 5)),
        ),
        (
            "history_list",
            Box::new(|o| build::history_list("grepui", o, "local", None, 10, T0)),
        ),
    ];
    for (name, b) in &builders {
        for bad in ["", "Bad Org", "a b", "../x", "x'; --", "A", &"a".repeat(33)] {
            assert!(b(bad).is_err(), "{name} accepted the org {bad:?}");
        }
        let q = b("acme").unwrap_or_else(|e| panic!("{name}: {e}"));
        // The org is the first bind, compared (or inserted) as a parameter, never in the SQL text.
        assert_eq!(q.binds[0], Bind::Str("acme".into()), "{name}");
        assert!(!q.sql.contains("acme"), "{name}: {}", q.sql);
        assert!(q.sql.contains("`org_id`"), "{name}: {}", q.sql);
        if name.starts_with("view_") && *name != "view_put" || name.starts_with("history_list") {
            assert!(q.sql.contains("`org_id` = ?"), "{name}: {}", q.sql);
        }
    }
}

#[test]
fn ac_0_3_3_org_id_in_every_table() {
    let sql = store::migration_statements("grepui").unwrap();
    for t in store::TENANT_TABLES {
        let ddl = sql.iter().find(|s| s.contains(&format!("`{t}`"))).unwrap();
        assert!(ddl.contains("`org_id` STRING"), "{t} has no org_id: {ddl}");
        let pk = ddl.split("PRIMARY KEY (").nth(1).unwrap();
        assert!(
            pk.starts_with("`org_id`"),
            "{t}: org_id leads the primary key: {pk}"
        );
    }
    // The only table without org_id is the version row: it holds no tenant data.
    let exempt: Vec<&str> = WRITE_TARGETS
        .iter()
        .copied()
        .filter(|t| !store::TENANT_TABLES.contains(t))
        .collect();
    assert_eq!(exempt, ["grepui_schema"]);
    let schema = sql.iter().find(|s| s.contains("`grepui_schema`")).unwrap();
    assert!(!schema.contains("org_id"));
}

#[tokio::test]
async fn ac_0_3_3_cross_org_invisible() {
    // The same builders and a store that scopes by the bound org: org a and org b never see each other.
    let m = MemStore::new();
    let view = |id: &str, name: &str| build::ViewRow {
        view_id: id.into(),
        owner: "local".into(),
        name: name.into(),
        kind: "logs".into(),
        spec: "{}".into(),
        created_ms: 1,
        updated_ms: 1,
        deleted: false,
    };
    m.exec(&build::view_put("grepui", "a", &view("1111111111111111", "a-view")).unwrap())
        .await
        .unwrap();
    m.exec(&build::view_put("grepui", "b", &view("1111111111111111", "b-view")).unwrap())
        .await
        .unwrap(); // same id, different org
    m.exec(&build::history_add("grepui", "a", "local", "logs", "{\"q\":\"a\"}", T0).unwrap())
        .await
        .unwrap();
    for (org, other) in [("a", "b"), ("b", "a")] {
        let list = m
            .fetch(&build::view_list("grepui", org, None).unwrap())
            .await
            .unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0]["name"], format!("{org}-view"));
        let got = m
            .fetch(&build::view_get("grepui", org, "1111111111111111").unwrap())
            .await
            .unwrap();
        assert_eq!(got[0]["name"], format!("{org}-view"));
        assert!(
            m.fetch(&build::view_get("grepui", other, "2222222222222222").unwrap())
                .await
                .unwrap()
                .is_empty()
        );
    }
    let ha = m
        .fetch(&build::history_list("grepui", "a", "local", None, 10, T0).unwrap())
        .await
        .unwrap();
    let hb = m
        .fetch(&build::history_list("grepui", "b", "local", None, 10, T0).unwrap())
        .await
        .unwrap();
    assert_eq!((ha.len(), hb.len()), (1, 0));
}

#[tokio::test]
#[ignore = "needs GreptimeDB: make db-up && make test-db"]
async fn ac_0_3_3_cross_org_invisible_live() {
    let db = format!("grepui_it_org_{}", std::process::id());
    let r = SqlxReader::lazy(&live_url(), Duration::from_secs(20)).unwrap();
    store::migrate(&r, &r, &db).await.unwrap();
    let view = |id: &str, name: &str| build::ViewRow {
        view_id: id.into(),
        owner: "local".into(),
        name: name.into(),
        kind: "logs".into(),
        spec: "{}".into(),
        created_ms: T0,
        updated_ms: T0,
        deleted: false,
    };
    // The same view id in two orgs is two rows.
    r.exec(&build::view_put(&db, "org-a", &view("aaaaaaaaaaaaaaaa", "from a")).unwrap())
        .await
        .unwrap();
    r.exec(&build::view_put(&db, "org-b", &view("aaaaaaaaaaaaaaaa", "from b")).unwrap())
        .await
        .unwrap();
    r.exec(&build::view_put(&db, "org-b", &view("bbbbbbbbbbbbbbbb", "only b")).unwrap())
        .await
        .unwrap();
    r.exec(&build::history_add(&db, "org-a", "local", "logs", "{\"q\":\"x\"}", T0).unwrap())
        .await
        .unwrap();
    let names = |rows: Vec<Value>| -> Vec<String> {
        let mut v: Vec<String> = rows
            .iter()
            .map(|r| r["name"].as_str().unwrap().to_string())
            .collect();
        v.sort();
        v
    };
    assert_eq!(
        names(
            r.fetch(&build::view_list(&db, "org-a", None).unwrap())
                .await
                .unwrap()
        ),
        ["from a"]
    );
    assert_eq!(
        names(
            r.fetch(&build::view_list(&db, "org-b", None).unwrap())
                .await
                .unwrap()
        ),
        ["from b", "only b"]
    );
    assert!(
        r.fetch(&build::view_get(&db, "org-a", "bbbbbbbbbbbbbbbb").unwrap())
            .await
            .unwrap()
            .is_empty(),
        "org a cannot read org b's view by id"
    );
    assert_eq!(
        r.fetch(&build::view_get(&db, "org-b", "aaaaaaaaaaaaaaaa").unwrap())
            .await
            .unwrap()[0]["name"],
        "from b"
    );
    assert_eq!(
        r.fetch(&build::history_list(&db, "org-a", "local", None, 10, T0).unwrap())
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        r.fetch(&build::history_list(&db, "org-b", "local", None, 10, T0).unwrap())
            .await
            .unwrap()
            .len(),
        0
    );
    drop_db(&db).await;
}

// ---- AC0.3.4 history ----------------------------------------------------------

#[tokio::test]
async fn ac_0_3_4_history_capped() {
    let store = MemStore::new();
    let now = clock(T0);
    let a = app_with_store(&store, &now);
    for i in 0..150 {
        now.store(T0 + i * 1000, Ordering::SeqCst);
        let r = send_json(
            &a,
            Method::POST,
            &format!("{BASE}/history"),
            &json!({"kind": "logs", "spec": {"q": format!("n={i}")}}),
        )
        .await;
        assert_eq!(r.status, StatusCode::CREATED);
    }
    let all = get(&a, &format!("{BASE}/history")).await.json()["data"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(all.len(), 100, "capped at the newest 100");
    assert_eq!(all[0]["spec"]["q"], "n=149", "newest first");
    assert_eq!(all[99]["spec"]["q"], "n=50");
    assert_eq!(
        get(&a, &format!("{BASE}/history?limit=5")).await.json()["data"]
            .as_array()
            .unwrap()
            .len(),
        5
    );
    assert_eq!(
        get(&a, &format!("{BASE}/history?limit=999999"))
            .await
            .json()["data"]
            .as_array()
            .unwrap()
            .len(),
        100
    );
    assert_eq!(
        get(&a, &format!("{BASE}/history?limit=0")).await.json()["data"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        get(&a, &format!("{BASE}/history?kind=evil")).await.status,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn ac_0_3_4_history_expires() {
    // The table itself expires rows after 30 days (TTL), and reads never return older ones.
    let ddl = store::migration_statements("grepui")
        .unwrap()
        .into_iter()
        .find(|s| s.contains("`grepui_history`"))
        .unwrap();
    assert!(ddl.contains("WITH (ttl = '30d')"), "{ddl}");
    let q = build::history_list("grepui", "default", "local", None, 10, T0).unwrap();
    assert_eq!(
        q.binds.last().unwrap(),
        &Bind::Str(grepui::query::ts_string(T0 - 30 * 86_400_000))
    );
    let store = MemStore::new();
    let now = clock(T0);
    let a = app_with_store(&store, &now);
    now.store(T0 - 31 * 86_400_000, Ordering::SeqCst);
    send_json(
        &a,
        Method::POST,
        &format!("{BASE}/history"),
        &json!({"kind": "logs", "spec": {"q": "old"}}),
    )
    .await;
    now.store(T0 - 29 * 86_400_000, Ordering::SeqCst);
    send_json(
        &a,
        Method::POST,
        &format!("{BASE}/history"),
        &json!({"kind": "logs", "spec": {"q": "recent"}}),
    )
    .await;
    now.store(T0, Ordering::SeqCst);
    let list = get(&a, &format!("{BASE}/history")).await.json()["data"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["spec"]["q"], "recent");
}

#[tokio::test]
async fn ac_0_3_4_history_rerun() {
    let store = MemStore::new();
    let now = clock(T0);
    let a = app_with_store(&store, &now);
    let spec = json!({"q": "level=error http.method=GET", "level": "ERROR", "range": "6h", "mode": "search"});
    send_json(
        &a,
        Method::POST,
        &format!("{BASE}/history"),
        &json!({"kind": "logs", "spec": spec}),
    )
    .await;
    now.store(T0 + 1000, Ordering::SeqCst);
    send_json(
        &a,
        Method::POST,
        &format!("{BASE}/history"),
        &json!({"kind": "sql", "spec": {"sql": "SELECT 1", "mode": "sql"}}),
    )
    .await;
    // Rerun means applying the stored spec again: it must come back exactly as recorded.
    let logs = get(&a, &format!("{BASE}/history?kind=logs")).await.json()["data"].clone();
    assert_eq!(logs.as_array().unwrap().len(), 1);
    assert_eq!(logs[0]["spec"], spec);
    assert_eq!(logs[0]["kind"], "logs");
    let all = get(&a, &format!("{BASE}/history")).await.json()["data"].clone();
    assert_eq!(all.as_array().unwrap().len(), 2);
    assert_eq!(all[0]["kind"], "sql", "newest first across kinds");
    assert_eq!(all[0]["spec"]["sql"], "SELECT 1");
    assert_eq!(all[1]["at_ms"], T0);
    // Specs may only remember the search.
    let bad = send_json(
        &a,
        Method::POST,
        &format!("{BASE}/history"),
        &json!({"kind": "logs", "spec": {"password": "x"}}),
    )
    .await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn ac_0_3_4_history_does_not_store_a_repeat() {
    let store = MemStore::new();
    let now = clock(T0);
    let a = app_with_store(&store, &now);
    let body = json!({"kind": "logs", "spec": {"q": "a=b", "level": "ERROR"}});
    assert_eq!(
        send_json(&a, Method::POST, &format!("{BASE}/history"), &body)
            .await
            .json()["data"]["recorded"],
        true
    );
    // Same search with keys in another order is the same search.
    let same = json!({"kind": "logs", "spec": {"level": "ERROR", "q": "a=b"}});
    let r = send_json(&a, Method::POST, &format!("{BASE}/history"), &same).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json()["data"]["recorded"], false);
    assert_eq!(store.write_count(), 1);
    send_json(
        &a,
        Method::POST,
        &format!("{BASE}/history"),
        &json!({"kind": "logs", "spec": {"q": "other"}}),
    )
    .await;
    assert_eq!(store.write_count(), 2);
}

#[tokio::test]
#[ignore = "needs GreptimeDB: make db-up && make test-db"]
async fn ac_0_3_4_history_roundtrip_live() {
    let db = format!("grepui_it_hist_{}", std::process::id());
    let (a, r) = live_app(&db);
    store::migrate(r.as_ref(), r.as_ref(), &db).await.unwrap();
    // Entries are timestamped by the app's clock; the unit that comes back must be the same
    // milliseconds (a wrong divisor once made every entry look 20 000 days old).
    let now = chrono::Utc::now().timestamp_millis();
    for (i, q) in ["first", "second", "third"].iter().enumerate() {
        let sp = json!({"q": q, "level": "ERROR", "range": "6h"});
        let r = send_json(
            &a,
            Method::POST,
            &format!("{BASE}/history"),
            &json!({"kind": "logs", "spec": sp}),
        )
        .await;
        assert_eq!(r.status, StatusCode::CREATED, "{i}: {}", r.text());
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let list = get(&a, &format!("{BASE}/history?kind=logs")).await;
    assert_eq!(list.status, StatusCode::OK, "{}", list.text());
    let rows = list.json()["data"].as_array().unwrap().clone();
    assert_eq!(rows.len(), 3);
    assert_eq!(
        rows.iter()
            .map(|r| r["spec"]["q"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["third", "second", "first"],
        "newest first"
    );
    for r in &rows {
        let at = r["at_ms"].as_i64().unwrap();
        assert!(
            (now - 5_000..=now + 5_000).contains(&at),
            "at_ms {at} is not within seconds of now {now}"
        );
        assert_eq!(r["spec"]["level"], "ERROR");
    }
    // The newest entry is not stored twice.
    let again = send_json(
        &a,
        Method::POST,
        &format!("{BASE}/history"),
        &json!({"kind": "logs", "spec": {"q": "third", "level": "ERROR", "range": "6h"}}),
    )
    .await;
    assert_eq!(again.json()["data"]["recorded"], false);
    assert_eq!(
        get(&a, &format!("{BASE}/history?kind=logs")).await.json()["data"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    // Other kinds and other orgs are separate.
    assert_eq!(
        get(&a, &format!("{BASE}/history?kind=sql")).await.json()["data"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    drop_db(&db).await;
}

// ---- AC0.3.5 stream details ----------------------------------------------------

const LIVE_DDL: &str = "CREATE TABLE IF NOT EXISTS `opentelemetry_logs` (\n  `timestamp` TIMESTAMP(9) NOT NULL,\n  `trace_id` STRING NULL SKIPPING INDEX WITH(granularity = '10240'),\n  `body` STRING NULL FULLTEXT INDEX WITH(analyzer = 'English', backend = 'bloom'),\n  `scope_name` STRING NULL INVERTED INDEX,\n  TIME INDEX (`timestamp`),\n  PRIMARY KEY (`scope_name`)\n)\nPARTITION ON COLUMNS (`scope_name`) (\n  `scope_name` < 'm',\n  `scope_name` >= 'm'\n)\nENGINE=mito\nWITH(\n  append_mode = 'true',\n  ttl = '7days',\n  'comment' = 'x'\n)";

#[test]
fn ac_0_3_5_ddl_parse() {
    let i = parse_ddl(LIVE_DDL);
    assert_eq!(i.time_index.as_deref(), Some("timestamp"));
    assert_eq!(i.primary_key, ["scope_name"]);
    assert_eq!(i.partition_columns, ["scope_name"]);
    assert_eq!(i.ttl.as_deref(), Some("7days"));
    assert!(i.append_mode);
    assert_eq!(
        i.indexes,
        [
            ("trace_id".to_string(), "skipping".to_string()),
            ("body".into(), "fulltext".into()),
            ("scope_name".into(), "inverted".into())
        ]
    );
    // Double-quoted identifiers (HTTP style) parse the same; a plain table has nothing extra.
    let plain = parse_ddl(
        "CREATE TABLE IF NOT EXISTS \"h\" (\n  \"k\" STRING NULL,\n  \"ts\" TIMESTAMP(3) NOT NULL,\n  TIME INDEX (\"ts\"),\n  PRIMARY KEY (\"k\")\n)\n\nENGINE=mito\nWITH(\n  ttl = '30days'\n)",
    );
    assert_eq!(
        plain,
        DdlInfo {
            time_index: Some("ts".into()),
            primary_key: vec!["k".into()],
            ttl: Some("30days".into()),
            ..Default::default()
        }
    );
    assert_eq!(parse_ddl(""), DdlInfo::default());
    // A column that merely has "ttl" in its name is not a TTL option.
    assert_eq!(parse_ddl("  `ttl_seconds` INT NULL,\n").ttl, None);
}

#[tokio::test]
async fn ac_0_3_5_stream_details_shape() {
    let a = app(Fake::new(|q| {
        Ok(if q.name == "stream_ddl" {
            vec![json!({"Create Table": LIVE_DDL})]
        } else {
            sample_db(q)?
        })
    }));
    let info = get(&a, &format!("{BASE}/streams/opentelemetry_logs/info"))
        .await
        .json();
    assert_eq!(info["data"]["name"], "opentelemetry_logs");
    assert_eq!(info["data"]["kind"], "logs");
    assert_eq!(info["data"]["engine"], "mito");
    assert_eq!(
        (
            info["data"]["data_bytes"].as_i64(),
            info["data"]["index_bytes"].as_i64(),
            info["data"]["rows_estimate"].as_i64()
        ),
        (Some(48051), Some(386), Some(2016))
    );
    assert_eq!(info["meta"]["query"], "stream_info");
    let ddl = get(&a, &format!("{BASE}/streams/opentelemetry_logs/ddl"))
        .await
        .json();
    assert_eq!(ddl["data"]["parsed"]["ttl"], "7days");
    assert_eq!(ddl["data"]["parsed"]["time_index"], "timestamp");
    assert_eq!(
        ddl["data"]["parsed"]["indexes"][1],
        json!(["body", "fulltext"])
    );
    assert!(
        ddl["data"]["ddl"]
            .as_str()
            .unwrap()
            .starts_with("CREATE TABLE")
    );
    assert_eq!(ddl["meta"]["query"], "stream_ddl");
    // Hostile names never reach the database; unknown streams are 404.
    let f = Fake::new(sample_db);
    let b = app(f.clone());
    for n in ["a%3Bb", "x%60y", "a%20b"] {
        assert_eq!(
            get(&b, &format!("{BASE}/streams/{n}/info")).await.status,
            StatusCode::BAD_REQUEST,
            "{n}"
        );
        assert_eq!(
            get(&b, &format!("{BASE}/streams/{n}/ddl")).await.status,
            StatusCode::BAD_REQUEST,
            "{n}"
        );
    }
    assert!(f.seen_names().is_empty());
    let empty = app(Fake::new(|_| Ok(vec![])));
    assert_eq!(
        get(&empty, &format!("{BASE}/streams/ghost/info"))
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        get(&empty, &format!("{BASE}/streams/ghost/ddl"))
            .await
            .status,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
#[ignore = "needs GreptimeDB: make db-up && make seed && make test-db"]
async fn ac_0_3_5_stream_details_live() {
    let url = live_url();
    let r = Arc::new(SqlxReader::lazy(&url, Duration::from_secs(20)).unwrap());
    let a = router(
        AppState::new(
            Config {
                greptime_mysql: url,
                ..Config::default()
            },
            r,
        )
        .unwrap(),
    );
    let info = get(&a, &format!("{BASE}/streams/opentelemetry_logs/info")).await;
    assert_eq!(info.status, StatusCode::OK, "{}", info.text());
    let d = &info.json()["data"];
    assert_eq!(d["engine"], "mito");
    // Rows are known as soon as they are written. Sizes only count data flushed to storage, so on
    // a freshly seeded database (CI) they are 0: assert they are sane numbers, not that they are
    // positive. (This assertion once failed CI for v0.3.0.)
    assert!(d["rows_estimate"].as_i64().unwrap() > 0, "{d}");
    assert!(
        d["data_bytes"].as_i64().unwrap() >= 0 && d["index_bytes"].as_i64().unwrap() >= 0,
        "{d}"
    );
    let ddl = get(&a, &format!("{BASE}/streams/opentelemetry_logs/ddl"))
        .await
        .json();
    let p = &ddl["data"]["parsed"];
    assert_eq!(p["time_index"], "timestamp");
    assert_eq!(p["append_mode"], true);
    assert!(
        p["indexes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i == &json!(["body", "fulltext"])),
        "{p}"
    );
    assert_eq!(
        get(&a, &format!("{BASE}/streams/no_such_table_xyz/info"))
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        get(&a, &format!("{BASE}/streams/no_such_table_xyz/ddl"))
            .await
            .status,
        StatusCode::NOT_FOUND
    );
}

// ---- AC0.3.6 data health ---------------------------------------------------------

#[test]
fn ac_0_3_6_state_thresholds() {
    assert_eq!(state_of(None, T0, 300), ("empty", None));
    assert_eq!(state_of(Some(T0 - 10_000), T0, 300), ("ok", Some(10)));
    assert_eq!(
        state_of(Some(T0 - 300_000), T0, 300),
        ("ok", Some(300)),
        "exactly at the threshold is still ok"
    );
    assert_eq!(state_of(Some(T0 - 301_000), T0, 300), ("quiet", Some(301)));
    assert_eq!(
        state_of(Some(T0 + 5_000), T0, 300),
        ("ok", Some(0)),
        "clock skew never gives a negative age"
    );
}

#[tokio::test]
async fn ac_0_3_6_ingest_rate_buckets() {
    let a = app(Fake::new(sample_db));
    let w = "from=1793399000000&to=1793402600000"; // one hour
    let j = get(&a, &format!("{BASE}/ingest/rate?{w}&stream=traces"))
        .await
        .json();
    assert_eq!(j["data"]["bucket_ms"], 60_000);
    assert_eq!(j["data"]["total"], 180);
    assert_eq!(
        j["data"]["buckets"][1],
        json!({"t": 1_793_399_160_000i64, "count": 120})
    );
    assert!(
        (j["data"]["per_minute"].as_f64().unwrap() - 3.0).abs() < 1e-9,
        "180 rows over 60 minutes"
    );
    assert_eq!(j["meta"]["query"], "ingest_rate");
    // The bucket size comes from the fixed set and the SQL carries no user number.
    let f = Fake::new(sample_db);
    get(&app(f.clone()), &format!("{BASE}/ingest/rate?{w}")).await;
    let q = f.seen.lock().unwrap()[0].clone();
    assert!(
        q.sql.contains("/ 60000000000) * 60000 AS bucket_ms"),
        "{}",
        q.sql
    );
    assert!(
        q.sql.contains("`public`.`opentelemetry_logs`"),
        "logs is the default stream"
    );
    for bad in ["stream=metrics", "stream=x%3By"] {
        assert_eq!(
            get(&a, &format!("{BASE}/ingest/rate?{w}&{bad}"))
                .await
                .status,
            StatusCode::BAD_REQUEST
        );
    }
    assert_eq!(
        get(&a, &format!("{BASE}/ingest/rate")).await.status,
        StatusCode::BAD_REQUEST,
        "a window is required"
    );
    assert_eq!(
        get(&a, &format!("{BASE}/ingest/services")).await.status,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn ac_0_3_6_quiet_stream_flagged() {
    // logs: newest 1793399269857 ; traces: null (empty).
    let newest = 1_793_399_269_857i64;
    let fresh = clock(newest + 10_000);
    let store = MemStore::new();
    let a = app_with_store(&store, &fresh);
    let j = get(&a, &format!("{BASE}/ingest/freshness")).await.json();
    let streams = j["data"]["streams"].as_array().unwrap();
    assert_eq!(
        (
            streams[0]["stream"].as_str(),
            streams[0]["state"].as_str(),
            streams[0]["age_secs"].as_i64()
        ),
        (Some("logs"), Some("ok"), Some(10))
    );
    assert_eq!(
        (
            streams[1]["stream"].as_str(),
            streams[1]["state"].as_str(),
            streams[1]["age_secs"].is_null()
        ),
        (Some("traces"), Some("empty"), true)
    );
    assert!(j["meta"].get("note").is_none());
    // An hour later the logs stream is quiet and the response says so.
    fresh.store(newest + 3_600_000, Ordering::SeqCst);
    let j = get(&a, &format!("{BASE}/ingest/freshness")).await.json();
    assert_eq!(j["data"]["streams"][0]["state"], "quiet");
    assert!(j["meta"]["note"].as_str().unwrap().contains("300 s"));
    // The threshold is a parameter, clamped to something sane.
    let j = get(&a, &format!("{BASE}/ingest/freshness?quiet_secs=7200"))
        .await
        .json();
    assert_eq!(j["data"]["streams"][0]["state"], "ok");
    assert_eq!(
        get(&a, &format!("{BASE}/ingest/freshness?quiet_secs=1"))
            .await
            .json()["data"]["quiet_secs"],
        10
    );
    assert_eq!(
        get(&a, &format!("{BASE}/ingest/freshness?quiet_secs=99999999"))
            .await
            .json()["data"]["quiet_secs"],
        86_400
    );
    // The scan is bounded to a look-back window (a bound timestamp), never the whole table.
    let f = Fake::new(sample_db);
    get(
        &app(f.clone()),
        &format!("{BASE}/ingest/freshness?lookback_hours=2"),
    )
    .await;
    let q = f
        .seen
        .lock()
        .unwrap()
        .iter()
        .find(|q| q.name == "ingest_freshness")
        .unwrap()
        .clone();
    assert_eq!(q.binds.len(), 2);
    assert!(
        q.sql.matches("`timestamp` >= ?").count() == 2 && q.sql.contains("UNION ALL"),
        "{}",
        q.sql
    );
    // Per-service last seen.
    let j = get(
        &app_with_store(&MemStore::new(), &clock(newest + 400_000)),
        &format!("{BASE}/ingest/services?from=1000&to=2000&quiet_secs=300"),
    )
    .await
    .json();
    assert_eq!(
        j["data"][0],
        json!({"service": "cart", "last_seen_ms": newest, "age_secs": 400, "spans": 10, "state": "quiet"})
    );
}

#[tokio::test]
#[ignore = "needs GreptimeDB: make db-up && make seed && make test-db"]
async fn ac_0_3_6_last_seen_live() {
    let url = live_url();
    let r = Arc::new(SqlxReader::lazy(&url, Duration::from_secs(20)).unwrap());
    let a = router(
        AppState::new(
            Config {
                greptime_mysql: url,
                ..Config::default()
            },
            r,
        )
        .unwrap(),
    );
    let f = get(
        &a,
        &format!("{BASE}/ingest/freshness?lookback_hours=744&quiet_secs=86400"),
    )
    .await;
    assert_eq!(f.status, StatusCode::OK, "{}", f.text());
    let streams = f.json()["data"]["streams"].as_array().unwrap().clone();
    for s in &streams {
        assert_eq!(s["state"], "ok", "seeded data is recent: {s}");
        assert!(s["newest_ms"].as_i64().unwrap() > 1_700_000_000_000);
    }
    assert_eq!(
        streams
            .iter()
            .map(|s| s["stream"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["logs", "traces"]
    );
    let now = chrono::Utc::now().timestamp_millis();
    let w = format!("from={}&to={}", now - 30 * 86_400_000, now + 3_600_000);
    let svc = get(&a, &format!("{BASE}/ingest/services?{w}")).await.json();
    let names: Vec<&str> = svc["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["service"].as_str().unwrap())
        .collect();
    for want in [
        "frontend",
        "cart",
        "checkout",
        "payment",
        "inventory",
        "postgres",
    ] {
        assert!(names.contains(&want), "{want} missing from {names:?}");
    }
    assert!(
        svc["data"][0]["last_seen_ms"].as_i64().unwrap()
            >= svc["data"][5]["last_seen_ms"].as_i64().unwrap(),
        "newest first"
    );
    let rate = get(&a, &format!("{BASE}/ingest/rate?{w}&stream=traces"))
        .await
        .json();
    let sum: i64 = rate["data"]["buckets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["count"].as_i64().unwrap())
        .sum();
    assert_eq!(sum, rate["data"]["total"].as_i64().unwrap());
    assert!(
        rate["data"]["total"].as_i64().unwrap() > 100
            && rate["data"]["per_minute"].as_f64().unwrap() > 0.0
    );
}

#[allow(dead_code)]
fn _unused(_: ReadError) {}
