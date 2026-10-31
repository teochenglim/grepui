//! AC0.14.1, AC0.14.2: trace table layouts (flat v1 and JSON2 v2) and the Connect page for LLM
//! SDKs. Live tests send OpenInference-shaped spans to a real trace v2 table.
mod common;
use axum::http::StatusCode;
use common::*;
use grepui::{
    api::{AppState, router},
    config::Config,
    layout,
    playground::{self, http, synth},
    query::{
        Bind, SqlxReader, Window,
        filter::parse,
        traces::{self, Layout, RCond, TCol, TraceFilter},
    },
};
use serde_json::{Value, json};
use std::{collections::HashSet, sync::Arc, time::Duration};

const BASE: &str = "/api/v1/orgs/default";
const W: &str = "from=1793398000000&to=1793402000000";

fn flat_columns() -> Vec<String> {
    [
        "timestamp",
        "timestamp_end",
        "trace_id",
        "span_id",
        "parent_span_id",
        "span_name",
        "service_name",
        "span_attributes.http.route",
        "span_attributes.llm.model_name",
        "resource_attributes.service.name",
    ]
    .map(String::from)
    .to_vec()
}

fn json_columns() -> Vec<String> {
    [
        "timestamp",
        "timestamp_end",
        "duration_nano",
        "trace_id",
        "span_id",
        "parent_span_id",
        "span_name",
        "service_name",
        "span_attributes",
        "scope_attributes",
        "resource_attributes",
        "span_events",
        "span_links",
    ]
    .map(String::from)
    .to_vec()
}

fn schema(cols: &[String]) -> Vec<Value> {
    cols.iter()
        .map(|c| json!({"column_name": c, "data_type": "string", "semantic_type": "FIELD"}))
        .collect()
}

fn w() -> Window {
    Window {
        from_ms: 1_000,
        to_ms: 9_000,
        limit: 50,
    }
}

// ---- AC0.14.1 --------------------------------------------------------------------------------

#[test]
fn ac_0_14_1_layout_detected() {
    assert_eq!(Layout::detect(&flat_columns()), Layout::Flat);
    assert_eq!(Layout::detect(&json_columns()), Layout::Json);
    // Only a column named exactly `span_attributes` makes a table JSON; flat attribute columns do not.
    assert_eq!(
        Layout::detect(&["span_attributes.x".to_string()]),
        Layout::Flat
    );
    assert_eq!(Layout::detect(&[]), Layout::Flat);
}

#[tokio::test]
async fn ac_0_14_1_layout_endpoint() {
    for (cols, want) in [(flat_columns(), "flat"), (json_columns(), "json")] {
        let a = app(Fake::new(move |q| {
            Ok(match q.name {
                "stream_schema" => schema(&cols),
                _ => sample_db(q)?,
            })
        }));
        let r = get(&a, &format!("{BASE}/traces/layout")).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
        assert_eq!(r.json()["data"]["layout"], want);
        assert_eq!(r.json()["data"]["table"], "opentelemetry_traces");
    }
}

#[test]
fn ac_0_14_1_json_filter_binds() {
    let cond = |q: &str, cols: &[String]| traces::resolve(&parse(q).unwrap(), cols);
    // On a JSON table any attribute can be asked for, and the key is data, not SQL.
    let c = cond(
        "llm.model_name=gpt-4o resource.openinference.project.name=support-bot",
        &json_columns(),
    )
    .unwrap();
    assert_eq!(
        c[0].col,
        TCol::Json {
            col: "span_attributes",
            key: "llm.model_name".into()
        }
    );
    assert_eq!(
        c[1].col,
        TCol::Json {
            col: "resource_attributes",
            key: "openinference.project.name".into()
        }
    );
    assert_eq!(
        cond("attr.tool.name=x", &json_columns()).unwrap()[0].col,
        TCol::Json {
            col: "span_attributes",
            key: "tool.name".into()
        }
    );
    assert_eq!(
        cond("span_attributes.k=v", &json_columns()).unwrap()[0].col,
        TCol::Json {
            col: "span_attributes",
            key: "k".into()
        }
    );
    // The same key on a flat table is only taken if it is a real column.
    assert!(cond("llm.model_name=gpt-4o", &flat_columns()).is_ok());
    assert!(
        cond("tool.name=x", &flat_columns()).is_err(),
        "flat tables still refuse unknown attributes"
    );
    // Plain columns are the same on both.
    assert_eq!(
        cond("service=cart", &json_columns()).unwrap()[0].col,
        TCol::Plain("service_name")
    );
    // The SQL: path and value are binds in order, whatever the key holds.
    let evil = RCond {
        col: TCol::Json {
            col: "span_attributes",
            key: "a\"]'; DROP TABLE t; --".into(),
        },
        op: grepui::query::filter::Op::Eq,
        value: "v".into(),
    };
    for (op, frag) in [
        (
            grepui::query::filter::Op::Eq,
            "json_get_string(`span_attributes`, ?) = ?",
        ),
        (
            grepui::query::filter::Op::Ne,
            "COALESCE(json_get_string(`span_attributes`, ?), '') != ?",
        ),
        (
            grepui::query::filter::Op::Like,
            "json_get_string(`span_attributes`, ?) LIKE ?",
        ),
    ] {
        let f = TraceFilter {
            conds: vec![RCond { op, ..evil.clone() }],
            ..Default::default()
        };
        for q in [
            traces::list("db", "t", &f, w(), 0).unwrap(),
            traces::histogram("db", "t", &f, w()).unwrap(),
        ] {
            assert!(q.sql.contains(frag), "{}", q.sql);
            assert!(!q.sql.contains("DROP"), "{}", q.sql);
            assert_eq!(q.sql.matches('?').count(), q.binds.len(), "{}", q.sql);
            // The escaped path is a bind, ahead of the value.
            assert!(
                q.binds
                    .iter()
                    .any(|b| matches!(b, Bind::Str(s) if s == "[\"a\\\"]'; DROP TABLE t; --\"]")),
                "{:?}",
                q.binds
            );
        }
    }
    // Keys that cannot travel as a path are refused.
    assert!(cond(&format!("{}=x", "k".repeat(300)), &json_columns()).is_err());
}

#[test]
fn ac_0_14_1_span_attributes_json() {
    // A row from a v2 table: attributes are objects keyed by the dotted name.
    let long = "p".repeat(100_000);
    let raw = json!({
        "span_id": "e3586378d5b65d18", "parent_span_id": "", "service_name": "llm-app", "span_name": "chat",
        "span_kind": "SPAN_KIND_INTERNAL", "span_status_code": "STATUS_CODE_UNSET", "scope_name": "demo",
        "start_ns": "1000000000", "end_ns": "2000000000",
        "span_attributes": {
            "openinference.span.kind": "LLM", "llm.token_count.total": 589, "llm.cost.total": 0.001178,
            "llm.input_messages.0.message.content": long, "gone": null,
        },
        "resource_attributes": {"openinference.project.name": "support-bot"},
        "span_events": [],
    });
    let s = layout::span_from_row(&raw).unwrap();
    let wf = layout::waterfall(&[s]);
    let row = &wf.rows[0];
    assert_eq!(row["attributes"]["openinference.span.kind"], "LLM");
    assert_eq!(
        row["attributes"]["llm.token_count.total"], 589,
        "numbers keep their type"
    );
    assert_eq!(row["attributes"]["llm.cost.total"], 0.001178);
    assert!(
        row["attributes"].get("gone").is_none(),
        "nulls are left out"
    );
    assert_eq!(row["resource"]["openinference.project.name"], "support-bot");
    // A prompt of 100 KB is cut to the cap with a note of how much there was.
    let c = row["attributes"]["llm.input_messages.0.message.content"]
        .as_str()
        .unwrap();
    assert!(
        c.len() < layout::ATTR_VALUE_MAX + 100 && c.contains("100000 bytes in all"),
        "{}",
        c.len()
    );
    // The flat layout still reads its columns.
    let flat = json!({
        "span_id": "a1", "service_name": "x", "span_name": "n", "start_ns": "1", "end_ns": "2",
        "span_attributes.http.route": "/cart", "resource_attributes.service.name": "x",
    });
    let wf = layout::waterfall(&[layout::span_from_row(&flat).unwrap()]);
    assert_eq!(wf.rows[0]["attributes"]["http.route"], "/cart");
    assert_eq!(wf.rows[0]["resource"]["service.name"], "x");
}

#[tokio::test]
async fn ac_0_14_1_api_filters_on_json_table() {
    let fake = Fake::new(|q| {
        Ok(match q.name {
            "stream_schema" => schema(&json_columns()),
            _ => sample_db(q)?,
        })
    });
    let a = app(fake.clone());
    // `tool.name` is not a column of a JSON table and is still accepted: it is read from the JSON.
    let r = get(&a, &format!("{BASE}/traces?{W}&q=tool.name%3Dlookup_order")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let q = fake
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
    assert!(q.binds.contains(&Bind::Str("[\"tool.name\"]".into())));
    assert!(q.binds.contains(&Bind::Str("lookup_order".into())));
    assert!(
        !q.sql.contains("tool.name"),
        "the key is not in the statement"
    );
}

/// A throwaway database with both kinds of table, checked against counts taken from the batches.
async fn live_apps() -> (
    axum::Router,
    axum::Router,
    synth::Batch,
    synth::Batch,
    String,
) {
    let base = std::env::var("GREPUI_TEST_DB")
        .expect("set GREPUI_TEST_DB=mysql://root@127.0.0.1:4002/public");
    let db = format!("demo_it_llm_{}", std::process::id());
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
    let flat = synth::generate(13, 40, now, 30);
    playground::send(
        "http://127.0.0.1:4000",
        &db,
        "opentelemetry_traces",
        "opentelemetry_logs",
        &flat,
    )
    .await
    .expect("seed flat");
    let llm = synth::generate_llm(5, 30, now, 30);
    playground::send_llm_traces(
        "http://127.0.0.1:4000",
        &db,
        "llm_traces",
        synth::LLM_PROJECT,
        &llm,
    )
    .await
    .expect("seed v2");
    let url = base.replace("/public", &format!("/{db}"));
    let mk = |table: &str| {
        let r = Arc::new(SqlxReader::lazy(&url, Duration::from_secs(30)).unwrap());
        let s = AppState::with_parts(
            Config {
                greptime_mysql: url.clone(),
                traces_table: table.into(),
                ..Config::default()
            },
            r.clone(),
            r,
            grepui::api::system_clock(),
        )
        .unwrap();
        router(s)
    };
    (mk("opentelemetry_traces"), mk("llm_traces"), flat, llm, db)
}

fn win_q() -> String {
    let to = chrono::Utc::now().timestamp_millis() + 600_000;
    format!("from={}&to={to}&limit=1000", to - 7_200_000)
}

fn attr<'a>(s: &'a synth::SpanRec, k: &str) -> Option<&'a synth::AttrVal> {
    s.attrs.iter().find(|(n, _)| n == k).map(|(_, v)| v)
}

fn trace_ids(b: &synth::Batch, f: impl Fn(&synth::SpanRec) -> bool) -> HashSet<String> {
    b.spans
        .iter()
        .filter(|s| f(s))
        .map(|s| s.trace_id.iter().map(|x| format!("{x:02x}")).collect())
        .collect()
}

fn is_str(s: &synth::SpanRec, k: &str, v: &str) -> bool {
    matches!(attr(s, k), Some(synth::AttrVal::Str(x)) if x == v)
}

#[tokio::test]
#[ignore = "needs GreptimeDB (make db-up)"]
async fn ac_0_14_1_both_layouts_live() {
    let (flat_app, v2_app, flat, llm, _db) = live_apps().await;
    let q = win_q();
    // Each table is recognised.
    assert_eq!(
        get(&flat_app, &format!("{BASE}/traces/layout"))
            .await
            .json()["data"]["layout"],
        "flat"
    );
    assert_eq!(
        get(&v2_app, &format!("{BASE}/traces/layout")).await.json()["data"]["layout"],
        "json"
    );
    // Flat table: a real attribute column filter, counted independently from the batch.
    let want = trace_ids(&flat, |s| {
        s.service == "payment"
            && matches!(attr(s, "http.status_code"), Some(synth::AttrVal::Int(500)))
    });
    let r = get(
        &flat_app,
        &format!("{BASE}/traces?{q}&q=http.status_code%3D500%20service%3Dpayment"),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let got: HashSet<String> = r.json()["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["trace_id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(got, want, "flat layout filter");
    // JSON table: every kind of attribute filter, against counts from the generated spans.
    let all_traces = trace_ids(&llm, |_| true);
    let list = |query: String| {
        let app = v2_app.clone();
        async move {
            let r = get(&app, &format!("{BASE}/traces?{query}")).await;
            assert_eq!(r.status, StatusCode::OK, "{}", r.text());
            r.json()["data"]
                .as_array()
                .unwrap()
                .iter()
                .map(|t| t["trace_id"].as_str().unwrap().to_string())
                .collect::<HashSet<String>>()
        }
    };
    assert_eq!(list(q.clone()).await, all_traces);
    assert_eq!(
        list(format!("{q}&q=openinference.span.kind%3DTOOL")).await,
        trace_ids(&llm, |s| is_str(s, "openinference.span.kind", "TOOL"))
    );
    assert!(!trace_ids(&llm, |s| is_str(s, "openinference.span.kind", "TOOL")).is_empty());
    for model in ["gpt-4o", "claude-sonnet-4"] {
        assert_eq!(
            list(format!("{q}&q=llm.model_name%3D{model}")).await,
            trace_ids(&llm, |s| is_str(s, "llm.model_name", model)),
            "{model}"
        );
    }
    // A number is matched as its text; `!=` and a substring match work too.
    let first = llm
        .spans
        .iter()
        .find_map(|s| match attr(s, "llm.token_count.total") {
            Some(synth::AttrVal::Int(n)) => Some(*n),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        list(format!("{q}&q=llm.token_count.total%3D{first}")).await,
        trace_ids(
            &llm,
            |s| matches!(attr(s, "llm.token_count.total"), Some(synth::AttrVal::Int(n)) if *n == first)
        )
    );
    assert_eq!(
        list(format!("{q}&q=llm.provider~anthro")).await,
        trace_ids(&llm, |s| is_str(s, "llm.provider", "anthropic"))
    );
    // The project is a resource attribute, and an unknown value matches nothing.
    assert_eq!(
        list(format!(
            "{q}&q=resource.openinference.project.name%3Dsupport-bot"
        ))
        .await,
        all_traces
    );
    assert!(
        list(format!("{q}&q=resource.openinference.project.name%3Dother"))
            .await
            .is_empty()
    );
    // Histogram counts the same traces the list returns.
    let h = get(
        &v2_app,
        &format!("{BASE}/traces/histogram?{q}&q=llm.model_name%3Dgpt-4o"),
    )
    .await;
    assert_eq!(h.status, StatusCode::OK, "{}", h.text());
    assert_eq!(
        h.json()["data"]["total"].as_u64().unwrap() as usize,
        trace_ids(&llm, |s| is_str(s, "llm.model_name", "gpt-4o")).len()
    );
    // Detail: the span panel has the attributes, messages included, and the project.
    let tid = all_traces.iter().next().unwrap().clone();
    let d = get(&v2_app, &format!("{BASE}/traces/{tid}")).await;
    assert_eq!(d.status, StatusCode::OK, "{}", d.text());
    let spans = d.json()["data"]["spans"].as_array().unwrap().clone();
    let chat = spans.iter().find(|s| s["name"] == "chat").unwrap();
    assert_eq!(chat["attributes"]["openinference.span.kind"], "LLM");
    assert!(chat["attributes"]["llm.token_count.total"].is_number());
    assert_eq!(
        chat["attributes"]["llm.input_messages.1.message.role"],
        "user"
    );
    assert!(
        chat["attributes"]["llm.input_messages.1.message.content"]
            .as_str()
            .unwrap()
            .starts_with("Where is order #")
    );
    assert_eq!(
        chat["resource"]["openinference.project.name"],
        synth::LLM_PROJECT
    );
    assert_eq!(
        spans.iter().filter(|s| s["service"] == "llm-app").count(),
        spans.len()
    );
    // The waterfall nests the way the agent ran.
    let root = spans.iter().find(|s| s["name"] == "support-agent").unwrap();
    assert_eq!(root["depth"], 0);
    assert_eq!(chat["depth"], 1);
    // The flame graph and the list columns work on the JSON table as on the flat one.
    assert_eq!(
        get(&v2_app, &format!("{BASE}/traces/{tid}/flame"))
            .await
            .status,
        StatusCode::OK
    );
    let sc = get(&v2_app, &format!("{BASE}/traces/scatter?{q}")).await;
    assert_eq!(sc.status, StatusCode::OK, "{}", sc.text());
    assert!(sc.json()["data"]["points"].as_array().unwrap().len() == all_traces.len());
    // Services and dependencies read span columns only, so they answer on the JSON table too.
    let sv = get(&v2_app, &format!("{BASE}/services?{q}")).await;
    assert_eq!(sv.status, StatusCode::OK, "{}", sv.text());
    let names: Vec<String> = sv.json()["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["service_name"].as_str().unwrap_or_default().to_string())
        .collect();
    assert_eq!(names, vec!["llm-app".to_string()], "{}", sv.text());
    assert_eq!(
        sv.json()["data"][0]["spans"].as_u64().unwrap() as usize,
        llm.spans.len()
    );
    // RED counts server and consumer spans; OpenInference spans are internal, so requests stay 0.
    assert_eq!(sv.json()["data"][0]["requests"], 0);
    let dp = get(&v2_app, &format!("{BASE}/dependencies?{q}")).await;
    assert_eq!(dp.status, StatusCode::OK, "{}", dp.text());
    // A filter on a flat table that names a column it lacks is still refused (no silent empty answer).
    let r = get(&flat_app, &format!("{BASE}/traces?{q}&q=tool.name%3Dx")).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
}

// ---- AC0.14.2 --------------------------------------------------------------------------------

#[tokio::test]
async fn ac_0_14_2_connect_llm_section() {
    let a = app(Fake::new(sample_db));
    let r = get(&a, &format!("{BASE}/connect")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let l = &r.json()["data"]["llm"];
    // The LLM table is its own, named for the org, and the headers select the v2 pipeline.
    assert_eq!(l["table"], "default_llm_traces");
    assert_eq!(
        l["headers"]["x-greptime-pipeline-name"],
        "greptime_trace_v2"
    );
    assert_eq!(
        l["headers"]["x-greptime-trace-table-name"],
        "default_llm_traces"
    );
    assert_eq!(l["headers"]["x-greptime-db-name"], "public");
    // The snippets carry the same headers, the endpoint, and the project attribute grepui reads.
    for k in ["python", "sdk_env", "otel_collector"] {
        let s = l[k].as_str().unwrap();
        assert!(
            s.contains("greptime_trace_v2") && s.contains("default_llm_traces"),
            "{k}: {s}"
        );
        assert!(s.contains("/v1/otlp/v1/traces"), "{k}");
    }
    let py = l["python"].as_str().unwrap();
    assert!(
        py.contains("OpenAIInstrumentor")
            && py.contains("openinference.project.name")
            && py.contains("OTLPSpanExporter")
    );
    assert!(
        l["sdk_env"]
            .as_str()
            .unwrap()
            .contains("OTEL_RESOURCE_ATTRIBUTES")
    );
    // The notes say what a user needs to know before sending prompts.
    let notes = l["notes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n.as_str().unwrap().to_lowercase())
        .collect::<Vec<_>>()
        .join("\n");
    for want in [
        "v1.3.0",
        "own table",
        "not migrated",
        "openinference.project.name",
        "ttl",
        "redaction",
        "32 kib",
    ] {
        assert!(notes.contains(want), "missing {want:?}: {notes}");
    }
    // The old traces entry is unchanged: flat tables keep working.
    assert!(
        r.json()["data"]["endpoints"][0]["headers"]["x-greptime-pipeline-name"]
            == "greptime_trace_v1"
    );
}

#[test]
fn ac_0_14_2_snippets_are_well_formed() {
    let l = grepui::api::connect::llm("http://db:4000", "prod", "acme");
    let py = l["python"].as_str().unwrap();
    // Valid Python layout: a balanced dict, every line either a statement or a continuation.
    assert_eq!(py.matches('{').count(), py.matches('}').count());
    assert_eq!(py.matches('(').count(), py.matches(')').count());
    assert!(py.contains("endpoint=\"http://db:4000/v1/otlp/v1/traces\""));
    assert!(py.contains("\"x-greptime-db-name\": \"prod\""));
    assert!(py.contains("\"x-greptime-trace-table-name\": \"acme_llm_traces\""));
    let col = l["otel_collector"].as_str().unwrap();
    assert!(
        col.contains("encoding: proto")
            && col.contains("traces_endpoint: http://db:4000/v1/otlp/v1/traces")
    );
    // The table name derived from the org is a valid identifier for any valid org name.
    for org in ["a", "my-org_1", "x".repeat(32).as_str()] {
        let t = grepui::api::connect::llm("http://h", "d", org)["table"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(grepui::query::ident(&t).is_ok(), "{t}");
    }
    let _ = json!({});
}

/// What the Connect page tells an SDK to send is accepted by GreptimeDB, and what arrives reads back.
#[tokio::test]
#[ignore = "needs GreptimeDB (make db-up)"]
async fn ac_0_14_2_connect_headers_accepted_live() {
    let db = format!("demo_it_conn_{}", std::process::id());
    let form = [("content-type", "application/x-www-form-urlencoded")];
    http::post(
        "http://127.0.0.1:4000",
        "/v1/sql",
        &form,
        format!("sql=CREATE DATABASE IF NOT EXISTS {db}").as_bytes(),
    )
    .await
    .unwrap();
    let l = grepui::api::connect::llm("http://127.0.0.1:4000", &db, "it");
    let headers: Vec<(String, String)> = l["headers"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(k, v)| (k.clone(), v.as_str().unwrap().to_string()))
        .collect();
    let now = chrono::Utc::now().timestamp_nanos_opt().unwrap() as u64;
    let batch = synth::generate_llm(9, 5, now, 5);
    let body = playground::proto::encode_spans(
        &batch,
        &[(
            "openinference.project.name",
            synth::AttrVal::Str("p".into()),
        )],
    );
    let mut hs: Vec<(&str, &str)> = vec![("content-type", "application/x-protobuf")];
    hs.extend(headers.iter().map(|(k, v)| (k.as_str(), v.as_str())));
    let (code, text) = http::post("http://127.0.0.1:4000", "/v1/otlp/v1/traces", &hs, &body)
        .await
        .unwrap();
    assert_eq!(code, 200, "{text}");
    let table = l["table"].as_str().unwrap();
    let (c, t) = http::post(
        "http://127.0.0.1:4000",
        &format!("/v1/sql?db={db}"),
        &form,
        format!("sql=SELECT count(*) FROM {table}").as_bytes(),
    )
    .await
    .unwrap();
    assert_eq!(c, 200, "{t}");
    assert!(t.contains(&format!("[[{}]]", batch.spans.len())), "{t}");
    // The documented flat-table header would have been refused for the same table (a v1 pipeline on a v2 table).
    let mut bad = hs.clone();
    for h in bad.iter_mut() {
        if h.0 == "x-greptime-pipeline-name" {
            h.1 = "greptime_trace_v1";
        }
    }
    let (code, _) = http::post("http://127.0.0.1:4000", "/v1/otlp/v1/traces", &bad, &body)
        .await
        .unwrap();
    assert_ne!(code, 200, "a v1 write into a v2 table is refused");
}
