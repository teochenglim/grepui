//! AC0.13.1: scheduled reports render a dashboard or saved view to HTML or CSV, are sent once per
//! schedule slot across replicas, and record failures.
mod common;
use axum::{Router, http::Method, http::StatusCode};
use common::*;
use grepui::{
    api::{Shared, reports::Scheduler},
    store::build,
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

struct T {
    app: Router,
    state: Shared,
    rec: Arc<Rec>,
    clock: Arc<AtomicI64>,
    store: Arc<MemStore>,
}

fn setup() -> T {
    let store = MemStore::new();
    *store.script.lock().unwrap() = Some(Box::new(|q| {
        Ok(match q.name {
            "dashboard_panel" => vec![
                json!({"service": "cart", "n": 3, "password": "hunter2"}),
                json!({"service": "<script>alert(1)</script>", "n": 4, "password": "p2"}),
            ],
            "logs_search" => vec![log_row(
                "1793399269857388008",
                "ERROR",
                "card declined, retry",
            )],
            _ => sample_db(q)?,
        })
    }));
    let clock = Arc::new(AtomicI64::new(T0));
    let rec = Arc::new(Rec::default());
    let c = clock.clone();
    let state = grepui::api::AppState::with_sender(
        grepui::config::Config {
            admin_writes: true,
            ..Default::default()
        },
        store.clone(),
        store.clone(),
        Arc::new(move || c.load(Ordering::SeqCst)),
        rec.clone(),
    )
    .unwrap();
    let app = grepui::api::router(state.clone());
    T {
        app,
        state,
        rec,
        clock,
        store,
    }
}

impl T {
    async fn post(&self, path: &str, body: Value) -> Resp {
        send_json(&self.app, Method::POST, &format!("{BASE}/{path}"), &body).await
    }
    async fn get(&self, path: &str) -> Resp {
        get(&self.app, &format!("{BASE}/{path}")).await
    }
    async fn dashboard(&self) -> String {
        let r = self
            .post(
                "dashboards",
                json!({"title": "Payments", "spec": {"tabs": [{"title": "Main", "panels": [
                    {"id": "p1", "type": "table", "title": "Slow services", "query": {"kind": "sql", "text": "SELECT service, n, password FROM t"}},
                    {"id": "p2", "type": "markdown", "title": "Notes", "content": "ignore me"},
                ]}]}}),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
        r.json()["data"]["id"].as_str().unwrap().to_string()
    }
    async fn view(&self) -> String {
        let r = self
            .post(
                "views",
                json!({"name": "Errors", "kind": "logs", "spec": {"q": "", "level": "ERROR", "mode": "search"}}),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
        r.json()["data"]["id"].as_str().unwrap().to_string()
    }
    async fn report(&self, target: &str, kind: &str, format: &str, schedule: &str) -> String {
        let r = self
            .post(
                "reports",
                json!({"name": "Daily", "kind": kind, "target": target, "format": format,
                       "schedule": schedule, "range_hours": 24, "recipients": ["ops@example.com", "lead@example.com"]}),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
        r.json()["data"]["id"].as_str().unwrap().to_string()
    }
    fn at(&self, secs: i64) {
        self.clock.store(T0 + secs * 1000, Ordering::SeqCst);
    }
    fn scheduler(&self) -> Scheduler {
        Scheduler::new(self.state.clone(), self.rec.clone()).with_settle(Duration::ZERO)
    }
    async fn history(&self, id: &str) -> Vec<Value> {
        self.get(&format!("reports/{id}/history")).await.json()["data"]
            .as_array()
            .unwrap()
            .clone()
    }
}

#[tokio::test]
async fn ac_0_13_1_report_renders_html() {
    let t = setup();
    // Rules are applied to what leaves in a report as to any response, as a viewer.
    let r = t
        .post(
            "redaction",
            json!({"field": "password", "mask": "full", "exempt_roles": ["admin"]}),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    grepui::api::redaction::reload(&t.state, t.state.org("default").unwrap())
        .await
        .unwrap();
    let d = t.dashboard().await;
    let id = t.report(&d, "dashboard", "html", "0 8 * * *").await;
    // Preview shows what would be sent, without sending.
    let p = t.get(&format!("reports/{id}/preview")).await;
    assert_eq!(p.status, StatusCode::OK, "{}", p.text());
    assert_eq!(t.rec.count(), 0);
    let j = p.json();
    assert_eq!(j["data"]["sections"], 1, "the text panel is not a table");
    assert_eq!(j["data"]["rows"], 2);
    // Send now: one email to both recipients with a self-contained HTML page.
    let r = t.post(&format!("reports/{id}/run"), json!({})).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(t.rec.count(), 1);
    let (name, m) = t.rec.sent.lock().unwrap()[0].clone();
    assert_eq!(name, "Daily");
    assert!(m.html);
    assert!(
        m.subject.starts_with("[grepui] Daily (2026-"),
        "{}",
        m.subject
    );
    assert!(
        m.body.starts_with("<!doctype html>") && m.body.contains("<table"),
        "{}",
        m.body
    );
    assert!(m.body.contains("Slow services") && m.body.contains("cart"));
    assert!(!m.body.contains("<script>"), "values are escaped");
    assert!(m.body.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
    assert!(
        !m.body.contains("hunter2") && !m.body.contains("p2<"),
        "redacted: {}",
        m.body
    );
    assert!(
        !m.body.contains("http://") && !m.body.contains("https://"),
        "nothing to load"
    );
    // CSV is the same data as text and is not marked HTML.
    let c = t.report(&d, "dashboard", "csv", "0 8 * * *").await;
    t.post(&format!("reports/{c}/run"), json!({})).await;
    let (_, m) = t.rec.sent.lock().unwrap()[1].clone();
    assert!(!m.html);
    assert!(m.body.starts_with("service,n,password\n"), "{:?}", m.body);
    assert!(m.body.contains("cart,3,"));
    assert!(!m.body.contains("hunter2"));
    // A saved logs view is a report too.
    let v = t.view().await;
    let vr = t.report(&v, "view", "html", "0 8 * * *").await;
    t.post(&format!("reports/{vr}/run"), json!({})).await;
    let (_, m) = t.rec.sent.lock().unwrap()[2].clone();
    assert!(
        m.body.contains("card declined, retry") && m.body.contains("ERROR"),
        "{}",
        m.body
    );
    assert!(m.body.contains("2026-"), "timestamps are readable");
    // What a report may name is checked when it is saved.
    for (body, want) in [
        (
            json!({"name": "x", "kind": "dashboard", "target": "0123456789abcdef", "format": "html", "schedule": "0 8 * * *", "recipients": ["a@b.co"]}),
            StatusCode::NOT_FOUND,
        ),
        (
            json!({"name": "x", "kind": "dashboard", "target": d, "format": "pdf", "schedule": "0 8 * * *", "recipients": ["a@b.co"]}),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({"name": "x", "kind": "dashboard", "target": d, "format": "html", "schedule": "whenever", "recipients": ["a@b.co"]}),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({"name": "x", "kind": "dashboard", "target": d, "format": "html", "schedule": "0 8 * * *", "recipients": ["not-an-address"]}),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({"name": "x", "kind": "dashboard", "target": d, "format": "html", "schedule": "0 8 * * *", "recipients": []}),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({"name": "x", "kind": "dashboard", "target": d, "format": "html", "schedule": "0 8 * * *", "recipients": ["a@b.co"], "extra": 1}),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
    ] {
        let r = t.post("reports", body.clone()).await;
        assert_eq!(r.status, want, "{body}: {}", r.text());
    }
    // List, update and delete.
    let l = t.get("reports").await.json();
    assert_eq!(l["data"].as_array().unwrap().len(), 3);
    let r = send_json(&t.app, Method::PUT, &format!("{BASE}/reports/{id}"),
        &json!({"name": "Renamed", "kind": "dashboard", "target": d, "format": "csv", "schedule": "0 9 * * *", "recipients": ["a@b.co"]})).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = send(&t.app, Method::DELETE, &format!("{BASE}/reports/{id}")).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(
        t.get("reports").await.json()["data"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        t.get(&format!("reports/{id}/preview")).await.status,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn ac_0_13_1_report_runs_once() {
    let t = setup();
    let d = t.dashboard().await;
    let id = t.report(&d, "dashboard", "html", "* * * * *").await;
    let disabled = t.report(&d, "dashboard", "html", "* * * * *").await;
    let r = send_json(&t.app, Method::PUT, &format!("{BASE}/reports/{disabled}"),
        &json!({"name": "Off", "kind": "dashboard", "target": d, "format": "html", "schedule": "* * * * *", "recipients": ["a@b.co"], "enabled": false})).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let org = t.state.org("default").unwrap();
    let (a, b) = (t.scheduler(), t.scheduler());
    assert_ne!(a.holder, b.holder);
    // Before the first slot nothing is due.
    assert!(a.tick(org).await.unwrap().is_empty());
    // One minute on, two replicas look at once: exactly one email.
    t.at(61);
    let (ra, rb) = tokio::join!(a.tick(org), b.tick(org));
    assert_eq!(ra.unwrap().len() + rb.unwrap().len(), 1);
    assert_eq!(t.rec.count(), 1);
    // The same slot is not sent again by either replica, now or on a later pass.
    assert!(a.tick(org).await.unwrap().is_empty());
    assert!(b.tick(org).await.unwrap().is_empty());
    assert_eq!(t.rec.count(), 1);
    // The next slot is sent once more, by whichever replica gets there.
    t.at(125);
    let n = b.tick(org).await.unwrap().len() + a.tick(org).await.unwrap().len();
    assert_eq!(n, 1);
    assert_eq!(t.rec.count(), 2);
    // A long outage gives one catch-up email, not one per missed minute.
    t.at(3 * 3600);
    a.tick(org).await.unwrap();
    b.tick(org).await.unwrap();
    assert_eq!(t.rec.count(), 3);
    // The state shows the last run, and the history lists each send.
    let l = t.get("reports").await.json();
    let me = l["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["id"] == id)
        .unwrap()
        .clone();
    assert_eq!(me["state"]["outcome"], "ok");
    assert_eq!(me["state"]["runs"], 3);
    let h = t.history(&id).await;
    assert_eq!(h.len(), 3);
    assert!(h.iter().all(|x| x["outcome"] == "scheduled_sent"), "{h:?}");
    // A report that is switched off never runs; an alert's history does not list reports.
    let all = t.get("alerts/history").await.json();
    assert!(
        all["data"]
            .as_array()
            .unwrap()
            .iter()
            .all(|x| !x["alert_id"].as_str().unwrap_or("").starts_with("report:"))
    );
    let _ = (&t.store, build::PLATFORM_ORG);
}

#[tokio::test]
async fn ac_0_13_1_report_failure_recorded() {
    let t = setup();
    let d = t.dashboard().await;
    let id = t.report(&d, "dashboard", "html", "* * * * *").await;
    let org = t.state.org("default").unwrap();
    let s = t.scheduler();
    // The mail server is down: the scheduled run fails, says why, and takes its slot (no storm).
    *t.rec.fail.lock().unwrap() = Some("connection refused by mail.example.com".into());
    t.at(61);
    assert_eq!(s.tick(org).await.unwrap().len(), 1);
    assert!(
        s.tick(org).await.unwrap().is_empty(),
        "the slot is taken even though it failed"
    );
    let h = t.history(&id).await;
    assert_eq!(h.len(), 1);
    assert_eq!(h[0]["outcome"], "scheduled_failed");
    assert!(
        h[0]["detail"]["error"]
            .as_str()
            .unwrap()
            .contains("mail.example.com")
    );
    let l = t.get("reports").await.json();
    let me = l["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["id"] == id)
        .unwrap()
        .clone();
    assert_eq!(me["state"]["outcome"], "failed");
    assert!(
        me["state"]["error"]
            .as_str()
            .unwrap()
            .contains("connection refused")
    );
    // "Send now" says so too, with an error status, and records it.
    t.at(70);
    let r = t.post(&format!("reports/{id}/run"), json!({})).await;
    assert_eq!(r.status, StatusCode::BAD_GATEWAY);
    assert!(r.json()["error"].as_str().unwrap().contains("not sent"));
    assert_eq!(t.history(&id).await[0]["outcome"], "manual_failed");
    // The next slot tries again and succeeds once the server is back.
    *t.rec.fail.lock().unwrap() = None;
    t.at(125);
    assert_eq!(s.tick(org).await.unwrap().len(), 1);
    assert_eq!(t.rec.count(), 1);
    let l = t.get("reports").await.json();
    let me = l["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["id"] == id)
        .unwrap()
        .clone();
    assert_eq!(me["state"]["outcome"], "ok");
    assert_eq!(me["state"]["error"], "");
    // A dashboard that was deleted is a recorded failure, not a crash.
    let r = send(&t.app, Method::DELETE, &format!("{BASE}/dashboards/{d}")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    t.at(190);
    assert_eq!(s.tick(org).await.unwrap().len(), 1);
    let h = t.history(&id).await;
    assert_eq!(h[0]["outcome"], "scheduled_failed");
    assert!(
        h[0]["detail"]["error"]
            .as_str()
            .unwrap()
            .contains("no such dashboard"),
        "{h:?}"
    );
    // Rules that are not loaded yet hold a report back rather than send unmasked data.
    let d2 = t.dashboard().await;
    let id2 = t.report(&d2, "dashboard", "html", "0 8 * * *").await;
    t.state.redact_ready.store(false, Ordering::SeqCst);
    let r = t.post(&format!("reports/{id2}/run"), json!({})).await;
    assert_eq!(r.status, StatusCode::BAD_GATEWAY);
    assert!(
        r.json()["error"]
            .as_str()
            .unwrap()
            .contains("redaction rules are not loaded")
    );
    assert_eq!(t.rec.count(), 1, "nothing was sent");
}

#[test]
fn ac_0_13_1_long_html_is_a_valid_mail() {
    use grepui::alerts::notify::mail_text_as;
    // One very long line, as an HTML report is: it must not reach the wire as one SMTP line.
    let body = format!("<!doctype html><p>{}</p>", "é".repeat(5000));
    let m = mail_text_as(
        "a@b.co",
        &["c@d.co".into()],
        "S",
        &body,
        "Mon, 1 Jan 2026 00:00:00 +0000",
        true,
    );
    assert!(m.contains("Content-Type: text/html; charset=utf-8"));
    assert!(m.contains("Content-Transfer-Encoding: base64"));
    assert!(
        m.split("\r\n").all(|l| l.len() <= 998),
        "no line over the SMTP limit"
    );
    let (head, b64) = m.split_once("\r\n\r\n").unwrap();
    assert!(head.contains("Subject: S"));
    // The body decodes back to the page.
    use base64::{Engine, engine::general_purpose::STANDARD};
    let raw = STANDARD.decode(b64.replace("\r\n", "")).unwrap();
    assert_eq!(String::from_utf8(raw).unwrap(), body);
    // Short plain text stays as it was.
    let m = mail_text_as("a@b.co", &["c@d.co".into()], "S", "hello\n.dot", "d", false);
    assert!(
        m.contains("8bit") && m.ends_with("hello\r\n..dot\r\n"),
        "{m:?}"
    );
}
