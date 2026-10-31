//! AC0.9.1 – AC0.9.8: rules, real-time and composite alerts, destinations and templates, dedup and
//! downtimes, incidents, the leader lease and anomaly detection.
mod common;
use axum::{Router, http::Method, http::StatusCode};
use common::*;
use grepui::{
    alerts::{
        self, Anomaly, Op, cond,
        cron::Cron,
        engine::{self, Engine, RuleState},
        notify, template,
    },
    api::{AppState, Shared, router},
    config::Config,
    playground::{http, synth},
    query::{Q, SqlxReader},
    store,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicI64, Ordering},
    },
    time::Duration,
};

const BASE: &str = "/api/v1/orgs/default";
const T0: i64 = 1_793_400_000_000;

struct T {
    app: Router,
    state: Shared,
    store: Arc<MemStore>,
    clock: Arc<AtomicI64>,
    rec: Arc<Rec>,
    /// What the rule's query returns.
    rows: Arc<Mutex<Vec<Value>>>,
    engine: Engine,
}

fn setup() -> T {
    let store = MemStore::new();
    let clock = Arc::new(AtomicI64::new(T0));
    let rec = Arc::new(Rec::default());
    let (app, state) = alert_app(&store, &clock, &rec);
    let rows = Arc::new(Mutex::new(vec![json!({"n": 9})]));
    let r = rows.clone();
    *store.script.lock().unwrap() = Some(Box::new(move |q: &Q| {
        Ok(match q.name {
            "alert_rule" | "alert_builder" => r.lock().unwrap().clone(),
            _ => sample_db(q)?,
        })
    }));
    let engine = Engine::new(state.clone(), rec.clone()).with_settle(Duration::ZERO);
    T {
        app,
        state,
        store,
        clock,
        rec,
        rows,
        engine,
    }
}

impl T {
    fn at(&self, secs: i64) {
        self.clock.store(T0 + secs * 1000, Ordering::SeqCst);
    }
    fn org(&self) -> &grepui::org::Org {
        self.state.org("default").unwrap()
    }
    async fn tick(&self) -> Vec<String> {
        self.engine.tick(self.org()).await.unwrap()
    }
    async fn post(&self, path: &str, body: Value) -> Resp {
        send_json(&self.app, Method::POST, &format!("{BASE}/{path}"), &body).await
    }
    async fn get(&self, path: &str) -> Resp {
        get(&self.app, &format!("{BASE}/{path}")).await
    }
    async fn rule(&self, body: Value) -> String {
        let r = self.post("alerts", body).await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
        r.json()["data"]["id"].as_str().unwrap().to_string()
    }
    async fn webhook(&self, name: &str) -> String {
        let r = self
            .post(
                "destinations",
                json!({"name": name, "kind": "webhook", "url": "http://127.0.0.1:9/hook"}),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
        r.json()["data"]["id"].as_str().unwrap().to_string()
    }
    async fn history(&self, id: &str) -> Vec<Value> {
        self.get(&format!("alerts/{id}/history")).await.json()["data"]
            .as_array()
            .unwrap()
            .clone()
    }
    async fn state_of(&self, id: &str) -> Value {
        let row = engine::item(&self.state, self.org(), "state", id)
            .await
            .unwrap()
            .unwrap();
        serde_json::from_str(&row.spec).unwrap()
    }
}

fn base(name: &str) -> Value {
    json!({
        "name": name, "kind": "scheduled",
        "query": {"kind": "sql", "text": "SELECT count(*) AS n FROM opentelemetry_logs WHERE timestamp >= $__from AND timestamp < $__to"},
        "condition": {"op": ">", "threshold": 5.0},
        "schedule": {"every_s": 60}, "lookback_s": 300,
    })
}

fn with(mut v: Value, k: &str, x: Value) -> Value {
    v[k] = x;
    v
}

fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

// ---- AC0.9.1 ---------------------------------------------------------------------------------

#[tokio::test]
async fn ac_0_9_1_scheduled_rule_fires() {
    let t = setup();
    let d = t.webhook("hook").await;
    let id = t
        .rule(with(base("errors high"), "destinations", json!([d])))
        .await;
    assert_eq!(t.tick().await, vec![id.clone()]);
    assert_eq!(t.rec.count(), 1);
    let (dest, msg) = t.rec.sent.lock().unwrap()[0].clone();
    assert_eq!(dest, "hook");
    assert!(msg.subject.contains("errors high"), "{}", msg.subject);
    assert_eq!(msg.payload["value"], 9.0);
    assert_eq!(t.state_of(&id).await["state"], "firing");
    // The query ran over the look-back window: both macros became timestamp literals.
    let q = t
        .store
        .seen
        .lock()
        .unwrap()
        .iter()
        .find(|q| q.name == "alert_rule")
        .cloned()
        .unwrap();
    assert!(!q.sql.contains("$__"), "{}", q.sql);
    // Not due again before the interval has passed.
    t.at(30);
    assert!(t.tick().await.is_empty());
    t.at(61);
    assert_eq!(t.tick().await, vec![id.clone()]);
    assert_eq!(t.rec.count(), 2);
    // Below the threshold the rule is ok and stays quiet.
    *t.rows.lock().unwrap() = vec![json!({"n": 2})];
    t.at(130);
    t.tick().await;
    assert_eq!(t.state_of(&id).await["state"], "ok");
    assert_eq!(t.rec.count(), 2);
}

#[tokio::test]
async fn ac_0_9_1_cooldown_respected() {
    let t = setup();
    let d = t.webhook("hook").await;
    let rule = with(
        with(base("slow down"), "destinations", json!([d])),
        "cooldown_s",
        json!(300),
    );
    let id = t.rule(rule).await;
    t.tick().await;
    assert_eq!(t.rec.count(), 1);
    for secs in [61, 122, 183, 244] {
        t.at(secs);
        t.tick().await;
    }
    assert_eq!(t.rec.count(), 1, "still inside the cooldown");
    let h = t.history(&id).await;
    assert_eq!(h[0]["outcome"], "suppressed_cooldown");
    t.at(305);
    t.tick().await;
    assert_eq!(t.rec.count(), 2, "the cooldown has ended");
}

#[test]
fn ac_0_9_1_condition_operators() {
    let cases = [
        (Op::Gt, 5.0, 5.0, false),
        (Op::Gt, 5.1, 5.0, true),
        (Op::Ge, 5.0, 5.0, true),
        (Op::Lt, 4.9, 5.0, true),
        (Op::Lt, 5.0, 5.0, false),
        (Op::Le, 5.0, 5.0, true),
        (Op::Eq, 5.0, 5.0, true),
        (Op::Eq, 5.5, 5.0, false),
        (Op::Ne, 5.5, 5.0, true),
        (Op::Ne, 5.0, 5.0, false),
    ];
    for (op, v, thr, want) in cases {
        assert_eq!(cond::breach(op, v, thr), want, "{op:?} {v} {thr}");
    }
    // The operators are spelled as symbols in the saved rule.
    let r = alerts::parse_rule(&with(
        base("ops"),
        "condition",
        json!({"op": ">=", "threshold": 1}),
    ))
    .unwrap();
    assert_eq!(r.condition.unwrap().op, Op::Ge);
    assert!(
        alerts::parse_rule(&with(
            base("ops"),
            "condition",
            json!({"op": "=>", "threshold": 1})
        ))
        .is_err()
    );
}

#[tokio::test]
async fn ac_0_9_1_preview_does_not_fire() {
    let t = setup();
    let d = t.webhook("hook").await;
    let rule = with(base("preview me"), "destinations", json!([d]));
    let writes = t.store.write_count();
    let r = t
        .get(&format!("alerts/preview?rule={}", enc(&rule.to_string())))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let d = &r.json()["data"];
    assert_eq!(d["state"], "firing");
    assert_eq!(d["would_notify"], false);
    assert_eq!(d["samples"][0]["value"], 9.0);
    assert!(
        d["message"]["subject"]
            .as_str()
            .unwrap()
            .contains("preview me")
    );
    assert_eq!(t.rec.count(), 0, "nothing was sent");
    assert_eq!(t.store.write_count(), writes, "nothing was written");
    // A saved rule previews the same way, and still writes nothing.
    let id = t.rule(rule).await;
    let writes = t.store.write_count();
    let r = t.get(&format!("alerts/{id}/preview")).await;
    assert_eq!(r.json()["data"]["state"], "firing");
    assert_eq!(t.store.write_count(), writes);
    assert_eq!(t.rec.count(), 0);
    // A rule that does not validate is refused before anything runs.
    let bad = with(
        base("x"),
        "query",
        json!({"kind": "sql", "text": "DROP TABLE t"}),
    );
    let r = t
        .get(&format!("alerts/preview?rule={}", enc(&bad.to_string())))
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn ac_0_9_1_rule_validation() {
    let t = setup();
    let bad = |v: Value| {
        let t = &t;
        async move { t.post("alerts", v).await.status }
    };
    assert_eq!(bad(with(base("a"), "name", json!(""))).await, 400);
    assert_eq!(bad(with(base("a"), "lookback_s", json!(0))).await, 400);
    assert_eq!(
        bad(with(base("a"), "extra", json!(1))).await,
        400,
        "unknown keys"
    );
    assert_eq!(
        bad(with(
            base("a"),
            "query",
            json!({"kind": "sql", "text": "SELECT 1; SELECT 2"})
        ))
        .await,
        400
    );
    assert_eq!(
        bad(with(
            base("a"),
            "query",
            json!({"kind": "promql", "text": "up; drop"})
        ))
        .await,
        400
    );
    assert_eq!(
        bad(with(
            base("a"),
            "schedule",
            json!({"every_s": 60, "cron": "* * * * *"})
        ))
        .await,
        400
    );
    assert_eq!(
        bad(with(base("a"), "schedule", json!({"cron": "61 * * * *"}))).await,
        400
    );
    assert_eq!(
        bad(with(base("a"), "destinations", json!(["nope"]))).await,
        400
    );
    assert_eq!(bad(with(base("a"), "condition", Value::Null)).await, 400);
    assert_eq!(bad(with(base("a"), "budget_ms", json!(100))).await, 400);
    // Builder queries only take column names.
    let b = |field: &str| {
        with(
            base("b"),
            "query",
            json!({"kind": "builder", "builder": {"source": "logs", "agg": "avg", "field": field}}),
        )
    };
    assert_eq!(bad(b("severity_number")).await, 201);
    assert_eq!(bad(b("x`; DROP TABLE t; --")).await, 400);
}

#[tokio::test]
async fn ac_0_9_1_builder_and_cron_rules() {
    let t = setup();
    let id = t
        .rule(with(
            base("builder"),
            "query",
            json!({"kind": "builder", "builder": {"source": "logs", "agg": "count",
                "filters": [{"field": "severity_text", "value": "ERROR"}], "group_by": ["service_name"]}}),
        ))
        .await;
    *t.rows.lock().unwrap() = vec![json!({"service_name": "cart", "value": 12.0})];
    t.tick().await;
    let q = t
        .store
        .seen
        .lock()
        .unwrap()
        .iter()
        .find(|q| q.name == "alert_builder")
        .cloned()
        .unwrap();
    assert!(
        q.sql.contains("`opentelemetry_logs`")
            && q.sql.contains("CAST(`severity_text` AS STRING) = ?"),
        "{}",
        q.sql
    );
    assert!(
        q.binds.iter().any(|b| b.show() == "ERROR"),
        "values are bound, not formatted"
    );
    assert!(!q.sql.contains("ERROR"));
    let st = t.state_of(&id).await;
    assert_eq!(st["state"], "firing");
    // Cron: due only in a minute that matches.
    let c = Cron::parse("*/10 * * * *").unwrap();
    assert!(c.matches(T0 - T0 % 600_000));
    assert_eq!(
        c.next_after(T0 - T0 % 600_000),
        Some(T0 - T0 % 600_000 + 600_000)
    );
    let r = alerts::parse_rule(&with(
        base("c"),
        "schedule",
        json!({"cron": "*/10 * * * *"}),
    ))
    .unwrap();
    let st = RuleState {
        last_eval_ms: T0 - T0 % 600_000,
        ..Default::default()
    };
    assert!(!engine::due(&r, &st, T0 - T0 % 600_000 + 599_000));
    assert!(engine::due(&r, &st, T0 - T0 % 600_000 + 600_000));
}

// ---- AC0.9.2 ---------------------------------------------------------------------------------

#[tokio::test]
async fn ac_0_9_2_realtime_interval() {
    let t = setup();
    let mut rule = base("live");
    rule["kind"] = json!("realtime");
    rule.as_object_mut().unwrap().remove("schedule");
    let id = t.rule(rule.clone()).await;
    let saved = t.get(&format!("alerts/{id}")).await.json()["data"]["spec"].clone();
    assert_eq!(
        saved["schedule"]["every_s"], 10,
        "the default is 10 seconds"
    );
    assert_eq!(saved["budget_ms"], alerts::DEFAULT_BUDGET_MS);
    assert_eq!(t.tick().await.len(), 1);
    t.at(5);
    assert!(t.tick().await.is_empty());
    t.at(10);
    assert_eq!(t.tick().await.len(), 1);
    // The interval is bounded: 5-60 seconds, and a real-time rule has no cron.
    let r = |s: Value| with(rule.clone(), "schedule", s);
    assert_eq!(t.post("alerts", r(json!({"every_s": 4}))).await.status, 400);
    assert_eq!(
        t.post("alerts", r(json!({"every_s": 61}))).await.status,
        400
    );
    assert_eq!(t.post("alerts", r(json!({"every_s": 5}))).await.status, 201);
    assert_eq!(
        t.post("alerts", r(json!({"cron": "* * * * *"})))
            .await
            .status,
        400
    );
}

#[tokio::test]
async fn ac_0_9_2_budget_skips_rule() {
    let t = setup();
    let mut rule = base("costly");
    rule["kind"] = json!("realtime");
    rule["budget_ms"] = json!(100);
    rule.as_object_mut().unwrap().remove("schedule");
    let id = t.rule(rule).await;
    // Its last run took 500 ms against a budget of 100.
    let st = RuleState {
        state: "ok".into(),
        last_eval_ms: T0 - 20_000,
        last_ms: 500,
        ..Default::default()
    };
    engine::save_state(&t.state, t.org(), &id, &st, T0)
        .await
        .unwrap();
    let before = t
        .store
        .seen
        .lock()
        .unwrap()
        .iter()
        .filter(|q| q.name == "alert_rule")
        .count();
    t.tick().await;
    let after = t
        .store
        .seen
        .lock()
        .unwrap()
        .iter()
        .filter(|q| q.name == "alert_rule")
        .count();
    assert_eq!(before, after, "the query was not run");
    assert_eq!(t.history(&id).await[0]["outcome"], "skipped_budget");
    let st = t.state_of(&id).await;
    assert_eq!(st["skip_until_ms"], T0 + 60_000, "paused for six intervals");
    // Paused: nothing runs until the pause is over; then it is tried again.
    t.at(30);
    assert!(t.tick().await.is_empty());
    t.at(60);
    assert_eq!(t.tick().await.len(), 1);
    assert_eq!(t.history(&id).await[0]["outcome"], "fired");
    assert!(cond::over_budget(500, Some(100)));
    assert!(!cond::over_budget(50, Some(100)));
    assert!(!cond::over_budget(5_000, None));
}

// ---- AC0.9.3 ---------------------------------------------------------------------------------

#[test]
fn ac_0_9_3_composite_logic() {
    use alerts::Expr::*;
    let on = |ids: &'static [&'static str]| move |id: &str| ids.contains(&id);
    let a = || Alert("a".into());
    let b = || Alert("b".into());
    assert!(alerts::eval_expr(&And(vec![a(), b()]), &on(&["a", "b"])));
    assert!(!alerts::eval_expr(&And(vec![a(), b()]), &on(&["a"])));
    assert!(alerts::eval_expr(&Or(vec![a(), b()]), &on(&["b"])));
    assert!(!alerts::eval_expr(&Or(vec![a(), b()]), &on(&[])));
    assert!(alerts::eval_expr(&Not(Box::new(a())), &on(&["b"])));
    assert!(alerts::eval_expr(
        &And(vec![a(), Not(Box::new(b()))]),
        &on(&["a"])
    ));
    // The JSON form is the natural one.
    let e: alerts::Expr =
        serde_json::from_value(json!({"and": [{"alert": "a"}, {"not": {"alert": "b"}}]})).unwrap();
    assert_eq!(e, And(vec![a(), Not(Box::new(b()))]));
}

#[tokio::test]
async fn ac_0_9_3_composite_fires_from_states() {
    let t = setup();
    let d = t.webhook("hook").await;
    // A fires (9 > 5), B does not (threshold 100).
    let a = t.rule(base("A")).await;
    let b = t
        .rule(with(
            base("B"),
            "condition",
            json!({"op": ">", "threshold": 100}),
        ))
        .await;
    let comp = |e: Value| json!({"name": "both", "kind": "composite", "composite": e, "destinations": [d.clone()], "schedule": {"every_s": 60}});
    let c = t
        .rule(comp(json!({"and": [{"alert": a}, {"not": {"alert": b}}]})))
        .await;
    let ran = t.tick().await;
    // Rules with queries run first, so the composite sees this tick's states.
    assert_eq!(ran.last().unwrap(), &c);
    assert_eq!(t.state_of(&c).await["state"], "firing");
    assert_eq!(t.rec.count(), 1);
    // B joins in: the composite clears.
    *t.rows.lock().unwrap() = vec![json!({"n": 500})];
    t.at(61);
    t.tick().await;
    assert_eq!(t.state_of(&c).await["state"], "ok");
}

#[tokio::test]
async fn ac_0_9_3_composite_cycle_rejected() {
    let t = setup();
    let a = t.rule(base("A")).await;
    let comp = |name: &str, e: Value| json!({"name": name, "kind": "composite", "composite": e});
    let x = t.rule(comp("X", json!({"alert": a}))).await;
    let y = t.rule(comp("Y", json!({"or": [{"alert": x}]}))).await;
    // X would now depend on Y, which depends on X.
    let put = |id: &str, e: Value| {
        let t = &t;
        let id = id.to_string();
        async move {
            send_json(
                &t.app,
                Method::PUT,
                &format!("{BASE}/alerts/{id}"),
                &comp("X", e),
            )
            .await
        }
    };
    let r = put(&x, json!({"alert": y})).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert!(
        r.json()["error"].as_str().unwrap().contains("itself"),
        "{}",
        r.text()
    );
    // Directly on itself, and through a longer chain.
    assert_eq!(put(&x, json!({"and": [{"alert": x}]})).await.status, 400);
    // A reference to nothing is refused too.
    assert_eq!(
        t.post("alerts", comp("Z", json!({"alert": "ffffffff"})))
            .await
            .status,
        400
    );
    // An alert other composites use cannot be deleted.
    let r = send_json(
        &t.app,
        Method::DELETE,
        &format!("{BASE}/alerts/{a}"),
        &json!({}),
    )
    .await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    // The pure check agrees.
    let others = BTreeMap::from([("x".to_string(), vec!["y".to_string()])]);
    assert!(alerts::has_cycle(
        "y",
        &alerts::Expr::Alert("x".into()),
        &others
    ));
    assert!(!alerts::has_cycle(
        "z",
        &alerts::Expr::Alert("x".into()),
        &others
    ));
}

// ---- AC0.9.4 ---------------------------------------------------------------------------------

#[test]
fn ac_0_9_4_template_variables() {
    let vars = BTreeMap::from([
        ("alert.name".to_string(), "disk".to_string()),
        ("labels.host".to_string(), "web-1\r\nBcc: x@y.z".to_string()),
    ]);
    let (s, unknown) = template::render("{{alert.name}} on {{ labels.host }} {{what}}", &vars);
    assert_eq!(
        s, "disk on web-1\nBcc: x@y.z ",
        "carriage returns are dropped"
    );
    assert_eq!(unknown, vec!["what"]);
    // The message the engine builds uses the rule's template and the labels.
    let rule = alerts::parse_rule(&with(
        base("disk full"),
        "template",
        json!({"subject": "{{state}}: {{alert.name}} {{labels.host}}", "body": "{{value}} {{threshold}}"}),
    ))
    .unwrap();
    let sample = engine::Sample {
        labels: BTreeMap::from([("host".to_string(), "web-1".to_string())]),
        value: 9.5,
        compared: None,
        breached: true,
    };
    let (m, unknown) = engine::build_message("default", "abc", &rule, "firing", &sample, T0);
    assert_eq!(m.subject, "firing: disk full web-1");
    assert_eq!(m.body, "9.5000 > 5");
    assert!(unknown.is_empty());
    assert_eq!(m.payload["labels"]["host"], "web-1");
    assert_eq!(m.payload["alert"]["name"], "disk full");
}

/// A one-shot HTTP server: answers every request with `status` and sends each request's text out.
async fn http_server(
    status: &'static str,
) -> (String, tokio::sync::mpsc::UnboundedReceiver<String>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = l.accept().await else {
                return;
            };
            let tx = tx.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 16 * 1024];
                let mut got = 0;
                loop {
                    let n = s.read(&mut buf[got..]).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    got += n;
                    let text = String::from_utf8_lossy(&buf[..got]).to_string();
                    if let Some((head, body)) = text.split_once("\r\n\r\n") {
                        let len = head
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                            })
                            .unwrap_or(0);
                        if body.len() >= len {
                            let _ = tx.send(text);
                            break;
                        }
                    }
                }
                let _ = s
                    .write_all(
                        format!(
                            "HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        )
                        .as_bytes(),
                    )
                    .await;
            });
        }
    });
    (format!("http://{addr}/hook?x=1"), rx)
}

#[tokio::test]
async fn ac_0_9_4_webhook_delivery() {
    let (url, mut rx) = http_server("200 OK").await;
    let payload =
        json!({"alert": {"name": "disk"}, "state": "firing", "message": "line1\nline2 \"quoted\""});
    notify::webhook(&url, &payload).await.unwrap();
    let req = rx.recv().await.unwrap();
    assert!(req.starts_with("POST /hook?x=1 HTTP/1.1\r\n"), "{req}");
    assert!(
        req.to_ascii_lowercase()
            .contains("content-type: application/json")
    );
    let body: Value = serde_json::from_str(req.split_once("\r\n\r\n").unwrap().1).unwrap();
    assert_eq!(
        body, payload,
        "the message is a JSON string, so it cannot change the structure"
    );
    // An error status is reported, not swallowed.
    let (url, _rx) = http_server("500 Internal Server Error").await;
    let e = notify::webhook(&url, &payload).await.unwrap_err();
    assert!(e.contains("HTTP 500"), "{e}");
}

#[tokio::test]
async fn ac_0_9_4_test_send_reports_error() {
    let store = MemStore::new();
    let clock = Arc::new(AtomicI64::new(T0));
    let app = app_with_store(&store, &clock);
    let mk = |url: String| {
        let app = app.clone();
        async move {
            let r = send_json(
                &app,
                Method::POST,
                &format!("{BASE}/destinations"),
                &json!({"name": "d", "kind": "webhook", "url": url}),
            )
            .await;
            assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
            r.json()["data"]["id"].as_str().unwrap().to_string()
        }
    };
    let test = |id: String| {
        let app = app.clone();
        async move {
            let r = send_json(
                &app,
                Method::POST,
                &format!("{BASE}/destinations/{id}/test"),
                &json!({}),
            )
            .await;
            assert_eq!(r.status, StatusCode::OK);
            r.json()["data"].clone()
        }
    };
    // Nothing listens on port 9: the answer says so.
    let bad = test(mk("http://127.0.0.1:9/x".into()).await).await;
    assert_eq!(bad["ok"], false);
    assert!(
        bad["error"].as_str().unwrap().contains("cannot connect"),
        "{bad}"
    );
    let (url, mut rx) = http_server("204 No Content").await;
    let good = test(mk(url).await).await;
    assert_eq!(good, json!({"ok": true}));
    let req = rx.recv().await.unwrap();
    assert!(req.contains("\"test\":true"), "{req}");
    // Email without a relay reports what is missing.
    let r = send_json(
        &app,
        Method::POST,
        &format!("{BASE}/destinations"),
        &json!({"name": "m", "kind": "email", "to": ["ops@example.com"]}),
    )
    .await;
    let id = r.json()["data"]["id"].as_str().unwrap().to_string();
    let mail = test(id).await;
    assert_eq!(mail["ok"], false);
    assert!(mail["error"].as_str().unwrap().contains("--smtp"), "{mail}");
}

#[tokio::test]
async fn ac_0_9_4_email_over_smtp() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    let got = tokio::spawn(async move {
        let (s, _) = l.accept().await.unwrap();
        let (r, mut w) = s.into_split();
        let mut r = BufReader::new(r);
        let mut log = vec![];
        w.write_all(b"220 hi\r\n").await.unwrap();
        let mut in_data = false;
        loop {
            let mut line = String::new();
            if r.read_line(&mut line).await.unwrap() == 0 {
                break;
            }
            let l = line.trim_end().to_string();
            log.push(l.clone());
            if in_data {
                if l == "." {
                    in_data = false;
                    w.write_all(b"250 queued\r\n").await.unwrap();
                }
                continue;
            }
            let reply = match l.split_whitespace().next().unwrap_or("") {
                "DATA" => {
                    in_data = true;
                    "354 go\r\n"
                }
                "QUIT" => "221 bye\r\n",
                _ => "250 ok\r\n",
            };
            w.write_all(reply.as_bytes()).await.unwrap();
            if reply.starts_with("221") {
                break;
            }
        }
        log
    });
    let smtp = notify::Smtp::parse(&format!("127.0.0.1:{port}"), "grepui@example.com").unwrap();
    notify::email(
        &smtp,
        &["a@example.com".into(), "b@example.com".into()],
        "disk é\r\nBcc: evil@example.com",
        "first\n.dot line\nlast",
    )
    .await
    .unwrap();
    let log = got.await.unwrap();
    assert!(
        log.contains(&"MAIL FROM:<grepui@example.com>".to_string()),
        "{log:?}"
    );
    assert!(log.contains(&"RCPT TO:<a@example.com>".to_string()));
    assert!(log.contains(&"RCPT TO:<b@example.com>".to_string()));
    let text = log.join("\n");
    assert!(
        text.contains("Subject: =?UTF-8?B?"),
        "non-ASCII subjects are encoded: {text}"
    );
    assert!(
        !text.contains("\nBcc:"),
        "a newline in the subject cannot add a header"
    );
    assert!(
        log.contains(&"..dot line".to_string()),
        "leading dots are stuffed"
    );
    // Addresses that could smuggle commands or headers never validate.
    for bad in [
        "a@b.c>\r\nRCPT TO:<x@y.z",
        "a b@c.d",
        "a@b",
        "<a@b.c>",
        "a@b.c,d@e.f",
    ] {
        assert!(!alerts::valid_email(bad), "{bad:?}");
    }
    assert!(alerts::valid_email("ops@example.com"));
}

#[tokio::test]
async fn ac_0_9_4_destination_rules() {
    let t = setup();
    let mk = |v: Value| {
        let t = &t;
        async move { t.post("destinations", v).await.status }
    };
    let hook = |u: &str| json!({"name": "h", "kind": "webhook", "url": u});
    assert_eq!(mk(hook("http://10.0.0.5:8080/in")).await, 201);
    for bad in [
        "https://example.com/x",
        "http://user:pw@example.com/",
        "http://169.254.169.254/latest/meta-data",
        "http://0.0.0.0/",
        "file:///etc/passwd",
        "not a url",
    ] {
        assert_eq!(mk(hook(bad)).await, 400, "{bad}");
    }
    assert_eq!(
        mk(json!({"name": "m", "kind": "email", "to": []})).await,
        400
    );
    assert_eq!(
        mk(json!({"name": "m", "kind": "email", "to": ["a@b.c\r\nBcc: x@y.z"]})).await,
        400
    );
    assert_eq!(mk(json!({"name": "m", "kind": "sms"})).await, 400);
    // A destination in use cannot be deleted.
    let d = t.webhook("used").await;
    t.rule(with(base("uses it"), "destinations", json!([d.clone()])))
        .await;
    let r = send_json(
        &t.app,
        Method::DELETE,
        &format!("{BASE}/destinations/{d}"),
        &json!({}),
    )
    .await;
    assert_eq!(r.status, StatusCode::CONFLICT);
}

// ---- AC0.9.5 ---------------------------------------------------------------------------------

#[tokio::test]
async fn ac_0_9_5_dedup_by_fingerprint() {
    let t = setup();
    let d = t.webhook("hook").await;
    *t.rows.lock().unwrap() = vec![
        json!({"service": "cart", "n": 9}),
        json!({"service": "payment", "n": 12}),
    ];
    let rule = |fields: Value| {
        let mut r = with(base("per service"), "destinations", json!([d.clone()]));
        r["dedup"] = json!({"fields": fields, "window_s": 600});
        r
    };
    let id = t.rule(rule(json!(["service"]))).await;
    t.tick().await;
    assert_eq!(t.rec.count(), 2, "one notification per service");
    // Inside the window the same two are duplicates.
    t.at(61);
    t.tick().await;
    assert_eq!(t.rec.count(), 2);
    assert_eq!(t.history(&id).await[0]["outcome"], "suppressed_dedup");
    // A new service is a new fingerprint and notifies on its own.
    *t.rows.lock().unwrap() = vec![
        json!({"service": "cart", "n": 9}),
        json!({"service": "search", "n": 20}),
    ];
    t.at(122);
    t.tick().await;
    assert_eq!(t.rec.count(), 3);
    assert_eq!(
        t.rec.sent.lock().unwrap()[2].1.payload["labels"]["service"],
        "search"
    );
    // After the window cart may notify again; search, seen later, is still inside its own.
    t.at(700);
    t.tick().await;
    assert_eq!(t.rec.count(), 4);
    t.at(800);
    t.tick().await;
    assert_eq!(
        t.rec.count(),
        5,
        "now search is out of its window; cart, seen at 700, is not"
    );
    // With no fingerprint fields every series is "the same alert": one notification.
    let t2 = setup();
    let d2 = t2.webhook("hook").await;
    *t2.rows.lock().unwrap() = vec![
        json!({"service": "a", "n": 9}),
        json!({"service": "b", "n": 9}),
    ];
    let mut r = with(base("one"), "destinations", json!([d2]));
    r["dedup"] = json!({"window_s": 600});
    t2.rule(r).await;
    t2.tick().await;
    assert_eq!(t2.rec.count(), 1);
    // The fingerprint ignores labels that are not listed, and depends on the rule.
    let l = |v: &str| {
        BTreeMap::from([
            ("host".to_string(), v.to_string()),
            ("pid".to_string(), "1".to_string()),
        ])
    };
    let f = ["host".to_string()];
    assert_eq!(
        cond::fingerprint("a", &l("x"), &f),
        cond::fingerprint("a", &l("x"), &f)
    );
    assert_ne!(
        cond::fingerprint("a", &l("x"), &f),
        cond::fingerprint("a", &l("y"), &f)
    );
    assert_ne!(
        cond::fingerprint("a", &l("x"), &f),
        cond::fingerprint("b", &l("x"), &f)
    );
}

#[tokio::test]
async fn ac_0_9_5_compare_with_past() {
    // Pure: ratio and difference, and no answer when there is nothing to compare with.
    assert_eq!(cond::compare_value("ratio", 120.0, Some(100.0)), Some(1.2));
    assert_eq!(cond::compare_value("diff", 120.0, Some(100.0)), Some(20.0));
    assert_eq!(cond::compare_value("ratio", 5.0, Some(0.0)), None);
    assert_eq!(cond::compare_value("diff", 5.0, None), None);
    // Through the engine: the second query ran an hour earlier.
    let t = setup();
    let d = t.webhook("hook").await;
    let rule = with(
        with(base("jump"), "destinations", json!([d])),
        "condition",
        json!({"op": ">", "threshold": 1.5, "compare": {"offset_s": 3600, "mode": "ratio"}}),
    );
    let id = t.rule(rule).await;
    let calls = Arc::new(Mutex::new(vec![]));
    let c = calls.clone();
    *t.store.script.lock().unwrap() = Some(Box::new(move |q: &Q| {
        if q.name != "alert_rule" {
            return sample_db(q);
        }
        let mut c = c.lock().unwrap();
        c.push(q.sql.clone());
        // First the current window (300 now), then the one an hour back (100).
        Ok(vec![json!({"n": if c.len() == 1 { 300 } else { 100 }})])
    }));
    t.tick().await;
    let calls = calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 2);
    assert_ne!(calls[0], calls[1], "the windows differ");
    assert_eq!(t.rec.count(), 1, "3x the hour before is above 1.5");
    let h = t.history(&id).await;
    assert_eq!(h[0]["detail"]["samples"][0]["compared"], 3.0);
    assert_eq!(h[0]["detail"]["samples"][0]["value"], 300.0);
}

#[tokio::test]
async fn ac_0_9_5_downtime_silences() {
    let t = setup();
    let d = t.webhook("hook").await;
    let id = t
        .rule(with(base("noisy"), "destinations", json!([d])))
        .await;
    let other = t.rule(with(base("other"), "name", json!("other"))).await;
    // A downtime for another alert does not silence this one.
    let r = t
        .post("downtimes", json!({"name": "elsewhere", "from_ms": T0 - 1000, "to_ms": T0 + 3_600_000, "alerts": [other]}))
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    t.tick().await;
    assert_eq!(t.rec.count(), 1);
    // One for this alert, in force now, does.
    let r = t
        .post("downtimes", json!({"name": "maintenance", "from_ms": T0, "to_ms": T0 + 300_000, "alerts": [id.clone()]}))
        .await;
    let down_id = r.json()["data"]["id"].as_str().unwrap().to_string();
    t.at(61);
    t.tick().await;
    assert_eq!(t.rec.count(), 1, "silenced");
    let h = t.history(&id).await;
    assert_eq!(h[0]["outcome"], "silenced");
    assert_eq!(
        h[0]["state"], "firing",
        "the rule still evaluates and records"
    );
    // The downtime ends: the next breach notifies.
    t.at(400);
    t.tick().await;
    assert_eq!(t.rec.count(), 2);
    // An empty list covers every alert; deleting a downtime ends it.
    let r = t
        .post(
            "downtimes",
            json!({"name": "all", "from_ms": T0, "to_ms": T0 + 3_600_000}),
        )
        .await;
    let all = r.json()["data"]["id"].as_str().unwrap().to_string();
    t.at(470);
    t.tick().await;
    assert_eq!(t.rec.count(), 2);
    let r = send_json(
        &t.app,
        Method::DELETE,
        &format!("{BASE}/downtimes/{all}"),
        &json!({}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    let _ = down_id;
    t.at(540);
    t.tick().await;
    assert_eq!(t.rec.count(), 3);
    assert_eq!(
        t.post("downtimes", json!({"name": "x", "from_ms": 10, "to_ms": 5}))
            .await
            .status,
        400
    );
}

#[tokio::test]
async fn ac_0_9_5_history_recorded() {
    let t = setup();
    let d = t.webhook("hook").await;
    let id = t.rule(with(base("hist"), "destinations", json!([d]))).await;
    t.tick().await; // fires
    *t.rows.lock().unwrap() = vec![json!({"n": 1})];
    t.at(61);
    t.tick().await; // ok
    // The database fails: the evaluation is recorded as an error.
    *t.store.script.lock().unwrap() = Some(Box::new(|q: &Q| {
        if q.name == "alert_rule" {
            Err(grepui::error::ReadError::Db("boom: secret detail".into()))
        } else {
            sample_db(q)
        }
    }));
    t.at(122);
    t.tick().await;
    let h = t.history(&id).await;
    assert_eq!(h.len(), 3, "every evaluation is recorded");
    let outcomes: Vec<&str> = h.iter().map(|r| r["outcome"].as_str().unwrap()).collect();
    assert_eq!(outcomes, ["error", "ok", "fired"], "newest first");
    assert_eq!(h[2]["detail"]["deliveries"][0]["ok"], true);
    assert_eq!(h[2]["detail"]["samples"][0]["value"], 9.0);
    let err = h[0]["detail"]["error"].as_str().unwrap();
    assert!(
        !err.contains("secret"),
        "database details stay out of the history: {err}"
    );
    assert_eq!(t.state_of(&id).await["state"], "error");
    // All alerts, and a limit.
    let all = t.get("alerts/history?limit=2").await.json()["data"]
        .as_array()
        .unwrap()
        .len();
    assert_eq!(all, 2);
    // The list shows the state next to each rule.
    let list = t.get("alerts").await.json()["data"].clone();
    assert_eq!(list[0]["status"]["state"], "error");
}

#[tokio::test]
async fn ac_0_9_5_alerts_import_export() {
    let t = setup();
    let d = t.webhook("hook").await;
    let a = t
        .rule(with(
            with(base("one"), "destinations", json!([d])),
            "folder",
            json!("ops"),
        ))
        .await;
    let mut dedup = base("two");
    dedup["dedup"] = json!({"fields": ["service"], "window_s": 60});
    t.rule(dedup).await;
    let exp = t.get("alerts/export").await.json()["data"].clone();
    assert_eq!(exp[alerts::BUNDLE_MARK], 1);
    assert_eq!(exp["alerts"].as_array().unwrap().len(), 2);
    // Import into a fresh store: same specs, destinations that do not exist there are dropped.
    let u = setup();
    let r = u.post("alerts/import", exp.clone()).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let out = r.json()["data"].clone();
    assert_eq!(out["imported"].as_array().unwrap().len(), 2);
    assert!(
        out["notes"][0].as_str().unwrap().contains("destinations"),
        "{out}"
    );
    let got = u.get(&format!("alerts/{a}")).await.json()["data"]["spec"].clone();
    let mut want = exp["alerts"][0]["spec"].clone();
    for e in exp["alerts"].as_array().unwrap() {
        if e["id"] == json!(a) {
            want = e["spec"].clone();
        }
    }
    want["destinations"] = json!([]);
    assert_eq!(got, want);
    // Importing again skips what exists; a single export document works; junk is refused.
    let again = u.post("alerts/import", exp.clone()).await.json()["data"].clone();
    assert_eq!(again["skipped"].as_array().unwrap().len(), 2);
    assert_eq!(again["imported"].as_array().unwrap().len(), 0);
    let one = json!({alerts::EXPORT_MARK: 1, "spec": base("solo")});
    assert_eq!(u.post("alerts/import", one).await.status, 201);
    assert_eq!(
        u.post("alerts/import", json!({"hello": 1})).await.status,
        400
    );
    let bad = json!({alerts::EXPORT_MARK: 1, "spec": with(base("bad"), "kind", json!("nope"))});
    assert_eq!(u.post("alerts/import", bad).await.status, 400);
}

// ---- AC0.9.6 ---------------------------------------------------------------------------------

#[tokio::test]
async fn ac_0_9_6_incident_lifecycle() {
    let t = setup();
    let a = t.rule(base("down")).await;
    let r = t
        .post(&format!("alerts/{a}/incident"), json!({"note": "paged"}))
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let inc = r.json()["data"].clone();
    let id = inc["id"].as_str().unwrap().to_string();
    assert_eq!(inc["spec"]["state"], "open");
    assert_eq!(inc["spec"]["created_ms"], T0);
    t.at(60);
    let r = t
        .post(&format!("incidents/{id}/ack"), json!({"note": "on it"}))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["data"]["spec"]["state"], "acknowledged");
    assert_eq!(r.json()["data"]["spec"]["acknowledged_ms"], T0 + 60_000);
    // It cannot be acknowledged twice.
    assert_eq!(
        t.post(&format!("incidents/{id}/ack"), json!({}))
            .await
            .status,
        409
    );
    t.at(120);
    let r = t
        .post(&format!("incidents/{id}/resolve"), json!({"note": "fixed"}))
        .await;
    let spec = r.json()["data"]["spec"].clone();
    assert_eq!(spec["state"], "resolved");
    assert_eq!(spec["resolved_ms"], T0 + 120_000);
    let tl: Vec<(&str, i64, &str)> = spec["timeline"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            (
                e["event"].as_str().unwrap(),
                e["ms"].as_i64().unwrap(),
                e["note"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        tl,
        [
            ("opened", T0, "paged"),
            ("acknowledged", T0 + 60_000, "on it"),
            ("resolved", T0 + 120_000, "fixed")
        ]
    );
    assert_eq!(
        t.post(&format!("incidents/{id}/resolve"), json!({}))
            .await
            .status,
        409
    );
    assert_eq!(
        t.post(&format!("incidents/{id}/ack"), json!({}))
            .await
            .status,
        409
    );
    // Open incidents can be resolved without an acknowledgement; the list filters by state.
    let b = t.rule(base("other")).await;
    let i2 = t
        .post(&format!("alerts/{b}/incident"), json!({}))
        .await
        .json()["data"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        t.post(&format!("incidents/{i2}/resolve"), json!({}))
            .await
            .status,
        200
    );
    let open = t.get("incidents?state=open").await.json()["data"]
        .as_array()
        .unwrap()
        .len();
    let all = t.get("incidents").await.json()["data"]
        .as_array()
        .unwrap()
        .len();
    assert_eq!((open, all), (0, 2));
    assert_eq!(t.post("incidents/ffff/ack", json!({})).await.status, 404);
}

#[tokio::test]
async fn ac_0_9_6_incident_from_alert() {
    let t = setup();
    let d = t.webhook("hook").await;
    let mut rule = with(base("pages"), "destinations", json!([d]));
    rule["open_incident"] = json!(true);
    let id = t.rule(rule).await;
    t.tick().await;
    let list = t.get("incidents").await.json()["data"].clone();
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["spec"]["alert_id"], json!(id));
    assert_eq!(list[0]["spec"]["title"], "pages");
    // Firing again adds to the same incident instead of opening another.
    t.at(61);
    t.tick().await;
    let list = t.get("incidents").await.json()["data"].clone();
    assert_eq!(list.as_array().unwrap().len(), 1);
    let events: Vec<&str> = list[0]["spec"]["timeline"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["event"].as_str().unwrap())
        .collect();
    assert_eq!(events, ["opened", "fired_again"]);
    // One active incident per alert, also by hand.
    assert_eq!(
        t.post(&format!("alerts/{id}/incident"), json!({}))
            .await
            .status,
        409
    );
    // Once resolved, the next firing opens a new one.
    let iid = list[0]["id"].as_str().unwrap().to_string();
    t.post(&format!("incidents/{iid}/resolve"), json!({})).await;
    t.at(122);
    t.tick().await;
    assert_eq!(
        t.get("incidents?state=open").await.json()["data"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        t.get("incidents").await.json()["data"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

// ---- AC0.9.7 ---------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ac_0_9_7_single_leader() {
    let t = setup();
    let d = t.webhook("hook").await;
    for i in 0..6 {
        t.rule(with(
            with(base(&format!("r{i}")), "destinations", json!([d.clone()])),
            "cooldown_s",
            json!(0),
        ))
        .await;
    }
    // Two replicas with their own identity, sharing the store, ticking at the same moment.
    let b = Arc::new(
        Engine::new(t.state.clone(), t.rec.clone()).with_settle(Duration::from_millis(60)),
    );
    let a = Arc::new(
        Engine::new(t.state.clone(), t.rec.clone()).with_settle(Duration::from_millis(60)),
    );
    let org = t.org().clone();
    let (oa, ob) = (org.clone(), org);
    let (ea, eb) = (a.clone(), b.clone());
    let ja = tokio::spawn(async move { ea.tick(&oa).await.unwrap() });
    let jb = tokio::spawn(async move { eb.tick(&ob).await.unwrap() });
    let (ra, rb) = (ja.await.unwrap(), jb.await.unwrap());
    assert_eq!(
        ra.len() + rb.len(),
        6,
        "each rule ran on exactly one replica: {ra:?} {rb:?}"
    );
    assert!(ra.iter().all(|r| !rb.contains(r)));
    assert_eq!(t.rec.count(), 6, "each rule notified once, not twice");
    // The lease names its holder; the list shows it.
    let list = t.get("alerts").await.json()["data"].clone();
    for r in list.as_array().unwrap() {
        let leader = r["status"]["leader"].as_str().unwrap();
        assert!(leader == a.holder || leader == b.holder);
    }
    assert_ne!(a.holder, b.holder);
}

#[tokio::test]
async fn ac_0_9_7_lease_takeover() {
    let t = setup();
    let d = t.webhook("hook").await;
    let id = t
        .rule(with(
            with(base("ha"), "destinations", json!([d])),
            "schedule",
            json!({"every_s": 30}),
        ))
        .await;
    let a = Engine::new(t.state.clone(), t.rec.clone()).with_settle(Duration::ZERO);
    let b = Engine::new(t.state.clone(), t.rec.clone()).with_settle(Duration::ZERO);
    assert_eq!(a.tick(t.org()).await.unwrap(), vec![id.clone()]);
    // A keeps renewing its lease while it runs; B stays out although the rule is due again.
    t.at(20);
    assert!(a.tick(t.org()).await.unwrap().is_empty());
    t.at(31);
    assert!(
        b.tick(t.org()).await.unwrap().is_empty(),
        "A holds the lease"
    );
    // A stops (no more ticks). Its last renewal was at 20 s, so the lease ends at 50 s.
    t.at(49);
    assert!(
        b.tick(t.org()).await.unwrap().is_empty(),
        "the lease has not run out yet"
    );
    t.at(51);
    assert_eq!(
        b.tick(t.org()).await.unwrap(),
        vec![id.clone()],
        "B takes over"
    );
    assert_eq!(t.rec.count(), 2);
    // A coming back is now the follower: B renews its lease and the rule is due again.
    t.at(70);
    assert!(b.tick(t.org()).await.unwrap().is_empty());
    t.at(85);
    assert!(a.tick(t.org()).await.unwrap().is_empty());
    // The pure decision.
    let l = cond::Lease {
        holder: "a".into(),
        expires_ms: 1000,
    };
    use cond::LeaseAction::*;
    assert_eq!(cond::lease_action(Some(&l), "b", 500, 1000), Follow);
    assert_eq!(
        cond::lease_action(Some(&l), "b", 1000, 1000),
        Claim,
        "expired"
    );
    assert_eq!(cond::lease_action(Some(&l), "a", 100, 1000), Hold);
    assert_eq!(
        cond::lease_action(Some(&l), "a", 900, 1000),
        Claim,
        "renew before it runs out"
    );
    assert_eq!(cond::lease_action(None, "a", 0, 1000), Claim);
}

#[tokio::test]
#[ignore = "needs GreptimeDB: make db-up && make test-db"]
async fn ac_0_9_7_lease_takeover_live() {
    let base_url = std::env::var("GREPUI_TEST_DB")
        .expect("set GREPUI_TEST_DB=mysql://root@127.0.0.1:4002/public");
    let (data_db, meta_db) = (
        format!("demo_it_alerts_{}", std::process::id()),
        format!("grepui_it_alerts_{}", std::process::id()),
    );
    let form = [("content-type", "application/x-www-form-urlencoded")];
    http::post(
        "http://127.0.0.1:4000",
        "/v1/sql",
        &form,
        format!("sql=CREATE DATABASE IF NOT EXISTS {data_db}").as_bytes(),
    )
    .await
    .unwrap();
    let now_ns = chrono::Utc::now().timestamp_nanos_opt().unwrap() as u64;
    let batch = synth::generate(13, 40, now_ns, 30);
    grepui::playground::send(
        "http://127.0.0.1:4000",
        &data_db,
        "opentelemetry_traces",
        "opentelemetry_logs",
        &batch,
    )
    .await
    .expect("seed");
    let url = base_url.replace("/public", &format!("/{data_db}"));
    let r = Arc::new(SqlxReader::lazy(&url, Duration::from_secs(30)).unwrap());
    store::migrate(r.as_ref(), r.as_ref(), &meta_db)
        .await
        .expect("migrate");
    let clock = Arc::new(AtomicI64::new(chrono::Utc::now().timestamp_millis()));
    let rec = Arc::new(Rec::default());
    let mk = || {
        let c = clock.clone();
        AppState::with_sender(
            Config {
                greptime_mysql: url.clone(),
                meta_database: meta_db.clone(),
                ..Config::default()
            },
            r.clone(),
            r.clone(),
            Arc::new(move || c.load(Ordering::SeqCst)),
            rec.clone(),
        )
        .unwrap()
    };
    let state = mk();
    let app = router(state.clone());
    let spans = batch.spans.len();
    // A rule over the seeded spans: they were all written within the last 30 minutes.
    let rule = json!({
        "name": "spans seen", "kind": "scheduled",
        "query": {"kind": "sql", "text": "SELECT service_name, count(*) AS n FROM opentelemetry_traces WHERE timestamp >= $__from AND timestamp < $__to GROUP BY 1 ORDER BY 1"},
        "condition": {"op": ">", "threshold": 0}, "lookback_s": 7200, "schedule": {"every_s": 30},
        "dedup": {"fields": ["service_name"], "window_s": 3600}, "open_incident": true,
        "destinations": [],
    });
    let resp = send_json(&app, Method::POST, &format!("{BASE}/alerts"), &rule).await;
    assert_eq!(resp.status, StatusCode::CREATED, "{}", resp.text());
    let id = resp.json()["data"]["id"].as_str().unwrap().to_string();
    let org = state.org("default").unwrap();
    // Two replicas: separate engines over the same database.
    let a = Engine::new(mk(), rec.clone()).with_settle(Duration::from_millis(150));
    let b = Engine::new(mk(), rec.clone()).with_settle(Duration::from_millis(150));
    let (ra, rb) = tokio::join!(a.tick(org), b.tick(org));
    let (ra, rb) = (ra.unwrap(), rb.unwrap());
    assert_eq!(
        ra.len() + rb.len(),
        1,
        "exactly one replica evaluated the rule: {ra:?} {rb:?}"
    );
    let (holder, other) = if ra.is_empty() { (&b, &a) } else { (&a, &b) };
    let h = get(&app, &format!("{BASE}/alerts/{id}/history"))
        .await
        .json()["data"]
        .clone();
    assert_eq!(h.as_array().unwrap().len(), 1);
    assert_eq!(h[0]["outcome"], "fired");
    // One fingerprint per service: the number of services in the seed.
    let services = batch
        .spans
        .iter()
        .map(|s| s.service)
        .collect::<std::collections::HashSet<_>>()
        .len();
    assert_eq!(h[0]["detail"]["fired"].as_array().unwrap().len(), services);
    assert!(spans > 0);
    // The per-service counts are exact: they add up to every seeded span.
    let total: f64 = h[0]["detail"]["samples"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["value"].as_f64().unwrap())
        .sum();
    assert_eq!(total as usize, spans);
    // A builder rule over the same table counts the server spans exactly.
    let want = batch.spans.iter().filter(|s| s.kind == 2).count() as f64;
    let b = json!({
        "name": "server spans", "kind": "scheduled", "lookback_s": 7200, "schedule": {"every_s": 30},
        "query": {"kind": "builder", "builder": {"source": "traces", "agg": "count", "filters": [{"field": "span_kind", "value": "SPAN_KIND_SERVER"}]}},
        "condition": {"op": ">", "threshold": 0},
    });
    let r = send_json(&app, Method::POST, &format!("{BASE}/alerts"), &b).await;
    let bid = r.json()["data"]["id"].as_str().unwrap().to_string();
    let p = get(&app, &format!("{BASE}/alerts/{bid}/preview")).await;
    assert_eq!(p.status, StatusCode::OK, "{}", p.text());
    assert_eq!(p.json()["data"]["samples"][0]["value"], want);
    let inc = get(&app, &format!("{BASE}/incidents")).await.json()["data"].clone();
    assert_eq!(inc.as_array().unwrap().len(), 1);
    // The holder stops. Its lease ends 30 s after the claim; a second later the other replica
    // takes the rule over and the lease row names it.
    clock.fetch_add((engine::LEASE_S + 1) * 1000, Ordering::SeqCst);
    let took = other.tick(org).await.unwrap();
    assert!(took.contains(&id), "the other replica took over: {took:?}");
    let row = engine::item(&state, org, "lease", &id)
        .await
        .unwrap()
        .unwrap();
    let lease: Value = serde_json::from_str(&row.spec).unwrap();
    assert_eq!(lease["holder"], other.holder.as_str());
    assert_ne!(other.holder, holder.holder);
    for db in [&data_db, &meta_db] {
        let _ = http::post(
            "http://127.0.0.1:4000",
            "/v1/sql",
            &form,
            format!("sql=DROP DATABASE IF EXISTS {db}").as_bytes(),
        )
        .await;
    }
}

// ---- AC0.9.8 ---------------------------------------------------------------------------------

fn anomaly() -> Anomaly {
    Anomaly {
        k: 3.0,
        window: 30,
        min_points: 10,
        direction: "both".into(),
    }
}

#[test]
fn ac_0_9_8_mad_flags_outlier() {
    let base = [10.0, 11.0, 9.0, 10.0, 10.0, 11.0, 9.0, 10.0, 10.0, 12.0];
    let a = anomaly();
    assert!(matches!(
        cond::judge(&base, 30.0, &a),
        cond::Verdict::Outlier { .. }
    ));
    assert!(matches!(
        cond::judge(&base, 10.5, &a),
        cond::Verdict::Normal { .. }
    ));
    assert!(matches!(
        cond::judge(&base, -20.0, &a),
        cond::Verdict::Outlier { .. }
    ));
    // Direction: only high or only low.
    let high = Anomaly {
        direction: "high".into(),
        ..a.clone()
    };
    let low = Anomaly {
        direction: "low".into(),
        ..a.clone()
    };
    assert!(matches!(
        cond::judge(&base, -20.0, &high),
        cond::Verdict::Normal { .. }
    ));
    assert!(matches!(
        cond::judge(&base, 30.0, &low),
        cond::Verdict::Normal { .. }
    ));
    assert!(matches!(
        cond::judge(&base, 30.0, &high),
        cond::Verdict::Outlier { .. }
    ));
    // k widens the band.
    let wide = Anomaly {
        k: 50.0,
        ..a.clone()
    };
    assert!(matches!(
        cond::judge(&base, 30.0, &wide),
        cond::Verdict::Normal { .. }
    ));
    // A flat baseline flags any real change.
    assert!(matches!(
        cond::judge(&[5.0; 12], 6.0, &a),
        cond::Verdict::Outlier { .. }
    ));
    assert!(matches!(
        cond::judge(&[5.0; 12], 5.0, &a),
        cond::Verdict::Normal { .. }
    ));
    // Only the points before the checked one count: window limits the baseline.
    let mut series = vec![10.0; 12];
    series.push(80.0);
    series.push(10.0);
    assert_eq!(cond::flag_all(&series, &a), vec![12]);
}

#[tokio::test]
async fn ac_0_9_8_anomaly_rule_fires() {
    let t = setup();
    let d = t.webhook("hook").await;
    let rule = json!({
        "name": "latency anomaly", "kind": "anomaly", "destinations": [d],
        "query": {"kind": "sql", "text": "SELECT bucket_ms, p95 FROM x WHERE t >= $__from"},
        "anomaly": {"k": 3.0, "window": 30, "min_points": 10},
        "lookback_s": 3600, "schedule": {"every_s": 60},
    });
    let id = t.rule(rule).await;
    let mut rows: Vec<Value> = (0..20)
        .map(|i| json!({"bucket_ms": T0 + i * 60_000, "p95": 100 + (i % 3)}))
        .collect();
    *t.rows.lock().unwrap() = rows.clone();
    t.tick().await;
    assert_eq!(t.rec.count(), 0, "a quiet series does not fire");
    assert_eq!(t.state_of(&id).await["state"], "ok");
    rows.push(json!({"bucket_ms": T0 + 20 * 60_000, "p95": 900}));
    *t.rows.lock().unwrap() = rows;
    t.at(61);
    t.tick().await;
    assert_eq!(t.rec.count(), 1, "the spike fires");
    assert_eq!(t.state_of(&id).await["state"], "firing");
    // The preview shows the series and which points were flagged.
    let p = t.get(&format!("alerts/{id}/preview")).await.json()["data"].clone();
    assert_eq!(p["series"][0]["flagged"], json!([20]));
    assert_eq!(p["series"][0]["points"].as_array().unwrap().len(), 21);
    let m = &p["message"]["body"];
    assert!(m.as_str().unwrap().contains("MAD"), "{m}");
}

#[tokio::test]
async fn ac_0_9_8_needs_history() {
    let a = anomaly();
    assert_eq!(
        cond::judge(&[10.0; 4], 500.0, &a),
        cond::Verdict::NeedsHistory { have: 4, need: 10 }
    );
    assert!(matches!(
        cond::judge(&[10.0; 10], 500.0, &a),
        cond::Verdict::Outlier { .. }
    ));
    // Non-finite points do not count towards the baseline.
    let mut v = vec![10.0; 8];
    v.extend([f64::NAN, f64::INFINITY]);
    assert_eq!(
        cond::judge(&v, 500.0, &a),
        cond::Verdict::NeedsHistory { have: 8, need: 10 }
    );
    // Through the engine: too few points means no alert and a history entry saying why.
    let t = setup();
    let d = t.webhook("hook").await;
    let id = t
        .rule(json!({
            "name": "young", "kind": "anomaly", "destinations": [d],
            "query": {"kind": "sql", "text": "SELECT bucket_ms, p95 FROM x"},
            "anomaly": {"min_points": 10}, "lookback_s": 3600,
        }))
        .await;
    let mut rows: Vec<Value> = (0..5)
        .map(|i| json!({"bucket_ms": T0 + i * 60_000, "p95": 100}))
        .collect();
    rows.push(json!({"bucket_ms": T0 + 5 * 60_000, "p95": 5000}));
    *t.rows.lock().unwrap() = rows;
    t.tick().await;
    assert_eq!(t.rec.count(), 0);
    let h = t.history(&id).await;
    assert_eq!(h[0]["outcome"], "needs_history");
    assert!(
        h[0]["detail"]["notes"][0]
            .as_str()
            .unwrap()
            .contains("needs history")
    );
    // The options are validated on save.
    let bad = |a: Value| {
        let t = &t;
        async move {
            t.post("alerts", json!({"name": "z", "kind": "anomaly", "query": {"kind": "sql", "text": "SELECT 1"}, "anomaly": a})).await.status
        }
    };
    assert_eq!(bad(json!({"k": 0.0})).await, 400);
    assert_eq!(bad(json!({"min_points": 2})).await, 400);
    assert_eq!(bad(json!({"window": 5, "min_points": 6})).await, 400);
    assert_eq!(bad(json!({"direction": "sideways"})).await, 400);
}

#[tokio::test]
async fn ac_0_9_1_rule_sql_stays_in_org_database() {
    let t = setup();
    // The database is only known when the rule runs, so such a rule can be saved...
    let other = with(
        base("peek"),
        "query",
        json!({"kind": "sql", "text": "SELECT count(*) AS n FROM grepui.grepui_alert_items"}),
    );
    let id = t.rule(other).await;
    // ...but it never reaches the database: the run is refused and recorded as an error.
    t.tick().await;
    assert!(
        !t.store
            .seen
            .lock()
            .unwrap()
            .iter()
            .any(|q| q.name == "alert_rule"),
        "the statement was not sent"
    );
    assert_eq!(t.state_of(&id).await["state"], "error");
    let h = t.history(&id).await;
    assert_eq!(h[0]["outcome"], "error");
    assert!(
        h[0]["detail"]["error"]
            .as_str()
            .unwrap()
            .contains("outside"),
        "{}",
        h[0]["detail"]
    );
    // Writes and several statements are refused when the rule is saved.
    for sql in [
        "INSERT INTO t VALUES (1)",
        "SELECT 1; DROP TABLE t",
        "ADMIN flush_table('t')",
        "TQL EVAL (0, 10, '1s') up",
    ] {
        let r = t
            .post(
                "alerts",
                with(base("bad"), "query", json!({"kind": "sql", "text": sql})),
            )
            .await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{sql}: {}", r.text());
    }
    // PromQL goes through the same check as the metrics page.
    for q in ["up{__schema__=\"x\"}", "up; select 1", "up # c"] {
        let r = t
            .post(
                "alerts",
                with(base("bad"), "query", json!({"kind": "promql", "text": q})),
            )
            .await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{q}");
    }
    let ok = t
        .post(
            "alerts",
            with(
                base("prom"),
                "query",
                json!({"kind": "promql", "text": "sum(rate(http_requests_total[5m]))"}),
            ),
        )
        .await;
    assert_eq!(ok.status, StatusCode::CREATED, "{}", ok.text());
}

#[tokio::test]
async fn ac_0_9_1_store_gate_knows_the_alert_tables() {
    use grepui::store::{build, check_write};
    let q = build::item_put("grepui", "default", "lease", &build::ItemRow::default()).unwrap();
    assert!(check_write(&q, "grepui").is_ok());
    let q = build::alert_history_put("grepui", "default", "a", "ok", "ok", "{}", 1).unwrap();
    assert!(check_write(&q, "grepui").is_ok());
    assert!(build::item_put("grepui", "default", "bogus", &build::ItemRow::default()).is_err());
    assert!(build::item_list("grepui", "Bad Org", "alert").is_err());
    // Org scoping: another org sees none of this org's items.
    let t = setup();
    t.rule(base("mine")).await;
    let others = build::item_list("grepui", "other", "alert").unwrap();
    let rows = grepui::query::Reader::fetch(t.store.as_ref(), &others)
        .await
        .unwrap();
    assert!(rows.is_empty());
}

#[test]
fn ac_0_9_1_demo_alert_is_valid() {
    let v: Value = serde_json::from_str(grepui::playground::DEMO_ALERT).unwrap();
    let r = alerts::parse_rule(&v).unwrap();
    assert_eq!(r.dedup.fields, vec!["service_name"]);
}
