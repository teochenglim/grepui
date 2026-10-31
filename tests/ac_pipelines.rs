//! AC0.10.1 – AC0.10.4: pipelines through GreptimeDB's API, saved SQL functions, CSV enrichment
//! tables and redaction.
mod common;
use axum::{Router, http::Method, http::StatusCode};
use common::*;
use grepui::{
    admin, enrich,
    functions::Function,
    org::Org,
    playground::{http, synth},
    query::{Bind, Q, SqlxReader},
    redact::{Rule, Rules},
    store,
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicI64, Ordering},
    },
    time::Duration,
};

const BASE: &str = "/api/v1/orgs/default";
const T0: i64 = 1_793_400_000_000;

fn clock() -> Arc<AtomicI64> {
    Arc::new(AtomicI64::new(T0))
}

fn org() -> Org {
    Org {
        name: "default".into(),
        database: "public".into(),
        traces_table: "opentelemetry_traces".into(),
        logs_table: "opentelemetry_logs".into(),
    }
}

// ---- a stand-in for GreptimeDB's HTTP API ------------------------------------------------------

#[derive(Debug, Clone)]
struct Req {
    method: String,
    path: String,
    ctype: String,
    body: String,
}

type Answer = Box<dyn Fn(&Req) -> (u16, String) + Send + Sync>;

struct Gdb {
    base: String,
    log: Arc<Mutex<Vec<Req>>>,
}

async fn gdb(answer: Answer) -> Gdb {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    let log = Arc::new(Mutex::new(vec![]));
    let (lg, answer) = (log.clone(), Arc::new(answer));
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = l.accept().await else {
                return;
            };
            let (lg, answer) = (lg.clone(), answer.clone());
            tokio::spawn(async move {
                let mut buf = vec![0u8; 256 * 1024];
                let mut got = 0;
                loop {
                    let n = s.read(&mut buf[got..]).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    got += n;
                    let text = String::from_utf8_lossy(&buf[..got]).to_string();
                    let Some((head, body)) = text.split_once("\r\n\r\n") else {
                        continue;
                    };
                    let len = head
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                        })
                        .unwrap_or(0);
                    if body.len() < len {
                        continue;
                    }
                    let mut first = head.lines().next().unwrap_or_default().split_whitespace();
                    let (method, path) = (
                        first.next().unwrap_or_default().to_string(),
                        first.next().unwrap_or_default().to_string(),
                    );
                    let ctype = head
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-type:")
                                .map(|v| v.trim().to_string())
                        })
                        .unwrap_or_default();
                    let req = Req {
                        method,
                        path,
                        ctype,
                        body: body.to_string(),
                    };
                    let (code, out) = answer(&req);
                    lg.lock().unwrap().push(req);
                    let _ = s
                        .write_all(format!("HTTP/1.1 {code} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{out}", out.len()).as_bytes())
                        .await;
                    return;
                }
            });
        }
    });
    Gdb { base, log }
}

impl Gdb {
    fn log(&self) -> Vec<Req> {
        self.log.lock().unwrap().clone()
    }
}

fn pipeline_rows(store: &Arc<MemStore>, rows: Vec<Value>) {
    *store.script.lock().unwrap() = Some(Box::new(move |q: &Q| {
        Ok(if q.name == "pipeline_list" {
            rows.clone()
        } else {
            sample_db(q)?
        })
    }));
}

fn cfg(admin: bool, base: &str) -> Config {
    Config {
        admin_writes: admin,
        pipeline_http: Some(base.to_string()),
        ..Config::default()
    }
}
use grepui::config::Config;

const YAML: &str = "version: 2\nprocessors:\n  - dissect:\n      fields:\n        - message\n      patterns:\n        - '%{ip} %{path}'\n      ignore_missing: true\n  - date:\n      fields:\n        - time\n      formats:\n        - \"%Y-%m-%d %H:%M:%S\"\n      ignore_missing: true\ntransform:\n  - fields:\n      - ip\n      - path\n    type: string\n  - fields:\n      - time\n    type: time\n    index: timestamp\n";

// ---- AC0.10.1 --------------------------------------------------------------------------------

#[tokio::test]
async fn ac_0_10_1_pipeline_crud_admin_only() {
    let g = gdb(Box::new(|r| match (r.method.as_str(), r.path.as_str()) {
        ("POST", p) if p.starts_with("/v1/pipelines/") => (200, r#"{"pipelines":[{"name":"x","version":"2026-10-31 07:47:57.880923897"}]}"#.into()),
        ("GET", _) => (200, json!({"pipelines": [{"name": "default.nginx", "version": "2026-10-31 07:47:57.880923897", "pipeline": YAML}]}).to_string()),
        ("DELETE", _) => (200, r#"{"pipelines":[]}"#.into()),
        _ => (400, "{}".into()),
    }))
    .await;
    let store = MemStore::new();
    let rows = vec![
        json!({"name": "default.nginx", "created_ns": 1_793_432_877_880_923_897i64}),
        json!({"name": "default.nginx", "created_ns": 1_793_432_900_000_000_001i64}),
        json!({"name": "other.secret", "created_ns": 5}),
        json!({"name": "default.bad name", "created_ns": 6}),
    ];
    pipeline_rows(&store, rows);
    // Without --admin-writes nothing is created or deleted, and GreptimeDB is never called.
    let (viewer, _) = app_cfg(&store, &clock(), cfg(false, &g.base));
    let r = send_json(
        &viewer,
        Method::PUT,
        &format!("{BASE}/pipelines/nginx"),
        &json!({"yaml": YAML}),
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    let r = send_json(
        &viewer,
        Method::DELETE,
        &format!("{BASE}/pipelines/nginx"),
        &json!({}),
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert!(g.log().is_empty(), "{:?}", g.log());
    // Reading works for everyone and only shows this org's pipelines.
    let list = get(&viewer, &format!("{BASE}/pipelines")).await.json()["data"].clone();
    assert_eq!(list["pipelines"].as_array().unwrap().len(), 1, "{list}");
    assert_eq!(list["pipelines"][0]["name"], "nginx");
    assert_eq!(
        list["pipelines"][0]["versions"].as_array().unwrap().len(),
        2
    );
    assert_eq!(
        list["pipelines"][0]["versions"][0]["version"],
        "2026-10-31 07:47:57.880923897"
    );
    let one = get(&viewer, &format!("{BASE}/pipelines/nginx"))
        .await
        .json()["data"]
        .clone();
    assert_eq!(one["yaml"], YAML);
    assert_eq!(
        get(&viewer, &format!("{BASE}/pipelines/secret"))
            .await
            .status,
        StatusCode::NOT_FOUND,
        "another org's pipeline is invisible"
    );
    // With admin writes: create sends the YAML to the org's own name.
    let (admin_app, _) = app_cfg(&store, &clock(), cfg(true, &g.base));
    let r = send_json(
        &admin_app,
        Method::PUT,
        &format!("{BASE}/pipelines/nginx"),
        &json!({"yaml": YAML}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["data"]["version"], "2026-10-31 07:47:57.880923897");
    let put = g.log().into_iter().find(|q| q.method == "POST").unwrap();
    assert_eq!(put.path, "/v1/pipelines/default.nginx");
    assert_eq!(put.ctype, "application/x-yaml");
    assert_eq!(put.body, YAML);
    // Delete removes every version, each by its own version string; one version when asked.
    let r = send_json(
        &admin_app,
        Method::DELETE,
        &format!("{BASE}/pipelines/nginx"),
        &json!({}),
    )
    .await;
    assert_eq!(r.json()["data"]["deleted"], 2, "{}", r.text());
    let dels: Vec<String> = g
        .log()
        .into_iter()
        .filter(|q| q.method == "DELETE")
        .map(|q| q.path)
        .collect();
    assert_eq!(
        dels,
        [
            "/v1/pipelines/default.nginx?version=2026-10-31%2007:47:57.880923897",
            "/v1/pipelines/default.nginx?version=2026-10-31%2007:48:20.000000001"
        ]
    );
    let r = send_json(
        &admin_app,
        Method::DELETE,
        &format!("{BASE}/pipelines/nginx?version=nope"),
        &json!({}),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    // Names are validated: no path tricks, no other org.
    for bad in ["Nginx", "a.b", "..%2Fx", "a%20b", "x-y"] {
        let r = send_json(
            &admin_app,
            Method::PUT,
            &format!("{BASE}/pipelines/{bad}"),
            &json!({"yaml": YAML}),
        )
        .await;
        assert!(r.status.is_client_error(), "{bad}: {}", r.status);
    }
    let before = g.log().len();
    let r = send_json(
        &admin_app,
        Method::PUT,
        &format!("{BASE}/pipelines/ok"),
        &json!({"yaml": ""}),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        g.log().len(),
        before,
        "an invalid request never reaches GreptimeDB"
    );
    // GreptimeDB's own complaint about the YAML is shown.
    let g2 = gdb(Box::new(|_| {
        (
            400,
            r#"{"error":"Yaml load error.: expected ',' at line 2"}"#.into(),
        )
    }))
    .await;
    let (a2, _) = app_cfg(&store, &clock(), cfg(true, &g2.base));
    let r = send_json(
        &a2,
        Method::PUT,
        &format!("{BASE}/pipelines/bad"),
        &json!({"yaml": "processors: [nope"}),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert!(
        r.json()["error"]
            .as_str()
            .unwrap()
            .contains("Yaml load error")
    );
}

#[test]
fn ac_0_10_1_pipeline_path_gate() {
    use grepui::pipelines::check_path;
    assert!(check_path("default", "/v1/pipelines/default.nginx"));
    assert!(check_path(
        "default",
        "/v1/pipelines/default.nginx?version=2026-10-31%2007:47:57.880923897"
    ));
    assert!(check_path("default", "/v1/pipelines/_dryrun"));
    for bad in [
        "/v1/pipelines/other.nginx",
        "/v1/pipelines/default.",
        "/v1/pipelines/default.A",
        "/v1/pipelines/default.a/../b",
        "/v1/sql",
        "/v1/pipelines/default.a?version=1&x=2",
        "/v1/pipelines/default.a?x=1",
    ] {
        assert!(!check_path("default", bad), "{bad}");
    }
    assert_eq!(
        grepui::pipelines::version_text(1_793_432_877_880_923_897),
        "2026-10-31 07:47:57.880923897"
    );
}

#[tokio::test]
async fn ac_0_10_1_pipeline_dry_run() {
    let g = gdb(Box::new(|r| {
        if r.body.contains("nope") {
            return (400, r#"{"error":"Failed to build pipeline: Yaml load error."}"#.into());
        }
        (200, json!([{"rows": [[{"key": "ip", "value": "10.0.0.1", "data_type": "STRING", "semantic_type": "FIELD"}, {"key": "path", "value": "/x", "data_type": "STRING", "semantic_type": "FIELD"}]],
            "schema": [{"name": "ip", "data_type": "STRING", "column_type": "FIELD"}, {"name": "path", "data_type": "STRING", "column_type": "FIELD"}], "table_name": "dry_run"}]).to_string())
    }))
    .await;
    let store = MemStore::new();
    // A dry-run writes nothing, so it works without --admin-writes.
    let (a, _) = app_cfg(&store, &clock(), cfg(false, &g.base));
    let data = json!([{"message": "10.0.0.1 /x"}]);
    let r = send_json(
        &a,
        Method::POST,
        &format!("{BASE}/pipelines/dry-run"),
        &json!({"yaml": YAML, "data": data}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let d = r.json()["data"].clone();
    assert_eq!(d["rows"][0], json!({"ip": "10.0.0.1", "path": "/x"}));
    assert_eq!(d["columns"][0]["name"], "ip");
    let sent = g.log()[0].clone();
    assert_eq!(sent.path, "/v1/pipelines/_dryrun");
    let body: Value = serde_json::from_str(&sent.body).unwrap();
    assert_eq!(body["pipeline"], YAML);
    assert_eq!(
        serde_json::from_str::<Value>(body["data"].as_str().unwrap()).unwrap(),
        data
    );
    // By name: the org's own prefix is added, never taken from the request.
    send_json(
        &a,
        Method::POST,
        &format!("{BASE}/pipelines/dry-run"),
        &json!({"name": "nginx", "data": data}),
    )
    .await;
    let body: Value = serde_json::from_str(&g.log()[1].body).unwrap();
    assert_eq!(body["pipeline_name"], "default.nginx");
    // The parse error comes back as the answer.
    let r = send_json(
        &a,
        Method::POST,
        &format!("{BASE}/pipelines/dry-run"),
        &json!({"yaml": "processors: [nope", "data": data}),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert!(
        r.json()["error"]
            .as_str()
            .unwrap()
            .contains("Yaml load error")
    );
    // Input limits.
    let n = g.log().len();
    for (body, what) in [
        (json!({"yaml": YAML, "data": []}), "empty"),
        (json!({"yaml": YAML, "data": ["x"]}), "not objects"),
        (
            json!({"yaml": YAML, "data": vec![json!({"a": 1}); 21]}),
            "too many",
        ),
        (json!({"yaml": YAML, "name": "x", "data": data}), "both"),
        (json!({"data": data}), "neither"),
        (json!({"name": "A b", "data": data}), "bad name"),
    ] {
        let r = send_json(
            &a,
            Method::POST,
            &format!("{BASE}/pipelines/dry-run"),
            &body,
        )
        .await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{what}");
    }
    assert_eq!(g.log().len(), n);
    // GreptimeDB down: a clear 502, no panic.
    let (dead, _) = app_cfg(&store, &clock(), cfg(false, "http://127.0.0.1:9"));
    let r = send_json(
        &dead,
        Method::POST,
        &format!("{BASE}/pipelines/dry-run"),
        &json!({"yaml": YAML, "data": data}),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_GATEWAY);
}

#[tokio::test]
#[ignore = "needs GreptimeDB: make db-up && make test-db"]
async fn ac_0_10_1_pipeline_live() {
    let base = std::env::var("GREPUI_TEST_DB")
        .expect("set GREPUI_TEST_DB=mysql://root@127.0.0.1:4002/public");
    let r = Arc::new(SqlxReader::lazy(&base, Duration::from_secs(30)).unwrap());
    let name = format!("it_{}", std::process::id());
    let (app, _) = {
        let c = clock();
        let state = grepui::api::AppState::with_parts(
            Config {
                greptime_mysql: base.clone(),
                admin_writes: true,
                pipeline_http: Some("http://127.0.0.1:4000".into()),
                ..Config::default()
            },
            r.clone(),
            r.clone(),
            grepui::api::system_clock(),
        )
        .unwrap();
        let _ = c;
        (grepui::api::router(state.clone()), state)
    };
    let data = json!([{"message": "10.0.0.1 /x", "time": "2026-10-31 07:00:00"}]);
    // Dry-run before saving, with the YAML inline.
    let r = send_json(
        &app,
        Method::POST,
        &format!("{BASE}/pipelines/dry-run"),
        &json!({"yaml": YAML, "data": data}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["data"]["rows"][0]["ip"], "10.0.0.1");
    // Create twice: two versions.
    for _ in 0..2 {
        let r = send_json(
            &app,
            Method::PUT,
            &format!("{BASE}/pipelines/{name}"),
            &json!({"yaml": YAML}),
        )
        .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    }
    let list = get(&app, &format!("{BASE}/pipelines")).await.json()["data"]["pipelines"].clone();
    let mine = list
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == json!(name))
        .expect("listed");
    assert_eq!(mine["versions"].as_array().unwrap().len(), 2);
    let one = get(&app, &format!("{BASE}/pipelines/{name}")).await.json()["data"].clone();
    assert_eq!(one["yaml"], YAML);
    // Dry-run by name uses the stored one.
    let r = send_json(
        &app,
        Method::POST,
        &format!("{BASE}/pipelines/dry-run"),
        &json!({"name": name, "data": data}),
    )
    .await;
    assert_eq!(r.json()["data"]["rows"][0]["path"], "/x", "{}", r.text());
    // Bad YAML is refused by GreptimeDB and nothing is stored.
    let r = send_json(
        &app,
        Method::PUT,
        &format!("{BASE}/pipelines/{name}_bad"),
        &json!({"yaml": "processors: [nope"}),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    // Delete one version, then the rest.
    let v = mine["versions"][0]["version"].as_str().unwrap().to_string();
    let r = send_json(
        &app,
        Method::DELETE,
        &format!("{BASE}/pipelines/{name}?version={}", v.replace(' ', "%20")),
        &json!({}),
    )
    .await;
    assert_eq!(r.json()["data"]["deleted"], 1, "{}", r.text());
    let r = send_json(
        &app,
        Method::DELETE,
        &format!("{BASE}/pipelines/{name}"),
        &json!({}),
    )
    .await;
    assert_eq!(r.json()["data"]["deleted"], 1, "{}", r.text());
    let list = get(&app, &format!("{BASE}/pipelines")).await.json()["data"]["pipelines"].clone();
    assert!(
        list.as_array()
            .unwrap()
            .iter()
            .all(|p| p["name"] != json!(name))
    );
}

// ---- AC0.10.2 --------------------------------------------------------------------------------

async fn save_fn(a: &Router, name: &str, params: Value, body: &str) -> Resp {
    send_json(
        a,
        Method::PUT,
        &format!("{BASE}/functions/{name}"),
        &json!({"params": params, "body": body, "description": "d"}),
    )
    .await
}

fn sql_app() -> (Router, Arc<MemStore>) {
    let store = MemStore::new();
    let (a, _) = app_cfg(&store, &clock(), Config::default());
    (a, store)
}

fn sql_get(sql: &str) -> String {
    let enc: String = sql
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect();
    format!("{BASE}/sql?sql={enc}&from=1000&to=2000")
}

#[tokio::test]
async fn ac_0_10_2_function_params_bound() {
    let (a, store) = sql_app();
    let r = save_fn(
        &a,
        "slow",
        json!(["ms", "svc"]),
        "(duration_nano / 1000000 > $1 AND service_name = $2)",
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let list = get(&a, &format!("{BASE}/functions")).await.json()["data"].clone();
    assert_eq!(list[0]["name"], "slow");
    // A call in SQL mode becomes placeholders; the arguments are binds, in order, and the text
    // never contains them.
    let hostile = "x') OR 1=1 --";
    let r = get(
        &a,
        &sql_get(&format!(
            "SELECT count(*) FROM opentelemetry_traces WHERE @slow(250, '{}')",
            hostile.replace('\'', "''")
        )),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let q = store
        .seen
        .lock()
        .unwrap()
        .iter()
        .find(|q| q.name == "sql_mode")
        .cloned()
        .unwrap();
    assert!(
        q.sql
            .contains("(duration_nano / 1000000 > ? AND service_name = ?)"),
        "{}",
        q.sql
    );
    assert!(
        !q.sql.contains("OR 1=1") && !q.sql.contains("@slow"),
        "{}",
        q.sql
    );
    assert_eq!(q.binds, vec![Bind::Int(250), Bind::Str(hostile.into())]);
    // Floats and booleans bind too.
    save_fn(
        &a,
        "between_",
        json!(["lo", "hi", "flag"]),
        "(x BETWEEN $1 AND $2 AND y = $3)",
    )
    .await;
    get(
        &a,
        &sql_get("SELECT 1 FROM t WHERE @between_(0.5, -2, true)"),
    )
    .await;
    let q = store
        .seen
        .lock()
        .unwrap()
        .iter()
        .rev()
        .find(|q| q.name == "sql_mode")
        .cloned()
        .unwrap();
    assert_eq!(
        q.binds,
        vec![Bind::Float(0.5), Bind::Int(-2), Bind::Bool(true)]
    );
    // Only literals are arguments; the arity must match; unknown names fail; strings and
    // comments are left alone.
    for (sql, why) in [
        ("SELECT @slow(1 + 1, 'a')", "expression argument"),
        ("SELECT @slow(now(), 'a')", "function argument"),
        ("SELECT @slow(1)", "arity"),
        ("SELECT @nope(1, 'a')", "unknown"),
        ("SELECT @slow", "no call"),
        ("SELECT @slow(1, 'a", "unclosed"),
        ("SELECT @slow(1, 'a\\')", "backslash"),
    ] {
        assert_eq!(
            get(&a, &sql_get(sql)).await.status,
            StatusCode::BAD_REQUEST,
            "{why}"
        );
    }
    let n = store
        .seen
        .lock()
        .unwrap()
        .iter()
        .filter(|q| q.name == "sql_mode")
        .count();
    let r = get(&a, &sql_get("SELECT '@nope(1)' AS a -- @nope(2)")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(
        store
            .seen
            .lock()
            .unwrap()
            .iter()
            .filter(|q| q.name == "sql_mode")
            .count(),
        n + 1
    );
    // The expanded statement still passes the SQL-mode check: a function cannot smuggle a write.
    save_fn(&a, "evil", json!([]), "1) ; DROP TABLE t --").await;
    assert_eq!(
        save_fn(&a, "evil2", json!([]), "x FROM other.t")
            .await
            .status,
        StatusCode::OK
    );
    let r = get(&a, &sql_get("SELECT 1 FROM t WHERE y IN (SELECT @evil2())")).await;
    assert_eq!(
        r.status,
        StatusCode::BAD_REQUEST,
        "a statement that leaves the org database is refused: {}",
        r.text()
    );
    // Functions expand only in SQL mode and panels: an alert rule keeps its text and is refused.
    let r = send_json(&a, Method::POST, &format!("{BASE}/alerts"), &json!({"name": "x", "kind": "scheduled",
        "query": {"kind": "sql", "text": "SELECT @slow(1, 'a')"}, "condition": {"op": ">", "threshold": 1}})).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
}

#[tokio::test]
async fn ac_0_10_2_function_in_panels_keeps_bind_order() {
    let store = MemStore::new();
    let (a, _) = app_cfg(&store, &clock(), Config::default());
    save_fn(&a, "gt", json!(["n"]), "(duration_nano > $1)").await;
    let spec = json!({"variables": [{"name": "svc", "kind": "custom", "options": ["cart", "payment"], "default": ["cart"]}],
        "tabs": [{"title": "T", "panels": [{"id": "p", "type": "stat", "title": "N",
            "query": {"kind": "sql", "text": "SELECT count(*) AS n FROM opentelemetry_traces WHERE service_name = $svc AND @gt(5) AND span_name = $svc"}}]}]});
    let r = send_json(
        &a,
        Method::POST,
        &format!("{BASE}/dashboards"),
        &json!({"title": "D", "spec": spec}),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let id = r.json()["data"]["id"].as_str().unwrap().to_string();
    let r = get(
        &a,
        &format!("{BASE}/dashboards/{id}/panels/p/data?from=1000&to=2000&var-svc=payment"),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let q = store
        .seen
        .lock()
        .unwrap()
        .iter()
        .find(|q| q.name == "dashboard_panel")
        .cloned()
        .unwrap();
    assert!(
        q.sql
            .contains("service_name = ? AND (duration_nano > ?) AND span_name = ?"),
        "{}",
        q.sql
    );
    assert_eq!(
        q.binds,
        vec![
            Bind::Str("payment".into()),
            Bind::Int(5),
            Bind::Str("payment".into())
        ],
        "variable, function and variable binds stay in text order"
    );
}

#[tokio::test]
async fn ac_0_10_2_function_recursion_rejected() {
    let (a, _) = sql_app();
    assert_eq!(
        save_fn(&a, "leaf", json!(["a"]), "lower($1)").await.status,
        StatusCode::OK
    );
    for (name, params, body, why) in [
        ("selfie", json!(["a"]), "@selfie($1)", "calls itself"),
        ("outer", json!(["a"]), "@leaf($1)", "calls another function"),
        ("mixed", json!(["a"]), "lower($1) + @x", "@variable"),
        ("qmark", json!(["a"]), "a = ?", "placeholder"),
        ("semi", json!(["a"]), "1; DROP TABLE t", "semicolon"),
        ("cmt", json!(["a"]), "1 -- x", "comment"),
        ("cmt2", json!(["a"]), "1 /* x */", "comment"),
        ("arity", json!(["a"]), "$2", "beyond arity"),
        ("zero", json!(["a"]), "$0", "$0"),
        ("dollar", json!(["a"]), "$x", "other dollar"),
        ("paren", json!(["a"]), "(lower($1)", "unbalanced"),
        ("quote", json!(["a"]), "lower('$1)", "open quote"),
        ("Upper", json!([]), "1", "bad name"),
        ("empty", json!([]), "  ", "empty body"),
    ] {
        let r = save_fn(&a, name, params, body).await;
        assert!(r.status.is_client_error(), "{why}: {}", r.status);
    }
    // A call inside a quoted part of a body is text, not a call.
    assert_eq!(
        save_fn(&a, "quoted", json!([]), "concat('@leaf(1)', ' $1')")
            .await
            .status,
        StatusCode::OK
    );
    // The pure check agrees.
    let f = Function {
        name: "x".into(),
        description: String::new(),
        params: vec![],
        body: "@x()".into(),
    };
    assert!(f.canonical().is_err());
    // Delete.
    let r = send_json(
        &a,
        Method::DELETE,
        &format!("{BASE}/functions/leaf"),
        &json!({}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(
        send_json(
            &a,
            Method::DELETE,
            &format!("{BASE}/functions/leaf"),
            &json!({})
        )
        .await
        .status,
        StatusCode::NOT_FOUND
    );
}

// ---- AC0.10.3 --------------------------------------------------------------------------------

const CSV: &str = "service,tier,owner,weight,live,zip\ncart,gold,ann,1.5,true,00123\npayment,gold,bob,2,false,00456\nsearch,silver,,0.25,true,99999\n";

#[test]
fn ac_0_10_3_csv_type_inference() {
    let up = enrich::parse_upload(&org(), "owners", CSV, &["service".to_string()]).unwrap();
    let types: Vec<(&str, &str)> = up
        .spec
        .columns
        .iter()
        .map(|(c, t)| (c.as_str(), t.sql()))
        .collect();
    assert_eq!(
        types,
        [
            ("service", "STRING"),
            ("tier", "STRING"),
            ("owner", "STRING"),
            ("weight", "DOUBLE"),
            ("live", "BOOLEAN"),
            ("zip", "STRING")
        ],
        "leading zeros stay text, whole numbers and decimals are told apart"
    );
    assert_eq!(up.spec.table, "enrich_owners");
    assert_eq!(
        up.spec.create_sql(),
        "CREATE TABLE IF NOT EXISTS `public`.`enrich_owners` (`service` STRING, `tier` STRING, `owner` STRING, `weight` DOUBLE, `live` BOOLEAN, `zip` STRING, `ts` TIMESTAMP(3) NOT NULL, TIME INDEX (`ts`), PRIMARY KEY (`service`))"
    );
    // A keyed table upserts at one time index; an unkeyed one keeps every row.
    assert_eq!(up.rows[0].last(), up.rows[1].last());
    let un = enrich::parse_upload(&org(), "owners", CSV, &[]).unwrap();
    assert_ne!(un.rows[0].last(), un.rows[1].last());
    assert!(!un.spec.create_sql().contains("PRIMARY KEY"));
    // Cells become typed binds; an empty text cell is empty text, an empty number is NULL.
    assert_eq!(up.rows[2][2], Bind::Str(String::new()));
    assert_eq!(up.rows[0][3], Bind::Float(1.5));
    assert_eq!(up.rows[0][4], Bind::Bool(true));
    let n = enrich::parse_upload(&org(), "n", "k,v\na,1\nb,\nc,3\n", &["k".to_string()]).unwrap();
    assert_eq!(
        (n.spec.columns[1].1, &n.rows[1][1]),
        (enrich::Ty::Bigint, &Bind::Null)
    );
    // Quoted cells with commas, quotes and newlines survive.
    let q = enrich::parse_upload(&org(), "q", "k,v\na,\"x, \"\"y\"\"\nz\"\n", &[]).unwrap();
    assert_eq!(q.rows[0][1], Bind::Str("x, \"y\"\nz".into()));
}

#[test]
fn ac_0_10_3_csv_names_validated() {
    let up = |name: &str, csv: &str, key: &[&str]| {
        enrich::parse_upload(
            &org(),
            name,
            csv,
            &key.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        )
    };
    for bad in [
        "",
        "Owners",
        "a-b",
        "a.b",
        "a b",
        "x`; DROP TABLE t; --",
        &"a".repeat(51),
    ] {
        assert!(up(bad, "k,v\na,1\n", &[]).is_err(), "table {bad:?}");
    }
    for (csv, why) in [
        ("k`x,v\na,1\n", "backtick in a column"),
        ("1k,v\na,1\n", "starts with a digit"),
        ("k k,v\na,1\n", "space"),
        ("k,K\na,1\n", "duplicate ignoring case"),
        ("k,ts\na,1\n", "reserved ts"),
        ("k,v\n", "no data row"),
        ("k,v\na\n", "short row"),
        ("k,v\na,1,2\n", "long row"),
        ("k,v\n\"a,1\n", "open quote"),
        (&format!("k,v\na,{}\n", "x".repeat(501)), "long cell"),
        ("k,v\na,\u{7}\n", "control character"),
    ] {
        assert!(up("t", csv, &[]).is_err(), "{why}");
    }
    // Caps.
    let wide = (0..31)
        .map(|i| format!("c{i}"))
        .collect::<Vec<_>>()
        .join(",");
    assert!(
        up("t", &format!("{wide}\n{}\n", vec!["1"; 31].join(",")), &[]).is_err(),
        "31 columns"
    );
    let many: String = (0..5001).map(|i| format!("{i},x\n")).collect();
    assert!(up("t", &format!("k,v\n{many}"), &[]).is_err(), "5001 rows");
    assert!(
        up("t", &format!("k,v\n{}", "a,b\n".repeat(5000)), &[]).is_ok(),
        "5000 rows"
    );
    assert!(
        up("t", &format!("k,v\na,{}\n", "x".repeat(1 << 20)), &[]).is_err(),
        "over 1 MB"
    );
    // Key columns: must exist, be text or whole numbers, hold no empty cells.
    assert!(up("t", "k,v\na,1\n", &["nope"]).is_err());
    assert!(up("t", "k,v\na,1.5\n", &["v"]).is_err());
    assert!(up("t", "k,v\n,1\n", &["k"]).is_err());
    assert!(up("t", "k,v\na,1\n", &["k", "k"]).is_err());
    assert!(up("t", "k,v\na,1\n", &["k"]).is_ok());
}

#[test]
fn ac_0_10_3_admin_gate_accepts_only_generated_statements() {
    let o = org();
    let up = enrich::parse_upload(&o, "owners", CSV, &["service".to_string()]).unwrap();
    let create = up.spec.create_sql();
    assert!(admin::check(&create, 0, &o).is_ok());
    assert!(admin::check(&up.spec.drop_sql(), 0, &o).is_ok());
    let ins = up.spec.insert(&up.rows[..2]);
    let body = ins.sql.strip_prefix("/* q:enrich_insert */ ").unwrap();
    assert!(admin::check(body, ins.binds.len(), &o).is_ok());
    assert_eq!(ins.binds.len(), 2 * 7);
    assert!(
        admin::check(body, ins.binds.len() + 1, &o).is_err(),
        "the bind count is part of the shape"
    );
    // Tampering: another database, a table that is not an enrichment table, extra statements,
    // different text.
    let other = Org {
        database: "other".into(),
        ..o.clone()
    };
    assert!(admin::check(&create, 0, &other).is_err(), "wrong database");
    for bad in [
        create.replace("`enrich_owners`", "`opentelemetry_traces`"),
        create.replace("`enrich_owners`", "`grepui_views`"),
        format!("{create}; DROP TABLE x"),
        create.replace("STRING,", "STRING DEFAULT 'x',"),
        create.replace("`public`.", "`grepui`."),
        "DROP TABLE IF EXISTS `public`.`opentelemetry_traces`".to_string(),
        "DROP TABLE IF EXISTS `public`.`enrich_a`; DROP TABLE x".to_string(),
        "DROP TABLE `public`.`enrich_a`".to_string(),
        body.replace(
            "INSERT INTO `public`.`enrich_owners`",
            "INSERT INTO `public`.`opentelemetry_logs`",
        ),
        body.replace("VALUES", "SELECT 1 UNION SELECT"),
        body.replace(", `ts`)", ")"),
    ] {
        assert!(
            admin::check(
                &bad,
                if bad.contains("INSERT") {
                    ins.binds.len()
                } else {
                    0
                },
                &o
            )
            .is_err(),
            "{bad}"
        );
    }
    assert_eq!(admin::ADMIN_WRITES.len(), 10);
}

fn enrich_app(admin: bool, existing: Vec<&'static str>) -> (Router, Arc<MemStore>) {
    let store = MemStore::new();
    let names: Vec<Value> = existing.iter().map(|n| json!({"table_name": n})).collect();
    *store.script.lock().unwrap() = Some(Box::new(move |q: &Q| {
        Ok(if q.name == "enrich_tables" {
            names.clone()
        } else {
            sample_db(q)?
        })
    }));
    let (a, _) = app_cfg(
        &store,
        &clock(),
        Config {
            admin_writes: admin,
            ..Config::default()
        },
    );
    (a, store)
}

#[tokio::test]
async fn ac_0_10_3_upload_creates_table_in_chunks() {
    let (a, store) = enrich_app(true, vec![]);
    let csv: String = std::iter::once("id,name\n".to_string())
        .chain((0..450).map(|i| format!("{i},n{i}\n")))
        .collect();
    let r = send_text(
        &a,
        Method::POST,
        &format!("{BASE}/enrichment/people?key=id"),
        &csv,
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let d = r.json()["data"].clone();
    assert_eq!(
        (d["rows"].as_i64(), d["table"].as_str()),
        (Some(450), Some("enrich_people"))
    );
    assert_eq!(d["columns"][0], json!({"name": "id", "type": "BIGINT"}));
    let ddl = store.ddl.lock().unwrap().clone();
    assert!(
        ddl.iter().any(|s| s.starts_with(
            "CREATE TABLE IF NOT EXISTS `public`.`enrich_people` (`id` BIGINT, `name` STRING, `ts`"
        )),
        "{ddl:?}"
    );
    let ins: Vec<Q> = store
        .writes
        .lock()
        .unwrap()
        .iter()
        .filter(|q| q.name == "enrich_insert")
        .cloned()
        .collect();
    assert_eq!(
        ins.iter().map(|q| q.binds.len() / 3).collect::<Vec<_>>(),
        [200, 200, 50],
        "chunks of 200 rows"
    );
    assert!(ins[0].sql.contains("VALUES (?, ?, ?), (?, ?, ?)"));
    assert!(!ins[0].sql.contains("n0"), "values are bound");
    // Without admin writes nothing happens; with an existing table it needs replace.
    let (viewer, vs) = enrich_app(false, vec![]);
    assert_eq!(
        send_text(
            &viewer,
            Method::POST,
            &format!("{BASE}/enrichment/people"),
            &csv
        )
        .await
        .status,
        StatusCode::FORBIDDEN
    );
    assert!(vs.ddl.lock().unwrap().is_empty());
    let (b, bs) = enrich_app(true, vec!["enrich_people"]);
    assert_eq!(
        send_text(
            &b,
            Method::POST,
            &format!("{BASE}/enrichment/people"),
            "k\na\n"
        )
        .await
        .status,
        StatusCode::CONFLICT
    );
    assert!(bs.ddl.lock().unwrap().is_empty());
    let r = send_text(
        &b,
        Method::POST,
        &format!("{BASE}/enrichment/people?replace=true"),
        "k\na\n",
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let ddl = bs.ddl.lock().unwrap().clone();
    assert!(
        ddl[0].starts_with("DROP TABLE IF EXISTS `public`.`enrich_people`")
            && ddl[1].starts_with("CREATE TABLE"),
        "{ddl:?}"
    );
    // A bad upload changes nothing.
    let (c, cs) = enrich_app(true, vec![]);
    assert_eq!(
        send_text(
            &c,
            Method::POST,
            &format!("{BASE}/enrichment/x"),
            "k`,v\na,1\n"
        )
        .await
        .status,
        StatusCode::BAD_REQUEST
    );
    assert!(cs.ddl.lock().unwrap().is_empty());
    // Drop only touches enrichment tables.
    let (d, ds) = enrich_app(true, vec!["enrich_people"]);
    assert_eq!(
        send_json(
            &d,
            Method::DELETE,
            &format!("{BASE}/enrichment/people"),
            &json!({})
        )
        .await
        .status,
        StatusCode::OK
    );
    assert_eq!(
        ds.ddl.lock().unwrap()[0],
        "DROP TABLE IF EXISTS `public`.`enrich_people`"
    );
    assert_eq!(
        send_json(
            &d,
            Method::DELETE,
            &format!("{BASE}/enrichment/opentelemetry_traces"),
            &json!({})
        )
        .await
        .status,
        StatusCode::NOT_FOUND
    );
    // The upload route accepts up to 1 MB while the rest of the API keeps its small limit.
    let big = format!("k,v\n{}", "abcdefgh,ijklmnop\n".repeat(20_000));
    let r = send_text(&a, Method::POST, &format!("{BASE}/enrichment/big"), &big).await;
    assert_eq!(
        r.status,
        StatusCode::BAD_REQUEST,
        "400 for the 5000-row cap, not 413 for the body: {}",
        r.text()
    );
    let r = send_json(
        &a,
        Method::POST,
        &format!("{BASE}/views"),
        &json!({"name": "x".repeat(70_000), "kind": "logs", "spec": {}}),
    )
    .await;
    assert_eq!(r.status, StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn ac_0_10_3_join_helper_builds_sql() {
    let (a, _) = enrich_app(true, vec![]);
    let j = |q: &str| {
        let (a, url) = (a.clone(), format!("{BASE}/enrichment/owners/join?{q}"));
        async move { get(&a, &url).await }
    };
    let r = j("stream=traces&left=service&key=service&columns=tier,owner").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let sql = r.json()["data"]["sql"].as_str().unwrap().to_string();
    assert_eq!(
        sql,
        "SELECT e.`tier`, e.`owner`, count(*) AS n FROM `public`.`opentelemetry_traces` AS l LEFT JOIN `public`.`enrich_owners` AS e ON l.`service_name` = e.`service` WHERE l.`timestamp` >= $__from AND l.`timestamp` < $__to GROUP BY 1, 2 ORDER BY n DESC LIMIT 100"
    );
    // The generated query is something SQL mode accepts.
    grepui::query::sqlmode::check(
        &grepui::query::sqlmode::expand_macros(&sql, 1000, 2000),
        "public",
    )
    .unwrap();
    let logs = j("stream=logs&left=attr:http.method&key=m&columns=owner")
        .await
        .json()["data"]["sql"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        logs.contains("json_get_string(l.`log_attributes`, '[\"http.method\"]') = e.`m`"),
        "{logs}"
    );
    assert!(
        j("stream=logs&left=service&key=service&columns=tier")
            .await
            .json()["data"]["sql"]
            .as_str()
            .unwrap()
            .contains("resource_attributes")
    );
    // Every part is validated: nothing from the request is pasted unchecked.
    for q in [
        "stream=metrics&left=service&key=k&columns=a",
        "stream=logs&left=attr:a'%20OR%201=1&key=k&columns=a",
        "stream=logs&left=column:a%60&key=k&columns=a",
        "stream=logs&left=service&key=k%60&columns=a",
        "stream=logs&left=service&key=k&columns=a%60,b",
        "stream=logs&left=service&key=k&columns=",
        "stream=traces&left=attr:x&key=k&columns=a",
        "stream=logs&left=service;DROP&key=k&columns=a",
    ] {
        assert_eq!(j(q).await.status, StatusCode::BAD_REQUEST, "{q}");
    }
    assert_eq!(
        get(
            &a,
            &format!("{BASE}/enrichment/Bad%20Name/join?stream=logs&left=service&key=k&columns=a")
        )
        .await
        .status,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
#[ignore = "needs GreptimeDB: make db-up && make test-db"]
async fn ac_0_10_3_enrichment_join_live() {
    let base = std::env::var("GREPUI_TEST_DB")
        .expect("set GREPUI_TEST_DB=mysql://root@127.0.0.1:4002/public");
    let data_db = format!("demo_it_enrich_{}", std::process::id());
    let form = [("content-type", "application/x-www-form-urlencoded")];
    http::post(
        "http://127.0.0.1:4000",
        "/v1/sql",
        &form,
        format!("sql=CREATE DATABASE IF NOT EXISTS {data_db}").as_bytes(),
    )
    .await
    .unwrap();
    let batch = synth::generate(
        13,
        40,
        chrono::Utc::now().timestamp_nanos_opt().unwrap() as u64,
        30,
    );
    grepui::playground::send(
        "http://127.0.0.1:4000",
        &data_db,
        "opentelemetry_traces",
        "opentelemetry_logs",
        &batch,
    )
    .await
    .expect("seed");
    let url = base.replace("/public", &format!("/{data_db}"));
    let r = Arc::new(SqlxReader::lazy(&url, Duration::from_secs(30)).unwrap());
    let state = grepui::api::AppState::with_parts(
        Config {
            greptime_mysql: url,
            admin_writes: true,
            ..Config::default()
        },
        r.clone(),
        r,
        grepui::api::system_clock(),
    )
    .unwrap();
    let app = grepui::api::router(state);
    // An owner table for the demo services; the last service is left out on purpose.
    let services: Vec<&str> = {
        let mut v: Vec<&str> = batch
            .spans
            .iter()
            .map(|s| s.service)
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        v.sort();
        v
    };
    let tier = |i: usize| {
        if i.is_multiple_of(2) {
            "gold"
        } else {
            "silver"
        }
    };
    let mut csv = String::from("service,tier,weight\n");
    for (i, s) in services.iter().enumerate().take(services.len() - 1) {
        csv.push_str(&format!("{s},{},{}\n", tier(i), i + 1));
    }
    let r = send_text(
        &app,
        Method::POST,
        &format!("{BASE}/enrichment/svc?key=service"),
        &csv,
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    // Re-uploading replaces rows in place (same key, same time index): still one row per service.
    assert_eq!(
        send_text(
            &app,
            Method::POST,
            &format!("{BASE}/enrichment/svc?key=service"),
            &csv
        )
        .await
        .status,
        StatusCode::CONFLICT
    );
    let r = send_text(
        &app,
        Method::POST,
        &format!("{BASE}/enrichment/svc?key=service&replace=true"),
        &csv,
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED);
    let listed = get(&app, &format!("{BASE}/enrichment")).await.json()["data"]["tables"].clone();
    assert_eq!(listed[0]["rows"], services.len() as i64 - 1);
    let prev = get(&app, &format!("{BASE}/enrichment/svc")).await.json()["data"].clone();
    assert_eq!(prev["rows"].as_array().unwrap().len(), services.len() - 1);
    // The helper's query, run through SQL mode, counts spans per tier exactly; spans of the
    // service without a row fall into the NULL group of the left join.
    let sql = get(
        &app,
        &format!("{BASE}/enrichment/svc/join?stream=traces&left=service&key=service&columns=tier"),
    )
    .await
    .json()["data"]["sql"]
        .as_str()
        .unwrap()
        .to_string();
    let enc: String = sql
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect();
    let now = chrono::Utc::now().timestamp_millis();
    let r = get(
        &app,
        &format!(
            "{BASE}/sql?sql={enc}&from={}&to={}",
            now - 7_200_000,
            now + 60_000
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let mut want: HashMap<Option<&str>, i64> = HashMap::new();
    for sp in &batch.spans {
        let t = services
            .iter()
            .position(|s| *s == sp.service)
            .filter(|i| *i < services.len() - 1)
            .map(tier);
        *want.entry(t).or_default() += 1;
    }
    let mut got: HashMap<Option<String>, i64> = HashMap::new();
    for row in r.json()["data"]["rows"].as_array().unwrap() {
        got.insert(
            row["tier"].as_str().map(String::from),
            row["n"].as_i64().unwrap(),
        );
    }
    assert_eq!(got.len(), want.len(), "{got:?} vs {want:?}");
    for (k, v) in want {
        assert_eq!(got[&k.map(String::from)], v, "{k:?}");
    }
    let _ = send_json(
        &app,
        Method::DELETE,
        &format!("{BASE}/enrichment/svc"),
        &json!({}),
    )
    .await;
    let _ = http::post(
        "http://127.0.0.1:4000",
        "/v1/sql",
        &form,
        format!("sql=DROP DATABASE IF EXISTS {data_db}").as_bytes(),
    )
    .await;
}

// ---- AC0.10.4 --------------------------------------------------------------------------------

fn log_with_secrets() -> Value {
    let mut r = log_row("1793399269857388008", "ERROR", "login failed");
    r["log_attributes"] = json!({
        "http.method": "POST", "user.password": "hunter2",
        "http.request.header.authorization": "Bearer abcdef123456",
        "nested": {"card": "4111111111111111", "ok": 1},
        "payload": "{\"password\":\"p4ss\",\"x\":1}",
    });
    r
}

/// Two apps over one store: an admin one and a viewer one that loads the rules from the store.
async fn redaction_apps() -> (Router, Router, Arc<MemStore>) {
    let store = MemStore::new();
    let row = log_with_secrets();
    *store.script.lock().unwrap() = Some(Box::new(move |q: &Q| {
        Ok(match q.name {
            "logs_search" | "logs_record" => vec![row.clone()],
            "sql_mode" => {
                vec![json!({"user": "a", "password": "hunter2", "attrs": {"api_key": "k-123"}})]
            }
            _ => sample_db(q)?,
        })
    }));
    let c = clock();
    let (admin_app, admin_state) = app_cfg(
        &store,
        &c,
        Config {
            admin_writes: true,
            ..Config::default()
        },
    );
    let (viewer_app, viewer_state) = app_cfg(&store, &c, Config::default());
    for (field, mask) in [
        ("password", "full"),
        ("authorization", "hash"),
        ("card", "partial"),
        ("api_key", "full"),
    ] {
        let r = send_json(
            &admin_app,
            Method::POST,
            &format!("{BASE}/redaction"),
            &json!({"field": field, "mask": mask, "exempt_roles": ["admin"]}),
        )
        .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    }
    // The viewer replica picks the rules up from the store, as the refresh task does.
    grepui::api::redaction::reload(&viewer_state, viewer_state.org("default").unwrap())
        .await
        .unwrap();
    let _ = admin_state;
    (admin_app, viewer_app, store)
}

#[tokio::test]
async fn ac_0_10_4_redaction_masks_response() {
    let (admin_app, viewer, _) = redaction_apps().await;
    let path = format!("{BASE}/logs?from=1000&to=2000");
    let v = get(&viewer, &path).await;
    assert_eq!(v.status, StatusCode::OK, "{}", v.text());
    assert_eq!(v.header("x-grepui-redacted"), "1");
    let text = v.text();
    for secret in ["hunter2", "abcdef123456", "4111111111111111", "p4ss"] {
        assert!(!text.contains(secret), "{secret} leaked: {text}");
    }
    let row = &v.json()["data"]["rows"][0];
    let attrs = if row["log_attributes"].is_object() {
        row["log_attributes"].clone()
    } else {
        row.clone()
    };
    let _ = attrs;
    assert!(text.contains("[redacted]"));
    assert!(
        text.contains("************1111"),
        "partial keeps the last four"
    );
    // The role that is exempt sees the real values.
    let a = get(&admin_app, &path).await;
    assert!(a.text().contains("hunter2") && a.text().contains("Bearer abcdef123456"));
    assert_eq!(a.header("x-grepui-redacted"), "");
    // SQL mode: column and nested keys.
    let s = get(&viewer, &sql_get("SELECT user, password FROM t")).await;
    assert!(
        !s.text().contains("hunter2") && !s.text().contains("k-123"),
        "{}",
        s.text()
    );
    assert!(
        s.json()["data"]["rows"][0]["user"] == "a",
        "other fields are untouched"
    );
    // The same hash for the same value: it can be used to correlate.
    let h1 = get(&viewer, &path).await.json();
    let h2 = get(&viewer, &path).await.json();
    assert_eq!(h1, h2);
    // The query description is not masked, and error responses pass through.
    assert!(
        v.json()["meta"]["sql"]
            .as_str()
            .unwrap()
            .contains("logs_search")
    );
    assert_eq!(
        get(&viewer, &format!("{BASE}/logs")).await.status,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn ac_0_10_4_redaction_masks_export() {
    let (admin_app, viewer, _) = redaction_apps().await;
    let csv = get(
        &viewer,
        &format!("{BASE}/logs/export?from=1000&to=2000&format=csv"),
    )
    .await;
    assert_eq!(csv.status, StatusCode::OK, "{}", csv.text());
    let t = csv.text();
    for secret in ["hunter2", "abcdef123456", "4111111111111111", "p4ss"] {
        assert!(!t.contains(secret), "{secret} leaked in csv: {t}");
    }
    assert!(
        t.starts_with("timestamp") || t.lines().next().unwrap().contains("body"),
        "{t}"
    );
    assert!(t.contains("login failed"), "other cells are intact");
    let nd = get(
        &viewer,
        &format!("{BASE}/logs/export?from=1000&to=2000&format=ndjson"),
    )
    .await;
    let t = nd.text();
    for secret in ["hunter2", "abcdef123456", "4111111111111111", "p4ss"] {
        assert!(!t.contains(secret), "{secret} leaked in ndjson: {t}");
    }
    let line: Value = serde_json::from_str(t.lines().next().unwrap()).unwrap();
    assert_eq!(line["body"], "login failed");
    assert_eq!(line["log_attributes"]["http.method"], "POST");
    // The exempt role still gets the real export.
    let real = get(
        &admin_app,
        &format!("{BASE}/logs/export?from=1000&to=2000&format=ndjson"),
    )
    .await
    .text();
    assert!(real.contains("hunter2"));
    // The rules file itself: a CSV column named like a rule is masked by header.
    let rules = Rules(vec![Rule {
        field: "password".into(),
        mask: "full".into(),
        exempt_roles: vec![],
    }]);
    let out = rules.apply_csv("user,password\na,x\nb,\"y,z\"\n", "viewer");
    assert_eq!(out, "user,password\na,[redacted]\nb,[redacted]\n");
    assert_eq!(
        rules.apply_ndjson("{\"password\":1}\nnot json\n", "viewer"),
        "{\"password\":\"[redacted]\"}\n[redacted]\n",
        "a line that cannot be checked is not passed on"
    );
}

#[tokio::test]
async fn ac_0_10_4_redaction_json_attributes() {
    let rules = Rules(vec![
        Rule {
            field: "authorization".into(),
            mask: "full".into(),
            exempt_roles: vec![],
        },
        Rule {
            field: "token".into(),
            mask: "partial".into(),
            exempt_roles: vec![],
        },
    ]);
    let mut v = json!({
        "a": {"b": [{"http.request.header.Authorization": "Bearer x"}, {"TOKEN": "abcdefgh"}]},
        "s": "[{\"x\": {\"token\": \"12345678\"}}]",
        "list": ["no", 1, null],
        "authorization": {"nested": "object"},
        "keep": "authorizations",
    });
    assert!(rules.apply_value(&mut v, "viewer"));
    assert_eq!(
        v["a"]["b"][0]["http.request.header.Authorization"], "[redacted]",
        "dotted keys and any case"
    );
    assert_eq!(v["a"]["b"][1]["TOKEN"], "****efgh");
    assert!(
        v["s"].as_str().unwrap().contains("****5678"),
        "JSON inside a string"
    );
    assert_eq!(
        v["authorization"], "[redacted]",
        "an object under a rule is replaced whole"
    );
    assert_eq!(
        v["keep"], "authorizations",
        "only the whole key or a dotted suffix matches"
    );
    assert_eq!(v["list"], json!(["no", 1, null]));
    // Rules validate.
    let (a, _) = app_cfg(
        &MemStore::new(),
        &clock(),
        Config {
            admin_writes: true,
            ..Config::default()
        },
    );
    for bad in [
        json!({"field": ""}),
        json!({"field": "x", "mask": "rot13"}),
        json!({"field": "x", "exempt_roles": ["root"]}),
        json!({"field": "x", "extra": 1}),
        json!({"field": "a\"b"}),
    ] {
        assert_eq!(
            send_json(&a, Method::POST, &format!("{BASE}/redaction"), &bad)
                .await
                .status,
            StatusCode::BAD_REQUEST,
            "{bad}"
        );
    }
}

#[tokio::test]
async fn ac_0_10_4_redaction_rules_need_admin_and_fail_closed() {
    let store = MemStore::new();
    let (viewer, state) = app_cfg(&store, &clock(), Config::default());
    // Rules are a security setting: only the admin stand-in changes them, and viewers can read.
    let r = send_json(
        &viewer,
        Method::POST,
        &format!("{BASE}/redaction"),
        &json!({"field": "x"}),
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert_eq!(
        get(&viewer, &format!("{BASE}/redaction")).await.status,
        StatusCode::OK
    );
    // Until the rules have loaded at startup, data responses wait instead of leaking.
    state.redact_ready.store(false, Ordering::SeqCst);
    let r = get(&viewer, &format!("{BASE}/logs?from=1000&to=2000")).await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        get(&viewer, "/api/v1/health").await.status,
        StatusCode::OK,
        "health does not wait"
    );
    assert_eq!(
        get(&viewer, &format!("{BASE}/redaction")).await.status,
        StatusCode::OK,
        "the rules page does not wait"
    );
    state.redact_ready.store(true, Ordering::SeqCst);
    assert_eq!(
        get(&viewer, &format!("{BASE}/logs?from=1000&to=2000"))
            .await
            .status,
        StatusCode::OK
    );
    // Update and delete take effect at once on the replica that made the change.
    let c = clock();
    let (admin_app, _) = app_cfg(
        &store,
        &c,
        Config {
            admin_writes: true,
            ..Config::default()
        },
    );
    let id = send_json(
        &admin_app,
        Method::POST,
        &format!("{BASE}/redaction"),
        &json!({"field": "body"}),
    )
    .await
    .json()["data"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let r = send_json(
        &admin_app,
        Method::PUT,
        &format!("{BASE}/redaction/{id}"),
        &json!({"field": "body", "mask": "full", "exempt_roles": []}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let masked = get(&admin_app, &format!("{BASE}/logs?from=1000&to=2000"))
        .await
        .text();
    assert!(
        masked.contains("[redacted]"),
        "now masked even for the admin role: {masked}"
    );
    send_json(
        &admin_app,
        Method::DELETE,
        &format!("{BASE}/redaction/{id}"),
        &json!({}),
    )
    .await;
    assert!(
        !get(&admin_app, &format!("{BASE}/logs?from=1000&to=2000"))
            .await
            .text()
            .contains("[redacted]")
    );
    assert_eq!(
        send_json(
            &admin_app,
            Method::DELETE,
            &format!("{BASE}/redaction/{id}"),
            &json!({})
        )
        .await
        .status,
        StatusCode::NOT_FOUND
    );
    let _ = store::SCHEMA_VERSION;
}

#[tokio::test]
async fn ac_0_10_4_public_links_are_always_viewers() {
    // Even where --admin-writes makes the caller an admin, a public dashboard link is a viewer.
    let store = MemStore::new();
    *store.script.lock().unwrap() = Some(Box::new(|q: &Q| {
        Ok(if q.name == "dashboard_panel" {
            vec![json!({"user": "a", "password": "hunter2"})]
        } else {
            sample_db(q)?
        })
    }));
    let (a, _) = app_cfg(
        &store,
        &clock(),
        Config {
            admin_writes: true,
            ..Config::default()
        },
    );
    send_json(
        &a,
        Method::POST,
        &format!("{BASE}/redaction"),
        &json!({"field": "password", "exempt_roles": ["admin"]}),
    )
    .await;
    let spec = json!({"tabs": [{"title": "T", "panels": [{"id": "p", "type": "table", "title": "t", "query": {"kind": "sql", "text": "SELECT user, password FROM t"}}]}]});
    let id = send_json(
        &a,
        Method::POST,
        &format!("{BASE}/dashboards"),
        &json!({"title": "D", "spec": spec}),
    )
    .await
    .json()["data"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let inside = get(
        &a,
        &format!("{BASE}/dashboards/{id}/panels/p/data?from=1000&to=2000"),
    )
    .await
    .text();
    assert!(inside.contains("hunter2"));
    let tok = send_json(
        &a,
        Method::POST,
        &format!("{BASE}/dashboards/{id}/shares"),
        &json!({"ttl_hours": 1}),
    )
    .await
    .json()["data"]["token"]
        .as_str()
        .unwrap()
        .to_string();
    let public = get(
        &a,
        &format!("{BASE}/public/{tok}/panels/p/data?from=1000&to=2000"),
    )
    .await;
    assert_eq!(public.status, StatusCode::OK, "{}", public.text());
    assert!(!public.text().contains("hunter2"), "{}", public.text());
}

#[test]
fn ac_0_10_2_demo_function_is_valid() {
    let f: Function = serde_json::from_str(grepui::playground::DEMO_FUNCTION).unwrap();
    assert!(f.canonical().is_ok());
}

#[tokio::test]
async fn ac_0_10_4_redaction_cannot_be_dodged_by_spelling_the_path() {
    let (admin_app, viewer, _) = redaction_apps().await;
    let _ = admin_app;
    // Percent-encoded org, upper-case route words and a trailing slash all reach the same handler
    // or none at all; the response is masked whenever it is served.
    for path in [
        "/api/v1/orgs/%64efault/logs?from=1000&to=2000",
        "/api/v1/orgs/d%65fault/logs?from=1000&to=2000",
        "/api/v1/orgs/default/%6cogs?from=1000&to=2000",
    ] {
        let r = get(&viewer, path).await;
        if r.status == StatusCode::OK {
            assert!(
                !r.text().contains("hunter2") && r.header("x-grepui-redacted") == "1",
                "{path}: {}",
                r.text()
            );
        }
    }
    // A data route whose path merely mentions "redaction" is still masked.
    let r = get(
        &viewer,
        &format!("{BASE}/sql?sql=SELECT%20password%20FROM%20redaction&from=1000&to=2000"),
    )
    .await;
    assert!(!r.text().contains("hunter2"), "{}", r.text());
    // Public links stay viewers even with an encoded `public` segment.
    let r = get(&viewer, &format!("{BASE}/%70ublic/x/variables")).await;
    assert!(r.status.is_client_error() || !r.text().contains("hunter2"));
}
