//! AC0.17.1 to AC0.17.3: the API is frozen by golden files (every response shape, every route and
//! its access level), the safety invariants still hold, and the supported database version is
//! checked. Regenerate the golden files on purpose with `UPDATE_GOLDEN=1 cargo test --test
//! ac_hardening`, then read the diff: a changed shape or permission is an API change.
mod common;
use axum::{Router, http::Method};
use common::*;
use grepui::{
    api::{self, ROUTES, WRITE_ROUTES},
    auth::{Access, access_for},
    probe,
    query::{
        Q, Window,
        llm::{self as qllm, By},
        sqlmode,
        traces::TraceFilter,
    },
    store,
};
use serde_json::{Map, Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::{Arc, atomic::AtomicI64},
};

fn golden_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(name)
}

/// Compare `got` with the golden file, or rewrite it when `UPDATE_GOLDEN` is set.
fn check_golden(name: &str, got: &str) {
    let path = golden_path(name);
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::write(&path, got).expect("write golden");
        return;
    }
    let want = std::fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("missing {name}: run with UPDATE_GOLDEN=1 to create it"));
    if want != got {
        let (w, g): (Vec<&str>, Vec<&str>) = (want.lines().collect(), got.lines().collect());
        let first = w
            .iter()
            .zip(g.iter())
            .position(|(a, b)| a != b)
            .unwrap_or(w.len().min(g.len()));
        panic!(
            "{name} differs from the golden file at line {}:\n  golden: {}\n  now:    {}\nA changed API shape or permission is an API change: if it is intended, run UPDATE_GOLDEN=1 cargo test --test ac_hardening and review the diff.",
            first + 1,
            w.get(first).unwrap_or(&"(end)"),
            g.get(first).unwrap_or(&"(end)")
        );
    }
}

// ---- response shapes --------------------------------------------------------------------------

/// The structure of a JSON value: keys and types, never values, so the golden file does not move
/// when data does. Arrays are the merge of their elements; a key missing from some elements is
/// written `key?`; null next to a type is `type?`.
fn shape(v: &Value) -> Value {
    match v {
        Value::Null => json!("null"),
        Value::Bool(_) => json!("bool"),
        Value::Number(n) => json!(if n.is_i64() || n.is_u64() {
            "int"
        } else {
            "number"
        }),
        Value::String(_) => json!("string"),
        Value::Array(a) => match a.iter().map(shape).reduce(|x, y| merge(&x, &y)) {
            Some(m) => json!({"[]": m}),
            None => json!("[]"),
        },
        Value::Object(o) => Value::Object(o.iter().map(|(k, v)| (k.clone(), shape(v))).collect()),
    }
}

fn nullable(s: &Value) -> Value {
    match s.as_str() {
        Some(t) if t.ends_with('?') => s.clone(),
        Some(t) => json!(format!("{t}?")),
        None => json!({"nullable": s}),
    }
}

fn merge(a: &Value, b: &Value) -> Value {
    if a == b {
        return a.clone();
    }
    match (a, b) {
        (Value::String(x), Value::String(y)) if x == "null" => nullable(&json!(y)),
        (Value::String(x), Value::String(y)) if y == "null" => nullable(&json!(x)),
        (Value::String(x), Value::String(y))
            if x.trim_end_matches('?') == y.trim_end_matches('?') =>
        {
            nullable(&json!(x.trim_end_matches('?')))
        }
        (Value::String(x), Value::String(y))
            if [x.as_str(), y.as_str()]
                .iter()
                .all(|t| matches!(*t, "int" | "number")) =>
        {
            json!("number")
        }
        (Value::Object(x), Value::Object(y)) => {
            let keys: BTreeSet<&String> = x.keys().chain(y.keys()).collect();
            let mut out = Map::new();
            for k in keys {
                match (x.get(k), y.get(k)) {
                    (Some(p), Some(q)) => out.insert(k.clone(), merge(p, q)),
                    (Some(p), None) | (None, Some(p)) => out.insert(format!("{k}?"), p.clone()),
                    (None, None) => None,
                };
            }
            Value::Object(out)
        }
        (x, Value::String(n)) | (Value::String(n), x) if n == "null" => nullable(x),
        _ => json!("mixed"),
    }
}

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

fn llm_rows(name: &str) -> Option<Vec<Value>> {
    let measures = json!({"calls": 4, "errors": 1, "p50_ns": 1e8, "p95_ns": 3e8, "p99_ns": 4e8, "avg_ns": 2e8,
        "prompt_tokens": 30, "completion_tokens": 9, "cost": 0.5, "uncosted_prompt": 10, "uncosted_completion": 5});
    let with = |extra: Value| {
        let mut m = measures.as_object().cloned().unwrap_or_default();
        m.extend(extra.as_object().cloned().unwrap_or_default());
        Value::Object(m)
    };
    Some(match name {
        "llm_totals" => vec![measures.clone()],
        "llm_breakdown" => vec![with(json!({"grp": "gpt-4o"}))],
        "llm_series" => vec![
            json!({"bucket_ms": 1793399000000_i64, "calls": 4, "errors": 1, "p95_ns": 3e8, "prompt_tokens": 30, "completion_tokens": 9, "cost": 0.5}),
        ],
        "llm_sessions" => vec![with(
            json!({"session_id": "s1", "traces": 2, "spans": 6, "llm_calls": 2, "first_ms": 1, "last_ms": 9, "users": 1, "user": "u1"}),
        )],
        "llm_users" => vec![with(
            json!({"user_id": "u1", "traces": 2, "spans": 6, "llm_calls": 2, "first_ms": 1, "last_ms": 9, "sessions": 1}),
        )],
        "llm_session_turns" => vec![with(
            json!({"trace_id": TID, "start_ms": 1, "duration_us": 5, "spans": 3, "input": "q", "output": "a"}),
        )],
        _ => return None,
    })
}

/// An app over a store, with a JSON-layout traces table and scripted LLM statements, admin on.
fn world() -> Router {
    let store = MemStore::new();
    let cols = json_cols();
    *store.script.lock().unwrap() = Some(Box::new(move |q: &Q| {
        if q.name == "stream_schema" {
            return Ok(cols.clone());
        }
        if let Some(r) = llm_rows(q.name) {
            return Ok(r);
        }
        sample_db(q)
    }));
    let clock = Arc::new(AtomicI64::new(1_793_399_000_000));
    app_admin(&store, &clock, true)
}

fn fill(route: &str) -> String {
    let id = if route.contains("/traces/{id}") {
        TID
    } else {
        "0123456789abcdef"
    };
    let name = if route.contains("/streams/") {
        "opentelemetry_logs"
    } else {
        "t"
    };
    route
        .replace("{org}", "default")
        .replace("{name}", name)
        .replace("{field}", "severity_text")
        .replace("{service}", "cart")
        .replace("{template}", "traces_red")
        .replace("{panel}", "p1")
        .replace("{share}", "0123456789abcdef")
        .replace("{token}", "0123456789abcdef0123456789abcdef")
        .replace("{stream}", "traces")
        .replace("{id}", id)
        .replace("{user}", "alice")
        .replace("{role}", "viewer")
        .replace("{trace_id}", TID)
}

const EXTRA: &str = "from=1793398000000&to=1793402000000&q=x%3Dy&query=up&by=model&sql=SELECT%201&ts_ns=1793399000000000000&ttl=7d&n=5";

async fn collect_shapes() -> String {
    let app = world();
    let mut out: BTreeMap<String, Value> = BTreeMap::new();
    for route in ROUTES {
        let url = format!("{}?{EXTRA}", fill(route));
        let r = get(&app, &url).await;
        let body: Value = serde_json::from_slice(&r.body).unwrap_or_else(|_| {
            json!(format!(
                "(non-JSON body, content-type {})",
                r.header("content-type")
            ))
        });
        // The envelope's `meta` is part of the contract on every data response.
        out.insert(
            format!("GET {route}"),
            json!({"status": r.status.as_u16(), "shape": shape(&body)}),
        );
    }
    // The writes with a body whose shape clients depend on, in an order that lets later ones work.
    let writes: Vec<(Method, &str, Value)> = vec![
        (
            Method::PUT,
            "/api/v1/orgs/{org}/llm/prices",
            json!({"model": "m", "prompt_per_1m": 1.0, "completion_per_1m": 2.0}),
        ),
        (
            Method::PUT,
            "/api/v1/orgs/{org}/llm/content",
            json!({"mode": "masked"}),
        ),
        (
            Method::PUT,
            "/api/v1/orgs/{org}/llm/feedback",
            json!({"trace_id": TID, "rating": "up"}),
        ),
        (
            Method::POST,
            "/api/v1/orgs/{org}/destinations",
            json!({"name": "hook", "kind": "webhook", "url": "http://127.0.0.1:9/hook"}),
        ),
        (
            Method::POST,
            "/api/v1/orgs/{org}/alerts",
            json!({"name": "a", "kind": "scheduled",
            "query": {"kind": "sql", "text": "SELECT count(*) AS n FROM opentelemetry_logs WHERE timestamp >= $__from AND timestamp < $__to"},
            "condition": {"op": ">", "threshold": 5.0}, "schedule": {"every_s": 60}, "lookback_s": 300}),
        ),
        (
            Method::PUT,
            "/api/v1/orgs/{org}/workgroups/{role}",
            json!({"max_concurrent": 4, "timeout_secs": 30, "max_rows": 1000, "max_window_hours": 24}),
        ),
        (
            Method::POST,
            "/api/v1/orgs/{org}/redaction",
            json!({"field": "password", "mask": "full"}),
        ),
    ];
    for (m, route, body) in writes {
        let r = send_json(&app, m.clone(), &fill(route), &body).await;
        let b: Value = serde_json::from_slice(&r.body).unwrap_or(Value::Null);
        out.insert(
            format!("{m} {route}"),
            json!({"status": r.status.as_u16(), "shape": shape(&b)}),
        );
    }
    serde_json::to_string_pretty(&out).unwrap_or_default() + "\n"
}

#[tokio::test]
async fn ac_0_17_1_golden_shapes() {
    check_golden("response_shapes.json", &collect_shapes().await);
}

#[tokio::test]
async fn ac_0_17_1_shapes_cover_every_route() {
    // The golden file names every route, and the data envelope is `{data, meta}` wherever a
    // route answers 200 with JSON (the "Show SQL" contract).
    let golden: Value = serde_json::from_str(
        &std::fs::read_to_string(golden_path("response_shapes.json")).expect("golden file"),
    )
    .expect("json");
    for route in ROUTES {
        assert!(
            golden.get(format!("GET {route}")).is_some(),
            "no golden shape for GET {route}"
        );
    }
    let mut data_routes = 0;
    for (k, v) in golden.as_object().unwrap() {
        if v["status"] == 200 && v["shape"].get("data").is_some() {
            assert!(
                v["shape"].get("meta").is_some(),
                "{k}: a data response without meta"
            );
            data_routes += 1;
        }
    }
    assert!(
        data_routes >= 40,
        "only {data_routes} routes answer with the {{data, meta}} envelope in the golden file"
    );
}

fn w() -> Window {
    Window {
        from_ms: 1_000,
        to_ms: 9_000,
        limit: 50,
    }
}

#[test]
fn ac_0_17_1_invariants_still_hold() {
    // 1. Every builder, old and new, is a tagged read with no stray statement and no write verb.
    let f = TraceFilter::default();
    let mut qs = grepui::query::build::all_for_tests();
    qs.extend([
        qllm::breakdown("db", "t", By::Model, &f, None, w()).unwrap(),
        qllm::totals("db", "t", &f, None, w()).unwrap(),
        qllm::series("db", "t", &f, None, w()).unwrap(),
        qllm::sessions("db", "t", &f, w(), 0).unwrap(),
        qllm::users("db", "t", &f, w(), 0).unwrap(),
        qllm::session_turns("db", "t", "s", w()).unwrap(),
        grepui::query::build::db_version(),
    ]);
    for q in &qs {
        assert!(
            q.sql.starts_with(&format!("/* q:{} */ ", q.name)),
            "{}",
            q.sql
        );
        let body = q.sql[format!("/* q:{} */ ", q.name).len()..]
            .trim_start()
            .to_ascii_uppercase();
        assert!(
            ["SELECT ", "SHOW ", "TQL "]
                .iter()
                .any(|v| body.starts_with(v)),
            "{}",
            q.sql
        );
        assert!(!q.sql.contains(';'), "{}", q.sql);
        if !body.starts_with("SHOW ") {
            for verb in [
                "INSERT ",
                "UPDATE ",
                "DELETE ",
                "DROP ",
                "ALTER ",
                "CREATE ",
                "TRUNCATE ",
            ] {
                assert!(!body.contains(verb), "{} contains {verb}", q.sql);
            }
        }
        assert_eq!(
            q.sql.matches('?').count(),
            q.binds.len(),
            "every placeholder has a bind: {}",
            q.sql
        );
    }
    // 2. The write allowlist and the tenant tables are exactly what ADR 0004 and ADR 0006 say.
    let targets = [
        "grepui_schema",
        "grepui_views",
        "grepui_history",
        "grepui_services",
        "grepui_folders",
        "grepui_dashboards",
        "grepui_shares",
        "grepui_alert_items",
        "grepui_alert_history",
        "grepui_audit",
    ];
    assert_eq!(grepui::query::WRITE_TARGETS, targets);
    assert_eq!(store::TENANT_TABLES.len(), 9);
    assert!(store::TENANT_TABLES.iter().all(|t| targets.contains(t)));
    assert_eq!(store::SCHEMA_VERSION, 5);
    assert_eq!(grepui::admin::ADMIN_WRITES.len(), 10);
    // 3. The store gate refuses anything but an INSERT into an allowlisted table of its own database.
    let ok = Q::new(
        "view_put",
        "INSERT INTO `grepui`.`grepui_views` (a) VALUES (?)",
        vec![grepui::query::Bind::Int(1)],
    );
    assert!(store::check_write(&ok, "grepui").is_ok());
    for sql in [
        "INSERT INTO `grepui`.`opentelemetry_logs` (a) VALUES (?)",
        "INSERT INTO `other`.`grepui_views` (a) VALUES (?)",
        "DELETE FROM `grepui`.`grepui_views`",
        "DROP TABLE `grepui`.`grepui_views`",
        "INSERT INTO `grepui`.`grepui_views` (a) VALUES (?); DROP TABLE x",
    ] {
        let bad = Q::new("view_put", sql, vec![]);
        assert!(store::check_write(&bad, "grepui").is_err(), "{sql}");
    }
    // 4. SQL mode: one read statement in the org's database, nothing else.
    assert!(sqlmode::check("SELECT 1", "db").is_ok());
    for sql in [
        "INSERT INTO t VALUES (1)",
        "DELETE FROM t",
        "DROP TABLE t",
        "ALTER TABLE t ADD COLUMN c INT",
        "CREATE TABLE t2 AS SELECT 1",
        "SELECT 1; SELECT 2",
        "SELECT 1; DROP TABLE t",
        "TQL EVAL (0, 1, '1s') up",
        "SELECT * FROM other_db.t",
        "COPY t TO '/tmp/x'",
        "SHOW CREATE TABLE other_db.t",
        "",
    ] {
        assert!(
            sqlmode::check(sql, "db").is_err(),
            "SQL mode accepted: {sql}"
        );
    }
}

#[test]
fn ac_0_17_1_state_tables_unreadable_in_sql_mode() {
    // Found in the 0.17.0 security review: SQL mode checked the database name only, so an org whose
    // database was the metadata database could read every tenant's users, sessions and rules.
    for sql in [
        "SELECT * FROM db.grepui_audit",
        "SELECT * FROM grepui_alert_items",
        "SELECT * FROM `GREPUI_views`",
        "SELECT * FROM greptime.db.grepui_alert_items",
        "SELECT * FROM t WHERE x IN (SELECT item_id FROM grepui_alert_items)",
        "WITH s AS (SELECT * FROM grepui_alert_items) SELECT * FROM s",
        "SELECT 1 UNION SELECT spec FROM db.grepui_alert_items",
        "DESC grepui_alert_items",
        "SHOW CREATE TABLE grepui_alert_items",
        "SHOW COLUMNS FROM grepui_alert_items",
    ] {
        assert!(
            sqlmode::check(sql, "db").is_err(),
            "SQL mode accepted: {sql}"
        );
        // Alert rules and dashboard SQL panels go through the same check.
        assert!(
            grepui::alerts::parse_rule(&json!({"name": "x", "kind": "scheduled", "query": {"kind": "sql", "text": sql},
                "condition": {"op": ">", "threshold": 1.0}, "schedule": {"every_s": 60}, "lookback_s": 300})).is_err()
                || !sql.to_ascii_uppercase().starts_with("SELECT"),
            "an alert rule accepted: {sql}"
        );
    }
    // Telemetry tables that merely look similar are fine.
    for sql in [
        "SELECT * FROM grepui FROM_x",
        "SELECT * FROM my_grepui_table",
        "SELECT * FROM opentelemetry_logs",
    ] {
        let _ = sqlmode::check(sql, "db");
    }
    assert!(sqlmode::check("SELECT * FROM my_grepui_table", "db").is_ok());
}

// ---- the route table ------------------------------------------------------------------------

fn access_label(a: Access) -> String {
    match a {
        Access::Anonymous => "anonymous".into(),
        Access::Authenticated => "authenticated".into(),
        Access::Platform => "platform".into(),
        Access::Org(r) => format!("org:{r:?}").to_lowercase(),
        Access::Deny => "deny".into(),
    }
}

fn route_table() -> String {
    let mut lines: Vec<String> = vec![];
    for route in ROUTES {
        lines.push(format!(
            "GET {route} {}",
            access_label(access_for(&Method::GET, route))
        ));
    }
    for (m, route) in WRITE_ROUTES {
        let method: Method = m.parse().unwrap();
        lines.push(format!(
            "{m} {route} {}",
            access_label(access_for(&method, route))
        ));
    }
    for (m, route) in [
        ("GET", "/healthz"),
        ("GET", "/metrics"),
        ("GET", "/api/v1/health"),
        ("GET", "/api/v1/info"),
        ("GET", "/api/v1/orgs"),
        ("GET", "/api/v1/auth/me"),
        ("POST", "/api/v1/auth/setup"),
        ("POST", "/api/v1/auth/login"),
        ("GET", "/api/v1/auth/oidc/login"),
        ("GET", "/api/v1/auth/oidc/callback"),
        ("POST", "/api/v1/auth/logout"),
        ("POST", "/api/v1/auth/password"),
    ] {
        let method: Method = m.parse().unwrap();
        lines.push(format!(
            "{m} {route} {}",
            access_label(access_for(&method, route))
        ));
    }
    lines.sort();
    lines.dedup();
    lines.join("\n") + "\n"
}

#[test]
fn ac_0_17_2_route_table_golden() {
    check_golden("routes.txt", &route_table());
    // Deny by default: a template nobody registered, and a method nobody declared, are denied.
    assert_eq!(
        access_for(&Method::GET, "/api/v1/orgs/{org}/not-a-route"),
        Access::Deny
    );
    assert_eq!(
        access_for(&Method::DELETE, "/api/v1/orgs/{org}/logs"),
        Access::Deny
    );
    assert_eq!(
        access_for(&Method::POST, "/api/v1/platform/orgs/{name}/nope"),
        Access::Deny
    );
}

#[test]
fn ac_0_17_2_no_unlisted_route() {
    // Every route the router registers is in ROUTES (so it is classified), and every ROUTES entry
    // is registered. The registrations are read from the router's source.
    let src = include_str!("../src/api/mod.rs");
    let mut registered: BTreeSet<String> = BTreeSet::new();
    let mut rest = src;
    while let Some(i) = rest.find(".route(") {
        rest = &rest[i + ".route(".len()..];
        let after = rest.trim_start();
        if let Some(q) = after.strip_prefix('"')
            && let Some(end) = q.find('"')
        {
            registered.insert(q[..end].to_string());
        }
    }
    let listed: BTreeSet<String> = ROUTES.iter().map(|r| r.to_string()).collect();
    let outside: BTreeSet<&str> = [
        "/healthz",
        "/metrics",
        "/api/v1/health",
        "/api/v1/info",
        "/api/v1/orgs",
        "/api/v1/auth/me",
        "/api/v1/auth/setup",
        "/api/v1/auth/login",
        "/api/v1/auth/oidc/login",
        "/api/v1/auth/oidc/callback",
        "/api/v1/auth/logout",
        "/api/v1/auth/password",
    ]
    .into();
    let unclassified: Vec<&String> = registered
        .iter()
        .filter(|r| !listed.contains(*r) && !outside.contains(r.as_str()))
        .collect();
    assert!(
        unclassified.is_empty(),
        "routes registered but not in ROUTES (so denied): {unclassified:?}"
    );
    let ghosts: Vec<&String> = listed.iter().filter(|r| !registered.contains(*r)).collect();
    assert!(
        ghosts.is_empty(),
        "ROUTES lists routes the router does not register: {ghosts:?}"
    );
    // Every state-changing route is classified too.
    for (m, route) in WRITE_ROUTES {
        assert!(listed.contains(*route), "{m} {route} is not in ROUTES");
        let method: Method = m.parse().unwrap();
        assert_ne!(access_for(&method, route), Access::Deny, "{m} {route}");
    }
    assert!(api::ROUTES.len() > 100, "{}", api::ROUTES.len());
}

// ---- supported database version -------------------------------------------------------------

#[test]
fn ac_0_17_3_supported_version_rule() {
    assert_eq!(probe::MIN_VERSION, (1, 3));
    // The MySQL protocol reports `8.4.2-GreptimeDB-<version>`: the tag decides, not the 8.4.
    assert!(probe::version_supported("8.4.2-GreptimeDB-1.3.0"));
    assert!(!probe::version_supported("8.4.2-GreptimeDB-1.2.1"));
    for ok in [
        "1.3.0",
        "v1.3.1",
        "1.4.0-nightly-20261101",
        "1.10.0",
        "2.0.0",
    ] {
        assert!(probe::version_supported(ok), "{ok}");
    }
    for old in ["1.2.1", "v1.2.9", "0.17.0", "1.0.0"] {
        assert!(!probe::version_supported(old), "{old}");
    }
    // Garbage is not a reason to refuse to start.
    assert!(probe::version_supported("") && probe::version_supported("custom-build"));
}

#[test]
fn ac_0_17_3_version_query_is_a_read() {
    let q = grepui::query::build::db_version();
    assert!(
        q.sql.starts_with("/* q:db_version */ SELECT version()"),
        "{}",
        q.sql
    );
    assert!(q.binds.is_empty());
    // The pinned version in the Makefile satisfies the rule.
    let makefile = include_str!("../Makefile");
    let pinned = makefile
        .lines()
        .find_map(|l| l.strip_prefix("GREPTIME_VERSION ?= "))
        .expect("pinned version");
    assert!(
        probe::version_supported(pinned.trim()),
        "the pinned GreptimeDB {pinned} must be supported"
    );
}

// ---- browser security headers ---------------------------------------------------------------

#[tokio::test]
async fn ac_0_17_4_security_headers_on_every_response() {
    let app = world();
    // The page, a stylesheet, a data route, an error, an unknown path and a refused method.
    for (m, uri) in [
        (Method::GET, "/"),
        (Method::GET, "/css/base.css"),
        (Method::GET, "/js/main.js"),
        (Method::GET, "/api/v1/orgs/default/llm/prices"),
        (Method::GET, "/api/v1/orgs/default/logs?from=2000&to=1000"),
        (Method::GET, "/api/v1/orgs/default/no-such-thing"),
        (Method::DELETE, "/api/v1/orgs/default/logs"),
        (Method::GET, "/healthz"),
    ] {
        let r = send(&app, m.clone(), uri).await;
        assert_eq!(r.header("content-security-policy"), api::CSP, "{m} {uri}");
        assert_eq!(r.header("x-content-type-options"), "nosniff", "{m} {uri}");
        assert_eq!(r.header("x-frame-options"), "DENY", "{m} {uri}");
        assert_eq!(r.header("referrer-policy"), "no-referrer", "{m} {uri}");
        assert!(
            r.header("permissions-policy").contains("camera=()"),
            "{m} {uri}"
        );
        assert_eq!(
            r.header("cross-origin-opener-policy"),
            "same-origin",
            "{m} {uri}"
        );
    }
}

#[test]
fn ac_0_17_4_csp_allows_what_the_ui_uses() {
    let csp = api::CSP;
    let directive = |name: &str| {
        csp.split(';')
            .map(str::trim)
            .find(|d| d.starts_with(name))
            .unwrap_or_else(|| panic!("no {name} in {csp}"))
            .to_string()
    };
    // Scripts only from the app itself: no inline script, no eval, no other origin.
    let script = directive("script-src");
    assert_eq!(script, "script-src 'self'");
    assert!(!csp.contains("unsafe-eval"));
    // The UI sets inline style attributes and uses data: icons; text panels may show https images.
    assert!(directive("style-src").contains("'unsafe-inline'"));
    assert!(directive("img-src").contains("data:"));
    // It talks to its own API only, cannot be framed, cannot change its base or post forms elsewhere.
    assert_eq!(directive("connect-src"), "connect-src 'self'");
    assert_eq!(directive("frame-ancestors"), "frame-ancestors 'none'");
    assert_eq!(directive("base-uri"), "base-uri 'none'");
    assert_eq!(directive("object-src"), "object-src 'none'");
    assert_eq!(directive("default-src"), "default-src 'self'");
    // No page of the app carries an inline <script> (it would be blocked).
    let index = include_str!("../web/static/index.html");
    assert!(
        !index.contains("<script>") && !index.contains("onclick="),
        "inline script in index.html"
    );
}
