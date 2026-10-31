//! AC0.15.1 to AC0.15.5: the normalised LLM view of a span, prices, evaluations and feedback, the
//! agent graph, and content protection. Offline tests use a v2-shaped span set behind `MemStore`;
//! the live test sends a generated OpenInference batch to a real trace v2 table.
mod common;
use axum::{Router, http::Method, http::StatusCode};
use common::*;
use grepui::{
    api::{AppState, redaction, router},
    auth::{Access, Role, access_for},
    config::Config,
    llm::Mode,
    playground::{self, http, synth},
    query::SqlxReader,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeSet, HashMap},
    sync::{Arc, atomic::AtomicI64},
    time::Duration,
};

const BASE: &str = "/api/v1/orgs/default";
const W: &str = "from=1793398000000&to=1793402000000";
const SECRET: &str = "secret prompt text";

fn jrow(
    id: &str,
    parent: Option<&str>,
    name: &str,
    status: &str,
    start_ms: i64,
    end_ms: i64,
    attrs: Value,
) -> Value {
    let (s, e) = (T_NS + start_ms * 1_000_000, T_NS + end_ms * 1_000_000);
    json!({
        "timestamp": s / 1_000_000, "start_ns": s.to_string(), "end_ns": e.to_string(), "trace_id": TID, "span_id": id,
        "parent_span_id": parent, "service_name": "llm-app", "span_name": name, "span_kind": "SPAN_KIND_INTERNAL",
        "span_status_code": status, "span_status_message": "", "scope_name": "demo",
        "span_attributes": attrs, "resource_attributes": {"openinference.project.name": "support-bot"},
        "span_events": [], "span_links": [],
    })
}

const OK: &str = "STATUS_CODE_OK";
const ERR: &str = "STATUS_CODE_ERROR";

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

/// An agent run: agent -> llm (tool call) -> tool (error); a second, GenAI-only model call with a
/// reported cost; an embedding with a vector; an evaluator span with an annotation and an event.
fn sample_rows(long: Option<&str>) -> Vec<Value> {
    let mut evaluator = jrow(
        "a000000000000006",
        Some("a000000000000001"),
        "judge",
        OK,
        92,
        95,
        json!({
        "openinference.span.kind": "EVALUATOR", "annotation.name": "correctness", "annotation.label": "correct",
        "annotation.score": 1, "annotation.explanation": "matches the order system"}),
    );
    evaluator["span_events"] = json!([{"name": "gen_ai.evaluation.result", "attributes": {
        "gen_ai.evaluation.name": "relevance", "gen_ai.evaluation.score.value": 0.8, "gen_ai.evaluation.score.label": "good"}}]);
    vec![
        jrow(
            "a000000000000001",
            None,
            "support-agent",
            OK,
            0,
            100,
            json!({
            "openinference.span.kind": "AGENT", "agent.name": "support-agent", "session.id": "s1", "user.id": "u1",
            "input.value": "Where is my order?", "output.value": "Shipped."}),
        ),
        jrow(
            "a000000000000002",
            Some("a000000000000001"),
            "chat",
            OK,
            10,
            60,
            {
                let mut a = json!({
                "openinference.span.kind": "LLM", "llm.model_name": "gpt-4o", "llm.provider": "openai",
                "llm.token_count.prompt": 100, "llm.token_count.completion": 20,
                "llm.input_messages.10.message.role": "user", "llm.input_messages.10.message.content": "last",
                "llm.input_messages.2.message.role": "assistant", "llm.input_messages.2.message.content": "ok",
                "llm.input_messages.1.message.role": "user", "llm.input_messages.1.message.content": SECRET,
                "llm.input_messages.0.message.role": "system", "llm.input_messages.0.message.content": "be brief",
                "llm.output_messages.0.message.role": "assistant",
                "llm.output_messages.0.message.tool_calls.0.tool_call.function.name": "lookup",
                "llm.output_messages.0.message.tool_calls.0.tool_call.function.arguments": "{\"order\": 7}"});
                if let Some(l) = long {
                    a["llm.input_messages.3.message.content"] = json!(l);
                }
                a
            },
        ),
        jrow(
            "a000000000000003",
            Some("a000000000000002"),
            "lookup",
            ERR,
            20,
            30,
            json!({
            "openinference.span.kind": "TOOL", "tool.name": "lookup", "tool.parameters": "{\"order\": 7}", "output.value": "not found"}),
        ),
        jrow(
            "a000000000000004",
            Some("a000000000000001"),
            "chat",
            OK,
            62,
            80,
            json!({
            "gen_ai.operation.name": "chat", "gen_ai.request.model": "claude-x", "gen_ai.provider.name": "anthropic",
            "gen_ai.usage.input_tokens": 10, "gen_ai.usage.output_tokens": 5, "llm.cost.total": 0.25,
            "gen_ai.conversation.id": "s1"}),
        ),
        jrow(
            "a000000000000005",
            Some("a000000000000001"),
            "embed",
            OK,
            81,
            90,
            json!({
            "openinference.span.kind": "EMBEDDING", "embedding.model_name": "te-3",
            "embedding.embeddings.0.embedding.text": "order status",
            "embedding.embeddings.0.embedding.vector": [0.123456, 0.654321, 0.111111]}),
        ),
        evaluator,
    ]
}

struct World {
    app: Router,
    store: Arc<MemStore>,
}

async fn world(rows: Vec<Value>, admin: bool) -> World {
    let store = MemStore::new();
    let schema = json_cols();
    *store.script.lock().unwrap() = Some(Box::new(move |q| {
        Ok(match q.name {
            "trace_spans" => rows.clone(),
            "stream_schema" => schema.clone(),
            _ => sample_db(q)?,
        })
    }));
    let clock = Arc::new(AtomicI64::new(1_793_399_000_000));
    World {
        app: app_admin(&store, &clock, admin),
        store,
    }
}

async fn detail(w: &World) -> Value {
    let r = get(&w.app, &format!("{BASE}/traces/{TID}?{W}")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r.json()["data"].clone()
}

fn span<'a>(d: &'a Value, id: &str) -> &'a Value {
    d["spans"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["span_id"] == id)
        .unwrap_or_else(|| panic!("no span {id}"))
}

// ---- AC0.15.1 ---------------------------------------------------------------------------------

#[tokio::test]
async fn ac_0_15_1_indexed_messages_in_order() {
    let w = world(sample_rows(None), true).await;
    let d = detail(&w).await;
    let llm = &span(&d, "a000000000000002")["llm"];
    let input = llm["messages"]["input"].as_array().unwrap();
    let roles: Vec<&str> = input.iter().map(|m| m["role"].as_str().unwrap()).collect();
    assert_eq!(
        roles,
        ["system", "user", "assistant", "user"],
        "index 10 comes after 2, not after 1"
    );
    assert_eq!(input[1]["content"], SECRET);
    assert_eq!(
        llm["messages"]["output"][0]["tool_calls"][0]["name"],
        "lookup"
    );
    assert_eq!(llm["kind"], "LLM");
    assert_eq!(llm["model"], "gpt-4o");
    assert_eq!(llm["tokens"]["total"], 120);
}

#[tokio::test]
async fn ac_0_15_1_genai_fills_gaps() {
    let w = world(sample_rows(None), true).await;
    let d = detail(&w).await;
    let llm = &span(&d, "a000000000000004")["llm"];
    assert_eq!(
        llm["kind"], "LLM",
        "the kind comes from gen_ai.operation.name"
    );
    assert_eq!(llm["model"], "claude-x");
    assert_eq!(llm["provider"], "anthropic");
    assert_eq!(llm["tokens"]["total"], 15);
    assert_eq!(
        llm["session"], "s1",
        "gen_ai.conversation.id is the session"
    );
}

#[tokio::test]
async fn ac_0_15_1_embeddings_without_vectors() {
    let w = world(sample_rows(None), true).await;
    let text = detail(&w).await.to_string();
    assert!(
        !text.contains("0.654321"),
        "vector values never reach the browser"
    );
    let d = detail(&w).await;
    let e = span(&d, "a000000000000005");
    assert_eq!(e["llm"]["embeddings"][0]["dims"], 3);
    assert_eq!(e["llm"]["embeddings"][0]["text"], "order status");
    assert_eq!(
        e["attributes"]["embedding.embeddings.0.embedding.vector"],
        "[vector, 3 dimensions]"
    );
}

#[tokio::test]
async fn ac_0_15_1_price_estimate_not_override() {
    let w = world(sample_rows(None), true).await;
    // No price yet: the span without a reported cost has none; the reported one stays.
    let d = detail(&w).await;
    assert!(span(&d, "a000000000000002")["llm"]["cost"]["total"].is_null());
    assert_eq!(span(&d, "a000000000000004")["llm"]["cost"]["total"], 0.25);
    for model in ["gpt-4o", "claude-x"] {
        let r = send_json(
            &w.app,
            Method::PUT,
            &format!("{BASE}/llm/prices"),
            &json!({"model": model, "prompt_per_1m": 2.0, "completion_per_1m": 4.0}),
        )
        .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    }
    let d = detail(&w).await;
    let est = &span(&d, "a000000000000002")["llm"]["cost"];
    assert!(
        (est["total"].as_f64().unwrap() - 0.00028).abs() < 1e-12,
        "{est}"
    );
    assert_eq!(est["estimated"], true);
    let rep = &span(&d, "a000000000000004")["llm"]["cost"];
    assert_eq!(
        rep["total"], 0.25,
        "a reported cost is never replaced by a price"
    );
    assert!(rep["estimated"].is_null());
    // Trace summary: the sum of both, one flag saying part of it was estimated.
    let sum = &d["trace"]["llm"];
    assert!(
        (sum["cost"].as_f64().unwrap() - 0.25028).abs() < 1e-9,
        "{sum}"
    );
    assert_eq!(sum["cost_estimated"], true);
}

#[tokio::test]
async fn ac_0_15_1_prices_crud_admin_only() {
    let w = world(sample_rows(None), true).await;
    let put = |m: Value| {
        let app = w.app.clone();
        async move { send_json(&app, Method::PUT, &format!("{BASE}/llm/prices"), &m).await }
    };
    assert_eq!(
        put(json!({"model": "m", "prompt_per_1m": 1.0, "completion_per_1m": 2.0}))
            .await
            .status,
        StatusCode::OK
    );
    let list = get(&w.app, &format!("{BASE}/llm/prices")).await.json();
    assert_eq!(list["data"][0]["model"], "m");
    assert_eq!(list["data"][0]["completion_per_1m"], 2.0);
    // Bad input is named, not stored.
    for bad in [
        json!({"model": "m", "prompt_per_1m": -1.0, "completion_per_1m": 2.0}),
        json!({"model": "", "prompt_per_1m": 1.0, "completion_per_1m": 2.0}),
        json!({"model": "a\nb", "prompt_per_1m": 1.0, "completion_per_1m": 2.0}),
        json!({"model": "m", "prompt_per_1m": 1e12, "completion_per_1m": 2.0}),
    ] {
        assert_eq!(put(bad).await.status, StatusCode::BAD_REQUEST);
    }
    let r = send(
        &w.app,
        Method::DELETE,
        &format!("{BASE}/llm/prices?model=m"),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(
        get(&w.app, &format!("{BASE}/llm/prices")).await.json()["data"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    assert_eq!(
        send(
            &w.app,
            Method::DELETE,
            &format!("{BASE}/llm/prices?model=m")
        )
        .await
        .status,
        StatusCode::NOT_FOUND
    );
    // Who may do what: everyone reads, only an org admin writes (also the content setting).
    let t = "/api/v1/orgs/{org}/llm/prices";
    assert_eq!(access_for(&Method::GET, t), Access::Org(Role::Viewer));
    assert_eq!(access_for(&Method::PUT, t), Access::Org(Role::Admin));
    assert_eq!(access_for(&Method::DELETE, t), Access::Org(Role::Admin));
    assert_eq!(
        access_for(&Method::PUT, "/api/v1/orgs/{org}/llm/content"),
        Access::Org(Role::Admin)
    );
}

// ---- AC0.15.2 ---------------------------------------------------------------------------------

#[tokio::test]
async fn ac_0_15_2_detail_has_llm_view() {
    let w = world(sample_rows(None), true).await;
    let d = detail(&w).await;
    let kinds: BTreeSet<&str> = d["spans"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|s| s["llm"]["kind"].as_str())
        .collect();
    assert_eq!(
        kinds,
        BTreeSet::from(["AGENT", "EMBEDDING", "EVALUATOR", "LLM", "TOOL"])
    );
    let tool = &span(&d, "a000000000000003")["llm"]["tool"];
    assert_eq!(tool["name"], "lookup");
    assert_eq!(tool["arguments"], "{\"order\": 7}");
    assert_eq!(span(&d, "a000000000000001")["llm"]["session"], "s1");
    assert_eq!(
        span(&d, "a000000000000001")["llm"]["input"]["value"],
        "Where is my order?"
    );
    // A trace with no LLM span has no summary and no view.
    let plain = world(
        vec![jrow(
            "a000000000000009",
            None,
            "GET /",
            OK,
            0,
            5,
            json!({"http.route": "/"}),
        )],
        true,
    )
    .await;
    let d = detail(&plain).await;
    assert!(d["trace"]["llm"].is_null() && d["spans"][0].get("llm").is_none());
}

#[tokio::test]
async fn ac_0_15_2_summary_counts_calls_once() {
    let mut rows = sample_rows(None);
    // A parent span that rolls up its child's tokens must not be counted again.
    rows[0]["span_attributes"]["llm.token_count.prompt"] = json!(999);
    let w = world(rows, true).await;
    let s = detail(&w).await["trace"]["llm"].clone();
    assert_eq!(s["llm_calls"], 2);
    assert_eq!(s["prompt_tokens"], 110);
    assert_eq!(s["completion_tokens"], 25);
    assert_eq!(s["total_tokens"], 135);
    let models: BTreeSet<&str> = s["models"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m.as_str().unwrap())
        .collect();
    assert_eq!(models, BTreeSet::from(["claude-x", "gpt-4o"]));
    assert_eq!(s["tools"][0], "lookup");
    assert_eq!(s["session"], "s1");
    assert_eq!(s["user"], "u1");
}

#[tokio::test]
async fn ac_0_15_2_kind_filter_binds() {
    let store = MemStore::new();
    let schema = json_cols();
    *store.script.lock().unwrap() = Some(Box::new(move |q| {
        Ok(match q.name {
            "stream_schema" => schema.clone(),
            _ => sample_db(q)?,
        })
    }));
    let clock = Arc::new(AtomicI64::new(1_793_399_000_000));
    let app = app_admin(&store, &clock, true);
    let r = get(
        &app,
        &format!("{BASE}/traces?{W}&q=openinference.span.kind%3DTOOL"),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let q = store
        .seen
        .lock()
        .unwrap()
        .iter()
        .find(|q| q.name == "traces_list")
        .cloned()
        .unwrap();
    assert!(
        q.sql.contains("json_get_string(`span_attributes`, ?)"),
        "{}",
        q.sql
    );
    assert!(
        !q.sql.contains("openinference"),
        "the key is a bind, not SQL text"
    );
    assert!(
        q.binds
            .iter()
            .any(|b| b.show().contains("openinference.span.kind"))
    );
    assert!(q.binds.iter().any(|b| b.show() == "TOOL"));
}

#[tokio::test]
async fn ac_0_15_2_long_value_capped() {
    let long = "x".repeat(200_000);
    let w = world(sample_rows(Some(&long)), true).await;
    let d = detail(&w).await;
    let s = span(&d, "a000000000000002");
    let raw = s["attributes"]["llm.input_messages.3.message.content"]
        .as_str()
        .unwrap();
    assert!(
        raw.len() < 33_000 && raw.contains("200000 bytes in all"),
        "{}",
        raw.len()
    );
    let msg = s["llm"]["messages"]["input"][3]["content"]
        .as_str()
        .unwrap();
    assert!(
        msg.len() < 33_000,
        "the normalised view is capped too: {}",
        msg.len()
    );
    assert!(d.to_string().len() < 200_000, "the response stays small");
}

// ---- AC0.15.3 ---------------------------------------------------------------------------------

#[tokio::test]
async fn ac_0_15_3_evaluations_from_attributes() {
    let w = world(sample_rows(None), true).await;
    let d = detail(&w).await;
    let ev = span(&d, "a000000000000006")["llm"]["evaluations"]
        .as_array()
        .unwrap()
        .clone();
    let by: HashMap<&str, &Value> = ev
        .iter()
        .map(|e| (e["name"].as_str().unwrap(), e))
        .collect();
    assert_eq!(by["correctness"]["label"], "correct");
    assert_eq!(by["correctness"]["score"], 1.0);
    assert_eq!(by["correctness"]["explanation"], "matches the order system");
}

#[tokio::test]
async fn ac_0_15_3_genai_evaluation_event() {
    let w = world(sample_rows(None), true).await;
    let d = detail(&w).await;
    let ev = span(&d, "a000000000000006")["llm"]["evaluations"]
        .as_array()
        .unwrap()
        .clone();
    let e = ev
        .iter()
        .find(|e| e["name"] == "relevance")
        .expect("event evaluation");
    assert_eq!(e["score"], 0.8);
    assert_eq!(e["label"], "good");
    assert_eq!(e["source"], "event");
}

#[tokio::test]
async fn ac_0_15_3_feedback_roundtrip() {
    let w = world(sample_rows(None), true).await;
    let url = format!("{BASE}/llm/feedback");
    let put = |b: Value| {
        let (app, url) = (w.app.clone(), url.clone());
        async move { send_json(&app, Method::PUT, &url, &b).await }
    };
    let r = put(json!({"trace_id": TID, "rating": "down", "note": "wrong order number", "label": "hallucination"})).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    // The same person on the same target updates instead of adding.
    put(json!({"trace_id": TID, "rating": "up"})).await;
    put(json!({"trace_id": TID, "span_id": "a000000000000002", "score": 0.5})).await;
    let all = get(&w.app, &format!("{url}?trace={TID}")).await.json()["data"].clone();
    assert_eq!(all.as_array().unwrap().len(), 2, "{all}");
    let one = get(&w.app, &format!("{url}?trace={TID}&span=a000000000000002"))
        .await
        .json()["data"]
        .clone();
    assert_eq!(one.as_array().unwrap().len(), 1);
    assert_eq!(one[0]["score"], 0.5);
    let trace_level = all
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["span_id"].is_null())
        .unwrap();
    assert_eq!(trace_level["rating"], "up");
    // Bad input is refused; delete is soft and only touches one's own.
    for bad in [
        json!({"trace_id": "zz"}),
        json!({"trace_id": TID}),
        json!({"trace_id": TID, "rating": "meh"}),
        json!({"trace_id": TID, "note": "n".repeat(2001)}),
        json!({"trace_id": TID, "score": 1e12}),
    ] {
        assert_eq!(put(bad).await.status, StatusCode::BAD_REQUEST);
    }
    let r = send(&w.app, Method::DELETE, &format!("{url}?trace={TID}")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(
        get(&w.app, &format!("{url}?trace={TID}")).await.json()["data"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        send(&w.app, Method::DELETE, &format!("{url}?trace={TID}"))
            .await
            .status,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn ac_0_15_3_feedback_roles() {
    let t = "/api/v1/orgs/{org}/llm/feedback";
    assert_eq!(access_for(&Method::GET, t), Access::Org(Role::Viewer));
    assert_eq!(
        access_for(&Method::PUT, t),
        Access::Org(Role::Editor),
        "editors write feedback"
    );
    assert_eq!(access_for(&Method::DELETE, t), Access::Org(Role::Editor));
}

#[tokio::test]
async fn ac_0_15_3_feedback_org_scoped() {
    let w = world(sample_rows(None), true).await;
    send_json(
        &w.app,
        Method::PUT,
        &format!("{BASE}/llm/feedback"),
        &json!({"trace_id": TID, "rating": "up"}),
    )
    .await;
    let items = w.store.items.lock().unwrap();
    let keys: Vec<_> = items
        .keys()
        .filter(|(_, kind, _)| kind == "feedback")
        .collect();
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].0, "default", "feedback rows carry their org");
}

// ---- AC0.15.4 ---------------------------------------------------------------------------------

async fn graph(w: &World) -> Value {
    let r = get(&w.app, &format!("{BASE}/traces/{TID}/agent-graph?{W}")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r.json()["data"].clone()
}

fn edge_set(g: &Value) -> BTreeSet<(String, String, u64)> {
    g["edges"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            (
                e["from"].as_str().unwrap().into(),
                e["to"].as_str().unwrap().into(),
                e["calls"].as_u64().unwrap(),
            )
        })
        .collect()
}

#[tokio::test]
async fn ac_0_15_4_graph_from_graph_node_attrs() {
    let w = world(sample_rows(None), true).await;
    let g = graph(&w).await;
    let nodes: BTreeSet<&str> = g["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        nodes,
        BTreeSet::from([
            "agent:support-agent",
            "llm:gpt-4o",
            "llm:claude-x",
            "tool:lookup",
            "embedding:embed",
            "evaluator:judge"
        ])
    );
    let want: BTreeSet<(String, String, u64)> = [
        ("agent:support-agent", "llm:gpt-4o"),
        ("agent:support-agent", "llm:claude-x"),
        ("llm:gpt-4o", "tool:lookup"),
        ("agent:support-agent", "embedding:embed"),
        ("agent:support-agent", "evaluator:judge"),
    ]
    .iter()
    .map(|(a, b)| (a.to_string(), b.to_string(), 1))
    .collect();
    assert_eq!(edge_set(&g), want);
    let tool = g["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["id"] == "tool:lookup")
        .unwrap();
    assert_eq!(tool["errors"], 1);
    // Explicit graph.node.* attributes name the nodes and the parent.
    let rows = vec![
        jrow(
            "a000000000000001",
            None,
            "p",
            OK,
            0,
            10,
            json!({"openinference.span.kind": "AGENT", "graph.node.id": "planner", "graph.node.name": "Planner"}),
        ),
        jrow(
            "a000000000000002",
            Some("a000000000000001"),
            "w",
            OK,
            1,
            9,
            json!({"openinference.span.kind": "AGENT", "graph.node.id": "worker", "graph.node.parent_id": "planner"}),
        ),
    ];
    let g = graph(&world(rows, true).await).await;
    assert_eq!(
        edge_set(&g),
        BTreeSet::from([("planner".to_string(), "worker".to_string(), 1)])
    );
    assert_eq!(
        g["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|n| n["id"] == "planner")
            .unwrap()["label"],
        "Planner"
    );
}

#[tokio::test]
async fn ac_0_15_4_cycle_and_orphan_tolerated() {
    let rows = vec![
        jrow(
            "a000000000000001",
            Some("a000000000000002"),
            "t",
            OK,
            0,
            10,
            json!({"openinference.span.kind": "TOOL", "tool.name": "x"}),
        ),
        jrow(
            "a000000000000002",
            Some("a000000000000001"),
            "t",
            OK,
            1,
            9,
            json!({"openinference.span.kind": "TOOL", "tool.name": "x"}),
        ),
        jrow(
            "a000000000000003",
            Some("ffffffffffffffff"),
            "o",
            OK,
            2,
            8,
            json!({"openinference.span.kind": "AGENT", "agent.name": "lost"}),
        ),
    ];
    let g = tokio::time::timeout(Duration::from_secs(5), graph(&world(rows, true).await))
        .await
        .expect("no hang");
    assert_eq!(g["nodes"].as_array().unwrap().len(), 2);
    assert_eq!(
        g["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|n| n["id"] == "tool:x")
            .unwrap()["calls"],
        2
    );
}

#[test]
fn ac_0_15_4_new_routes_classified() {
    let t = "/api/v1/orgs/{org}/traces/{id}/agent-graph";
    assert!(grepui::api::ROUTES.contains(&t));
    assert_eq!(access_for(&Method::GET, t), Access::Org(Role::Viewer));
    assert_eq!(access_for(&Method::POST, t), Access::Deny);
}

fn tid_hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

#[tokio::test]
#[ignore = "needs GreptimeDB (make db-up)"]
async fn ac_0_15_4_graph_matches_batch_live() {
    let base = std::env::var("GREPUI_TEST_DB")
        .expect("set GREPUI_TEST_DB=mysql://root@127.0.0.1:4002/public");
    let db = format!("demo_it_graph_{}", std::process::id());
    let form = [("content-type", "application/x-www-form-urlencoded")];
    http::post(
        "http://127.0.0.1:4000",
        "/v1/sql",
        &form,
        format!("sql=CREATE DATABASE IF NOT EXISTS {db}").as_bytes(),
    )
    .await
    .unwrap();
    let now = chrono::Utc::now().timestamp_nanos_opt().unwrap() as u64;
    let batch = synth::generate_llm(21, 25, now, 30);
    playground::send_llm_traces(
        "http://127.0.0.1:4000",
        &db,
        "llm_traces",
        synth::LLM_PROJECT,
        &batch,
    )
    .await
    .expect("seed");
    let url = base.replace("/public", &format!("/{db}"));
    let r = Arc::new(SqlxReader::lazy(&url, Duration::from_secs(30)).unwrap());
    let st = AppState::with_parts(
        Config {
            greptime_mysql: url.clone(),
            traces_table: "llm_traces".into(),
            ..Config::default()
        },
        r.clone(),
        r,
        grepui::api::system_clock(),
    )
    .unwrap();
    let app = router(st);
    let to = chrono::Utc::now().timestamp_millis() + 600_000;
    let win = format!("from={}&to={to}", to - 7_200_000);
    // Expected graph of three traces, computed from the generated spans, not from grepui.
    let ids: BTreeSet<[u8; 16]> = batch.spans.iter().map(|s| s.trace_id).take(40).collect();
    for trace in ids.into_iter().take(3) {
        let spans: Vec<&synth::SpanRec> =
            batch.spans.iter().filter(|s| s.trace_id == trace).collect();
        let kind = |s: &synth::SpanRec| -> String {
            match s.attrs.iter().find(|(k, _)| k == "openinference.span.kind") {
                Some((_, synth::AttrVal::Str(v))) => v.clone(),
                _ => String::new(),
            }
        };
        let s_attr = |s: &synth::SpanRec, k: &str| -> Option<String> {
            s.attrs
                .iter()
                .find(|(n, _)| n == k)
                .and_then(|(_, v)| match v {
                    synth::AttrVal::Str(x) => Some(x.clone()),
                    _ => None,
                })
        };
        let node = |s: &synth::SpanRec| -> String {
            match kind(s).as_str() {
                "AGENT" => format!("agent:{}", s_attr(s, "agent.name").unwrap()),
                "LLM" => format!("llm:{}", s_attr(s, "llm.model_name").unwrap()),
                "TOOL" => format!("tool:{}", s_attr(s, "tool.name").unwrap()),
                k => format!("{}:{}", k.to_lowercase(), s.name),
            }
        };
        let mut want: BTreeSet<(String, String)> = BTreeSet::new();
        let mut calls: HashMap<String, u64> = HashMap::new();
        for s in &spans {
            *calls.entry(node(s)).or_default() += 1;
            if let Some(p) = s.parent.and_then(|p| spans.iter().find(|x| x.span_id == p)) {
                want.insert((node(p), node(s)));
            }
        }
        let r = get(
            &app,
            &format!("{BASE}/traces/{}/agent-graph?{win}", tid_hex(&trace)),
        )
        .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
        let g = r.json()["data"].clone();
        let got: BTreeSet<(String, String)> =
            edge_set(&g).into_iter().map(|(a, b, _)| (a, b)).collect();
        assert_eq!(got, want, "edges of trace {}", tid_hex(&trace));
        for n in g["nodes"].as_array().unwrap() {
            assert_eq!(
                n["calls"].as_u64().unwrap(),
                calls[n["id"].as_str().unwrap()],
                "calls of {}",
                n["id"]
            );
        }
        assert_eq!(g["nodes"].as_array().unwrap().len(), calls.len());
    }
}

// ---- AC0.15.5 ---------------------------------------------------------------------------------

#[tokio::test]
async fn ac_0_15_5_viewer_sees_masked_content() {
    let w = world(sample_rows(None), false).await;
    let r = get(&w.app, &format!("{BASE}/traces/{TID}?{W}")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let t = r.text();
    for secret in [
        SECRET,
        "Where is my order?",
        "{\\\"order\\\": 7}",
        "order status",
        "be brief",
    ] {
        assert!(!t.contains(secret), "{secret} leaked: {t}");
    }
    assert!(t.contains("[masked: 18 characters]"), "{t}");
    // What is not content stays: model, tokens, roles, names, ids.
    let d = r.json()["data"].clone();
    let llm = &span(&d, "a000000000000002")["llm"];
    assert_eq!(llm["model"], "gpt-4o");
    assert_eq!(llm["tokens"]["total"], 120);
    assert_eq!(llm["messages"]["input"][1]["role"], "user");
    assert_eq!(
        llm["messages"]["output"][0]["tool_calls"][0]["name"],
        "lookup"
    );
}

#[tokio::test]
async fn ac_0_15_5_admin_sees_content() {
    let w = world(sample_rows(None), true).await;
    let t = get(&w.app, &format!("{BASE}/traces/{TID}?{W}"))
        .await
        .text();
    assert!(t.contains(SECRET) && t.contains("Where is my order?"));
    assert!(!t.contains("[masked"));
}

/// A second app over the same store (a viewer's), with the settings read in.
async fn viewer_over(store: &Arc<MemStore>) -> Router {
    let clock = Arc::new(AtomicI64::new(1_793_399_000_000));
    let c = clock.clone();
    let st = AppState::with_parts(
        Config {
            admin_writes: false,
            ..Config::default()
        },
        store.clone(),
        store.clone(),
        Arc::new(move || c.load(std::sync::atomic::Ordering::SeqCst)),
    )
    .unwrap();
    assert!(redaction::reload_all(&st).await);
    router(st)
}

#[tokio::test]
async fn ac_0_15_5_setting_hidden_masked_visible() {
    let admin = world(sample_rows(None), true).await;
    assert_eq!(
        get(&admin.app, &format!("{BASE}/llm/content")).await.json()["data"]["mode"],
        "masked",
        "masked until an admin says otherwise"
    );
    for (mode, leaks, marker) in [
        ("hidden", false, false),
        ("masked", false, true),
        ("visible", true, false),
    ] {
        let r = send_json(
            &admin.app,
            Method::PUT,
            &format!("{BASE}/llm/content"),
            &json!({"mode": mode}),
        )
        .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
        let viewer = viewer_over(&admin.store).await;
        let t = get(&viewer, &format!("{BASE}/traces/{TID}?{W}"))
            .await
            .text();
        assert_eq!(t.contains(SECRET), leaks, "{mode}");
        assert_eq!(t.contains("[masked"), marker, "{mode}");
        let _ = &t;
    }
    let bad = send_json(
        &admin.app,
        Method::PUT,
        &format!("{BASE}/llm/content"),
        &json!({"mode": "everything"}),
    )
    .await;
    assert!(bad.status.is_client_error());
}

#[test]
fn ac_0_15_5_public_links_always_masked() {
    use grepui::api::llm::effective_mode;
    // An org that shows content to viewers still masks it on a public link, and admins on one.
    assert_eq!(effective_mode(false, Mode::Visible, true), Mode::Masked);
    assert_eq!(effective_mode(false, Mode::Hidden, true), Mode::Hidden);
    assert_eq!(effective_mode(false, Mode::Visible, false), Mode::Visible);
    assert_eq!(effective_mode(true, Mode::Hidden, false), Mode::Visible);
    assert_eq!(effective_mode(true, Mode::Visible, true), Mode::Masked);
}

#[tokio::test]
async fn ac_0_15_5_caps_in_export_and_reports() {
    // The layer covers newline-delimited exports as well as JSON bodies (no per-route code).
    let store = MemStore::new();
    *store.script.lock().unwrap() = Some(Box::new(|q| {
        Ok(match q.name {
            "logs_search" | "logs_export" => vec![
                json!({"timestamp": 1, "body": "x", "log_attributes": {"input.value": SECRET}}),
            ],
            _ => sample_db(q)?,
        })
    }));
    let clock = Arc::new(AtomicI64::new(1_793_399_000_000));
    let app = app_admin(&store, &clock, false);
    let r = get(&app, &format!("{BASE}/logs/export?{W}&format=ndjson")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(!r.text().contains(SECRET), "{}", r.text());
    // CSV is covered too: a cell that holds a JSON attribute map is protected like a JSON body.
    let r = get(&app, &format!("{BASE}/logs/export?{W}&format=csv")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(!r.text().contains(SECRET), "{}", r.text());
    // A report masks rows with the same function, for a reader outside the app.
    let mut rows = json!([{"llm.input_messages.0.message.content": SECRET, "n": 1}]);
    grepui::llm::protect(
        &mut rows,
        grepui::api::llm::effective_mode(false, Mode::Visible, true),
    );
    assert!(!rows.to_string().contains(SECRET) && rows[0]["n"] == 1);
}
