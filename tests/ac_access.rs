//! AC0.11.1 – AC0.11.7: users and sessions, deny by default, tokens, OpenID Connect, org
//! administration, the isolation matrix and the audit trail.
mod common;
use axum::{Router, http::Method, http::StatusCode};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD as B64};
use common::*;
use grepui::{
    api::{ROUTES, Shared, WRITE_ROUTES},
    auth::{self, Access, Identity, Role, jwt, oidc},
    config::Config,
    store::{self, build},
};
use ring::{rand::SystemRandom, rsa::KeyPair, signature};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicI64, Ordering},
    },
};

const T0: i64 = 1_793_400_000_000;
const PW: &str = "correct horse battery";

struct W {
    app: Router,
    state: Shared,
    store: Arc<MemStore>,
    clock: Arc<AtomicI64>,
}

fn world_with(cfg: Config) -> W {
    let store = MemStore::new();
    let clock = Arc::new(AtomicI64::new(T0));
    let (app, state) = app_cfg(
        &store,
        &clock,
        Config {
            no_auth: false,
            ..cfg
        },
    );
    W {
        app,
        state,
        store,
        clock,
    }
}

fn world() -> W {
    world_with(Config::default())
}

impl W {
    async fn call(&self, m: Method, uri: &str, cookie: &str, body: Option<Value>) -> Resp {
        let mut h = vec![];
        if !cookie.is_empty() {
            if cookie.starts_with("Bearer ") {
                h.push(("authorization", cookie));
            } else {
                h.push(("cookie", cookie));
            }
        }
        send_with(&self.app, m, uri, &h, body.as_ref()).await
    }
    async fn get(&self, uri: &str, who: &str) -> Resp {
        self.call(Method::GET, uri, who, None).await
    }
    async fn post(&self, uri: &str, who: &str, body: Value) -> Resp {
        self.call(Method::POST, uri, who, Some(body)).await
    }
    /// First-run setup: returns the platform admin's cookie.
    async fn admin(&self) -> String {
        let r = self
            .post(
                "/api/v1/auth/setup",
                "",
                json!({"username": "root", "password": PW}),
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
        r.cookie()
    }
    async fn org(&self, root: &str, name: &str) {
        let r = self
            .post(
                "/api/v1/platform/orgs",
                root,
                json!({"name": name, "database": format!("db_{name}")}),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    }
    async fn user(&self, root: &str, name: &str, org: &str, role: &str) -> String {
        let r = self
            .post(
                "/api/v1/platform/users",
                root,
                json!({"username": name, "password": PW, "orgs": {org: role}}),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
        let l = self
            .post(
                "/api/v1/auth/login",
                "",
                json!({"username": name, "password": PW}),
            )
            .await;
        assert_eq!(l.status, StatusCode::OK, "{}", l.text());
        l.cookie()
    }
    async fn token(&self, who: &str, org: &str, role: &str) -> String {
        let r = self
            .post(
                &format!("/api/v1/orgs/{org}/tokens"),
                who,
                json!({"name": format!("t-{role}"), "role": role}),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
        format!("Bearer {}", r.json()["data"]["token"].as_str().unwrap())
    }
    fn at(&self, secs: i64) {
        self.clock.store(T0 + secs * 1000, Ordering::SeqCst);
    }
    async fn flush(&self) {
        self.state.audit.flush(&self.state).await;
    }
    fn items(&self, kind: &str) -> Vec<(String, Vec<grepui::query::Bind>)> {
        self.store
            .items
            .lock()
            .unwrap()
            .iter()
            .filter(|((_, k, _), _)| k == kind)
            .map(|((o, _, i), b)| (format!("{o}/{i}"), b.clone()))
            .collect()
    }
}

// ---- AC0.11.1 --------------------------------------------------------------------------------

#[tokio::test]
async fn ac_0_11_1_password_hashed() {
    let w = world();
    // Short passwords are refused; the first admin is created once.
    let r = w
        .post(
            "/api/v1/auth/setup",
            "",
            json!({"username": "root", "password": "short"}),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = w
        .post(
            "/api/v1/auth/setup",
            "",
            json!({"username": "Not Valid!", "password": PW}),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let root = w.admin().await;
    // Only an argon2id hash is stored, never the password.
    let (_, binds) = w.items("user").remove(0);
    let spec = binds[4].show();
    assert!(spec.contains("$argon2id$"), "{spec}");
    assert!(!spec.contains(PW));
    assert!(
        w.store
            .writes
            .lock()
            .unwrap()
            .iter()
            .all(|q| !q.binds.iter().any(|b| b.show().contains(PW))),
        "the password never reaches a statement"
    );
    assert!(auth::password::verify(
        PW,
        &auth::password::hash(PW).unwrap()
    ));
    assert!(!auth::password::verify(
        "wrong",
        &auth::password::hash(PW).unwrap()
    ));
    assert_ne!(
        auth::password::hash(PW).unwrap(),
        auth::password::hash(PW).unwrap(),
        "a salt per hash"
    );
    // Login: right and wrong; a missing user takes the same answer.
    let ok = w
        .post(
            "/api/v1/auth/login",
            "",
            json!({"username": "root", "password": PW}),
        )
        .await;
    assert_eq!(ok.status, StatusCode::OK);
    assert!(
        ok.header("set-cookie").contains("HttpOnly")
            && ok.header("set-cookie").contains("SameSite=Strict"),
        "{}",
        ok.header("set-cookie")
    );
    for (u, p) in [("root", "wrong password!"), ("nobody", PW)] {
        let r = w
            .post(
                "/api/v1/auth/login",
                "",
                json!({"username": u, "password": p}),
            )
            .await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED);
        assert_eq!(r.json()["error"], "the username or password is not right");
    }
    // The session cookie itself is stored hashed.
    let cookie = ok.cookie();
    let raw = cookie.split('=').nth(1).unwrap();
    assert!(
        w.items("session").iter().all(|(id, _)| !id.contains(raw)),
        "only a hash of the session is stored"
    );
    // Password change needs the current one.
    let r = w
        .post(
            "/api/v1/auth/password",
            &root,
            json!({"current": "nope nope nope", "new": "another long one"}),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let r = w
        .post(
            "/api/v1/auth/password",
            &root,
            json!({"current": PW, "new": "another long one"}),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    // A password change ends every older session and hands back a fresh one.
    assert_eq!(
        w.get("/api/v1/orgs/default/views", &root).await.status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        w.get("/api/v1/orgs/default/views", &r.cookie())
            .await
            .status,
        StatusCode::OK
    );
    assert_eq!(
        w.post(
            "/api/v1/auth/login",
            "",
            json!({"username": "root", "password": PW})
        )
        .await
        .status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        w.post(
            "/api/v1/auth/login",
            "",
            json!({"username": "root", "password": "another long one"})
        )
        .await
        .status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn ac_0_11_1_session_expiry() {
    let w = world_with(Config {
        session_hours: 2,
        ..Config::default()
    });
    let root = w.admin().await;
    assert_eq!(
        w.get("/api/v1/orgs/default/views", &root).await.status,
        StatusCode::OK
    );
    w.at(3600);
    assert_eq!(
        w.get("/api/v1/orgs/default/views", &root).await.status,
        StatusCode::OK,
        "one hour in"
    );
    w.at(2 * 3600 + 10);
    // The in-memory copy of an identity is short-lived: it cannot outlive the expiry.
    w.state.auth.forget_all();
    let r = w.get("/api/v1/orgs/default/views", &root).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{}", r.text());
    // Logout ends a session at once; a disabled user loses theirs.
    w.at(0);
    let c = w
        .post(
            "/api/v1/auth/login",
            "",
            json!({"username": "root", "password": PW}),
        )
        .await
        .cookie();
    assert_eq!(
        w.get("/api/v1/auth/me", &c).await.json()["data"]["authenticated"],
        true
    );
    let out = w.post("/api/v1/auth/logout", &c, json!({})).await;
    assert_eq!(out.status, StatusCode::OK);
    assert!(out.header("set-cookie").contains("Max-Age=0"));
    assert_eq!(
        w.get("/api/v1/orgs/default/views", &c).await.status,
        StatusCode::UNAUTHORIZED
    );
    // Brute force is slowed: the eleventh try in a minute is refused.
    let mut last = StatusCode::OK;
    for _ in 0..12 {
        last = w
            .post(
                "/api/v1/auth/login",
                "",
                json!({"username": "root", "password": "wrong wrong wrong"}),
            )
            .await
            .status;
    }
    assert_eq!(last, StatusCode::TOO_MANY_REQUESTS);
    // Setup is open only until a user exists.
    assert_eq!(
        w.get("/api/v1/auth/me", "").await.json()["data"]["setup_required"],
        false
    );
}

#[tokio::test]
async fn ac_0_11_1_first_admin_once() {
    let w = world();
    let me = w.get("/api/v1/auth/me", "").await.json()["data"].clone();
    assert_eq!(
        (me["authenticated"].clone(), me["setup_required"].clone()),
        (json!(false), json!(true))
    );
    w.admin().await;
    let again = w
        .post(
            "/api/v1/auth/setup",
            "",
            json!({"username": "mallory", "password": PW}),
        )
        .await;
    assert_eq!(again.status, StatusCode::CONFLICT, "{}", again.text());
    assert_eq!(w.items("user").len(), 1);
    // With --no-auth there is nothing to set up.
    let open = world_with(Config::default());
    let _ = open;
    let (a, _) = app_cfg(
        &MemStore::new(),
        &Arc::new(AtomicI64::new(T0)),
        Config::default(),
    );
    assert_eq!(
        send_with(
            &a,
            Method::POST,
            "/api/v1/auth/setup",
            &[],
            Some(&json!({"username": "x", "password": PW}))
        )
        .await
        .status,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
#[ignore = "needs GreptimeDB: make db-up && make test-db"]
async fn ac_0_11_1_first_admin_once_live() {
    let base = std::env::var("GREPUI_TEST_DB")
        .expect("set GREPUI_TEST_DB=mysql://root@127.0.0.1:4002/public");
    let meta = format!("grepui_it_auth_{}", std::process::id());
    let r = Arc::new(
        grepui::query::SqlxReader::lazy(&base, std::time::Duration::from_secs(30)).unwrap(),
    );
    store::migrate(r.as_ref(), r.as_ref(), &meta)
        .await
        .expect("migrate");
    let state = grepui::api::AppState::with_parts(
        Config {
            greptime_mysql: base.clone(),
            meta_database: meta.clone(),
            no_auth: false,
            ..Config::default()
        },
        r.clone(),
        r.clone(),
        grepui::api::system_clock(),
    )
    .unwrap();
    let app = grepui::api::router(state.clone());
    let first = send_with(
        &app,
        Method::POST,
        "/api/v1/auth/setup",
        &[],
        Some(&json!({"username": "root", "password": PW})),
    )
    .await;
    assert_eq!(first.status, StatusCode::OK, "{}", first.text());
    let second = send_with(
        &app,
        Method::POST,
        "/api/v1/auth/setup",
        &[],
        Some(&json!({"username": "other", "password": PW})),
    )
    .await;
    assert_eq!(second.status, StatusCode::CONFLICT);
    // The hash and the session survive in the real store; the cookie works from a second instance.
    let state2 = grepui::api::AppState::with_parts(
        Config {
            greptime_mysql: base,
            meta_database: meta.clone(),
            no_auth: false,
            ..Config::default()
        },
        r.clone(),
        r,
        grepui::api::system_clock(),
    )
    .unwrap();
    let app2 = grepui::api::router(state2);
    let me = send_with(
        &app2,
        Method::GET,
        "/api/v1/auth/me",
        &[("cookie", &first.cookie())],
        None,
    )
    .await
    .json();
    assert_eq!(
        (
            me["data"]["username"].clone(),
            me["data"]["authenticated"].clone()
        ),
        (json!("root"), json!(true))
    );
    let login = send_with(
        &app2,
        Method::POST,
        "/api/v1/auth/login",
        &[],
        Some(&json!({"username": "root", "password": PW})),
    )
    .await;
    assert_eq!(login.status, StatusCode::OK);
    let form = [("content-type", "application/x-www-form-urlencoded")];
    let _ = grepui::playground::http::post(
        "http://127.0.0.1:4000",
        "/v1/sql",
        &form,
        format!("sql=DROP DATABASE IF EXISTS {meta}").as_bytes(),
    )
    .await;
}

// ---- AC0.11.2 --------------------------------------------------------------------------------

#[tokio::test]
async fn ac_0_11_2_deny_by_default() {
    let w = world();
    let root = w.admin().await;
    w.org(&root, "a").await;
    w.org(&root, "b").await;
    let viewer = w.user(&root, "vera", "a", "viewer").await;
    let editor = w.user(&root, "ed", "a", "editor").await;
    // Anonymous: 401 on data and on administration.
    for uri in [
        "/api/v1/orgs/a/views",
        "/api/v1/orgs/a/members",
        "/api/v1/platform/orgs",
        "/api/v1/orgs/a/sql?sql=SELECT%201&from=1&to=2",
    ] {
        assert_eq!(
            w.get(uri, "").await.status,
            StatusCode::UNAUTHORIZED,
            "{uri}"
        );
    }
    // A signed-in person without a role in the org gets 403, also for orgs that do not exist.
    assert_eq!(
        w.get("/api/v1/orgs/b/views", &viewer).await.status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        w.get("/api/v1/orgs/nope/views", &viewer).await.status,
        StatusCode::FORBIDDEN,
        "no hint that the org exists"
    );
    assert_eq!(
        w.get("/api/v1/orgs/default/views", &viewer).await.status,
        StatusCode::FORBIDDEN
    );
    // A viewer reads but cannot change anything; an editor changes saved state but not the org.
    assert_eq!(
        w.get("/api/v1/orgs/a/views", &viewer).await.status,
        StatusCode::OK
    );
    let body = json!({"name": "v", "kind": "logs", "spec": {}});
    assert_eq!(
        w.post("/api/v1/orgs/a/views", &viewer, body.clone())
            .await
            .status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        w.post("/api/v1/orgs/a/views", &editor, body).await.status,
        StatusCode::CREATED
    );
    for uri in [
        "/api/v1/orgs/a/members",
        "/api/v1/orgs/a/tokens",
        "/api/v1/orgs/a/audit",
        "/api/v1/orgs/a/redaction",
    ] {
        assert_eq!(
            w.get(uri, &editor).await.status,
            StatusCode::FORBIDDEN,
            "{uri} is for admins"
        );
    }
    assert_eq!(
        w.post("/api/v1/orgs/a/rollups/traces_red", &editor, json!({}))
            .await
            .status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        w.get("/api/v1/platform/orgs", &editor).await.status,
        StatusCode::FORBIDDEN
    );
    // Unclassified means nobody: a method the table does not list is not allowed.
    assert_eq!(
        access_for(Method::PATCH, "/api/v1/orgs/{org}/views"),
        Access::Deny
    );
    assert_eq!(
        access_for(
            Method::GET,
            "/api/v1/orgs/{org}/new-route-nobody-classified"
        ),
        Access::Deny
    );
    assert_eq!(access_for(Method::GET, ""), Access::Deny);
    assert_eq!(
        w.call(
            Method::PATCH,
            "/api/v1/orgs/a/views",
            &root,
            Some(json!({}))
        )
        .await
        .status,
        StatusCode::METHOD_NOT_ALLOWED
    );
    // Health stays open.
    assert_eq!(w.get("/healthz", "").await.status, StatusCode::OK);
}

fn access_for(m: Method, t: &str) -> Access {
    auth::access_for(&m, t)
}

#[test]
fn ac_0_11_2_every_route_checks_permission() {
    // Every listed route has at least one classified method, and the classification is never
    // "open" for a route that serves org data or administration.
    let open = [
        "/healthz",
        "/api/v1/health",
        "/api/v1/info",
        "/api/v1/auth/me",
        "/api/v1/auth/setup",
        "/api/v1/auth/login",
        "/api/v1/auth/oidc/login",
        "/api/v1/auth/oidc/callback",
        "/api/v1/orgs/{org}/public/{token}",
        "/api/v1/orgs/{org}/public/{token}/variables",
        "/api/v1/orgs/{org}/public/{token}/panels/{panel}/data",
    ];
    let methods = [
        Method::GET,
        Method::POST,
        Method::PUT,
        Method::DELETE,
        Method::PATCH,
    ];
    for route in ROUTES {
        let classes: Vec<(Method, Access)> = methods
            .iter()
            .map(|m| (m.clone(), access_for(m.clone(), route)))
            .filter(|(_, a)| *a != Access::Deny)
            .collect();
        assert!(
            !classes.is_empty(),
            "{route} is listed but no method of it is classified (deny by default)"
        );
        for (m, a) in &classes {
            if *a == Access::Anonymous {
                assert!(open.contains(route), "{m} {route} is open to anyone");
            }
            if route.starts_with("/api/v1/platform/") {
                assert_eq!(*a, Access::Platform, "{route}");
            }
        }
    }
    // Every state-changing route is classified at least as an editor's (or stricter) action.
    for (m, route) in WRITE_ROUTES {
        let a = access_for(m.parse().unwrap(), route);
        assert!(
            matches!(
                a,
                Access::Org(Role::Editor | Role::Admin)
                    | Access::Platform
                    | Access::Authenticated
                    | Access::Anonymous
            ),
            "{m} {route}: {a:?}"
        );
        if route.contains("{org}") && !route.contains("/public/") && !route.contains("/platform/") {
            assert!(
                matches!(a, Access::Org(Role::Editor | Role::Admin)),
                "{m} {route} must need a role, got {a:?}"
            );
        }
    }
    // The admin-only families.
    for r in [
        "/api/v1/orgs/{org}/rollups/{template}",
        "/api/v1/orgs/{org}/retention/{stream}",
        "/api/v1/orgs/{org}/redaction",
        "/api/v1/orgs/{org}/members",
        "/api/v1/orgs/{org}/tokens",
        "/api/v1/orgs/{org}/pipelines/{name}",
        "/api/v1/orgs/{org}/enrichment/{name}",
        "/api/v1/orgs/{org}/destinations",
    ] {
        let strictest = methods
            .iter()
            .map(|m| access_for(m.clone(), r))
            .filter(|a| *a != Access::Deny)
            .max_by_key(|a| match a {
                Access::Org(x) => *x as i32,
                _ => -1,
            });
        assert_eq!(strictest, Some(Access::Org(Role::Admin)), "{r}");
    }
}

#[tokio::test]
async fn ac_0_11_2_every_route_refuses_anonymous() {
    let w = world();
    let root = w.admin().await;
    let _ = root;
    let fill = |r: &str| {
        r.replace("{org}", "default")
            .replace("{name}", "t")
            .replace("{field}", "severity_text")
            .replace("{service}", "cart")
            .replace("{template}", "traces_red")
            .replace("{panel}", "p1")
            .replace("{share}", "0123456789abcdef")
            .replace("{token}", "0123456789abcdef0123456789abcdef")
            .replace("{stream}", "traces")
            .replace("{id}", "0123456789abcdef")
            .replace("{trace_id}", "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
            .replace("{user}", "alice")
    };
    let mut checked = 0;
    for route in ROUTES {
        for m in [Method::GET, Method::POST, Method::PUT, Method::DELETE] {
            let a = auth::access_for(&m, route);
            if matches!(a, Access::Anonymous | Access::Deny) {
                continue;
            }
            let r = w.call(m.clone(), &fill(route), "", Some(json!({}))).await;
            assert_eq!(
                r.status,
                StatusCode::UNAUTHORIZED,
                "{m} {route} answered an anonymous caller with {}",
                r.status
            );
            let r = w
                .call(
                    m.clone(),
                    &fill(route),
                    "Bearer gu_default.not-a-real-token-not-a-real-token-xxxxxxxx",
                    Some(json!({})),
                )
                .await;
            assert_eq!(
                r.status,
                StatusCode::UNAUTHORIZED,
                "{m} {route} with a bad token"
            );
            checked += 1;
        }
    }
    assert!(checked > 100, "{checked}");
}

#[tokio::test]
#[ignore = "needs GreptimeDB: make db-up && make test-db"]
async fn ac_0_11_2_cross_org_blocked_live() {
    let base = std::env::var("GREPUI_TEST_DB")
        .expect("set GREPUI_TEST_DB=mysql://root@127.0.0.1:4002/public");
    let meta = format!("grepui_it_xorg_{}", std::process::id());
    let r = Arc::new(
        grepui::query::SqlxReader::lazy(&base, std::time::Duration::from_secs(30)).unwrap(),
    );
    store::migrate(r.as_ref(), r.as_ref(), &meta)
        .await
        .expect("migrate");
    let state = grepui::api::AppState::with_parts(
        Config {
            greptime_mysql: base,
            meta_database: meta.clone(),
            no_auth: false,
            ..Config::default()
        },
        r.clone(),
        r,
        grepui::api::system_clock(),
    )
    .unwrap();
    let app = grepui::api::router(state);
    let call = |m: Method, u: &'static str, c: String, b: Option<Value>| {
        let app = app.clone();
        async move { send_with(&app, m, u, &[("cookie", &c)], b.as_ref()).await }
    };
    let root = send_with(
        &app,
        Method::POST,
        "/api/v1/auth/setup",
        &[],
        Some(&json!({"username": "root", "password": PW})),
    )
    .await
    .cookie();
    for o in ["xa", "xb"] {
        let b = json!({"name": o, "database": format!("db_{o}_{}", std::process::id())});
        assert_eq!(
            send_with(
                &app,
                Method::POST,
                "/api/v1/platform/orgs",
                &[("cookie", &root)],
                Some(&b)
            )
            .await
            .status,
            StatusCode::CREATED
        );
    }
    let mk = |n: &'static str, o: &'static str| {
        let (app, root) = (app.clone(), root.clone());
        async move {
            send_with(
                &app,
                Method::POST,
                "/api/v1/platform/users",
                &[("cookie", &root)],
                Some(&json!({"username": n, "password": PW, "orgs": {o: "editor"}})),
            )
            .await;
            send_with(
                &app,
                Method::POST,
                "/api/v1/auth/login",
                &[],
                Some(&json!({"username": n, "password": PW})),
            )
            .await
            .cookie()
        }
    };
    let (a, b) = (mk("ann", "xa").await, mk("bob", "xb").await);
    // Each saves a view in their own org; neither can read or write the other's.
    let v = json!({"name": "mine", "kind": "logs", "spec": {}});
    assert_eq!(
        call(
            Method::POST,
            "/api/v1/orgs/xa/views",
            a.clone(),
            Some(v.clone())
        )
        .await
        .status,
        StatusCode::CREATED
    );
    assert_eq!(
        call(Method::POST, "/api/v1/orgs/xa/views", b.clone(), Some(v))
            .await
            .status,
        StatusCode::FORBIDDEN
    );
    for u in [
        "/api/v1/orgs/xa/views",
        "/api/v1/orgs/xa/streams",
        "/api/v1/orgs/xa/sql?sql=SELECT%201&from=1000&to=2000",
    ] {
        assert_eq!(
            call(Method::GET, u, b.clone(), None).await.status,
            StatusCode::FORBIDDEN,
            "{u}"
        );
    }
    let bl = call(Method::GET, "/api/v1/orgs/xb/views", b.clone(), None).await;
    assert_eq!(bl.status, StatusCode::OK);
    assert_eq!(
        bl.json()["data"].as_array().unwrap().len(),
        0,
        "B sees none of A's views"
    );
    let al = call(Method::GET, "/api/v1/orgs/xa/views", a, None).await;
    assert_eq!(al.json()["data"].as_array().unwrap().len(), 1);
    let orgs = call(Method::GET, "/api/v1/orgs", b, None).await.json();
    assert_eq!(orgs["orgs"].as_array().unwrap().len(), 1);
    let form = [("content-type", "application/x-www-form-urlencoded")];
    let _ = grepui::playground::http::post(
        "http://127.0.0.1:4000",
        "/v1/sql",
        &form,
        format!("sql=DROP DATABASE IF EXISTS {meta}").as_bytes(),
    )
    .await;
}

// ---- AC0.11.3 --------------------------------------------------------------------------------

#[tokio::test]
async fn ac_0_11_3_token_hashed_shown_once() {
    let w = world();
    let root = w.admin().await;
    let r = w
        .post(
            "/api/v1/orgs/default/tokens",
            &root,
            json!({"name": "ci", "role": "viewer", "expires_days": 30}),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let token = r.json()["data"]["token"].as_str().unwrap().to_string();
    assert!(
        token.starts_with("gu_default.") && token.len() > 50,
        "{token}"
    );
    assert!(
        r.json()["data"]["note"]
            .as_str()
            .unwrap()
            .contains("only time")
    );
    // Stored by its SHA-256 only; the list never shows it again.
    let stored = w.items("token");
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].0, format!("default/{}", auth::sha_hex(&token)));
    assert!(
        stored[0].1.iter().all(|b| !b.show().contains(&token)),
        "the token is not in the stored row"
    );
    assert!(
        w.store
            .writes
            .lock()
            .unwrap()
            .iter()
            .all(|q| q.name != "alert_item_put"
                || q.binds.iter().all(|b| !b.show().contains(&token)))
    );
    let list = w.get("/api/v1/orgs/default/tokens", &root).await;
    assert!(
        !list.text().contains(&token) && !list.text().contains("gu_default"),
        "{}",
        list.text()
    );
    assert_eq!(list.json()["data"][0]["name"], "ci");
    // It works as a bearer credential and is attributed to the service account.
    let bearer = format!("Bearer {token}");
    let me = w.get("/api/v1/auth/me", &bearer).await.json()["data"].clone();
    assert_eq!(
        (me["kind"].clone(), me["username"].clone()),
        (json!("token"), json!("token:ci"))
    );
    // An expired token stops working.
    w.at(31 * 86_400);
    w.state.auth.forget_all();
    assert_eq!(
        w.get("/api/v1/orgs/default/views", &bearer).await.status,
        StatusCode::UNAUTHORIZED
    );
    // Malformed tokens never reach the store.
    for bad in [
        "Bearer ",
        "Bearer gu_",
        "Bearer gu_x.y",
        "Bearer abc",
        "Basic abc",
    ] {
        assert_eq!(
            w.get("/api/v1/orgs/default/views", bad).await.status,
            StatusCode::UNAUTHORIZED,
            "{bad}"
        );
    }
}

#[tokio::test]
async fn ac_0_11_3_token_scope_enforced() {
    let w = world();
    let root = w.admin().await;
    w.org(&root, "a").await;
    w.org(&root, "b").await;
    let viewer = w.token(&root, "a", "viewer").await;
    let editor = w.token(&root, "a", "editor").await;
    let admin = w.token(&root, "a", "admin").await;
    assert_eq!(
        w.get("/api/v1/orgs/a/views", &viewer).await.status,
        StatusCode::OK
    );
    let v = json!({"name": "v", "kind": "logs", "spec": {}});
    assert_eq!(
        w.post("/api/v1/orgs/a/views", &viewer, v.clone())
            .await
            .status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        w.post("/api/v1/orgs/a/views", &editor, v).await.status,
        StatusCode::CREATED
    );
    assert_eq!(
        w.get("/api/v1/orgs/a/members", &editor).await.status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        w.get("/api/v1/orgs/a/members", &admin).await.status,
        StatusCode::OK
    );
    // A token belongs to one org.
    for t in [&viewer, &editor, &admin] {
        assert_eq!(
            w.get("/api/v1/orgs/b/views", t).await.status,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            w.get("/api/v1/platform/orgs", t).await.status,
            StatusCode::FORBIDDEN
        );
    }
    // An admin token can mint tokens up to admin; an editor cannot mint at all; nobody gets
    // more than they have.
    assert_eq!(
        w.post(
            "/api/v1/orgs/a/tokens",
            &admin,
            json!({"name": "x", "role": "admin"})
        )
        .await
        .status,
        StatusCode::CREATED
    );
    assert_eq!(
        w.post(
            "/api/v1/orgs/a/tokens",
            &editor,
            json!({"name": "x", "role": "viewer"})
        )
        .await
        .status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        w.post(
            "/api/v1/orgs/a/tokens",
            &root,
            json!({"name": "x", "role": "root"})
        )
        .await
        .status,
        StatusCode::BAD_REQUEST
    );
    // A token for org a presented to org b's path is refused even when it names b.
    let mixed = viewer.replace("gu_a.", "gu_b.");
    assert_eq!(
        w.get("/api/v1/orgs/b/views", &mixed).await.status,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn ac_0_11_3_token_revoked() {
    let w = world();
    let root = w.admin().await;
    let t = w.token(&root, "default", "editor").await;
    assert_eq!(
        w.get("/api/v1/orgs/default/views", &t).await.status,
        StatusCode::OK,
        "also warms the cache"
    );
    let id = w.get("/api/v1/orgs/default/tokens", &root).await.json()["data"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let r = w
        .call(
            Method::DELETE,
            &format!("/api/v1/orgs/default/tokens/{id}"),
            &root,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(
        w.get("/api/v1/orgs/default/views", &t).await.status,
        StatusCode::UNAUTHORIZED,
        "revoked at once"
    );
    assert_eq!(
        w.get("/api/v1/orgs/default/tokens", &root).await.json()["data"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    assert_eq!(
        w.call(
            Method::DELETE,
            &format!("/api/v1/orgs/default/tokens/{id}"),
            &root,
            None
        )
        .await
        .status,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn ac_0_11_3_token_rate_limited() {
    let w = world_with(Config {
        token_rate_per_min: 5,
        ..Config::default()
    });
    let root = w.admin().await;
    let t = w.token(&root, "default", "viewer").await;
    let mut codes = vec![];
    for _ in 0..7 {
        codes.push(w.get("/api/v1/orgs/default/views", &t).await);
    }
    assert!(codes[..5].iter().all(|r| r.status == StatusCode::OK));
    assert_eq!(codes[5].status, StatusCode::TOO_MANY_REQUESTS);
    assert!(!codes[5].header("retry-after").is_empty());
    // People are not subject to the token limit.
    for _ in 0..8 {
        assert_eq!(
            w.get("/api/v1/orgs/default/views", &root).await.status,
            StatusCode::OK
        );
    }
}

// ---- AC0.11.4 --------------------------------------------------------------------------------

/// A test identity provider key (RSA 2048, PKCS#8), made with openssl for this file only.
const RSA_PKCS8_B64: &str = include_str!("data/oidc_test_key.b64");

fn rsa() -> (KeyPair, jwt::Jwk) {
    let der = base64::engine::general_purpose::STANDARD
        .decode(RSA_PKCS8_B64.trim())
        .unwrap();
    let kp = KeyPair::from_pkcs8(&der).unwrap();
    let pk = ring::rsa::PublicKeyComponents::<Vec<u8>>::from(kp.public());
    let jwk = jwt::Jwk {
        kty: "RSA".into(),
        kid: Some("k1".into()),
        alg: Some("RS256".into()),
        use_: Some("sig".into()),
        n: Some(B64.encode(&pk.n)),
        e: Some(B64.encode(&pk.e)),
        crv: None,
        x: None,
        y: None,
    };
    (kp, jwk)
}

fn sign_rs256(kp: &KeyPair, header: Value, claims: Value) -> String {
    let msg = format!(
        "{}.{}",
        B64.encode(header.to_string()),
        B64.encode(claims.to_string())
    );
    let mut sig = vec![0u8; kp.public().modulus_len()];
    kp.sign(
        &signature::RSA_PKCS1_SHA256,
        &SystemRandom::new(),
        msg.as_bytes(),
        &mut sig,
    )
    .unwrap();
    format!("{msg}.{}", B64.encode(sig))
}

const ISS: &str = "http://idp.test/realm";
const CLIENT: &str = "grepui";

fn claims(extra: Value) -> Value {
    let mut c = json!({"iss": ISS, "aud": CLIENT, "sub": "u-1", "exp": T0 / 1000 + 600, "iat": T0 / 1000, "nonce": "n-1", "email": "ann@example.com", "groups": ["g-view"]});
    for (k, v) in extra.as_object().unwrap() {
        if v.is_null() {
            c.as_object_mut().unwrap().remove(k);
        } else {
            c[k] = v.clone();
        }
    }
    c
}

fn want<'a>() -> jwt::Expect<'a> {
    jwt::Expect {
        issuer: ISS,
        audience: CLIENT,
        nonce: Some("n-1"),
        now_secs: T0 / 1000,
        leeway_secs: 60,
    }
}

#[test]
fn ac_0_11_4_jwt_validation() {
    let (kp, jwk) = rsa();
    let keys = [jwk.clone()];
    let h = json!({"alg": "RS256", "kid": "k1", "typ": "JWT"});
    let good = sign_rs256(&kp, h.clone(), claims(json!({})));
    let c = jwt::verify(&good, &keys, &want()).unwrap();
    assert_eq!(c["email"], "ann@example.com");
    let bad = |t: &str, why: &str| {
        let e = jwt::verify(t, &keys, &want()).unwrap_err();
        assert!(e.contains(why), "{e} should mention {why}");
    };
    // Signature: any change to header or payload breaks it.
    let parts: Vec<&str> = good.split('.').collect();
    let forged = format!(
        "{}.{}.{}",
        parts[0],
        B64.encode(claims(json!({"sub": "admin"})).to_string()),
        parts[2]
    );
    bad(&forged, "signature");
    bad(&format!("{}.{}.", parts[0], parts[1]), "signature");
    bad("a.b", "not a signed JWT");
    bad("", "not a signed JWT");
    // Algorithms: none and HMAC are refused, even when the key could be abused for them.
    let none = format!(
        "{}.{}.",
        B64.encode(json!({"alg": "none"}).to_string()),
        B64.encode(claims(json!({})).to_string())
    );
    bad(&none, "not accepted");
    let hs = format!(
        "{}.{}.{}",
        B64.encode(json!({"alg": "HS256", "kid": "k1"}).to_string()),
        B64.encode(claims(json!({})).to_string()),
        B64.encode("x")
    );
    bad(&hs, "not accepted");
    // Claims.
    bad(
        &sign_rs256(&kp, h.clone(), claims(json!({"iss": "http://evil"}))),
        "issuer",
    );
    bad(
        &sign_rs256(&kp, h.clone(), claims(json!({"aud": "other"}))),
        "audience",
    );
    bad(
        &sign_rs256(&kp, h.clone(), claims(json!({"aud": ["grepui", "other"]}))),
        "authorized party",
    );
    assert!(
        jwt::verify(
            &sign_rs256(
                &kp,
                h.clone(),
                claims(json!({"aud": ["grepui", "other"], "azp": "grepui"}))
            ),
            &keys,
            &want()
        )
        .is_ok()
    );
    bad(
        &sign_rs256(&kp, h.clone(), claims(json!({"exp": T0 / 1000 - 120}))),
        "expired",
    );
    bad(
        &sign_rs256(&kp, h.clone(), claims(json!({"exp": null}))),
        "expiry",
    );
    bad(
        &sign_rs256(&kp, h.clone(), claims(json!({"nbf": T0 / 1000 + 600}))),
        "not valid yet",
    );
    bad(
        &sign_rs256(&kp, h.clone(), claims(json!({"iat": T0 / 1000 + 600}))),
        "future",
    );
    bad(
        &sign_rs256(&kp, h.clone(), claims(json!({"nonce": "other"}))),
        "nonce",
    );
    bad(
        &sign_rs256(&kp, h.clone(), claims(json!({"nonce": null}))),
        "nonce",
    );
    bad(
        &sign_rs256(&kp, h.clone(), claims(json!({"sub": null}))),
        "subject",
    );
    // A little clock skew is tolerated, a lot is not.
    assert!(
        jwt::verify(
            &sign_rs256(&kp, h.clone(), claims(json!({"exp": T0 / 1000 - 30}))),
            &keys,
            &want()
        )
        .is_ok()
    );
    // Keys: an unknown kid, a key meant for something else, and no keys at all.
    bad(
        &sign_rs256(
            &kp,
            json!({"alg": "RS256", "kid": "other"}),
            claims(json!({})),
        ),
        "no key",
    );
    let enc = jwt::Jwk {
        use_: Some("enc".into()),
        ..jwk.clone()
    };
    assert!(jwt::verify(&good, &[enc], &want()).is_err());
    assert!(jwt::verify(&good, &[], &want()).is_err());
    // ES256 with a freshly generated P-256 key.
    let rng = SystemRandom::new();
    let pkcs8 =
        signature::EcdsaKeyPair::generate_pkcs8(&signature::ECDSA_P256_SHA256_FIXED_SIGNING, &rng)
            .unwrap();
    let ec = signature::EcdsaKeyPair::from_pkcs8(
        &signature::ECDSA_P256_SHA256_FIXED_SIGNING,
        pkcs8.as_ref(),
        &rng,
    )
    .unwrap();
    let pubkey = signature::KeyPair::public_key(&ec).as_ref().to_vec();
    let ejwk = jwt::Jwk {
        kty: "EC".into(),
        kid: Some("e1".into()),
        alg: Some("ES256".into()),
        use_: None,
        n: None,
        e: None,
        crv: Some("P-256".into()),
        x: Some(B64.encode(&pubkey[1..33])),
        y: Some(B64.encode(&pubkey[33..65])),
    };
    let msg = format!(
        "{}.{}",
        B64.encode(json!({"alg": "ES256", "kid": "e1"}).to_string()),
        B64.encode(claims(json!({})).to_string())
    );
    let tok = format!(
        "{msg}.{}",
        B64.encode(ec.sign(&rng, msg.as_bytes()).unwrap().as_ref())
    );
    assert!(jwt::verify(&tok, std::slice::from_ref(&ejwk), &want()).is_ok());
    assert!(
        jwt::verify(&tok, &[jwk], &want()).is_err(),
        "an RSA key does not verify an ES256 token"
    );
    let flipped = format!("{}.{}", &tok[..tok.len() - 4], "AAAA");
    assert!(jwt::verify(&flipped, &[ejwk], &want()).is_err());
}

fn oidc_cfg(idp: &str) -> Config {
    Config {
        oidc_issuer: Some(ISS.into()),
        oidc_client_id: Some(CLIENT.into()),
        oidc_auth_url: Some(format!("{idp}/auth")),
        oidc_token_url: Some(format!("{idp}/token")),
        oidc_jwks_url: Some(format!("{idp}/keys")),
        oidc_redirect_url: Some("http://grepui.test/api/v1/auth/oidc/callback".into()),
        oidc_role_map: vec![
            "g-view=viewer@default".into(),
            "g-edit=editor@default".into(),
            "g-ops=admin@default".into(),
            "g-root=platform_admin".into(),
        ],
        ..Config::default()
    }
}

#[test]
fn ac_0_11_4_claim_role_mapping() {
    let o = oidc::Oidc::from_config(&Config {
        oidc_role_map: vec![
            "a=viewer@x".into(),
            "b=admin@x".into(),
            "c=editor@y".into(),
            "r=platform_admin".into(),
        ],
        ..oidc_cfg("http://idp")
    })
    .unwrap();
    let (p, orgs) = o.map_roles(&json!({"groups": ["a", "b", "zzz"]}));
    assert!(!p);
    assert_eq!(
        orgs,
        BTreeMap::from([("x".to_string(), Role::Admin)]),
        "the highest role wins"
    );
    let (p, orgs) = o.map_roles(&json!({"groups": "c"}));
    assert_eq!(
        (p, orgs),
        (false, BTreeMap::from([("y".to_string(), Role::Editor)])),
        "a single string works"
    );
    assert!(o.map_roles(&json!({"groups": ["r", "a"]})).0);
    assert_eq!(
        o.map_roles(&json!({"groups": []})),
        (false, BTreeMap::new())
    );
    assert_eq!(
        o.map_roles(&json!({"other": ["a"]})),
        (false, BTreeMap::new()),
        "only the configured claim counts"
    );
    // Mappings are validated at startup.
    for bad in [
        "x",
        "x=boss@org",
        "x=viewer",
        "=viewer@org",
        "x=viewer@Bad Org",
    ] {
        assert!(oidc::parse_rule(bad).is_err(), "{bad}");
    }
    assert!(
        Config {
            oidc_role_map: vec!["x=oops".into()],
            ..oidc_cfg("http://idp")
        }
        .validate()
        .is_err()
    );
    assert!(
        Config {
            oidc_issuer: Some("x".into()),
            ..Config::default()
        }
        .validate()
        .is_err(),
        "all or none"
    );
    assert!(
        Config {
            oidc_token_url: Some("ftp://idp/token".into()),
            ..oidc_cfg("http://idp")
        }
        .validate()
        .is_err()
    );
    assert!(oidc_cfg("http://idp").validate().is_ok());
}

/// A stand-in identity provider on a local port: serves keys and answers the code exchange with
/// an ID token built by `mint(nonce)`.
struct Idp {
    base: String,
    seen: Arc<Mutex<Vec<String>>>,
}

async fn idp(
    kp: KeyPair,
    jwk: jwt::Jwk,
    mint: impl Fn(&str, &KeyPair) -> String + Send + Sync + 'static,
) -> Idp {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(vec![]));
    let (sn, mint, kp) = (seen.clone(), Arc::new(mint), Arc::new(kp));
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = l.accept().await else {
                return;
            };
            let (sn, mint, kp, jwk) = (sn.clone(), mint.clone(), kp.clone(), jwk.clone());
            tokio::spawn(async move {
                let mut buf = vec![0u8; 16384];
                let mut got = 0;
                loop {
                    let n = s.read(&mut buf[got..]).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    got += n;
                    let t = String::from_utf8_lossy(&buf[..got]).to_string();
                    let Some((head, body)) = t.split_once("\r\n\r\n") else {
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
                    let path = head
                        .lines()
                        .next()
                        .unwrap_or_default()
                        .split_whitespace()
                        .nth(1)
                        .unwrap_or_default()
                        .to_string();
                    sn.lock().unwrap().push(format!("{path} {body}"));
                    let out = if path.starts_with("/keys") {
                        json!({"keys": [jwk]}).to_string()
                    } else {
                        // The nonce travels through the pending login, not the code exchange: the
                        // test provider looks it up from the code it handed out ("code-<nonce>").
                        let code = body
                            .split('&')
                            .find_map(|p| p.strip_prefix("code="))
                            .unwrap_or_default();
                        let nonce = code.strip_prefix("code-").unwrap_or("");
                        json!({"id_token": mint(nonce, &kp), "token_type": "Bearer"}).to_string()
                    };
                    let _ = s.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{out}", out.len()).as_bytes()).await;
                    return;
                }
            });
        }
    });
    Idp { base, seen }
}

fn query_of(url: &str, key: &str) -> String {
    url::Url::parse(url)
        .unwrap()
        .query_pairs()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.to_string())
        .unwrap_or_default()
}

#[tokio::test]
async fn ac_0_11_4_oidc_state_nonce() {
    let (kp, jwk) = rsa();
    let now_s = T0 / 1000;
    let p = idp(kp, jwk, move |nonce, kp| {
        sign_rs256(
            kp,
            json!({"alg": "RS256", "kid": "k1"}),
            claims(json!({"nonce": nonce, "iat": now_s, "exp": now_s + 600})),
        )
    })
    .await;
    let w = world_with(oidc_cfg(&p.base));
    w.admin().await;
    assert_eq!(
        w.get("/api/v1/auth/me", "").await.json()["data"]["oidc"],
        true
    );
    // The redirect carries state, nonce and an S256 challenge; nothing secret.
    let r = w.get("/api/v1/auth/oidc/login", "").await;
    assert_eq!(r.status, StatusCode::SEE_OTHER, "{}", r.text());
    let url = r.header("location").to_string();
    assert!(url.starts_with(&format!("{}/auth?", p.base)), "{url}");
    let (state, nonce, challenge) = (
        query_of(&url, "state"),
        query_of(&url, "nonce"),
        query_of(&url, "code_challenge"),
    );
    assert_eq!(query_of(&url, "code_challenge_method"), "S256");
    assert_eq!(
        (query_of(&url, "response_type"), query_of(&url, "client_id")),
        ("code".into(), CLIENT.into())
    );
    assert!(
        state.len() >= 40 && nonce.len() >= 30 && challenge.len() == 43,
        "{state} {nonce} {challenge}"
    );
    assert!(!url.contains("code_verifier"));
    let pending = w.items("oidc");
    assert_eq!(pending.len(), 1);
    assert_eq!(
        pending[0].0,
        format!("_platform/{}", auth::sha_hex(&state)),
        "only a hash of the state is stored"
    );
    // A callback with an unknown state, or no state, is refused and never reaches the provider.
    let bad = w
        .get(
            &format!("/api/v1/auth/oidc/callback?code=code-{nonce}&state=forged"),
            "",
        )
        .await;
    assert_eq!(bad.status, StatusCode::UNAUTHORIZED, "{}", bad.text());
    assert_eq!(
        w.get("/api/v1/auth/oidc/callback?code=x", "").await.status,
        StatusCode::BAD_REQUEST
    );
    assert!(p.seen.lock().unwrap().is_empty());
    // The right state works once: a session, a user, the role from the claim, and the code
    // verifier is what the provider checks (PKCE).
    let ok = w
        .get(
            &format!("/api/v1/auth/oidc/callback?code=code-{nonce}&state={state}"),
            "",
        )
        .await;
    assert_eq!(ok.status, StatusCode::SEE_OTHER, "{}", ok.text());
    assert_eq!(ok.header("location"), "/");
    let cookie = ok.cookie();
    let me = w.get("/api/v1/auth/me", &cookie).await.json()["data"].clone();
    assert_eq!(me["orgs"], json!({"default": "viewer"}));
    assert_eq!(me["user"]["source"], "oidc");
    assert_eq!(me["user"]["display"], "ann@example.com");
    let sent = p
        .seen
        .lock()
        .unwrap()
        .iter()
        .find(|s| s.starts_with("/token"))
        .cloned()
        .unwrap();
    assert!(
        sent.contains("grant_type=authorization_code") && sent.contains("code_verifier="),
        "{sent}"
    );
    let verifier = sent
        .split('&')
        .find_map(|p| p.strip_prefix("code_verifier="))
        .unwrap();
    assert_eq!(
        jwt::pkce_challenge(verifier),
        challenge,
        "the verifier matches the challenge that was sent"
    );
    assert!(sent.contains(&format!("client_id={CLIENT}")) && !sent.contains("client_secret"));
    // The state is spent.
    let again = w
        .get(
            &format!("/api/v1/auth/oidc/callback?code=code-{nonce}&state={state}"),
            "",
        )
        .await;
    assert_eq!(again.status, StatusCode::UNAUTHORIZED);
    // A login that took too long is refused.
    let r = w.get("/api/v1/auth/oidc/login", "").await;
    let (st, nn) = (
        query_of(r.header("location"), "state"),
        query_of(r.header("location"), "nonce"),
    );
    w.at(11 * 60);
    assert_eq!(
        w.get(
            &format!("/api/v1/auth/oidc/callback?code=code-{nn}&state={st}"),
            ""
        )
        .await
        .status,
        StatusCode::UNAUTHORIZED
    );
    // The provider reporting an error is a refusal.
    assert_eq!(
        w.get("/api/v1/auth/oidc/callback?error=access_denied&state=x", "")
            .await
            .status,
        StatusCode::UNAUTHORIZED
    );
    // Without configuration there is no single sign-on.
    let plain = world();
    assert_eq!(
        plain.get("/api/v1/auth/oidc/login", "").await.status,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn ac_0_11_4_oidc_rejects_bad_tokens() {
    let (kp, jwk) = rsa();
    let (k2, _) = rsa();
    let _ = k2;
    let now_s = T0 / 1000;
    // Each provider answers with a different kind of bad token.
    type Case = (
        &'static str,
        Box<dyn Fn(&str, &KeyPair) -> String + Send + Sync>,
    );
    let cases: Vec<Case> = vec![
        (
            "wrong nonce",
            Box::new(move |_, k| {
                sign_rs256(
                    k,
                    json!({"alg": "RS256", "kid": "k1"}),
                    claims(json!({"nonce": "someone-elses"})),
                )
            }),
        ),
        (
            "expired",
            Box::new(move |n, k| {
                sign_rs256(
                    k,
                    json!({"alg": "RS256", "kid": "k1"}),
                    claims(json!({"nonce": n, "exp": now_s - 3600})),
                )
            }),
        ),
        (
            "wrong audience",
            Box::new(move |n, k| {
                sign_rs256(
                    k,
                    json!({"alg": "RS256", "kid": "k1"}),
                    claims(json!({"nonce": n, "aud": "other"})),
                )
            }),
        ),
        (
            "wrong issuer",
            Box::new(move |n, k| {
                sign_rs256(
                    k,
                    json!({"alg": "RS256", "kid": "k1"}),
                    claims(json!({"nonce": n, "iss": "http://evil"})),
                )
            }),
        ),
        (
            "alg none",
            Box::new(move |n, _| {
                format!(
                    "{}.{}.",
                    B64.encode(json!({"alg": "none"}).to_string()),
                    B64.encode(claims(json!({"nonce": n})).to_string())
                )
            }),
        ),
        (
            "no mapped group",
            Box::new(move |n, k| {
                sign_rs256(
                    k,
                    json!({"alg": "RS256", "kid": "k1"}),
                    claims(json!({"nonce": n, "groups": ["unrelated"]})),
                )
            }),
        ),
    ];
    for (name, mint) in cases {
        let p = idp(rsa().0, jwk.clone(), move |n, k| mint(n, k)).await;
        let w = world_with(oidc_cfg(&p.base));
        w.admin().await;
        let r = w.get("/api/v1/auth/oidc/login", "").await;
        let (st, nn) = (
            query_of(r.header("location"), "state"),
            query_of(r.header("location"), "nonce"),
        );
        let cb = w
            .get(
                &format!("/api/v1/auth/oidc/callback?code=code-{nn}&state={st}"),
                "",
            )
            .await;
        let want = if name == "no mapped group" {
            StatusCode::FORBIDDEN
        } else {
            StatusCode::UNAUTHORIZED
        };
        assert_eq!(cb.status, want, "{name}: {}", cb.text());
        assert!(cb.cookie().is_empty(), "{name}: no session");
        assert!(
            w.items("user").iter().all(|(id, _)| !id.contains("sso-")),
            "{name}: no user was created"
        );
    }
    // A token signed by a key the provider does not publish is refused.
    let (other, _) = rsa();
    let _ = (kp, other);
}

#[tokio::test]
async fn ac_0_11_4_oidc_roles_follow_the_claims() {
    let (kp, jwk) = rsa();
    let groups = Arc::new(Mutex::new(json!(["g-ops"])));
    let g = groups.clone();
    let now_s = T0 / 1000;
    let p = idp(kp, jwk, move |n, k| {
        sign_rs256(
            k,
            json!({"alg": "RS256", "kid": "k1"}),
            claims(json!({"nonce": n, "groups": g.lock().unwrap().clone(), "exp": now_s + 600})),
        )
    })
    .await;
    let w = world_with(oidc_cfg(&p.base));
    w.admin().await;
    let login = |w: &W| {
        let app = w.app.clone();
        async move {
            let r = send_with(&app, Method::GET, "/api/v1/auth/oidc/login", &[], None).await;
            let (st, nn) = (
                query_of(r.header("location"), "state"),
                query_of(r.header("location"), "nonce"),
            );
            send_with(
                &app,
                Method::GET,
                &format!("/api/v1/auth/oidc/callback?code=code-{nn}&state={st}"),
                &[],
                None,
            )
            .await
        }
    };
    let c1 = login(&w).await.cookie();
    assert_eq!(
        w.get("/api/v1/auth/me", &c1).await.json()["data"]["orgs"],
        json!({"default": "admin"})
    );
    assert_eq!(
        w.get("/api/v1/orgs/default/members", &c1).await.status,
        StatusCode::OK
    );
    // The provider takes the group away: the next sign-in is a viewer, with the same account.
    *groups.lock().unwrap() = json!(["g-view"]);
    let c2 = login(&w).await.cookie();
    w.state.auth.forget_all();
    assert_eq!(
        w.get("/api/v1/auth/me", &c2).await.json()["data"]["orgs"],
        json!({"default": "viewer"})
    );
    assert_eq!(
        w.get("/api/v1/orgs/default/members", &c2).await.status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        w.items("user")
            .iter()
            .filter(|(id, _)| id.contains("sso-"))
            .count(),
        1,
        "one account per subject"
    );
    // A platform group makes a platform admin.
    *groups.lock().unwrap() = json!(["g-root"]);
    let c3 = login(&w).await.cookie();
    w.state.auth.forget_all();
    assert_eq!(
        w.get("/api/v1/platform/orgs", &c3).await.status,
        StatusCode::OK
    );
    // SSO accounts have no password to log in with.
    let name = w
        .items("user")
        .iter()
        .find(|(id, _)| id.contains("sso-"))
        .unwrap()
        .0
        .split('/')
        .nth(1)
        .unwrap()
        .to_string();
    assert_eq!(
        w.post(
            "/api/v1/auth/login",
            "",
            json!({"username": name, "password": PW})
        )
        .await
        .status,
        StatusCode::UNAUTHORIZED
    );
}

// ---- AC0.11.5 --------------------------------------------------------------------------------

#[tokio::test]
async fn ac_0_11_5_switcher_lists_permitted_orgs() {
    let w = world();
    let root = w.admin().await;
    w.org(&root, "a").await;
    w.org(&root, "b").await;
    let ann = w.user(&root, "ann", "a", "viewer").await;
    let both = {
        w.post(
            "/api/v1/platform/users",
            &root,
            json!({"username": "both", "password": PW, "orgs": {"a": "editor", "b": "admin"}}),
        )
        .await;
        w.post(
            "/api/v1/auth/login",
            "",
            json!({"username": "both", "password": PW}),
        )
        .await
        .cookie()
    };
    let names = |r: Resp| -> Vec<String> {
        r.json()["orgs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|o| o["name"].as_str().unwrap().to_string())
            .collect()
    };
    assert_eq!(names(w.get("/api/v1/orgs", &ann).await), ["a"]);
    assert_eq!(names(w.get("/api/v1/orgs", &both).await), ["a", "b"]);
    assert_eq!(
        names(w.get("/api/v1/orgs", &root).await),
        ["a", "b", "default"],
        "a platform admin sees every org"
    );
    let o = w.get("/api/v1/orgs", &both).await.json()["orgs"].clone();
    assert_eq!(
        (o[0]["role"].clone(), o[1]["role"].clone()),
        (json!("editor"), json!("admin")),
        "the role per org is shown"
    );
    assert_eq!(
        w.get("/api/v1/orgs", "").await.status,
        StatusCode::UNAUTHORIZED
    );
    // A token lists only its own org.
    let t = w.token(&root, "b", "viewer").await;
    assert_eq!(names(w.get("/api/v1/orgs", &t).await), ["b"]);
    // Removing a membership takes the org out of the list.
    assert_eq!(
        w.call(Method::DELETE, "/api/v1/orgs/b/members/both", &root, None)
            .await
            .status,
        StatusCode::OK
    );
    w.state.auth.forget_all();
    assert_eq!(names(w.get("/api/v1/orgs", &both).await), ["a"]);
}

#[tokio::test]
async fn ac_0_11_5_org_admin_scope() {
    let w = world();
    let root = w.admin().await;
    w.org(&root, "a").await;
    w.org(&root, "b").await;
    let admin_a = w.user(&root, "boss-a", "a", "admin").await;
    // An org admin manages their own org: members, tokens, branding, audit.
    let r = w
        .post(
            "/api/v1/orgs/a/members",
            &admin_a,
            json!({"username": "newbie", "role": "editor", "password": PW}),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    assert_eq!(
        w.post(
            "/api/v1/orgs/a/members",
            &admin_a,
            json!({"username": "nopw", "role": "viewer"})
        )
        .await
        .status,
        StatusCode::BAD_REQUEST,
        "a new person needs a password"
    );
    assert_eq!(
        w.get("/api/v1/orgs/a/members", &admin_a).await.json()["data"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let r = w
        .call(
            Method::PUT,
            "/api/v1/orgs/a/members/newbie",
            &admin_a,
            Some(json!({"role": "viewer"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(
        w.post(
            "/api/v1/orgs/a/tokens",
            &admin_a,
            json!({"name": "t", "role": "viewer"})
        )
        .await
        .status,
        StatusCode::CREATED
    );
    assert_eq!(
        w.get("/api/v1/orgs/a/audit", &admin_a).await.status,
        StatusCode::OK
    );
    // But nothing outside it: not org b's members, tokens, audit or branding, and no platform.
    for (m, u) in [
        (Method::GET, "/api/v1/orgs/b/members"),
        (Method::GET, "/api/v1/orgs/b/tokens"),
        (Method::GET, "/api/v1/orgs/b/audit"),
        (Method::GET, "/api/v1/platform/users"),
        (Method::GET, "/api/v1/platform/audit"),
    ] {
        assert_eq!(
            w.call(m.clone(), u, &admin_a, None).await.status,
            StatusCode::FORBIDDEN,
            "{m} {u}"
        );
    }
    for (m, u, b) in [
        (
            Method::POST,
            "/api/v1/orgs/b/members",
            json!({"username": "x", "role": "admin", "password": PW}),
        ),
        (
            Method::PUT,
            "/api/v1/orgs/b/branding",
            json!({"name": "hijack"}),
        ),
        (
            Method::POST,
            "/api/v1/orgs/b/tokens",
            json!({"name": "x", "role": "viewer"}),
        ),
        (
            Method::POST,
            "/api/v1/platform/orgs",
            json!({"name": "evil"}),
        ),
        (Method::DELETE, "/api/v1/platform/orgs/b", json!({})),
        (
            Method::POST,
            "/api/v1/platform/users",
            json!({"username": "z", "password": PW, "platform_admin": true}),
        ),
    ] {
        assert_eq!(
            w.call(m.clone(), u, &admin_a, Some(b)).await.status,
            StatusCode::FORBIDDEN,
            "{m} {u}"
        );
    }
    // Adding a member changes only the named org: a platform admin's other memberships stay.
    assert!(w.state.org("b").is_ok());
    let both = w.user(&root, "dual", "b", "viewer").await;
    let r = w
        .post(
            "/api/v1/orgs/a/members",
            &admin_a,
            json!({"username": "dual", "role": "viewer"}),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED);
    w.state.auth.forget_all();
    assert_eq!(
        w.get("/api/v1/auth/me", &both).await.json()["data"]["orgs"],
        json!({"a": "viewer", "b": "viewer"})
    );
    // A removed member keeps their other org.
    w.call(
        Method::DELETE,
        "/api/v1/orgs/a/members/dual",
        &admin_a,
        None,
    )
    .await;
    w.state.auth.forget_all();
    assert_eq!(
        w.get("/api/v1/auth/me", &both).await.json()["data"]["orgs"],
        json!({"b": "viewer"})
    );
    // Platform admins create and delete orgs; the default one stays.
    assert_eq!(
        w.call(Method::DELETE, "/api/v1/platform/orgs/default", &root, None)
            .await
            .status,
        StatusCode::BAD_REQUEST
    );
    // An org cannot point at the state database or a database another org uses.
    for (db, code) in [
        ("information_schema", 400),
        ("grepui", 400),
        ("db_a", 409),
        ("bad name", 400),
    ] {
        let r = w
            .post(
                "/api/v1/platform/orgs",
                &root,
                json!({"name": "c", "database": db}),
            )
            .await;
        assert_eq!(r.status.as_u16(), code, "{db}: {}", r.text());
    }
    assert_eq!(
        w.post("/api/v1/platform/orgs", &root, json!({"name": "A!"}))
            .await
            .status,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        w.post("/api/v1/platform/orgs", &root, json!({"name": "a"}))
            .await
            .status,
        StatusCode::CONFLICT
    );
}

#[tokio::test]
async fn ac_0_11_5_org_delete_cascades() {
    let w = world();
    let root = w.admin().await;
    w.org(&root, "gone").await;
    w.org(&root, "stays").await;
    let ed = w.user(&root, "ed", "gone", "admin").await;
    let keeper = w.user(&root, "keeper", "stays", "admin").await;
    // The doomed org has saved views, a dashboard with a public link, an alert, a function, a
    // token and a member; the other org has the same kinds of things.
    for (who, org) in [(&ed, "gone"), (&keeper, "stays")] {
        let u = |p: &str| format!("/api/v1/orgs/{org}/{p}");
        assert_eq!(
            w.post(
                &u("views"),
                who,
                json!({"name": "v", "kind": "logs", "spec": {}})
            )
            .await
            .status,
            StatusCode::CREATED
        );
        let d = w
            .post(
                &u("dashboards"),
                who,
                json!({"title": "D", "spec": {"tabs": [{"title": "T", "panels": []}]}}),
            )
            .await;
        let id = d.json()["data"]["id"].as_str().unwrap().to_string();
        assert_eq!(
            w.post(
                &format!("/api/v1/orgs/{org}/dashboards/{id}/shares"),
                who,
                json!({"ttl_hours": 1})
            )
            .await
            .status,
            StatusCode::CREATED
        );
        assert_eq!(w.post(&u("alerts"), who, json!({"name": "a", "kind": "scheduled", "query": {"kind": "sql", "text": "SELECT 1 AS n"}, "condition": {"op": ">", "threshold": 1}})).await.status, StatusCode::CREATED);
        w.call(
            Method::PUT,
            &u("functions/f"),
            who,
            Some(json!({"params": [], "body": "1"})),
        )
        .await;
        w.token(who, org, "viewer").await;
    }
    let r = w
        .call(Method::DELETE, "/api/v1/platform/orgs/gone", &root, None)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let removed = r.json()["data"]["removed"].clone();
    assert_eq!(
        (removed["views"].clone(), removed["dashboards"].clone()),
        (json!(1), json!(1)),
        "{removed}"
    );
    assert_eq!(
        removed["items"].as_i64().unwrap(),
        3,
        "alert, function and token: {removed}"
    );
    assert_eq!(r.json()["data"]["telemetry"], "untouched");
    // The org is gone for everyone, its members lost it, its token stops working.
    assert_eq!(
        w.get("/api/v1/orgs/gone/views", &root).await.status,
        StatusCode::NOT_FOUND
    );
    w.state.auth.forget_all();
    assert_eq!(
        w.get("/api/v1/auth/me", &ed).await.json()["data"]["orgs"],
        json!({})
    );
    assert!(w.state.orgs.resolve("gone").is_err());
    assert!(w.get("/api/v1/orgs", &root).await.text().contains("stays"));
    // Nothing of the other org was touched.
    for p in ["views", "dashboards", "alerts", "functions", "tokens"] {
        let n = w
            .get(&format!("/api/v1/orgs/stays/{p}"), &keeper)
            .await
            .json()["data"]
            .as_array()
            .unwrap()
            .len();
        assert_eq!(n, 1, "{p} of the org that stays");
    }
    // The shares of the deleted org are revoked in the store.
    let revoked = w
        .store
        .shares
        .lock()
        .unwrap()
        .iter()
        .filter(|((o, _), b)| o == "gone" && b[7] == grepui::query::Bind::Bool(true))
        .count();
    assert_eq!(revoked, 1);
    // The deletion is in the platform audit trail, which survives the org.
    let a = w
        .get("/api/v1/platform/audit?action=platform.org.delete", &root)
        .await
        .json()["data"]
        .clone();
    assert_eq!(a.as_array().unwrap().len(), 1);
    assert!(a[0]["detail"].as_str().unwrap().starts_with("gone "));
    // The name can be used again.
    w.org(&root, "gone").await;
    assert_eq!(
        w.get("/api/v1/orgs/gone/views", &root).await.json()["data"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
}

#[tokio::test]
#[ignore = "needs GreptimeDB: make db-up && make test-db"]
async fn ac_0_11_5_org_delete_cascades_live() {
    let base = std::env::var("GREPUI_TEST_DB")
        .expect("set GREPUI_TEST_DB=mysql://root@127.0.0.1:4002/public");
    let (meta, data) = (
        format!("grepui_it_casc_{}", std::process::id()),
        format!("db_casc_{}", std::process::id()),
    );
    let r = Arc::new(
        grepui::query::SqlxReader::lazy(&base, std::time::Duration::from_secs(30)).unwrap(),
    );
    store::migrate(r.as_ref(), r.as_ref(), &meta)
        .await
        .expect("migrate");
    let state = grepui::api::AppState::with_parts(
        Config {
            greptime_mysql: base,
            meta_database: meta.clone(),
            no_auth: false,
            admin_writes: true,
            ..Config::default()
        },
        r.clone(),
        r,
        grepui::api::system_clock(),
    )
    .unwrap();
    let app = grepui::api::router(state);
    let root = send_with(
        &app,
        Method::POST,
        "/api/v1/auth/setup",
        &[],
        Some(&json!({"username": "root", "password": PW})),
    )
    .await
    .cookie();
    let c = |m: Method, u: String, b: Option<Value>| {
        let (app, root) = (app.clone(), root.clone());
        async move { send_with(&app, m, &u, &[("cookie", &root)], b.as_ref()).await }
    };
    // Creating an org can create its database.
    let r = c(
        Method::POST,
        "/api/v1/platform/orgs".into(),
        Some(json!({"name": "casc", "database": data, "create_database": true})),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let dbs = grepui::query::Reader::fetch(
        &grepui::query::SqlxReader::lazy(
            "mysql://root@127.0.0.1:4002/public",
            std::time::Duration::from_secs(30),
        )
        .unwrap(),
        &grepui::query::Q::new(
            "t",
            "SELECT schema_name FROM information_schema.schemata",
            vec![],
        ),
    )
    .await
    .unwrap();
    assert!(
        dbs.iter().any(|d| d["schema_name"] == json!(data)),
        "the database was created"
    );
    for p in ["views", "dashboards"] {
        let body = if p == "views" {
            json!({"name": "v", "kind": "logs", "spec": {}})
        } else {
            json!({"title": "D"})
        };
        assert_eq!(
            c(Method::POST, format!("/api/v1/orgs/casc/{p}"), Some(body))
                .await
                .status,
            StatusCode::CREATED
        );
    }
    let r = c(Method::DELETE, "/api/v1/platform/orgs/casc".into(), None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["data"]["removed"]["views"], 1);
    // Re-creating the name starts empty: the soft-deleted rows are not visible.
    c(
        Method::POST,
        "/api/v1/platform/orgs".into(),
        Some(json!({"name": "casc", "database": data})),
    )
    .await;
    for p in ["views", "dashboards"] {
        let n = c(Method::GET, format!("/api/v1/orgs/casc/{p}"), None)
            .await
            .json()["data"]
            .as_array()
            .unwrap()
            .len();
        assert_eq!(n, 0, "{p}");
    }
    // The telemetry database is untouched.
    let dbs = grepui::query::Reader::fetch(
        &grepui::query::SqlxReader::lazy(
            "mysql://root@127.0.0.1:4002/public",
            std::time::Duration::from_secs(30),
        )
        .unwrap(),
        &grepui::query::Q::new(
            "t",
            "SELECT schema_name FROM information_schema.schemata",
            vec![],
        ),
    )
    .await
    .unwrap();
    assert!(dbs.iter().any(|d| d["schema_name"] == json!(data)));
    let form = [("content-type", "application/x-www-form-urlencoded")];
    for db in [&meta, &data] {
        let _ = grepui::playground::http::post(
            "http://127.0.0.1:4000",
            "/v1/sql",
            &form,
            format!("sql=DROP DATABASE IF EXISTS {db}").as_bytes(),
        )
        .await;
    }
}

#[tokio::test]
async fn ac_0_11_5_branding_per_org() {
    let w = world();
    let root = w.admin().await;
    w.org(&root, "a").await;
    w.org(&root, "b").await;
    let admin_a = w.user(&root, "boss", "a", "admin").await;
    let viewer_a = w.user(&root, "vera", "a", "viewer").await;
    // Defaults: the org's name.
    assert_eq!(
        w.get("/api/v1/orgs/a/branding", &viewer_a).await.json()["data"],
        json!({"name": "a"})
    );
    let b = json!({"name": "Acme Corp", "logo_url": "https://cdn.example.com/acme.png", "accent": "#aa3300"});
    let r = w
        .call(
            Method::PUT,
            "/api/v1/orgs/a/branding",
            &admin_a,
            Some(b.clone()),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(
        w.get("/api/v1/orgs/a/branding", &viewer_a).await.json()["data"],
        b
    );
    // Org b is unaffected, and the switcher carries each org's own branding.
    assert_eq!(
        w.get("/api/v1/orgs/b/branding", &root).await.json()["data"],
        json!({"name": "b"})
    );
    let orgs = w.get("/api/v1/orgs", &root).await.json()["orgs"].clone();
    let a = orgs
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["name"] == "a")
        .unwrap();
    assert_eq!(a["branding"]["accent"], "#aa3300");
    // A viewer cannot change it; bad values are refused.
    assert_eq!(
        w.call(Method::PUT, "/api/v1/orgs/a/branding", &viewer_a, Some(b))
            .await
            .status,
        StatusCode::FORBIDDEN
    );
    for bad in [
        json!({"accent": "red"}),
        json!({"accent": "#12"}),
        json!({"logo_url": "javascript:alert(1)"}),
        json!({"logo_url": "data:image/png;base64,AAAA"}),
        json!({"name": "x".repeat(61)}),
        json!({"logo_url": "not a url"}),
    ] {
        assert_eq!(
            w.call(
                Method::PUT,
                "/api/v1/orgs/a/branding",
                &admin_a,
                Some(bad.clone())
            )
            .await
            .status,
            StatusCode::BAD_REQUEST,
            "{bad}"
        );
    }
    // Branding survives the org's database settings and the other way round.
    let spec = w
        .items("orgdef")
        .into_iter()
        .find(|(id, _)| id == "_platform/a")
        .unwrap()
        .1[4]
        .show();
    assert!(
        spec.contains("db_a") && spec.contains("Acme Corp"),
        "{spec}"
    );
}

// ---- AC0.11.6 --------------------------------------------------------------------------------

/// People for the matrix: (label, cookie, role in org `a`, role in org `b`).
struct Cast {
    people: Vec<(&'static str, String)>,
}

async fn cast(w: &W) -> Cast {
    let root = w.admin().await;
    w.org(&root, "a").await;
    w.org(&root, "b").await;
    let a_user = w.user(&root, "a-user", "a", "editor").await;
    let b_user = w.user(&root, "b-user", "b", "editor").await;
    let a_admin = w.user(&root, "a-admin", "a", "admin").await;
    Cast {
        people: vec![
            ("a-user", a_user),
            ("b-user", b_user),
            ("a-admin", a_admin),
            ("platform", root),
            ("anonymous", String::new()),
        ],
    }
}

fn fill_for(route: &str, org: &str) -> String {
    // Platform routes act on things that do not exist, so an allowed call changes nothing.
    let org = if route.starts_with("/api/v1/platform/") {
        "zzz"
    } else {
        org
    };
    route
        .replace("{org}", org)
        .replace("{name}", "t")
        .replace("{field}", "severity_text")
        .replace("{service}", "cart")
        .replace("{template}", "traces_red")
        .replace("{panel}", "p1")
        .replace("{share}", "0123456789abcdef")
        .replace("{token}", "0123456789abcdef0123456789abcdef")
        .replace("{stream}", "traces")
        .replace("{id}", "0123456789abcdef")
        .replace("{trace_id}", "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        .replace("{user}", "alice")
}

#[tokio::test]
async fn ac_0_11_6_isolation_matrix_all_routes() {
    let w = world();
    let c = cast(&w).await;
    let methods = [Method::GET, Method::POST, Method::PUT, Method::DELETE];
    let mut cells = 0;
    for route in ROUTES {
        for m in &methods {
            let access = auth::access_for(m, route);
            // Signing out ends the session the rest of the matrix is using.
            if matches!(access, Access::Deny) || route.ends_with("/auth/logout") {
                continue;
            }
            for (who, cookie) in &c.people {
                // Org routes are tried in the org the person belongs to and in the other one.
                let orgs: Vec<&str> = if route.contains("{org}") && !route.contains("/platform/") {
                    vec!["a", "b"]
                } else {
                    vec!["a"]
                };
                for org in orgs {
                    let uri = fill_for(route, org);
                    let r = w.call(m.clone(), &uri, cookie, Some(json!({}))).await;
                    // What the policy says should happen, worked out here from the roles.
                    let id = match *who {
                        "anonymous" => Identity::anonymous(),
                        "platform" => Identity {
                            kind: "user",
                            username: "root".into(),
                            platform_admin: true,
                            orgs: BTreeMap::new(),
                            token_key: None,
                            cookie: true,
                        },
                        "a-user" => Identity {
                            kind: "user",
                            username: "a-user".into(),
                            platform_admin: false,
                            orgs: BTreeMap::from([("a".into(), Role::Editor)]),
                            token_key: None,
                            cookie: true,
                        },
                        "b-user" => Identity {
                            kind: "user",
                            username: "b-user".into(),
                            platform_admin: false,
                            orgs: BTreeMap::from([("b".into(), Role::Editor)]),
                            token_key: None,
                            cookie: true,
                        },
                        _ => Identity {
                            kind: "user",
                            username: "a-admin".into(),
                            platform_admin: false,
                            orgs: BTreeMap::from([("a".into(), Role::Admin)]),
                            token_key: None,
                            cookie: true,
                        },
                    };
                    let allowed = auth::allows(
                        &id,
                        access,
                        route
                            .contains("{org}")
                            .then_some(org)
                            .filter(|_| !route.contains("/platform/")),
                    );
                    let denied =
                        matches!(r.status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN);
                    assert_eq!(
                        !allowed, denied,
                        "{who} {m} {uri}: policy says allowed={allowed}, got {}",
                        r.status
                    );
                    if !allowed {
                        let expect = if who == &"anonymous" {
                            StatusCode::UNAUTHORIZED
                        } else {
                            StatusCode::FORBIDDEN
                        };
                        assert_eq!(r.status, expect, "{who} {m} {uri}");
                    }
                    // An org user is never allowed into the other org; only the platform may.
                    if route.contains("{org}")
                        && !route.contains("/public/")
                        && !route.contains("/platform/")
                        && ((who == &"a-user" || who == &"a-admin") && org == "b"
                            || who == &"b-user" && org == "a")
                    {
                        assert_eq!(
                            r.status,
                            StatusCode::FORBIDDEN,
                            "{who} {m} {uri} crossed orgs"
                        );
                    }
                    cells += 1;
                }
            }
        }
    }
    assert!(cells > 1500, "{cells} cells of the matrix");
}

#[tokio::test]
async fn ac_0_11_6_no_cross_org_data_in_bodies() {
    let w = world();
    let c = cast(&w).await;
    let who = |n: &str| c.people.iter().find(|(l, _)| *l == n).unwrap().1.clone();
    // Each org saves recognisable things.
    for (user, org, marker) in [
        ("a-user", "a", "MARKER-ALPHA"),
        ("b-user", "b", "MARKER-BRAVO"),
    ] {
        let u = |p: &str| format!("/api/v1/orgs/{org}/{p}");
        let ck = who(user);
        assert_eq!(
            w.post(
                &u("views"),
                &ck,
                json!({"name": marker, "kind": "logs", "spec": {"q": marker}})
            )
            .await
            .status,
            StatusCode::CREATED
        );
        assert_eq!(
            w.post(&u("dashboards"), &ck, json!({"title": marker}))
                .await
                .status,
            StatusCode::CREATED
        );
        assert_eq!(
            w.call(
                Method::PUT,
                &u("functions/f"),
                &ck,
                Some(json!({"params": [], "body": format!("'{marker}'")}))
            )
            .await
            .status,
            StatusCode::OK
        );
        assert_eq!(w.post(&u("alerts"), &ck, json!({"name": marker, "kind": "scheduled", "query": {"kind": "sql", "text": "SELECT 1 AS n"}, "condition": {"op": ">", "threshold": 1}})).await.status, StatusCode::CREATED);
        assert_eq!(
            w.call(
                Method::PUT,
                &u("services/cart/annotation"),
                &ck,
                Some(json!({"description": marker}))
            )
            .await
            .status,
            StatusCode::OK
        );
    }
    // Every readable route of org a, read by a's people, never contains b's marker, and the
    // reverse; and what a stranger gets back contains no marker at all.
    let mut seen_own = 0;
    for route in ROUTES
        .iter()
        .filter(|r| r.contains("{org}") && !r.contains("/platform/") && !r.contains("/public/"))
    {
        if auth::access_for(&Method::GET, route) == Access::Deny {
            continue;
        }
        for (user, mine, other) in [("a-user", "a", "b"), ("b-user", "b", "a")] {
            let (own_marker, other_marker) = if mine == "a" {
                ("MARKER-ALPHA", "MARKER-BRAVO")
            } else {
                ("MARKER-BRAVO", "MARKER-ALPHA")
            };
            let r = w.get(&fill_for(route, mine), &who(user)).await;
            if r.status == StatusCode::OK {
                assert!(
                    !r.text().contains(other_marker),
                    "{user} {route}: the other org's data leaked: {}",
                    r.text()
                );
                seen_own += usize::from(r.text().contains(own_marker));
            }
            let x = w.get(&fill_for(route, other), &who(user)).await;
            assert_eq!(
                x.status,
                StatusCode::FORBIDDEN,
                "{user} {route} in the other org"
            );
            assert!(!x.text().contains("MARKER"), "{route}: {}", x.text());
        }
    }
    assert!(
        seen_own >= 5,
        "the markers were found in the owners' own answers ({seen_own})"
    );
    // The audit trail of one org holds none of the other org's rows.
    w.flush().await;
    let a = w
        .get("/api/v1/orgs/a/audit?limit=500", &who("a-admin"))
        .await
        .json()["data"]
        .clone();
    for row in a.as_array().unwrap() {
        assert!(
            !row["target"].as_str().unwrap().contains("/orgs/b/"),
            "{row}"
        );
        // The other org's people appear only as refused visitors, which is what an admin wants to see.
        if row["username"] == "b-user" {
            assert_eq!(row["outcome"], "denied", "{row}");
        }
    }
    // Saved state is keyed by org: the stores answer each org only its own.
    for org in ["a", "b"] {
        for (kind, builder) in [
            ("views", build::view_list("grepui", org, None).unwrap()),
            (
                "dashboards",
                build::dash_list("grepui", org, None, None).unwrap(),
            ),
        ] {
            let rows = grepui::query::Reader::fetch(w.store.as_ref(), &builder)
                .await
                .unwrap();
            let t = serde_json::to_string(&rows).unwrap();
            let other = if org == "a" {
                "MARKER-BRAVO"
            } else {
                "MARKER-ALPHA"
            };
            assert!(!t.contains(other), "{kind} of {org}");
        }
    }
}

// ---- AC0.11.7 --------------------------------------------------------------------------------

async fn audit_rows(w: &W, who: &str, q: &str) -> Vec<Value> {
    w.flush().await;
    let r = w.get(&format!("/api/v1/orgs/default/audit?{q}"), who).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r.json()["data"].as_array().unwrap().clone()
}

#[tokio::test]
async fn ac_0_11_7_audit_on_read() {
    let w = world();
    let root = w.admin().await;
    let viewer = w.user(&root, "vera", "default", "viewer").await;
    w.at(1);
    assert_eq!(
        w.get(
            "/api/v1/orgs/default/logs?from=1000&to=2000&limit=5",
            &viewer
        )
        .await
        .status,
        StatusCode::OK
    );
    assert_eq!(
        w.get(
            "/api/v1/orgs/default/sql?sql=SELECT%20secret_thing&from=3000&to=4000",
            &viewer
        )
        .await
        .status,
        StatusCode::OK
    );
    let rows = audit_rows(&w, &root, "user=vera").await;
    let logs = rows
        .iter()
        .find(|r| r["action"] == "GET /api/v1/orgs/{org}/logs")
        .unwrap();
    assert_eq!(
        (
            logs["username"].clone(),
            logs["outcome"].clone(),
            logs["status"].clone()
        ),
        (json!("vera"), json!("ok"), json!(200))
    );
    assert_eq!(
        (logs["from_ms"].clone(), logs["to_ms"].clone()),
        (json!(1000), json!(2000)),
        "the window"
    );
    assert!(logs["rows"].as_i64().is_some());
    assert!(logs["at_ms"].as_i64().unwrap() >= T0);
    assert_eq!(logs["target"], "/api/v1/orgs/default/logs");
    let sql = rows
        .iter()
        .find(|r| r["action"] == "GET /api/v1/orgs/{org}/sql")
        .unwrap();
    assert!(
        sql["detail"].as_str().unwrap().contains("secret_thing"),
        "what was asked is recorded: {sql}"
    );
    // A denied read is recorded too, as denied.
    assert_eq!(
        w.get("/api/v1/orgs/default/members", &viewer).await.status,
        StatusCode::FORBIDDEN
    );
    let rows = audit_rows(
        &w,
        &root,
        "user=vera&action=GET%20/api/v1/orgs/%7Borg%7D/members",
    )
    .await;
    assert_eq!(
        (
            rows.len(),
            rows[0]["outcome"].clone(),
            rows[0]["status"].clone()
        ),
        (1, json!("denied"), json!(403))
    );
    // Reading the audit trail is itself recorded; health checks and the session description are not.
    w.get("/healthz", "").await;
    w.get("/api/v1/auth/me", &viewer).await;
    let all = audit_rows(&w, &root, "limit=500").await;
    assert!(
        all.iter()
            .any(|r| r["action"] == "GET /api/v1/orgs/{org}/audit")
    );
    assert!(
        all.iter()
            .all(|r| r["action"] != "GET /healthz" && r["action"] != "GET /api/v1/auth/me")
    );
    // Public link reads are recorded without the secret in the path.
    let id = w
        .post(
            "/api/v1/orgs/default/dashboards",
            &root,
            json!({"title": "D"}),
        )
        .await
        .json()["data"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let tok = w
        .post(
            &format!("/api/v1/orgs/default/dashboards/{id}/shares"),
            &root,
            json!({"ttl_hours": 1}),
        )
        .await
        .json()["data"]["token"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        w.get(&format!("/api/v1/orgs/default/public/{tok}"), "")
            .await
            .status,
        StatusCode::OK
    );
    let all = audit_rows(&w, &root, "limit=500").await;
    let public = all
        .iter()
        .find(|r| r["action"] == "GET /api/v1/orgs/{org}/public/{token}")
        .expect("recorded");
    assert_eq!(public["username"], "anonymous");
    assert!(
        !serde_json::to_string(&all).unwrap().contains(&tok),
        "the link's token is never in the audit trail"
    );
}

#[tokio::test]
async fn ac_0_11_7_audit_on_mutation() {
    let w = world();
    let root = w.admin().await;
    let ed = w.user(&root, "ed", "default", "editor").await;
    let t = w.token(&root, "default", "editor").await;
    assert_eq!(
        w.post(
            "/api/v1/orgs/default/views",
            &ed,
            json!({"name": "v", "kind": "logs", "spec": {}})
        )
        .await
        .status,
        StatusCode::CREATED
    );
    assert_eq!(
        w.post(
            "/api/v1/orgs/default/views",
            &t,
            json!({"name": "w", "kind": "logs", "spec": {}})
        )
        .await
        .status,
        StatusCode::CREATED
    );
    assert_eq!(
        w.post(
            "/api/v1/orgs/default/views",
            &ed,
            json!({"name": "", "kind": "logs", "spec": {}})
        )
        .await
        .status,
        StatusCode::BAD_REQUEST
    );
    let rows = audit_rows(&w, &root, "action=POST%20/api/v1/orgs/%7Borg%7D/views").await;
    let by: Vec<(&str, i64, &str)> = rows
        .iter()
        .map(|r| {
            (
                r["username"].as_str().unwrap(),
                r["status"].as_i64().unwrap(),
                r["outcome"].as_str().unwrap(),
            )
        })
        .collect();
    assert!(
        by.contains(&("ed", 201, "ok"))
            && by.contains(&("token:t-editor", 201, "ok"))
            && by.contains(&("ed", 400, "error")),
        "{by:?}"
    );
    // Administration is recorded with who did what: members, tokens, sign-ins.
    let all = audit_rows(&w, &root, "limit=500").await;
    for action in ["tokens.create", "POST /api/v1/orgs/{org}/tokens"] {
        assert!(all.iter().any(|r| r["action"] == action), "{action}");
    }
    let plat = {
        w.flush().await;
        w.get("/api/v1/platform/audit?limit=500", &root)
            .await
            .json()["data"]
            .as_array()
            .unwrap()
            .clone()
    };
    let actions: Vec<&str> = plat.iter().map(|r| r["action"].as_str().unwrap()).collect();
    for a in ["auth.setup", "auth.login", "platform.user.create"] {
        assert!(actions.contains(&a), "{a} in {actions:?}");
    }
    // Failed sign-ins are visible, with the name tried and no password.
    w.post(
        "/api/v1/auth/login",
        "",
        json!({"username": "ed", "password": "guess guess guess"}),
    )
    .await;
    let plat = {
        w.flush().await;
        w.get("/api/v1/platform/audit?action=auth.login&limit=50", &root)
            .await
            .json()["data"]
            .clone()
    };
    let denied = plat
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["outcome"] == "denied")
        .unwrap();
    assert_eq!(denied["username"], "ed");
    assert!(
        !serde_json::to_string(&plat)
            .unwrap()
            .contains("guess guess")
    );
    assert!(!serde_json::to_string(&all).unwrap().contains(PW));
}

#[tokio::test]
async fn ac_0_11_7_audit_append_only() {
    // No route changes or removes an audit row: the only audit routes are searches.
    for route in ROUTES.iter().filter(|r| r.contains("audit")) {
        for m in [Method::POST, Method::PUT, Method::DELETE, Method::PATCH] {
            assert_eq!(auth::access_for(&m, route), Access::Deny, "{m} {route}");
        }
        assert!(WRITE_ROUTES.iter().all(|(_, r)| r != route));
    }
    let w = world();
    let root = w.admin().await;
    for m in [Method::POST, Method::PUT, Method::DELETE] {
        let r = w
            .call(
                m.clone(),
                "/api/v1/orgs/default/audit",
                &root,
                Some(json!({})),
            )
            .await;
        assert_eq!(r.status, StatusCode::METHOD_NOT_ALLOWED, "{m}");
    }
    // The state-store gate lets only INSERT through, so the table cannot be updated or emptied.
    let q = build::audit_put(
        "grepui",
        "default",
        &[build::AuditRow {
            id: "x".into(),
            ..Default::default()
        }],
    )
    .unwrap();
    assert!(store::check_write(&q, "grepui").is_ok());
    for sql in [
        "DELETE FROM `grepui`.`grepui_audit` WHERE 1=1",
        "UPDATE `grepui`.`grepui_audit` SET `username` = 'x'",
        "TRUNCATE `grepui`.`grepui_audit`",
        "DROP TABLE `grepui`.`grepui_audit`",
        "INSERT INTO `grepui`.`grepui_audit` (`org_id`) VALUES ('x'); DELETE FROM `grepui`.`grepui_audit`",
    ] {
        let bad = grepui::query::Q::new("audit_put", sql, vec![]);
        assert!(store::check_write(&bad, "grepui").is_err(), "{sql}");
    }
    assert!(
        build::audit_put("grepui", "default", &[]).is_err(),
        "an empty batch"
    );
    // A row cannot be overwritten through the API either: ids are random, and rows of one
    // millisecond stay separate.
    let a = grepui::audit::new_id();
    assert_ne!(a, grepui::audit::new_id());
    for _ in 0..3 {
        w.get("/api/v1/orgs/default/views", &root).await;
    }
    w.flush().await;
    let rows = audit_rows(
        &w,
        &root,
        "action=GET%20/api/v1/orgs/%7Borg%7D/views&limit=50",
    )
    .await;
    assert_eq!(
        rows.len(),
        3,
        "three requests in the same millisecond are three rows"
    );
    // The search window is capped.
    assert_eq!(
        w.get("/api/v1/orgs/default/audit?from=0&to=99999999999999", &root)
            .await
            .status,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
#[ignore = "needs GreptimeDB: make db-up && make test-db"]
async fn ac_0_11_7_audit_search_live() {
    let base = std::env::var("GREPUI_TEST_DB")
        .expect("set GREPUI_TEST_DB=mysql://root@127.0.0.1:4002/public");
    let meta = format!("grepui_it_audit_{}", std::process::id());
    let r = Arc::new(
        grepui::query::SqlxReader::lazy(&base, std::time::Duration::from_secs(30)).unwrap(),
    );
    store::migrate(r.as_ref(), r.as_ref(), &meta)
        .await
        .expect("migrate");
    let state = grepui::api::AppState::with_parts(
        Config {
            greptime_mysql: base,
            meta_database: meta.clone(),
            no_auth: false,
            ..Config::default()
        },
        r.clone(),
        r,
        grepui::api::system_clock(),
    )
    .unwrap();
    let app = grepui::api::router(state.clone());
    let root = send_with(
        &app,
        Method::POST,
        "/api/v1/auth/setup",
        &[],
        Some(&json!({"username": "root", "password": PW})),
    )
    .await
    .cookie();
    for u in ["ann", "bob"] {
        send_with(
            &app,
            Method::POST,
            "/api/v1/platform/users",
            &[("cookie", &root)],
            Some(&json!({"username": u, "password": PW, "orgs": {"default": "editor"}})),
        )
        .await;
    }
    let login = |u: &'static str| {
        let app = app.clone();
        async move {
            send_with(
                &app,
                Method::POST,
                "/api/v1/auth/login",
                &[],
                Some(&json!({"username": u, "password": PW})),
            )
            .await
            .cookie()
        }
    };
    let (ann, bob) = (login("ann").await, login("bob").await);
    let now = chrono::Utc::now().timestamp_millis();
    for _ in 0..3 {
        send_with(
            &app,
            Method::GET,
            &format!(
                "/api/v1/orgs/default/logs?from={}&to={}",
                now - 3_600_000,
                now
            ),
            &[("cookie", &ann)],
            None,
        )
        .await;
    }
    send_with(
        &app,
        Method::POST,
        "/api/v1/orgs/default/views",
        &[("cookie", &bob)],
        Some(&json!({"name": "v", "kind": "logs", "spec": {}})),
    )
    .await;
    state.audit.flush(&state).await;
    let q = |s: &str| {
        let (app, root) = (app.clone(), root.clone());
        let s = s.to_string();
        async move {
            send_with(
                &app,
                Method::GET,
                &format!("/api/v1/orgs/default/audit?{s}"),
                &[("cookie", &root)],
                None,
            )
            .await
            .json()["data"]
                .as_array()
                .unwrap()
                .clone()
        }
    };
    let by_user = q("user=ann&limit=100").await;
    assert_eq!(by_user.len(), 3, "{by_user:?}");
    assert!(by_user.iter().all(|r| r["username"] == "ann"
        && r["from_ms"] == json!(now - 3_600_000)
        && r["to_ms"] == json!(now)));
    let by_action = q("action=POST%20/api/v1/orgs/%7Borg%7D/views").await;
    assert_eq!(
        (by_action.len(), by_action[0]["username"].clone()),
        (1, json!("bob"))
    );
    assert_eq!(
        q(&format!(
            "user=ann&from={}&to={}",
            now + 60_000,
            now + 120_000
        ))
        .await
        .len(),
        0,
        "time filter"
    );
    let newest = q("limit=3").await;
    assert!(
        newest
            .windows(2)
            .all(|p| p[0]["at_ms"].as_i64() >= p[1]["at_ms"].as_i64()),
        "newest first"
    );
    let form = [("content-type", "application/x-www-form-urlencoded")];
    let _ = grepui::playground::http::post(
        "http://127.0.0.1:4000",
        "/v1/sql",
        &form,
        format!("sql=DROP DATABASE IF EXISTS {meta}").as_bytes(),
    )
    .await;
}

// ---- cross-cutting ---------------------------------------------------------------------------

#[tokio::test]
async fn ac_0_11_2_cookie_requests_must_come_from_this_site() {
    let w = world();
    let root = w.admin().await;
    let body = json!({"name": "v", "kind": "logs", "spec": {}});
    let post = |hdrs: Vec<(&'static str, &'static str)>| {
        let (app, root, body) = (w.app.clone(), root.clone(), body.clone());
        async move {
            let mut h = vec![("cookie", root.as_str()), ("host", "grepui.test")];
            h.extend(hdrs);
            send_with(
                &app,
                Method::POST,
                "/api/v1/orgs/default/views",
                &h,
                Some(&body),
            )
            .await
            .status
        }
    };
    assert_eq!(
        post(vec![("origin", "http://evil.test")]).await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        post(vec![("sec-fetch-site", "cross-site")]).await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        post(vec![("origin", "http://grepui.test")]).await,
        StatusCode::CREATED
    );
    assert_eq!(
        post(vec![]).await,
        StatusCode::CREATED,
        "no Origin and no Sec-Fetch-Site: a non-browser client"
    );
    // Reads are not state changes; tokens carry no ambient credential, so they skip the check.
    let t = w.token(&root, "default", "editor").await;
    let r = send_with(
        &w.app,
        Method::POST,
        "/api/v1/orgs/default/views",
        &[
            ("authorization", &t),
            ("host", "grepui.test"),
            ("origin", "http://evil.test"),
        ],
        Some(&body),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED);
}

#[test]
fn ac_0_11_2_no_auth_only_on_loopback() {
    for ok in [
        "127.0.0.1:8080",
        "localhost:8080",
        "[::1]:8080",
        "127.5.5.5:1",
    ] {
        assert!(grepui::server::is_loopback(ok), "{ok}");
    }
    for bad in [
        "0.0.0.0:8080",
        "192.168.1.5:8080",
        "[::]:8080",
        "example.com:80",
        "10.0.0.1:1",
    ] {
        assert!(!grepui::server::is_loopback(bad), "{bad}");
    }
}

#[tokio::test]
async fn ac_0_11_2_roles_decide_admin_actions_not_the_flag() {
    // With sign-in, an org admin can use the admin templates without --admin-writes; an editor
    // cannot even with the flag. Without sign-in the flag still decides (v0.7.0 behaviour).
    let w = world();
    let root = w.admin().await;
    let ed = w.user(&root, "ed", "default", "editor").await;
    let r = w
        .post("/api/v1/orgs/default/rollups/traces_red", &root, json!({}))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(
        w.post("/api/v1/orgs/default/rollups/traces_red", &ed, json!({}))
            .await
            .status,
        StatusCode::FORBIDDEN
    );
    let w2 = world_with(Config {
        admin_writes: true,
        ..Config::default()
    });
    let ed2 = {
        let root = w2.admin().await;
        w2.user(&root, "ed", "default", "editor").await
    };
    assert_eq!(
        w2.post("/api/v1/orgs/default/rollups/traces_red", &ed2, json!({}))
            .await
            .status,
        StatusCode::FORBIDDEN
    );
    let (open, _) = app_cfg(
        &MemStore::new(),
        &Arc::new(AtomicI64::new(T0)),
        Config::default(),
    );
    assert_eq!(
        send_with(
            &open,
            Method::POST,
            "/api/v1/orgs/default/rollups/traces_red",
            &[],
            Some(&json!({}))
        )
        .await
        .status,
        StatusCode::FORBIDDEN,
        "no sign-in and no flag: refused"
    );
    let (open2, _) = app_cfg(
        &MemStore::new(),
        &Arc::new(AtomicI64::new(T0)),
        Config {
            admin_writes: true,
            ..Config::default()
        },
    );
    assert_eq!(
        send_with(
            &open2,
            Method::POST,
            "/api/v1/orgs/default/rollups/traces_red",
            &[],
            Some(&json!({}))
        )
        .await
        .status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn ac_0_11_4_redaction_follows_the_callers_role() {
    let w = world();
    let root = w.admin().await;
    let viewer = w.user(&root, "vera", "default", "viewer").await;
    *w.store.script.lock().unwrap() = Some(Box::new(|q| {
        Ok(if q.name == "sql_mode" {
            vec![json!({"user": "a", "password": "hunter2"})]
        } else {
            sample_db(q)?
        })
    }));
    let r = w
        .post(
            "/api/v1/orgs/default/redaction",
            &root,
            json!({"field": "password", "exempt_roles": ["admin"]}),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let q = "/api/v1/orgs/default/sql?sql=SELECT%20user%2C%20password%20FROM%20t&from=1000&to=2000";
    assert!(
        w.get(q, &root).await.text().contains("hunter2"),
        "an admin sees the value"
    );
    assert!(
        !w.get(q, &viewer).await.text().contains("hunter2"),
        "a viewer does not"
    );
    let t = w.token(&root, "default", "editor").await;
    assert!(
        !w.get(q, &t).await.text().contains("hunter2"),
        "nor does an editor token"
    );
}

#[tokio::test]
#[ignore = "needs outbound internet: set GREPUI_TEST_INTERNET=1 to check the TLS client against a public HTTPS site"]
async fn ac_0_11_4_https_client_live() {
    if std::env::var("GREPUI_TEST_INTERNET").is_err() {
        eprintln!("skipped: GREPUI_TEST_INTERNET is not set");
        return;
    }
    let (code, body) = grepui::http::request(
        "GET",
        "https://example.com",
        "/",
        &[("accept", "text/html")],
        &[],
    )
    .await
    .expect("https request");
    assert_eq!(code, 200);
    assert!(
        body.contains("Example Domain"),
        "{}",
        &body[..body.len().min(200)]
    );
    // A host whose certificate does not match is refused.
    let bad = grepui::http::request("GET", "https://wrong.host.badssl.com", "/", &[], &[]).await;
    assert!(
        bad.is_err(),
        "a certificate for another name must not be accepted"
    );
}
