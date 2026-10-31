//! AC0.1.1 – AC0.1.5: config, health, embedded UI, read-only routes, org routing.
mod common;
use axum::http::{Method, StatusCode};
use clap::{CommandFactory, Parser};
use common::*;
use grepui::{
    api::{self, ROUTES},
    config::Config,
    error::ReadError,
    org::{self, Orgs},
    probe,
    query::{self, WRITE_TARGETS, build},
};
use serde_json::json;

#[derive(Parser)]
struct Wrap {
    #[command(flatten)]
    cfg: Config,
}

fn parse(args: &[&str]) -> Config {
    let mut v = vec!["x"];
    v.extend_from_slice(args);
    Wrap::try_parse_from(v).expect("parse").cfg
}

// ---- AC0.1.1 config ---------------------------------------------------------

#[test]
fn ac_0_1_1_defaults_and_overrides() {
    let d = parse(&[]);
    assert_eq!(d.greptime_mysql, "mysql://root@127.0.0.1:4002/public");
    assert_eq!(d.database().unwrap(), "public");
    assert_eq!(
        (d.traces_table.as_str(), d.logs_table.as_str()),
        ("opentelemetry_traces", "opentelemetry_logs")
    );
    d.validate().unwrap();

    let o = parse(&[
        "--listen",
        "0.0.0.0:9",
        "--greptime-mysql",
        "mysql://u:p@db:4002/tenant_a",
        "--traces-table",
        "spans",
    ]);
    assert_eq!(o.listen, "0.0.0.0:9");
    assert_eq!(o.database().unwrap(), "tenant_a");
    assert_eq!(o.traces_table, "spans");

    // Environment names are part of the contract (compose and the Dockerfile use them).
    let cmd = Wrap::command();
    let env_of = |id: &str| {
        cmd.get_arguments()
            .find(|a| a.get_id() == id)
            .and_then(|a| a.get_env())
            .map(|e| e.to_string_lossy().into_owned())
    };
    assert_eq!(env_of("listen").as_deref(), Some("GREPUI_LISTEN"));
    assert_eq!(env_of("greptime_mysql").as_deref(), Some("GREPTIME_MYSQL"));
    assert_eq!(
        env_of("greptime_http_public").as_deref(),
        Some("GREPTIME_HTTP_PUBLIC")
    );
}

#[test]
fn ac_0_1_1_invalid_table_ident_rejected() {
    for bad in ["t; DROP TABLE x", "a b", "", "t`x", "a-b", "x.y"] {
        let c = Config {
            traces_table: bad.into(),
            ..Config::default()
        };
        let e = c.validate().unwrap_err().to_string();
        assert!(e.contains("--traces-table"), "{bad}: {e}");
        let c = Config {
            logs_table: bad.into(),
            ..Config::default()
        };
        assert!(
            c.validate()
                .unwrap_err()
                .to_string()
                .contains("--logs-table"),
            "{bad}"
        );
    }
    let bad_db = Config {
        greptime_mysql: "mysql://root@h:4002/bad-db".into(),
        ..Config::default()
    };
    assert!(bad_db.validate().is_err());
    let bad_url = Config {
        greptime_mysql: "not a url".into(),
        ..Config::default()
    };
    assert!(bad_url.validate().is_err());
}

// ---- AC0.1.2 health ---------------------------------------------------------

#[tokio::test]
async fn ac_0_1_2_healthz_ignores_db() {
    let f = Fake::new(|_| Err(ReadError::Db("down".into())));
    let a = app(f.clone());
    let r = get(&a, "/healthz").await;
    assert_eq!((r.status, r.text().as_str()), (StatusCode::OK, "ok"));
    assert!(
        f.seen_names().is_empty(),
        "/healthz must not touch the database"
    );
}

#[tokio::test]
async fn ac_0_1_2_health_follows_db() {
    let up = app(Fake::new(sample_db));
    let r = get(&up, "/api/v1/health").await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json()["db"], "up");

    let down = app(Fake::new(|_| Err(ReadError::Timeout)));
    let r = get(&down, "/api/v1/health").await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(r.json()["db"], "down");
}

#[tokio::test]
async fn ac_0_1_2_info_reports_version() {
    let r = get(&app(Fake::new(sample_db)), "/api/v1/info").await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json()["version"], grepui::VERSION);
    assert_eq!(r.json()["name"], "grepui");
}

#[tokio::test]
async fn ac_0_1_2_healthcheck_probe() {
    let a = app(Fake::new(sample_db));
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap().to_string();
    tokio::spawn(async move { axum::serve(l, a).await.unwrap() });
    assert!(probe::healthy(&addr).await);
    let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let dead = closed.local_addr().unwrap().to_string();
    drop(closed);
    assert!(!probe::healthy(&dead).await);
}

// ---- AC0.1.3 embedded UI ----------------------------------------------------

#[tokio::test]
async fn ac_0_1_3_spa_fallback() {
    let a = app(Fake::new(sample_db));
    for path in ["/", "/orgs/default/streams", "/anything/deep/route"] {
        let r = get(&a, path).await;
        assert_eq!(r.status, StatusCode::OK, "{path}");
        assert!(r.header("content-type").starts_with("text/html"), "{path}");
        assert!(r.text().to_lowercase().contains("<!doctype html"), "{path}");
    }
    assert_eq!(
        get(&a, "/js/does-not-exist.js").await.status,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn ac_0_1_3_api_404_is_json() {
    let a = app(Fake::new(sample_db));
    for (m, p) in [
        (Method::GET, "/api/v1/nope"),
        (Method::POST, "/api/nope"),
        (Method::GET, "/api"),
    ] {
        let r = send(&a, m, p).await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{p}");
        assert!(
            r.header("content-type").starts_with("application/json"),
            "{p}"
        );
        assert!(r.json()["error"].is_string(), "{p}");
    }
}

#[tokio::test]
async fn ac_0_1_3_asset_headers() {
    let a = app(Fake::new(sample_db));
    let css = get(&a, "/css/base.css").await;
    assert_eq!(css.status, StatusCode::OK);
    assert!(css.header("content-type").starts_with("text/css"));
    assert!(css.header("cache-control").contains("max-age"));
    assert!(!css.header("etag").is_empty());
    let js = get(&a, "/js/main.js").await;
    assert_eq!(js.status, StatusCode::OK);
    assert!(js.header("content-type").starts_with("text/javascript"));
    assert_eq!(get(&a, "/").await.header("cache-control"), "no-cache");

    // Revalidation: the same ETag answers 304 with no body; a different one gets the file.
    let etag = css.header("etag").to_string();
    let req = |tag: &str| {
        axum::http::Request::builder()
            .uri("/css/base.css")
            .header("if-none-match", tag.to_string())
            .body(axum::body::Body::empty())
            .unwrap()
    };
    let hit = tower::ServiceExt::oneshot(a.clone(), req(&etag))
        .await
        .unwrap();
    assert_eq!(hit.status(), StatusCode::NOT_MODIFIED);
    let miss = tower::ServiceExt::oneshot(a.clone(), req("\"other\""))
        .await
        .unwrap();
    assert_eq!(miss.status(), StatusCode::OK);
}

#[test]
fn ac_0_1_3_embedded_file_list_is_complete() {
    fn walk(dir: &std::path::Path, root: &std::path::Path, out: &mut Vec<String>) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, root, out);
            } else {
                out.push(
                    p.strip_prefix(root)
                        .unwrap()
                        .to_string_lossy()
                        .replace('\\', "/"),
                );
            }
        }
    }
    let root = std::path::Path::new("web/static");
    let mut on_disk = vec![];
    walk(root, root, &mut on_disk);
    assert!(
        on_disk.len() >= 10,
        "UI is split into many small files, found {}",
        on_disk.len()
    );
    for f in on_disk {
        assert!(
            grepui::ui::FILES.iter().any(|(n, _)| *n == f),
            "{f} is not embedded"
        );
    }
}

// ---- AC0.1.4 read-only by construction --------------------------------------

fn fill(route: &str) -> String {
    route
        .replace("{org}", "default")
        .replace("{name}", "t")
        .replace("{field}", "severity_text")
        .replace("{service}", "cart")
        .replace("{template}", "traces_red")
        .replace("{panel}", "p1")
        .replace("{share}", "0123456789abcdef")
        .replace("{token}", "0123456789abcdef0123456789abcdef")
        .replace("{stream}", "traces")
        .replace("{id}", "0123456789abcdef")
        .replace("{user}", "alice")
        .replace("{role}", "viewer")
        .replace("{trace_id}", "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
}

#[tokio::test]
async fn ac_0_1_4_non_get_rejected() {
    // Every route rejects a state-changing method unless it is declared in WRITE_ROUTES (v0.3.0).
    let a = app(Fake::new(sample_db));
    for route in ROUTES {
        for m in [Method::POST, Method::PUT, Method::PATCH, Method::DELETE] {
            if grepui::api::WRITE_ROUTES
                .iter()
                .any(|(wm, wp)| wp == route && *wm == m.as_str())
            {
                continue;
            }
            let r = send(&a, m.clone(), &fill(route)).await;
            assert_eq!(r.status, StatusCode::METHOD_NOT_ALLOWED, "{m} {route}");
        }
    }
}

#[tokio::test]
async fn ac_0_1_4_every_listed_route_exists() {
    let a = app(Fake::new(sample_db));
    for route in ROUTES {
        let r = get(&a, &format!("{}?from=1000&to=2000", fill(route))).await;
        // A handler may answer 404 with a JSON error ("no such dashboard"); an unrouted path
        // answers 404 with no body.
        assert!(
            r.status != StatusCode::NOT_FOUND || r.json().get("error").is_some(),
            "{route} is listed but not routed"
        );
        // Write-only routes (PUT/DELETE/POST) answer GET with 405 by design.
        let write_only = route.ends_with("/annotation")
            || route.ends_with("/rollups/{template}")
            || route.ends_with("/backfill")
            || route.ends_with("/retention/{stream}")
            || route.ends_with("/import")
            || route.ends_with("/move")
            || route.ends_with("/shares/{share}")
            || route.ends_with("/folders/{id}")
            || route.ends_with("/incident")
            || route.ends_with("/test")
            || route.ends_with("/ack")
            || route.ends_with("/resolve")
            || route.ends_with("/destinations")
            || route.ends_with("/destinations/{id}")
            || route.ends_with("/downtimes/{id}")
            || route.ends_with("/dry-run")
            || route.ends_with("/functions/{name}")
            || route.ends_with("/redaction/{id}")
            || route.ends_with("/auth/setup")
            || route.ends_with("/auth/login")
            || route.ends_with("/auth/logout")
            || route.ends_with("/auth/password")
            || route.ends_with("/platform/orgs/{org}")
            || route.ends_with("/platform/users/{user}")
            || route.ends_with("/members/{user}")
            || route.ends_with("/workgroups/{role}")
            || route.ends_with("/platform/sources/{name}")
            || route.ends_with("/cancel")
            || route.ends_with("/tokens/{id}");
        if !write_only {
            assert_ne!(r.status, StatusCode::METHOD_NOT_ALLOWED, "{route}");
        }
    }
}

#[test]
fn ac_0_1_4_builders_are_read_only() {
    let qs = build::all_for_tests();
    assert!(qs.len() >= 5);
    for q in qs {
        let tag = format!("/* q:{} */ ", q.name);
        assert!(q.sql.starts_with(&tag), "{}", q.sql);
        let body = q.sql[tag.len()..].trim_start().to_ascii_uppercase();
        assert!(
            ["SELECT ", "SHOW ", "TQL "]
                .iter()
                .any(|v| body.starts_with(v)),
            "{}",
            q.sql
        );
        assert!(!q.sql.contains(';'), "{}", q.sql);
        // `SHOW CREATE TABLE` is a read; every other statement must not mention a write verb.
        if !body.starts_with("SHOW ") {
            for w in [
                "INSERT ",
                "UPDATE ",
                "DELETE ",
                "DROP ",
                "ALTER ",
                "CREATE ",
                "TRUNCATE ",
            ] {
                assert!(!body.contains(w), "{} contains {w}", q.sql);
            }
        }
    }
}

#[test]
fn ac_0_1_4_write_allowlist_is_pinned() {
    // v0.1.0/0.2.0 had no write targets; from v0.3.0 they are exactly the metadata tables (ADR 0004).
    assert_eq!(
        WRITE_TARGETS,
        [
            "grepui_schema",
            "grepui_views",
            "grepui_history",
            "grepui_services",
            "grepui_folders",
            "grepui_dashboards",
            "grepui_shares",
            "grepui_alert_items",
            "grepui_alert_history",
            "grepui_audit"
        ]
    );
}

// ---- AC0.1.5 org routing ----------------------------------------------------

#[tokio::test]
async fn ac_0_1_5_org_routes_resolve_default() {
    let f = Fake::new(sample_db);
    let a = app_with(
        Config {
            greptime_mysql: "mysql://root@h:4002/tenant_a".into(),
            ..Config::default()
        },
        f.clone(),
    );
    let r = get(&a, "/api/v1/orgs/default/streams").await;
    assert_eq!(r.status, StatusCode::OK);
    // The database the org maps to is bound into the query, not taken from the request.
    assert_eq!(
        f.seen.lock().unwrap()[0].binds,
        vec![query::Bind::Str("tenant_a".into())]
    );
    assert_eq!(
        get(&a, "/api/v1/orgs").await.json()["orgs"],
        json!([{"name": "default", "role": "admin", "branding": {}}])
    );
}

#[tokio::test]
async fn ac_0_1_5_unknown_org_404() {
    let f = Fake::new(sample_db);
    let a = app(f.clone());
    let r = get(&a, "/api/v1/orgs/acme/streams").await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert!(
        f.seen_names().is_empty(),
        "an unknown org must not reach the database"
    );
}

#[tokio::test]
async fn ac_0_1_5_org_name_validated() {
    for ok in ["default", "acme-1", "a_b", "x"] {
        assert!(org::valid_name(ok), "{ok}");
    }
    for bad in [
        "",
        "Acme",
        "a b",
        "../x",
        "a/b",
        "-x",
        "x'y",
        &"a".repeat(33),
    ] {
        assert!(!org::valid_name(bad), "{bad}");
    }
    let a = app(Fake::new(sample_db));
    assert_eq!(
        get(&a, "/api/v1/orgs/Bad%20Name/streams").await.status,
        StatusCode::BAD_REQUEST
    );
    let orgs = Orgs::from_config(&Config::default()).unwrap();
    assert!(orgs.resolve("nope").is_err() && orgs.resolve("default").is_ok());
    let _ = api::ROUTES;
}
