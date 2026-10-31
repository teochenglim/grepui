//! AC0.8.1 – AC0.8.6: folders, dashboards and tabs; panel types and safe text; variables bound as
//! parameters; refresh and comparison; import, export and move; public links and playlists.
mod common;
use axum::{Router, http::Method, http::StatusCode};
use common::*;
use grepui::{
    api::{AppState, router},
    config::Config,
    dash::{self, panels, time, vars},
    playground::{http, send as seed, synth},
    query::{Bind, SqlxReader, Window, ts_string},
    store::{self, build},
};
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    sync::{
        Arc,
        atomic::{AtomicI64, Ordering},
    },
    time::Duration,
};

const BASE: &str = "/api/v1/orgs/default";
const T0: i64 = 1_793_400_000_000;

fn setup() -> (Router, Arc<MemStore>, Arc<AtomicI64>) {
    let store = MemStore::new();
    let clock = Arc::new(AtomicI64::new(T0));
    (app_with_store(&store, &clock), store, clock)
}

fn spec_with(panels: Value) -> Value {
    json!({"tabs": [{"title": "Main", "panels": panels}]})
}

async fn create(a: &Router, title: &str, spec: Value) -> Value {
    let r = send_json(
        a,
        Method::POST,
        &format!("{BASE}/dashboards"),
        &json!({"title": title, "spec": spec}),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    r.json()["data"].clone()
}

fn id_of(v: &Value) -> String {
    v["id"].as_str().unwrap().to_string()
}

// ---- AC0.8.1 CRUD ---------------------------------------------------------------------

#[tokio::test]
async fn ac_0_8_1_dashboard_crud() {
    let (a, _, clock) = setup();
    let f = send_json(
        &a,
        Method::POST,
        &format!("{BASE}/folders"),
        &json!({"name": "Payments"}),
    )
    .await;
    assert_eq!(f.status, StatusCode::CREATED, "{}", f.text());
    let fid = id_of(&f.json()["data"]);
    let d = create(&a, "Latency", json!({"tabs": [{"title": "Overview", "panels": [{"type": "stat", "title": "p95", "query": {"kind": "sql", "text": "SELECT 1 AS v"}}]}, {"title": "Details"}]})).await;
    let did = id_of(&d);
    assert_eq!(
        d["spec"]["tabs"].as_array().unwrap().len(),
        2,
        "tabs are part of the spec"
    );
    assert_eq!(d["spec"]["tabs"][1]["panels"], json!([]));
    // Rename, move, edit.
    clock.store(T0 + 5_000, Ordering::SeqCst);
    let r = send_json(
        &a,
        Method::PUT,
        &format!("{BASE}/dashboards/{did}"),
        &json!({"title": "Latency v2", "folder_id": fid}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let got = get(&a, &format!("{BASE}/dashboards/{did}")).await.json()["data"].clone();
    assert_eq!(
        (
            got["title"].as_str().unwrap(),
            got["folder_id"].as_str().unwrap()
        ),
        ("Latency v2", fid.as_str())
    );
    assert_eq!(
        (
            got["created_ms"].as_i64().unwrap(),
            got["updated_ms"].as_i64().unwrap()
        ),
        (T0, T0 + 5_000)
    );
    let list = get(&a, &format!("{BASE}/dashboards?folder={fid}"))
        .await
        .json();
    assert_eq!(list["data"].as_array().unwrap().len(), 1);
    assert!(
        list["data"][0].get("spec").is_none(),
        "the list does not carry specs"
    );
    // Folder rename.
    let r = send_json(
        &a,
        Method::PUT,
        &format!("{BASE}/folders/{fid}"),
        &json!({"name": "Payments team"}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(
        get(&a, &format!("{BASE}/folders")).await.json()["data"][0]["name"],
        "Payments team"
    );
    // Delete is a soft delete and the dashboard is gone for readers.
    assert_eq!(
        send(&a, Method::DELETE, &format!("{BASE}/dashboards/{did}"))
            .await
            .status,
        StatusCode::OK
    );
    assert_eq!(
        get(&a, &format!("{BASE}/dashboards/{did}")).await.status,
        StatusCode::NOT_FOUND
    );
    assert!(
        get(&a, &format!("{BASE}/dashboards")).await.json()["data"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    // Bad input.
    for (uri, body) in [
        ("dashboards", json!({"title": ""})),
        ("dashboards", json!({"title": "x", "spec": {"tabs": []}})),
        ("folders", json!({"name": "a\u{0}b"})),
    ] {
        let r = send_json(&a, Method::POST, &format!("{BASE}/{uri}"), &body).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{body}: {}", r.text());
    }
    assert_eq!(
        get(&a, &format!("{BASE}/dashboards/nothex")).await.status,
        StatusCode::BAD_REQUEST
    );
    let r = send_json(
        &a,
        Method::POST,
        &format!("{BASE}/dashboards"),
        &json!({"title": "x", "folder_id": "0123456789abcdef"}),
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::NOT_FOUND,
        "a folder that does not exist"
    );
}

#[tokio::test]
async fn ac_0_8_1_delete_cascades() {
    let (a, store, _) = setup();
    let fid = id_of(
        &send_json(
            &a,
            Method::POST,
            &format!("{BASE}/folders"),
            &json!({"name": "A"}),
        )
        .await
        .json()["data"],
    );
    let other = id_of(
        &send_json(
            &a,
            Method::POST,
            &format!("{BASE}/folders"),
            &json!({"name": "B"}),
        )
        .await
        .json()["data"],
    );
    let mut inside = vec![];
    for t in ["one", "two"] {
        let d = send_json(
            &a,
            Method::POST,
            &format!("{BASE}/dashboards"),
            &json!({"title": t, "folder_id": fid}),
        )
        .await
        .json()["data"]
            .clone();
        inside.push(id_of(&d));
    }
    let keep = send_json(
        &a,
        Method::POST,
        &format!("{BASE}/dashboards"),
        &json!({"title": "keep", "folder_id": other}),
    )
    .await
    .json()["data"]
        .clone();
    let root = create(&a, "root", empty()).await;
    let r = send(&a, Method::DELETE, &format!("{BASE}/folders/{fid}")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["data"]["dashboards_deleted"], 2);
    for id in &inside {
        assert_eq!(
            get(&a, &format!("{BASE}/dashboards/{id}")).await.status,
            StatusCode::NOT_FOUND,
            "{id} went with its folder"
        );
    }
    let names: Vec<_> = get(&a, &format!("{BASE}/dashboards")).await.json()["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["title"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        names,
        ["keep", "root"],
        "dashboards of other folders and the root stay"
    );
    let folders: Vec<_> = get(&a, &format!("{BASE}/folders")).await.json()["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(folders, ["B"]);
    assert_eq!(id_of(&keep).len(), 16);
    assert_eq!(id_of(&root).len(), 16);
    // Deleting twice is a 404, not a second cascade.
    assert_eq!(
        send(&a, Method::DELETE, &format!("{BASE}/folders/{fid}"))
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    // Soft deletes: rows are still in the store, flagged.
    assert!(
        store
            .dashes
            .lock()
            .unwrap()
            .values()
            .filter(|b| b[9] == Bind::Bool(true))
            .count()
            == 2
    );
}

fn empty() -> Value {
    json!({"tabs": [{"title": "Main"}]})
}

#[tokio::test]
async fn ac_0_8_1_ids_stable() {
    let (a, _, _) = setup();
    // Panels without ids get one when saved, and keep it on every later save.
    let d = create(&a, "Stable", spec_with(json!([{"type": "line", "title": "a", "query": {"kind": "promql", "text": "up"}}, {"type": "markdown", "title": "b", "content": "hi"}]))).await;
    let did = id_of(&d);
    let tab_id = d["spec"]["tabs"][0]["id"].as_str().unwrap().to_string();
    let panel_ids: Vec<String> = d["spec"]["tabs"][0]["panels"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(panel_ids.len(), 2);
    assert!(panel_ids.iter().all(|p| !p.is_empty()) && panel_ids[0] != panel_ids[1]);
    // Re-saving the document the server returned changes nothing about identity.
    let again = send_json(
        &a,
        Method::PUT,
        &format!("{BASE}/dashboards/{did}"),
        &json!({"spec": d["spec"], "title": "Renamed"}),
    )
    .await
    .json()["data"]
        .clone();
    assert_eq!(again["id"], d["id"]);
    assert_eq!(again["spec"], d["spec"], "canonical form is a fixed point");
    assert_eq!(again["spec"]["tabs"][0]["id"], tab_id.as_str());
    // Edits keep the ids they do not touch; a new panel gets a new id.
    let mut edited = d["spec"].clone();
    edited["tabs"][0]["panels"][0]["title"] = json!("changed");
    edited["tabs"][0]["panels"].as_array_mut().unwrap().push(
        json!({"type": "stat", "title": "new", "query": {"kind": "sql", "text": "SELECT 1"}}),
    );
    let after = send_json(
        &a,
        Method::PUT,
        &format!("{BASE}/dashboards/{did}"),
        &json!({"spec": edited}),
    )
    .await
    .json()["data"]
        .clone();
    let ids: Vec<&str> = after["spec"]["tabs"][0]["panels"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["id"].as_str().unwrap())
        .collect();
    assert_eq!(&ids[..2], [panel_ids[0].as_str(), panel_ids[1].as_str()]);
    assert!(!panel_ids.iter().any(|p| p == ids[2]));
    // The canonical function is idempotent on arbitrary valid input.
    let c = dash::canonical(&spec_with(
        json!([{"type": "pie", "title": "x", "query": {"kind": "sql", "text": "select 1"}}]),
    ))
    .unwrap();
    assert_eq!(dash::canonical(&c).unwrap(), c);
    // Duplicate ids are refused rather than silently renumbered.
    let dup = spec_with(
        json!([{"id": "p", "type": "stat", "title": "a", "query": {"kind": "sql", "text": "s"}}, {"id": "p", "type": "stat", "title": "b", "query": {"kind": "sql", "text": "s"}}]),
    );
    assert!(
        dash::canonical(&dup)
            .unwrap_err()
            .1
            .contains("duplicate id")
    );
}

// ---- AC0.8.2 panels -------------------------------------------------------------------

#[test]
fn ac_0_8_2_panel_types_validate() {
    // Every type of R-B2 exists.
    let names: HashSet<&str> = panels::TYPES.iter().map(|t| t.name).collect();
    for t in [
        "area",
        "stacked_area",
        "bar_v",
        "bar_h",
        "stacked_bar_v",
        "stacked_bar_h",
        "line",
        "scatter",
        "pie",
        "donut",
        "heatmap",
        "stat",
        "gauge",
        "table",
        "geo_map",
        "sankey",
        "markdown",
        "html",
    ] {
        assert!(names.contains(t), "missing panel type {t}");
    }
    let q = json!({"kind": "sql", "text": "SELECT 1"});
    for t in names.iter().filter(|t| !matches!(**t, "markdown" | "html")) {
        let ok = dash::canonical(&spec_with(json!([{"type": t, "title": "t", "query": q}])));
        assert!(ok.is_ok(), "{t}: {:?}", ok.err().map(|e| e.1));
    }
    // Spec errors say where.
    for (spec, needle) in [
        (
            spec_with(json!([{"type": "pie3d", "title": "t", "query": q}])),
            "spec.tabs[0].panels[0].type: unknown panel type",
        ),
        (
            spec_with(json!([{"type": "line", "title": "t"}])),
            "panels[0].query: is required",
        ),
        (
            spec_with(
                json!([{"type": "line", "title": "t", "query": {"kind": "mongo", "text": "x"}}]),
            ),
            "query.kind: must be sql or promql",
        ),
        (
            spec_with(json!([{"type": "markdown", "title": "t", "query": q}])),
            "has content, not a query",
        ),
        (
            spec_with(json!([{"type": "line", "title": "t", "query": q, "content": "x"}])),
            "only markdown and html panels have content",
        ),
        (
            spec_with(json!([{"type": "line", "title": "", "query": q}])),
            "title",
        ),
        (
            spec_with(json!([{"type": "line", "title": "t", "query": q, "w": 13}])),
            "panels[0].w",
        ),
        (
            spec_with(json!([{"type": "line", "title": "t", "query": q, "script": "alert(1)"}])),
            "unknown key \"script\"",
        ),
        (
            spec_with(
                json!([{"type": "line", "title": "t", "query": q, "options": {"onload": 1}}]),
            ),
            "options: unknown key",
        ),
        (
            json!({"tabs": [{"title": "t", "panels": [], "extra": 1}]}),
            "unknown key \"extra\"",
        ),
        (
            json!({"version": 2, "tabs": [{"title": "t"}]}),
            "spec.version",
        ),
        (json!({"tabs": []}), "spec.tabs"),
    ] {
        let e = dash::canonical(&spec).unwrap_err().1;
        assert!(e.contains(needle), "wanted {needle:?} in {e:?}");
    }
    // Each type checks the shape of what its query returned (N number, T text).
    use panels::Col::{N, T};
    let cases: &[(&str, &[panels::Col], bool)] = &[
        ("line", &[N, N], true),
        ("line", &[N, N, N], true),
        ("line", &[N, T, N], true),
        ("line", &[T, N], false),
        ("area", &[N, N], true),
        ("stacked_area", &[N, T, N], true),
        ("scatter", &[N, N], true),
        ("bar_v", &[T, N], true),
        ("bar_h", &[T, N, N], true),
        ("stacked_bar_v", &[N, N], true),
        ("bar_h", &[T], false),
        ("pie", &[T, N], true),
        ("donut", &[T, N], true),
        ("pie", &[N, N], false),
        ("heatmap", &[T, T, N], true),
        ("heatmap", &[N, N, N], true),
        ("heatmap", &[N, N], false),
        ("stat", &[N], true),
        ("gauge", &[T, N], true),
        ("stat", &[T], false),
        ("table", &[T], true),
        ("table", &[], true),
        ("geo_map", &[N, N], true),
        ("geo_map", &[N, N, N, T], true),
        ("geo_map", &[T, N], false),
        ("sankey", &[T, T, N], true),
        ("sankey", &[T, N, N], false),
    ];
    for (ty, cols, ok) in cases {
        assert_eq!(panels::check_shape(ty, cols).is_ok(), *ok, "{ty} {cols:?}");
    }
    let e = panels::check_shape("sankey", &[T, N]).unwrap_err();
    assert!(
        e.contains("source") && e.contains("target"),
        "the error says what is needed: {e}"
    );
    // Column kinds come from the rows.
    let (names, kinds) = panels::kinds(&[
        json!({"a": "x", "b": 1, "c": null}),
        json!({"a": "y", "b": 2.5, "c": null}),
    ]);
    assert_eq!(names, ["a", "b", "c"]);
    assert_eq!(kinds, [T, N, N], "an all-null column is not text");
}

#[test]
fn ac_0_8_2_html_sanitised() {
    let bad = [
        "<script>alert(1)</script>hello",
        "<img src=x onerror=alert(1)>",
        "<a href=\"javascript:alert(1)\">x</a>",
        "<a href=\" JaVaScRiPt:alert(1)\">x</a>",
        "<a href=\"&#106;avascript:alert(1)\">x</a>",
        "<a href=\"data:text/html,<script>alert(1)</script>\">x</a>",
        "<p onclick=\"alert(1)\">x</p>",
        "<iframe src=\"https://evil\"></iframe>",
        "<svg onload=alert(1)><circle/></svg>",
        "<style>body{display:none}</style>x",
        "<object data=x></object><embed src=x>",
        "<form action=https://evil><input name=p></form>",
        "<!-- <script>alert(1)</script> -->x",
        "<scr<script>ipt>alert(1)</scr</script>ipt>",
        "<div style=\"background:url(javascript:alert(1))\">x</div>",
        "<math><mtext></mtext></math>",
        "<meta http-equiv=refresh content=\"0;url=https://evil\">",
        "<a href=https://ok.example onmouseover=alert(1)>x</a>",
    ];
    for b in bad {
        let out = panels::sanitize_html(b).to_ascii_lowercase();
        for needle in [
            "<script",
            "onerror",
            "onclick",
            "onload",
            "onmouseover",
            "javascript:",
            "<iframe",
            "<svg",
            "<style",
            "<object",
            "<embed",
            "<form",
            "<input",
            "<meta",
            "style=",
            "data:text",
            "alert(1)</",
        ] {
            assert!(!out.contains(needle), "{b:?} -> {out:?} still has {needle}");
        }
    }
    // Plain formatting survives; links get a safe rel.
    let ok = panels::sanitize_html(
        "<h2>Title</h2><p>a <b>bold</b> and <a href=\"https://example.com/x?a=1&b=2\" title=\"t\">link</a></p><ul><li>one</li></ul><table><tr><td colspan=2>c</td></tr></table>",
    );
    assert!(
        ok.contains("<h2>Title</h2>")
            && ok.contains("<b>bold</b>")
            && ok.contains("<li>one</li>")
            && ok.contains("colspan=\"2\""),
        "{ok}"
    );
    assert!(
        ok.contains("href=\"https://example.com/x?a=1&amp;b=2\"")
            && ok.contains("rel=\"noopener noreferrer nofollow\""),
        "{ok}"
    );
    // Unclosed tags are closed, stray angle brackets are text, and it is idempotent.
    assert_eq!(panels::sanitize_html("<p><b>open"), "<p><b>open</b></p>");
    assert_eq!(panels::sanitize_html("1 < 2 > 0"), "1 &lt; 2 &gt; 0");
    for s in [
        "<p>a<br>b</p>",
        "x <a href=\"mailto:a@b.c\">m</a>",
        "<script>x</script>",
    ] {
        let once = panels::sanitize_html(s);
        assert_eq!(panels::sanitize_html(&once), once, "{s}");
    }
    // Hostile nesting is bounded.
    let deep = "<div>".repeat(500);
    assert!(panels::sanitize_html(&deep).matches("<div>").count() <= 64);
}

#[test]
fn ac_0_8_2_markdown_no_script() {
    let md = "# Title\n\nSome **bold** and *em* and `code <b>`.\n\n- one\n- two <script>alert(1)</script>\n\n1. first\n2. second\n\n[ok](https://example.com) [bad](javascript:alert(1)) [bad2](  JAVASCRIPT:alert(1))\n\n<img src=x onerror=alert(1)>\n\n```\n<script>alert(2)</script>\n```\n![pic](https://evil/p.png)";
    let out = panels::markdown_to_html(md);
    let low = out.to_ascii_lowercase();
    for needle in [
        "<script",
        "<img",
        "<iframe",
        "href=\"javascript",
        "href=\"  javascript",
        "href=\" javascript",
    ] {
        assert!(!low.contains(needle), "{needle} in {out}");
    }
    assert!(
        out.contains("<h1>Title</h1>")
            && out.contains("<strong>bold</strong>")
            && out.contains("<em>em</em>"),
        "{out}"
    );
    assert!(out.contains("<code>code &lt;b&gt;</code>"), "{out}");
    assert!(
        out.contains("<ul><li>one</li><li>two &lt;script&gt;alert(1)&lt;/script&gt;</li></ul>"),
        "{out}"
    );
    assert!(
        out.contains("<ol><li>first</li><li>second</li></ol>"),
        "{out}"
    );
    assert!(out.contains("<a href=\"https://example.com\""), "{out}");
    assert!(
        out.contains("&lt;script&gt;alert(2)&lt;/script&gt;"),
        "fenced code is text: {out}"
    );
    assert_eq!(panels::markdown_to_html(""), "");
    assert_eq!(panels::markdown_to_html("a & b"), "<p>a &amp; b</p>");
}

#[test]
fn ac_0_8_2_table_pivot_totals() {
    let rows = vec![
        json!({"service": "cart", "code": "200", "n": 10}),
        json!({"service": "cart", "code": "500", "n": 2}),
        json!({"service": "pay", "code": "200", "n": 7}),
        json!({"service": "pay", "code": "200", "n": 3}),
        json!({"service": "pay", "code": "404", "n": 1.5}),
        json!({"service": "cart", "code": "200"}),
    ];
    let (cols, out) = panels::pivot(&rows, "service", "code", "n");
    assert_eq!(cols, ["service", "200", "500", "404", "Total"]);
    assert_eq!(out.len(), 3);
    assert_eq!(
        out[0],
        json!({"service": "cart", "200": 10.0, "500": 2.0, "404": 0.0, "Total": 12.0})
    );
    assert_eq!(
        out[1],
        json!({"service": "pay", "200": 10.0, "500": 0.0, "404": 1.5, "Total": 11.5}),
        "duplicate cells add up"
    );
    assert_eq!(
        out[2],
        json!({"service": "Total", "200": 20.0, "500": 2.0, "404": 1.5, "Total": 23.5})
    );
    // The pivot option is validated, and unknown shapes are refused.
    let bad = spec_with(
        json!([{"type": "table", "title": "t", "query": {"kind": "sql", "text": "s"}, "options": {"pivot": {"row": "a"}}}]),
    );
    assert!(dash::canonical(&bad).is_err());
    let good = spec_with(
        json!([{"type": "table", "title": "t", "query": {"kind": "sql", "text": "s"}, "options": {"pivot": {"row": "service", "column": "code", "value": "n"}}}]),
    );
    assert!(dash::canonical(&good).is_ok());
    assert!(
        panels::pivot(&[], "a", "b", "c").1.len() == 1,
        "no rows is just a total row"
    );
}

#[tokio::test]
async fn ac_0_8_2_panel_data_endpoint() {
    let (a, store, _) = setup();
    *store.script.lock().unwrap() = Some(Box::new(|q| {
        Ok(match q.name {
            "dashboard_panel" if q.sql.contains("pivot") => vec![
                json!({"service": "cart", "code": "200", "n": 10}),
                json!({"service": "cart", "code": "500", "n": 2}),
                json!({"service": "pay", "code": "200", "n": 5}),
            ],
            "dashboard_panel" if q.sql.contains("bad_shape") => vec![json!({"a": "x", "b": "y"})],
            "dashboard_panel" => vec![
                json!({"service": "cart", "n": 3}),
                json!({"service": "pay", "n": 4}),
            ],
            _ => vec![],
        })
    }));
    let spec = spec_with(json!([
        {"id": "pie1", "type": "pie", "title": "p", "query": {"kind": "sql", "text": "SELECT service, n FROM t"}},
        {"id": "piv", "type": "table", "title": "t", "query": {"kind": "sql", "text": "SELECT 1 /* pivot */"}, "options": {"pivot": {"row": "service", "column": "code", "value": "n"}}},
        {"id": "bad", "type": "line", "title": "l", "query": {"kind": "sql", "text": "SELECT 1 /* bad_shape */"}},
        {"id": "md", "type": "markdown", "title": "m", "content": "**hi** <script>x</script>"},
        {"id": "htm", "type": "html", "title": "h", "content": "<p onclick=x>a</p><script>b</script>"}
    ]));
    let d = create(&a, "Data", spec).await;
    let base = format!("{BASE}/dashboards/{}/panels", id_of(&d));
    let w = "from=1793398000000&to=1793402000000";
    let r = get(&a, &format!("{base}/pie1/data?{w}")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    assert_eq!(
        (
            j["data"]["columns"].clone(),
            j["data"]["shape_error"].clone()
        ),
        (json!(["service", "n"]), Value::Null)
    );
    assert_eq!(j["data"]["rows"].as_array().unwrap().len(), 2);
    assert_eq!(j["meta"]["query"], "dashboard_panel");
    let j = get(&a, &format!("{base}/piv/data?{w}")).await.json();
    assert_eq!(
        j["data"]["columns"],
        json!(["service", "200", "500", "Total"])
    );
    assert_eq!(
        j["data"]["rows"][2],
        json!({"service": "Total", "200": 15.0, "500": 2.0, "Total": 17.0})
    );
    let j = get(&a, &format!("{base}/bad/data?{w}")).await.json();
    assert!(
        j["data"]["shape_error"].as_str().unwrap().contains("time"),
        "a wrong shape is reported on the panel, not as a failure: {j}"
    );
    let j = get(&a, &format!("{base}/md/data")).await.json();
    assert!(
        j["data"]["html"]
            .as_str()
            .unwrap()
            .contains("<strong>hi</strong>")
            && !j["data"]["html"].as_str().unwrap().contains("<script")
    );
    let j = get(&a, &format!("{base}/htm/data")).await.json();
    assert_eq!(j["data"]["html"], "<p>a</p>");
    assert_eq!(
        get(&a, &format!("{base}/nope/data?{w}")).await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        get(&a, &format!("{base}/pie1/data?from=5&to=1"))
            .await
            .status,
        StatusCode::BAD_REQUEST
    );
}

#[test]
fn ac_0_8_2_demo_dashboard_is_valid() {
    // The playground's dashboard goes through the same validation as an import, uses every panel
    // type, and its script tag is gone after sanitising.
    let raw: Value = serde_json::from_str(grepui::playground::DEMO_DASHBOARD).unwrap();
    let c = dash::canonical(&raw).unwrap();
    let used: HashSet<&str> = c["tabs"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|t| t["panels"].as_array().unwrap().iter())
        .filter_map(|p| p["type"].as_str())
        .collect();
    for t in panels::TYPES {
        assert!(
            used.contains(t.name),
            "the demo dashboard has no {} panel",
            t.name
        );
    }
    let html = dash::find_panel(&c, "status").unwrap()["content"]
        .as_str()
        .unwrap();
    assert!(!panels::sanitize_html(html).contains("<script"));
    assert_eq!(dash::canonical(&c).unwrap(), c);
}

// ---- AC0.8.3 variables ----------------------------------------------------------------

fn decls() -> Vec<Value> {
    dash::canonical(&json!({"variables": [
        {"name": "service", "kind": "custom", "options": ["cart", "pay"], "default": ["cart"]},
        {"name": "codes", "kind": "custom", "options": ["200", "500", "404"], "multi": true, "default": ["200"]},
        {"name": "region", "kind": "query", "query": "SELECT DISTINCT region FROM t", "multi": true}
    ], "tabs": [{"title": "t"}]})).unwrap()["variables"].as_array().unwrap().clone()
}

fn sel(pairs: &[(&str, &[&str])]) -> vars::Selected {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.iter().map(|s| s.to_string()).collect()))
        .collect()
}

#[test]
fn ac_0_8_3_variable_resolution() {
    let d = decls();
    // The default applies when nothing is selected; a selection overrides it.
    let (sql, binds) =
        vars::bind_sql("SELECT * FROM t WHERE svc = $service", &d, &sel(&[])).unwrap();
    assert_eq!(
        (sql.as_str(), binds),
        (
            "SELECT * FROM t WHERE svc = ?",
            vec![Bind::Str("cart".into())]
        )
    );
    let (_, binds) = vars::bind_sql("SELECT $service", &d, &sel(&[("service", &["pay"])])).unwrap();
    assert_eq!(binds, vec![Bind::Str("pay".into())]);
    // Window macros, strings, quoted names and comments are left alone; order of binds = order of use.
    let (sql, binds) = vars::bind_sql("SELECT '$service' AS \"$codes\", `$service` FROM t -- $service\nWHERE ts >= $__from AND a = $service /* $codes */ AND b = $service", &d, &sel(&[])).unwrap();
    assert!(
        sql.contains("'$service' AS \"$codes\", `$service`")
            && sql.contains("-- $service")
            && sql.contains("/* $codes */"),
        "{sql}"
    );
    assert!(
        sql.contains("$__from") && sql.contains("a = ?") && sql.contains("b = ?"),
        "{sql}"
    );
    assert_eq!(binds.len(), 2);
    // Unknown variables and illegal values are refused, never passed through.
    assert!(
        vars::bind_sql("SELECT $nope", &d, &sel(&[]))
            .unwrap_err()
            .1
            .contains("not a variable")
    );
    assert!(
        vars::bind_sql("SELECT $service", &d, &sel(&[("service", &["hacker"])]))
            .unwrap_err()
            .1
            .contains("not one of the values")
    );
    // Names are checked when a spec is saved.
    for bad in ["1abc", "a b", "__from", "a-b", "", "x".repeat(33).as_str()] {
        let spec = json!({"variables": [{"name": bad, "kind": "custom", "options": ["a"]}], "tabs": [{"title": "t"}]});
        assert!(dash::canonical(&spec).is_err(), "{bad:?}");
    }
    let dup = json!({"variables": [{"name": "a", "kind": "custom", "options": ["x"]}, {"name": "a", "kind": "custom", "options": ["y"]}], "tabs": [{"title": "t"}]});
    assert!(
        dash::canonical(&dup)
            .unwrap_err()
            .1
            .contains("declared twice")
    );
    // PromQL: plain substitution of safe values only.
    assert_eq!(
        vars::substitute_promql("up{svc=\"$service\"}", &d, &sel(&[("service", &["pay"])]))
            .unwrap(),
        "up{svc=\"pay\"}"
    );
    assert!(
        vars::selected_from_query("var-service=pay&from=1&var-codes=200&var-codes=500").unwrap()["codes"]
            == ["200", "500"]
    );
    assert!(vars::selected_from_query("var-1x=a").is_err());
}

#[test]
fn ac_0_8_3_variable_multiselect() {
    let d = decls();
    let (sql, binds) = vars::bind_sql(
        "SELECT * FROM t WHERE code IN ($codes)",
        &d,
        &sel(&[("codes", &["200", "500", "404"])]),
    )
    .unwrap();
    assert_eq!(sql, "SELECT * FROM t WHERE code IN (?, ?, ?)");
    assert_eq!(binds, ["200", "500", "404"].map(|s| Bind::Str(s.into())));
    // An empty multi-select matches nothing instead of failing; a single-select must have a value.
    let nodefault = dash::canonical(&json!({"variables": [{"name": "m", "kind": "custom", "options": ["a"], "multi": true}, {"name": "s", "kind": "custom", "options": ["a"]}], "tabs": [{"title": "t"}]})).unwrap()["variables"].as_array().unwrap().clone();
    let (sql, binds) = vars::bind_sql("WHERE x IN ($m)", &nodefault, &sel(&[])).unwrap();
    assert_eq!((sql.as_str(), binds.len()), ("WHERE x IN (NULL)", 0));
    assert!(
        vars::bind_sql("WHERE x = $s", &nodefault, &sel(&[]))
            .unwrap_err()
            .1
            .contains("no value")
    );
    // Several values for a single-select variable are refused.
    assert!(
        vars::bind_sql(
            "WHERE x = $service",
            &d,
            &sel(&[("service", &["cart", "pay"])])
        )
        .unwrap_err()
        .1
        .contains("not multi-select")
    );
    // PromQL joins them for a regex matcher.
    assert_eq!(
        vars::substitute_promql("up{c=~\"$codes\"}", &d, &sel(&[("codes", &["200", "500"])]))
            .unwrap(),
        "up{c=~\"200|500\"}"
    );
    // The same variable twice binds twice, in order.
    let (sql, binds) = vars::bind_sql(
        "a IN ($codes) OR b IN ($codes)",
        &d,
        &sel(&[("codes", &["200", "500"])]),
    )
    .unwrap_err_or_ok();
    assert_eq!(sql, "a IN (?, ?) OR b IN (?, ?)");
    assert_eq!(binds.len(), 4);
}

trait OrOk<T> {
    fn unwrap_err_or_ok(self) -> T;
}
impl<T> OrOk<T> for Result<T, grepui::error::ApiError> {
    fn unwrap_err_or_ok(self) -> T {
        match self {
            Ok(v) => v,
            Err(e) => panic!("{}", e.1),
        }
    }
}

#[tokio::test]
async fn ac_0_8_3_variable_injection_is_bound() {
    let (a, store, _) = setup();
    *store.script.lock().unwrap() = Some(Box::new(|q| {
        Ok(if q.name == "dashboard_panel" {
            vec![json!({"n": 0})]
        } else {
            vec![]
        })
    }));
    let spec = json!({
        "variables": [{"name": "svc", "kind": "query", "query": "SELECT DISTINCT service FROM t", "multi": true}, {"name": "fixed", "kind": "custom", "options": ["a", "b"]}],
        "tabs": [{"title": "t", "panels": [
            {"id": "q", "type": "stat", "title": "s", "query": {"kind": "sql", "text": "SELECT count(*) AS n FROM t WHERE service IN ($svc) AND ts >= $__from"}},
            {"id": "pq", "type": "line", "title": "p", "query": {"kind": "promql", "text": "up{job=\"$svc\"}"}}
        ]}]
    });
    let d = create(&a, "Inject", spec).await;
    let base = format!("{BASE}/dashboards/{}/panels", id_of(&d));
    let evil = "x' OR '1'='1'; DROP TABLE t; --";
    let enc = |s: &str| url::form_urlencoded::byte_serialize(s.as_bytes()).collect::<String>();
    let r = get(
        &a,
        &format!(
            "{base}/q/data?from=1793398000000&to=1793402000000&var-svc={}&var-svc=ok",
            enc(evil)
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let q = store
        .seen
        .lock()
        .unwrap()
        .iter()
        .rev()
        .find(|q| q.name == "dashboard_panel")
        .cloned()
        .unwrap();
    assert!(q.sql.contains("service IN (?, ?)"), "{}", q.sql);
    assert!(
        !q.sql.contains("DROP") && !q.sql.contains("1'='1"),
        "the value never enters the statement: {}",
        q.sql
    );
    assert_eq!(q.binds[0], Bind::Str(evil.into()), "it arrives as a value");
    assert_eq!(q.binds[1], Bind::Str("ok".into()));
    // The statement still passes the SQL-mode checks, because only `?` was added.
    grepui::query::sqlmode::check(&q.sql["/* q:dashboard_panel */ ".len()..], "public").unwrap();
    // A variable cannot smuggle a statement through the window macros either.
    let r = get(
        &a,
        &format!(
            "{base}/q/data?from=1793398000000&to=1793402000000&var-svc={}",
            enc("$__to")
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    let q = store
        .seen
        .lock()
        .unwrap()
        .iter()
        .rev()
        .find(|q| q.name == "dashboard_panel")
        .cloned()
        .unwrap();
    assert_eq!(q.binds[0], Bind::Str("$__to".into()));
    // PromQL has no bind parameters, so hostile values are refused outright.
    for bad in [
        "a\"} or vector(1) #",
        "x\\",
        "{job=\"a\"}",
        "a;b",
        "up and on() up",
    ] {
        let r = get(
            &a,
            &format!(
                "{base}/pq/data?from=1793398000000&to=1793402000000&var-svc={}",
                enc(bad)
            ),
        )
        .await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{bad:?}: {}", r.text());
    }
    // Values that name another database never reach TQL.
    let r = get(
        &a,
        &format!(
            "{base}/pq/data?from=1793398000000&to=1793402000000&var-svc={}",
            enc("__schema__")
        ),
    )
    .await;
    assert!(
        r.status == StatusCode::BAD_REQUEST
            || !store
                .seen
                .lock()
                .unwrap()
                .iter()
                .any(|q| q.sql.contains("__schema__")),
        "{}",
        r.text()
    );
    // A custom variable allows only its own values.
    let r = get(
        &a,
        &format!("{BASE}/dashboards/{}/variables?var-fixed=zzz", id_of(&d)),
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::OK,
        "options do not depend on the selection of others"
    );
}

// ---- AC0.8.4 refresh and compare ------------------------------------------------------

#[test]
fn ac_0_8_4_refresh_floor() {
    assert_eq!(time::refresh_ms("off"), Some(0));
    assert_eq!(time::refresh_ms("5s"), Some(5_000));
    assert_eq!(time::refresh_ms("1m"), Some(60_000));
    for bad in [
        "", "1s", "2s", "500ms", "0", "-5s", "30", "1d", "OFF", "5 s",
    ] {
        assert_eq!(
            time::refresh_ms(bad),
            None,
            "{bad:?} is not an offered interval"
        );
    }
    // Every offered interval respects the floor.
    assert!(
        time::REFRESH
            .iter()
            .filter_map(|r| time::refresh_ms(r))
            .all(|ms| ms == 0 || ms >= time::REFRESH_FLOOR_MS)
    );
    // The spec only accepts offered intervals.
    assert_eq!(
        dash::canonical(&json!({"refresh": "10s", "tabs": [{"title": "t"}]})).unwrap()["refresh"],
        "10s"
    );
    assert_eq!(
        dash::canonical(&json!({"tabs": [{"title": "t"}]})).unwrap()["refresh"],
        "off"
    );
    for bad in ["1s", "fast", "0s", "100ms"] {
        assert!(
            dash::canonical(&json!({"refresh": bad, "tabs": [{"title": "t"}]})).is_err(),
            "{bad}"
        );
    }
    assert!(dash::canonical(&json!({"time": {"range": "90m"}, "tabs": [{"title": "t"}]})).is_err());
}

#[tokio::test]
async fn ac_0_8_4_compare_shifts_window() {
    let w = Window {
        from_ms: T0 - 3_600_000,
        to_ms: T0,
        limit: 10,
    };
    for (c, off) in [
        ("1h", 3_600_000),
        ("1d", 86_400_000),
        ("7d", 7 * 86_400_000),
        ("prev", 3_600_000),
    ] {
        let (cw, o) = time::compare_window(w, c).unwrap();
        assert_eq!(o, off, "{c}");
        assert_eq!(
            (cw.from_ms, cw.to_ms),
            (w.from_ms - off, w.to_ms - off),
            "{c}"
        );
        assert_eq!(cw.to_ms - cw.from_ms, w.to_ms - w.from_ms, "same length");
    }
    // `prev` follows the window length.
    let long = Window {
        from_ms: T0 - 6 * 3_600_000,
        to_ms: T0,
        limit: 10,
    };
    assert_eq!(time::compare_window(long, "prev").unwrap().1, 6 * 3_600_000);
    assert!(time::compare_window(w, "2d").is_err());
    assert!(
        time::compare_window(
            Window {
                from_ms: 1000,
                to_ms: 2000,
                limit: 1
            },
            "7d"
        )
        .is_err(),
        "not before the epoch"
    );
    // Rows of the past period are moved forward so they line up under the current one.
    let mut rows = vec![
        json!({"t": 100, "v": 1}),
        json!({"t": "x", "v": 2}),
        json!({"v": 3}),
    ];
    time::shift_rows(&mut rows, "t", 1000);
    assert_eq!(
        rows,
        [
            json!({"t": 1100, "v": 1}),
            json!({"t": "x", "v": 2}),
            json!({"v": 3})
        ]
    );
    // End to end: the second query runs one period earlier and the answer carries the offset.
    let (a, store, _) = setup();
    *store.script.lock().unwrap() = Some(Box::new(|q| {
        Ok(if q.name == "dashboard_panel" {
            vec![
                json!({"time": 5_000, "v": 1.0}),
                json!({"time": 6_000, "v": 2.0}),
            ]
        } else {
            vec![]
        })
    }));
    let d = create(&a, "Cmp", spec_with(json!([{"id": "l", "type": "line", "title": "l", "query": {"kind": "sql", "text": "SELECT time, v FROM t WHERE ts >= $__from AND ts < $__to"}}]))).await;
    let from = T0 - 3_600_000;
    let r = get(
        &a,
        &format!(
            "{BASE}/dashboards/{}/panels/l/data?from={from}&to={T0}&compare=1d",
            id_of(&d)
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let c = &r.json()["data"]["compare"];
    assert_eq!(c["offset_ms"], 86_400_000);
    assert_eq!(
        (c["from"].as_i64().unwrap(), c["to"].as_i64().unwrap()),
        (from - 86_400_000, T0 - 86_400_000)
    );
    assert_eq!(
        c["rows"][0]["time"],
        5_000 + 86_400_000,
        "time shifted forward"
    );
    let seen: Vec<String> = store
        .seen
        .lock()
        .unwrap()
        .iter()
        .filter(|q| q.name == "dashboard_panel")
        .map(|q| q.sql.clone())
        .collect();
    assert_eq!(seen.len(), 2);
    assert!(
        seen[0].contains(&ts_string(from)) && seen[1].contains(&ts_string(from - 86_400_000)),
        "{seen:?}"
    );
    let r = get(
        &a,
        &format!(
            "{BASE}/dashboards/{}/panels/l/data?from={from}&to={T0}&compare=2d",
            id_of(&d)
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
}

// ---- AC0.8.5 import, export, move -----------------------------------------------------

#[tokio::test]
async fn ac_0_8_5_export_import_roundtrip() {
    let (a, _, _) = setup();
    let spec = json!({
        "time": {"range": "6h"}, "refresh": "30s",
        "variables": [{"name": "svc", "label": "Service", "kind": "custom", "options": ["cart", "pay"], "multi": true, "default": ["cart"]}],
        "tabs": [
            {"title": "Overview", "panels": [{"type": "line", "title": "rate", "w": 8, "h": 4, "query": {"kind": "promql", "text": "rate(x[5m])"}, "options": {"unit": "req/s", "legend": true}}, {"type": "html", "title": "note", "content": "<p>hi</p>"}]},
            {"title": "Tables", "panels": [{"type": "table", "title": "t", "query": {"kind": "sql", "text": "SELECT a, b, c FROM t"}, "options": {"pivot": {"row": "a", "column": "b", "value": "c"}}}]}
        ]
    });
    let d = create(&a, "Original", spec).await;
    let exported = get(&a, &format!("{BASE}/dashboards/{}/export", id_of(&d)))
        .await
        .json()["data"]
        .clone();
    assert_eq!(exported["grepui_dashboard"], 1);
    assert_eq!(exported["title"], "Original");
    // Import creates a new dashboard (new id) with an identical spec, ids included.
    let r = send_text(
        &a,
        Method::POST,
        &format!("{BASE}/dashboards/import"),
        &exported.to_string(),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let new_id = r.json()["data"]["imported"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(new_id, id_of(&d));
    let copy = get(&a, &format!("{BASE}/dashboards/{new_id}")).await.json()["data"].clone();
    assert_eq!(copy["spec"], d["spec"], "identical dashboard");
    assert_eq!(copy["title"], "Original");
    let again = get(&a, &format!("{BASE}/dashboards/{new_id}/export"))
        .await
        .json()["data"]
        .clone();
    assert_eq!(
        again, exported,
        "export of the copy equals the first export"
    );
    // Bulk export and bundle import.
    let bundle = get(&a, &format!("{BASE}/dashboards/export")).await.json()["data"].clone();
    assert_eq!(bundle["grepui_dashboards"], 1);
    assert_eq!(bundle["dashboards"].as_array().unwrap().len(), 2);
    let r = send_text(
        &a,
        Method::POST,
        &format!("{BASE}/dashboards/import"),
        &bundle.to_string(),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    assert_eq!(r.json()["data"]["imported"].as_array().unwrap().len(), 2);
    assert_eq!(
        get(&a, &format!("{BASE}/dashboards")).await.json()["data"]
            .as_array()
            .unwrap()
            .len(),
        4
    );
    // Import into a folder.
    let fid = id_of(
        &send_json(
            &a,
            Method::POST,
            &format!("{BASE}/folders"),
            &json!({"name": "F"}),
        )
        .await
        .json()["data"],
    );
    let r = send_text(
        &a,
        Method::POST,
        &format!("{BASE}/dashboards/import?folder={fid}"),
        &exported.to_string(),
    )
    .await;
    let nid = r.json()["data"]["imported"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        get(&a, &format!("{BASE}/dashboards/{nid}")).await.json()["data"]["folder_id"],
        fid.as_str()
    );
}

#[tokio::test]
async fn ac_0_8_5_bulk_move() {
    let (a, _, clock) = setup();
    let f1 = id_of(
        &send_json(
            &a,
            Method::POST,
            &format!("{BASE}/folders"),
            &json!({"name": "One"}),
        )
        .await
        .json()["data"],
    );
    let f2 = id_of(
        &send_json(
            &a,
            Method::POST,
            &format!("{BASE}/folders"),
            &json!({"name": "Two"}),
        )
        .await
        .json()["data"],
    );
    let mut ids = vec![];
    for t in ["a", "b", "c"] {
        ids.push(id_of(&create(&a, t, empty()).await));
    }
    clock.store(T0 + 1000, Ordering::SeqCst);
    let r = send_json(
        &a,
        Method::POST,
        &format!("{BASE}/dashboards/move"),
        &json!({"ids": [ids[0], ids[1]], "folder_id": f1}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["data"]["moved"], 2);
    let in_f1: Vec<_> = get(&a, &format!("{BASE}/dashboards?folder={f1}"))
        .await
        .json()["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["title"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(in_f1, ["a", "b"]);
    // Move again (to another folder, then to the root); ids and specs do not change.
    let before = get(&a, &format!("{BASE}/dashboards/{}", ids[0]))
        .await
        .json()["data"]["spec"]
        .clone();
    assert_eq!(
        send_json(
            &a,
            Method::POST,
            &format!("{BASE}/dashboards/move"),
            &json!({"ids": [ids[0]], "folder_id": f2})
        )
        .await
        .status,
        StatusCode::OK
    );
    assert_eq!(
        send_json(
            &a,
            Method::POST,
            &format!("{BASE}/dashboards/move"),
            &json!({"ids": [ids[1]], "folder_id": ""})
        )
        .await
        .status,
        StatusCode::OK
    );
    let moved = get(&a, &format!("{BASE}/dashboards/{}", ids[0]))
        .await
        .json()["data"]
        .clone();
    assert_eq!(
        (moved["folder_id"].as_str().unwrap(), moved["spec"].clone()),
        (f2.as_str(), before)
    );
    assert_eq!(
        get(&a, &format!("{BASE}/dashboards/{}", ids[1]))
            .await
            .json()["data"]["folder_id"],
        ""
    );
    // A missing id or folder fails the whole move before anything changes.
    let r = send_json(
        &a,
        Method::POST,
        &format!("{BASE}/dashboards/move"),
        &json!({"ids": [ids[2], "0123456789abcdef"], "folder_id": f1}),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert_eq!(
        get(&a, &format!("{BASE}/dashboards/{}", ids[2]))
            .await
            .json()["data"]["folder_id"],
        "",
        "c was not moved"
    );
    let r = send_json(
        &a,
        Method::POST,
        &format!("{BASE}/dashboards/move"),
        &json!({"ids": [ids[2]], "folder_id": "0123456789abcdef"}),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert_eq!(
        send_json(
            &a,
            Method::POST,
            &format!("{BASE}/dashboards/move"),
            &json!({"ids": [], "folder_id": f1})
        )
        .await
        .status,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn ac_0_8_5_import_rejects_malformed() {
    let (a, store, _) = setup();
    let post = |body: &str| {
        let a = a.clone();
        let body = body.to_string();
        async move {
            send_text(
                &a,
                Method::POST,
                &format!("{BASE}/dashboards/import"),
                &body,
            )
            .await
        }
    };
    let err = |r: &Resp| r.json()["error"].as_str().unwrap_or_default().to_string();
    // Not JSON: the error says where.
    let r = post("{\n  \"grepui_dashboard\": 1,\n  \"title\": \"x\",\n}").await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert!(err(&r).contains("invalid JSON at line 4"), "{}", err(&r));
    let r = post("").await;
    assert!(err(&r).contains("invalid JSON"), "{}", err(&r));
    // JSON that is not a dashboard export.
    assert!(
        err(&post("[]").await).contains("must be an object")
            || err(&post("[]").await).contains("not a grepui")
    );
    assert!(err(&post("{\"title\": \"x\"}").await).contains("not a grepui dashboard export"));
    assert!(err(&post("{\"grepui_dashboard\": 1, \"spec\": {}}").await).contains(".title"));
    assert!(err(&post("{\"grepui_dashboard\": 1, \"title\": \"x\", \"spec\": {\"tabs\": [{\"title\": \"t\", \"panels\": [{\"type\": \"line\", \"title\": \"p\"}]}]}}").await).contains("spec.tabs[0].panels[0].query: is required"));
    assert!(err(&post("{\"grepui_dashboard\": 1, \"title\": \"x\", \"spec\": {\"tabs\": [{\"title\": \"t\", \"panels\": [{\"type\": \"bogus\", \"title\": \"p\"}]}]}}").await).contains("panels[0].type: unknown panel type"));
    assert!(err(&post("{\"grepui_dashboard\": 1, \"title\": \"x\", \"spec\": {\"tabs\": []}, \"owner\": \"x\"}").await).contains("unknown key \"owner\""));
    // Inside a bundle the path names the dashboard.
    let r = post("{\"grepui_dashboards\": 1, \"dashboards\": [{\"grepui_dashboard\": 1, \"title\": \"ok\", \"spec\": {\"tabs\": [{\"title\": \"t\"}]}}, {\"grepui_dashboard\": 1, \"title\": \"bad\", \"spec\": {\"tabs\": [{\"title\": \"t\", \"panels\": [{\"type\": \"x\", \"title\": \"p\"}]}]}}]}").await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert!(
        err(&r).contains("dashboards[1].spec.tabs[0].panels[0].type"),
        "{}",
        err(&r)
    );
    // Nothing from any rejected import was stored, not even the valid first dashboard of a bundle.
    assert!(store.dashes.lock().unwrap().is_empty());
    assert_eq!(post("{\"grepui_dashboard\": 1, \"title\": \"x\", \"spec\": {\"tabs\": [{\"title\": \"t\"}]}}").await.status, StatusCode::CREATED);
}

// ---- AC0.8.6 public links and playlists -----------------------------------------------

async fn shared(a: &Router, did: &str, hours: i64) -> Value {
    let r = send_json(
        a,
        Method::POST,
        &format!("{BASE}/dashboards/{did}/shares"),
        &json!({"ttl_hours": hours}),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    r.json()["data"].clone()
}

#[tokio::test]
async fn ac_0_8_6_public_link_readonly() {
    let (a, store, _) = setup();
    *store.script.lock().unwrap() = Some(Box::new(|q| {
        Ok(if q.name == "dashboard_panel" {
            vec![json!({"n": 1})]
        } else {
            vec![]
        })
    }));
    let d = create(&a, "Public", json!({"variables": [{"name": "v", "kind": "query", "query": "SELECT secret_col FROM secret_table"}], "tabs": [{"title": "t", "panels": [{"id": "s", "type": "stat", "title": "s", "query": {"kind": "sql", "text": "SELECT n FROM secret_table"}}, {"id": "m", "type": "markdown", "title": "m", "content": "hello"}]}]})).await;
    let did = id_of(&d);
    let sh = shared(&a, &did, 24).await;
    let token = sh["token"].as_str().unwrap().to_string();
    assert_eq!(token.len(), 32);
    assert!(sh["path"].as_str().unwrap().ends_with(&token));
    // Only a hash is stored.
    let stored = store
        .shares
        .lock()
        .unwrap()
        .values()
        .next()
        .unwrap()
        .clone();
    assert!(
        !stored.iter().any(|b| b.show() == token),
        "the token itself is never stored"
    );
    assert_eq!(stored[3].show().len(), 64);
    // No query text leaves the server in a public view.
    let r = get(&a, &format!("{BASE}/public/{token}")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["data"]["read_only"], true);
    let text = r.text();
    assert!(
        !text.contains("secret_table") && !text.contains("secret_col") && !text.contains("SELECT"),
        "{text}"
    );
    assert_eq!(r.json()["data"]["title"], "Public");
    // Panel data works and runs the saved query; a panel of another dashboard is not reachable.
    let r = get(
        &a,
        &format!("{BASE}/public/{token}/panels/s/data?from=1793398000000&to=1793402000000"),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["data"]["rows"], json!([{"n": 1}]));
    assert_eq!(
        get(&a, &format!("{BASE}/public/{token}/panels/other/data"))
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        get(&a, &format!("{BASE}/public/{token}/panels/m/data"))
            .await
            .json()["data"]["html"],
        "<p>hello</p>"
    );
    // Read-only: nothing under /public accepts a write, and a token is not a dashboard id.
    for m in [Method::POST, Method::PUT, Method::DELETE, Method::PATCH] {
        for uri in [
            format!("{BASE}/public/{token}"),
            format!("{BASE}/public/{token}/panels/s/data"),
            format!("{BASE}/public/{token}/variables"),
        ] {
            let r = send(&a, m.clone(), &uri).await;
            assert_eq!(r.status, StatusCode::METHOD_NOT_ALLOWED, "{m} {uri}");
        }
    }
    let writes_before = store.write_count();
    assert_eq!(
        get(&a, &format!("{BASE}/dashboards/{token}")).await.status,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(store.write_count(), writes_before);
    // Unknown, malformed and cross-org tokens are all the same 404.
    for t in ["0123456789abcdef0123456789abcdef", "short", &"g".repeat(32)] {
        assert_eq!(
            get(&a, &format!("{BASE}/public/{t}")).await.status,
            StatusCode::NOT_FOUND,
            "{t}"
        );
    }
    assert_eq!(
        get(
            &a,
            "/api/v1/orgs/other/public/0123456789abcdef0123456789abcdef"
        )
        .await
        .status,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn ac_0_8_6_public_link_expires_revoked() {
    let (a, _, clock) = setup();
    let did = id_of(&create(&a, "Exp", empty()).await);
    let sh = shared(&a, &did, 2).await;
    let (token, share_id) = (
        sh["token"].as_str().unwrap().to_string(),
        sh["id"].as_str().unwrap().to_string(),
    );
    assert_eq!(sh["expires_ms"], T0 + 2 * 3_600_000);
    assert_eq!(
        get(&a, &format!("{BASE}/public/{token}")).await.status,
        StatusCode::OK
    );
    clock.store(T0 + 2 * 3_600_000 - 1, Ordering::SeqCst);
    assert_eq!(
        get(&a, &format!("{BASE}/public/{token}")).await.status,
        StatusCode::OK,
        "valid until the last millisecond"
    );
    clock.store(T0 + 2 * 3_600_000, Ordering::SeqCst);
    let r = get(&a, &format!("{BASE}/public/{token}")).await;
    assert_eq!(r.status, StatusCode::GONE, "{}", r.text());
    assert!(r.json()["error"].as_str().unwrap().contains("expired"));
    assert_eq!(
        get(&a, &format!("{BASE}/public/{token}/panels/x/data"))
            .await
            .status,
        StatusCode::GONE
    );
    // Revoke a live link.
    clock.store(T0, Ordering::SeqCst);
    let sh2 = shared(&a, &did, 24).await;
    let t2 = sh2["token"].as_str().unwrap().to_string();
    let list = get(&a, &format!("{BASE}/dashboards/{did}/shares"))
        .await
        .json()["data"]
        .clone();
    assert_eq!(list.as_array().unwrap().len(), 2);
    assert!(
        list.as_array()
            .unwrap()
            .iter()
            .all(|s| s.get("token").is_none()),
        "tokens are not listed"
    );
    let r = send(
        &a,
        Method::DELETE,
        &format!(
            "{BASE}/dashboards/{did}/shares/{}",
            sh2["id"].as_str().unwrap()
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["data"]["active"], false);
    assert_eq!(
        get(&a, &format!("{BASE}/public/{t2}")).await.status,
        StatusCode::NOT_FOUND,
        "a revoked link is gone"
    );
    // The other link is unaffected; revoking with the wrong dashboard fails.
    assert_eq!(
        get(&a, &format!("{BASE}/public/{token}")).await.status,
        StatusCode::OK
    );
    let other = id_of(&create(&a, "Other", empty()).await);
    assert_eq!(
        send(
            &a,
            Method::DELETE,
            &format!("{BASE}/dashboards/{other}/shares/{share_id}")
        )
        .await
        .status,
        StatusCode::NOT_FOUND
    );
    // Deleting the dashboard kills its links.
    send(&a, Method::DELETE, &format!("{BASE}/dashboards/{did}")).await;
    assert_eq!(
        get(&a, &format!("{BASE}/public/{token}")).await.status,
        StatusCode::NOT_FOUND
    );
    // TTL limits.
    for h in [0, -1, 24 * 91] {
        let r = send_json(
            &a,
            Method::POST,
            &format!("{BASE}/dashboards/{other}/shares"),
            &json!({"ttl_hours": h}),
        )
        .await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{h}");
    }
}

#[tokio::test]
async fn ac_0_8_6_playlist_rotation() {
    // Pure rotation: depends on the clock only.
    assert_eq!(time::rotation(3, 30, 0), (0, 30_000));
    assert_eq!(time::rotation(3, 30, 29_999), (0, 1));
    assert_eq!(time::rotation(3, 30, 30_000), (1, 30_000));
    assert_eq!(time::rotation(3, 30, 60_000), (2, 30_000));
    assert_eq!(time::rotation(3, 30, 90_000), (0, 30_000), "wraps around");
    assert_eq!(time::rotation(1, 30, 123_456).0, 0);
    assert_eq!(time::rotation(0, 30, 5).0, 0);
    assert_eq!(
        time::rotation(3, 1, 10_000).1,
        10_000,
        "intervals below the floor are raised to it"
    );
    // Two displays asking at the same time see the same dashboard.
    let seen: HashSet<usize> = (0..20).map(|_| time::rotation(5, 15, 77_000).0).collect();
    assert_eq!(seen.len(), 1);
    // Through the API: the listed dashboards in order, as the clock moves.
    let (a, _, clock) = setup();
    let ids: Vec<String> = {
        let mut v = vec![];
        for t in ["a", "b", "c"] {
            v.push(id_of(&create(&a, t, empty()).await));
        }
        v
    };
    let r = send_json(
        &a,
        Method::POST,
        &format!("{BASE}/playlists"),
        &json!({"title": "Wall", "spec": {"dashboards": ids, "interval_s": 20}}),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let pid = id_of(&r.json()["data"]);
    let mut order = vec![];
    for step in 0..6 {
        clock.store(60_000_000 + step * 20_000 + 500, Ordering::SeqCst);
        let j = get(&a, &format!("{BASE}/playlists/{pid}/now")).await.json()["data"].clone();
        order.push(j["dashboard_id"].as_str().unwrap().to_string());
        assert_eq!(j["next_in_ms"], 19_500);
    }
    let expect: Vec<String> = (0..6)
        .map(|i| ids[((60_000_000 / 20_000 + i) % 3) as usize].clone())
        .collect();
    assert_eq!(order, expect);
    // Validation, listing, edit, delete.
    for bad in [
        json!({"dashboards": []}),
        json!({"dashboards": ["x"]}),
        json!({"dashboards": ids, "interval_s": 3}),
        json!({"dashboards": ids, "extra": 1}),
        json!({}),
    ] {
        let r = send_json(
            &a,
            Method::POST,
            &format!("{BASE}/playlists"),
            &json!({"title": "bad", "spec": bad}),
        )
        .await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{bad}");
    }
    assert_eq!(
        get(&a, &format!("{BASE}/playlists")).await.json()["data"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        get(&a, &format!("{BASE}/dashboards")).await.json()["data"]
            .as_array()
            .unwrap()
            .len(),
        3,
        "playlists are not dashboards"
    );
    assert_eq!(
        get(&a, &format!("{BASE}/dashboards/{pid}")).await.status,
        StatusCode::NOT_FOUND
    );
    let r = send_json(
        &a,
        Method::PUT,
        &format!("{BASE}/playlists/{pid}"),
        &json!({"spec": {"dashboards": [ids[0]]}}),
    )
    .await;
    assert_eq!(
        r.json()["data"]["spec"]["interval_s"],
        30,
        "the default interval applies"
    );
    assert_eq!(
        send(&a, Method::DELETE, &format!("{BASE}/playlists/{pid}"))
            .await
            .status,
        StatusCode::OK
    );
    assert_eq!(
        get(&a, &format!("{BASE}/playlists/{pid}/now")).await.status,
        StatusCode::NOT_FOUND
    );
}

// ---- live ------------------------------------------------------------------------------

fn enc(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

struct Live {
    app: Router,
    data_db: String,
    meta_db: String,
    batch: synth::Batch,
}

async fn live() -> Live {
    let base = std::env::var("GREPUI_TEST_DB")
        .expect("set GREPUI_TEST_DB=mysql://root@127.0.0.1:4002/public");
    static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    let (data_db, meta_db) = (
        format!("demo_it_db_{}_{n}", std::process::id()),
        format!("grepui_it_db_{}_{n}", std::process::id()),
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
    seed(
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
    store::migrate(r.as_ref(), r.as_ref(), &meta_db)
        .await
        .expect("migrate");
    let app = router(
        AppState::with_parts(
            Config {
                greptime_mysql: url,
                meta_database: meta_db.clone(),
                ..Config::default()
            },
            r.clone(),
            r,
            grepui::api::system_clock(),
        )
        .unwrap(),
    );
    Live {
        app,
        data_db,
        meta_db,
        batch,
    }
}

impl Live {
    async fn drop(self) {
        let form = [("content-type", "application/x-www-form-urlencoded")];
        for db in [&self.data_db, &self.meta_db] {
            let _ = http::post(
                "http://127.0.0.1:4000",
                "/v1/sql",
                &form,
                format!("sql=DROP DATABASE IF EXISTS {db}").as_bytes(),
            )
            .await;
        }
    }
    fn window() -> String {
        let now = chrono::Utc::now().timestamp_millis();
        format!("from={}&to={}", now - 3 * 3_600_000, now + 3_600_000)
    }
}

#[tokio::test]
#[ignore = "needs GreptimeDB: make db-up && make test-db"]
async fn ac_0_8_1_dashboard_crud_live() {
    let l = live().await;
    let a = &l.app;
    let fid = id_of(
        &send_json(
            a,
            Method::POST,
            &format!("{BASE}/folders"),
            &json!({"name": "Live"}),
        )
        .await
        .json()["data"],
    );
    let spec = json!({
        "variables": [
            {"name": "svc", "kind": "query", "query": "SELECT DISTINCT service_name FROM opentelemetry_traces ORDER BY 1", "multi": true},
            {"name": "kind", "kind": "custom", "options": ["SPAN_KIND_SERVER", "SPAN_KIND_CLIENT"], "default": ["SPAN_KIND_SERVER"]}
        ],
        "tabs": [{"title": "T", "panels": [
            {"id": "count", "type": "stat", "title": "Spans", "query": {"kind": "sql", "text": "SELECT count(*) AS n FROM opentelemetry_traces WHERE service_name IN ($svc) AND span_kind = $kind"}},
            {"id": "by", "type": "pie", "title": "By service", "query": {"kind": "sql", "text": "SELECT service_name, count(*) AS n FROM opentelemetry_traces GROUP BY 1 ORDER BY 1"}},
            {"id": "tl", "type": "line", "title": "Spans per minute", "query": {"kind": "sql", "text": "SELECT CAST(date_bin('1 minute'::INTERVAL, timestamp) AS BIGINT) / 1000000 AS t, count(*) AS n FROM opentelemetry_traces WHERE timestamp >= $__from AND timestamp < $__to GROUP BY 1 ORDER BY 1"}}
        ]}]
    });
    let r = send_json(
        a,
        Method::POST,
        &format!("{BASE}/dashboards"),
        &json!({"title": "Live dash", "folder_id": fid, "spec": spec}),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let d = r.json()["data"].clone();
    let did = id_of(&d);
    // Reload: the stored spec is exactly the canonical one.
    let got = get(a, &format!("{BASE}/dashboards/{did}")).await.json()["data"].clone();
    assert_eq!(got["spec"], d["spec"]);
    assert_eq!(
        (
            got["title"].as_str().unwrap(),
            got["folder_id"].as_str().unwrap()
        ),
        ("Live dash", fid.as_str())
    );
    // Update in place (same key, same ts): still one row, new title.
    let r = send_json(
        a,
        Method::PUT,
        &format!("{BASE}/dashboards/{did}"),
        &json!({"title": "Renamed"}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let list = get(a, &format!("{BASE}/dashboards")).await.json()["data"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["title"], "Renamed");
    // Variable options come from the data.
    let services: HashSet<&str> = l.batch.spans.iter().map(|s| s.service).collect();
    let vars = get(
        a,
        &format!("{BASE}/dashboards/{did}/variables?{}", Live::window()),
    )
    .await
    .json()["data"]
        .clone();
    let opts: HashSet<String> = vars[0]["options"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o.as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        opts,
        services
            .iter()
            .map(|s| s.to_string())
            .collect::<HashSet<_>>()
    );
    // Panels return exact numbers, with the multi-select bound as parameters.
    let w = Live::window();
    let count = |svcs: &[&str]| {
        let q: String = svcs
            .iter()
            .map(|s| format!("&var-svc={}", enc(s)))
            .collect();
        format!("{BASE}/dashboards/{did}/panels/count/data?{w}{q}")
    };
    let want = |svcs: &[&str]| {
        l.batch
            .spans
            .iter()
            .filter(|s| svcs.contains(&s.service) && s.kind == synth::KIND_SERVER)
            .count() as i64
    };
    for svcs in [
        vec!["cart"],
        vec!["cart", "payment"],
        vec!["frontend", "cart", "checkout"],
    ] {
        let r = get(a, &count(&svcs)).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
        assert_eq!(
            r.json()["data"]["rows"][0]["n"].as_i64().unwrap(),
            want(&svcs),
            "{svcs:?}"
        );
    }
    // Hostile values are plain values: nothing matches, nothing breaks.
    let r = get(a, &count(&["x' OR '1'='1", "cart"])).await;
    assert_eq!(
        r.json()["data"]["rows"][0]["n"].as_i64().unwrap(),
        want(&["cart"])
    );
    let r = get(a, &count(&["'; DROP TABLE opentelemetry_traces; --"])).await;
    assert_eq!(r.json()["data"]["rows"][0]["n"], 0);
    // The traces table is intact.
    let r = get(a, &count(&["frontend"])).await;
    assert!(r.json()["data"]["rows"][0]["n"].as_i64().unwrap() > 0);
    // A pie reads categories, a line reads a time series, both pass their shape checks.
    let pie = get(a, &format!("{BASE}/dashboards/{did}/panels/by/data?{w}"))
        .await
        .json()["data"]
        .clone();
    assert_eq!(pie["shape_error"], Value::Null);
    assert_eq!(pie["rows"].as_array().unwrap().len(), services.len());
    let tl = get(
        a,
        &format!("{BASE}/dashboards/{did}/panels/tl/data?{w}&compare=1d"),
    )
    .await
    .json()["data"]
        .clone();
    assert_eq!(tl["shape_error"], Value::Null, "{tl}");
    let total: i64 = tl["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["n"].as_i64().unwrap())
        .sum();
    assert_eq!(total, l.batch.spans.len() as i64);
    assert!(
        tl["compare"]["rows"].as_array().unwrap().is_empty(),
        "nothing a day earlier"
    );
    // A public link shows the same numbers without the query text.
    let sh = send_json(
        a,
        Method::POST,
        &format!("{BASE}/dashboards/{did}/shares"),
        &json!({"ttl_hours": 1}),
    )
    .await
    .json()["data"]
        .clone();
    let token = sh["token"].as_str().unwrap();
    let r = get(
        a,
        &format!("{BASE}/public/{token}/panels/count/data?{w}&var-svc=cart"),
    )
    .await;
    assert_eq!(
        r.json()["data"]["rows"][0]["n"].as_i64().unwrap(),
        want(&["cart"])
    );
    assert!(
        !get(a, &format!("{BASE}/public/{token}"))
            .await
            .text()
            .contains("opentelemetry_traces")
    );
    // Folder delete cascades in the real store, and the dashboard stays gone after a reload.
    let r = send(a, Method::DELETE, &format!("{BASE}/folders/{fid}")).await;
    assert_eq!(r.json()["data"]["dashboards_deleted"], 1, "{}", r.text());
    assert_eq!(
        get(a, &format!("{BASE}/dashboards/{did}")).await.status,
        StatusCode::NOT_FOUND
    );
    assert!(
        get(a, &format!("{BASE}/folders")).await.json()["data"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    // Export/import against the real store.
    let d2 = id_of(
        &send_json(
            a,
            Method::POST,
            &format!("{BASE}/dashboards"),
            &json!({"title": "Two", "spec": spec}),
        )
        .await
        .json()["data"],
    );
    let ex = get(a, &format!("{BASE}/dashboards/{d2}/export"))
        .await
        .json()["data"]
        .clone();
    let r = send_text(
        a,
        Method::POST,
        &format!("{BASE}/dashboards/import"),
        &ex.to_string(),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    assert_eq!(
        get(a, &format!("{BASE}/dashboards")).await.json()["data"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let _ = build::folder_list; // the builders are what the store tests above exercise
    l.drop().await;
}

#[allow(dead_code)]
fn unused(_: &Config) {}
