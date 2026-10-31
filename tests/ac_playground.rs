//! AC0.2.9: playground mode (ADR 0005): deterministic demo data, valid OTLP, guarded seeding,
//! and no ingest path in `serve`.
mod common;
use common::*;
use grepui::config::admin_url;
use grepui::playground::{
    has_traces, is_demo_database, plan_seed, proto,
    synth::{self, Batch},
};
use serde_json::json;
use std::{collections::HashMap, fs, path::Path};

const NOW: u64 = 1_793_400_000_000_000_000;

fn batch(seed: u64) -> Batch {
    synth::generate(seed, 120, NOW, 120)
}

// ---- generator ---------------------------------------------------------------

#[test]
fn ac_0_2_9_generator_is_deterministic() {
    assert_eq!(batch(7), batch(7));
    assert_ne!(batch(7), batch(8));
    assert_eq!(
        batch(1).spans.len(),
        batch(1).logs.len(),
        "one log line per span"
    );
    assert!(synth::generate(1, 0, NOW, 60).spans.is_empty());
    // The wire bytes are stable too (same seed, same payload).
    assert_eq!(
        proto::encode_traces(&batch(3)),
        proto::encode_traces(&batch(3))
    );
}

#[test]
fn ac_0_2_9_generated_traces_are_well_formed() {
    let b = batch(7);
    let mut by_trace: HashMap<[u8; 16], Vec<&synth::SpanRec>> = HashMap::new();
    for s in &b.spans {
        by_trace.entry(s.trace_id).or_default().push(s);
    }
    assert_eq!(by_trace.len(), 120);
    for spans in by_trace.values() {
        let roots: Vec<_> = spans.iter().filter(|s| s.parent.is_none()).collect();
        assert_eq!(roots.len(), 1, "exactly one root per trace");
        assert_eq!(roots[0].service, "frontend");
        let ids: HashMap<_, _> = spans.iter().map(|s| (s.span_id, *s)).collect();
        assert_eq!(ids.len(), spans.len(), "span ids are unique within a trace");
        for s in spans {
            assert!(s.start_ns < s.end_ns);
            if let Some(p) = s.parent {
                let parent = ids.get(&p).expect("parent is in the same trace");
                assert!(
                    parent.start_ns <= s.start_ns && s.end_ns <= parent.end_ns,
                    "a parent spans its children"
                );
            }
        }
    }
    // Variety for the views: errors, warnings, every service, and time spread over the window.
    assert!(b.spans.iter().any(|s| s.error) && b.spans.iter().any(|s| !s.error));
    assert!(
        b.logs.iter().any(|l| l.severity_text == "ERROR")
            && b.logs.iter().any(|l| l.severity_text == "WARN")
    );
    for svc in synth::SERVICES {
        assert!(b.spans.iter().any(|s| s.service == *svc), "{svc}");
    }
    let (lo, hi) = (
        b.spans.iter().map(|s| s.start_ns).min().unwrap(),
        b.spans.iter().map(|s| s.start_ns).max().unwrap(),
    );
    assert!(
        NOW - lo <= 121 * 60 * 1_000_000_000 && hi <= NOW + 1_000_000_000,
        "within the requested window"
    );
    assert!(
        hi - lo > 60 * 60 * 1_000_000_000,
        "spread over the window, not clustered"
    );
    // Logs point at real spans.
    let span_ids: std::collections::HashSet<_> =
        b.spans.iter().map(|s| (s.trace_id, s.span_id)).collect();
    assert!(
        b.logs
            .iter()
            .all(|l| span_ids.contains(&(l.trace_id, l.span_id)))
    );
}

// ---- protobuf ------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Val {
    Var(u64),
    Fix(u64),
    Len(Vec<u8>),
}

/// Decode one message level into (field number, value). Panics on malformed input.
fn fields(mut b: &[u8]) -> Vec<(u32, Val)> {
    fn varint(b: &mut &[u8]) -> u64 {
        let (mut n, mut shift) = (0u64, 0);
        loop {
            let byte = b[0];
            *b = &b[1..];
            n |= u64::from(byte & 0x7F) << shift;
            if byte & 0x80 == 0 {
                return n;
            }
            shift += 7;
        }
    }
    let mut out = vec![];
    while !b.is_empty() {
        let key = varint(&mut b);
        let num = (key >> 3) as u32;
        out.push((
            num,
            match key & 7 {
                0 => Val::Var(varint(&mut b)),
                1 => {
                    let v = u64::from_le_bytes(b[..8].try_into().unwrap());
                    b = &b[8..];
                    Val::Fix(v)
                }
                2 => {
                    let n = varint(&mut b) as usize;
                    let v = b[..n].to_vec();
                    b = &b[n..];
                    Val::Len(v)
                }
                w => panic!("unexpected wire type {w}"),
            },
        ));
    }
    out
}

fn len(f: &[(u32, Val)], n: u32) -> Vec<Vec<u8>> {
    f.iter()
        .filter(|(k, _)| *k == n)
        .filter_map(|(_, v)| {
            if let Val::Len(b) = v {
                Some(b.clone())
            } else {
                None
            }
        })
        .collect()
}

fn one_len(f: &[(u32, Val)], n: u32) -> Vec<u8> {
    let v = len(f, n);
    assert_eq!(v.len(), 1, "field {n}");
    v.into_iter().next().unwrap()
}

fn string_attr(kvs: &[Vec<u8>], key: &str) -> Option<String> {
    kvs.iter().find_map(|kv| {
        let f = fields(kv);
        (String::from_utf8(one_len(&f, 1)).ok()? == key)
            .then(|| String::from_utf8(one_len(&fields(&one_len(&f, 2)), 1)).unwrap())
    })
}

#[test]
fn ac_0_2_9_payload_is_valid_otlp() {
    let b = batch(7);
    // ExportTraceServiceRequest -> ResourceSpans(1) -> { resource(1), scope_spans(2) -> spans(2) }
    let req = fields(&proto::encode_traces(&b));
    assert!(
        req.iter().all(|(k, _)| *k == 1),
        "only resource_spans at the top level"
    );
    let mut spans_seen = 0;
    let mut services = vec![];
    for rs in len(&req, 1) {
        let rs = fields(&rs);
        let resource = fields(&one_len(&rs, 1));
        let svc =
            string_attr(&len(&resource, 1), "service.name").expect("service.name on the resource");
        services.push(svc.clone());
        assert!(string_attr(&len(&resource, 1), "service.version").is_some());
        for ss in len(&rs, 2) {
            let ss = fields(&ss);
            let scope = fields(&one_len(&ss, 1));
            assert_eq!(one_len(&scope, 1), b"demo", "instrumentation scope name");
            for sp in len(&ss, 2) {
                let f = fields(&sp);
                assert_eq!(one_len(&f, 1).len(), 16, "trace_id is 16 bytes");
                assert_eq!(one_len(&f, 2).len(), 8, "span_id is 8 bytes");
                assert!(
                    len(&f, 4).iter().all(|p| p.is_empty() || p.len() == 8),
                    "parent is empty or 8 bytes"
                );
                assert!(!one_len(&f, 5).is_empty(), "name");
                let kind = f
                    .iter()
                    .find_map(|(k, v)| (*k == 6).then_some(v.clone()))
                    .unwrap();
                assert!(matches!(kind, Val::Var(2 | 3)));
                let (start, end) = (
                    f.iter().find(|(k, _)| *k == 7).unwrap().1.clone(),
                    f.iter().find(|(k, _)| *k == 8).unwrap().1.clone(),
                );
                let (Val::Fix(s), Val::Fix(e)) = (start, end) else {
                    panic!("fixed64 times")
                };
                assert!(s < e && s > 1_000_000_000_000_000_000);
                let status = fields(&one_len(&f, 15));
                assert!(matches!(status[0], (3, Val::Var(1 | 2))));
                assert!(string_attr(&len(&f, 9), "http.route").is_some());
                spans_seen += 1;
            }
        }
    }
    assert_eq!(spans_seen, b.spans.len());
    services.sort();
    assert_eq!(
        services,
        [
            "cart",
            "checkout",
            "frontend",
            "inventory",
            "payment",
            "postgres"
        ]
    );

    // ExportLogsServiceRequest -> ResourceLogs(1) -> scope_logs(2) -> log_records(2)
    let req = fields(&proto::encode_logs(&b));
    let mut logs_seen = 0;
    for rl in len(&req, 1) {
        for sl in len(&fields(&rl), 2) {
            for rec in len(&fields(&sl), 2) {
                let f = fields(&rec);
                assert!(
                    matches!(f.iter().find(|(k, _)| *k == 1).unwrap().1, Val::Fix(t) if t > 1_000_000_000_000_000_000)
                );
                assert!(matches!(
                    f.iter().find(|(k, _)| *k == 2).unwrap().1,
                    Val::Var(9 | 13 | 17)
                ));
                assert!(
                    ["INFO", "WARN", "ERROR"]
                        .contains(&String::from_utf8(one_len(&f, 3)).unwrap().as_str())
                );
                assert!(
                    !one_len(&fields(&one_len(&f, 5)), 1).is_empty(),
                    "body string_value"
                );
                assert_eq!(one_len(&f, 9).len(), 16);
                assert_eq!(one_len(&f, 10).len(), 8);
                logs_seen += 1;
            }
        }
    }
    assert_eq!(logs_seen, b.logs.len());
    // Varint edge cases.
    assert_eq!(proto::varint(0), [0]);
    assert_eq!(proto::varint(300), [0xAC, 0x02]);
    assert_eq!(proto::varint(u64::MAX).len(), 10);
}

// ---- guards ------------------------------------------------------------------

#[test]
fn ac_0_2_9_seeds_only_demo_database() {
    for ok in ["demo", "demo_a", "demo_tenant_2"] {
        assert!(is_demo_database(ok), "{ok}");
        assert!(plan_seed(ok, false, false).unwrap(), "{ok}");
    }
    for bad in [
        "public", "prod", "demo_", "demo2", "demox", "mydemo", "tenant_a", "Demo", "",
    ] {
        assert!(!is_demo_database(bad), "{bad}");
        let e = plan_seed(bad, false, false).unwrap_err().to_string();
        assert!(e.contains("refusing"), "{bad}: {e}");
        assert!(
            plan_seed(bad, false, true).is_err(),
            "--force must not bypass the database guard: {bad}"
        );
    }
    assert_eq!(
        admin_url("mysql://u:p@db:4002/demo").unwrap(),
        "mysql://u:p@db:4002/public"
    );
}

#[tokio::test]
async fn ac_0_2_9_skips_when_data_exists() {
    assert!(
        !plan_seed("demo", true, false).unwrap(),
        "existing data is not seeded again"
    );
    assert!(
        plan_seed("demo", true, true).unwrap(),
        "--force seeds anyway"
    );
    assert!(plan_seed("demo", false, false).unwrap());

    let with = Fake::new(|_| Ok(vec![json!({"n": 42})]));
    assert!(has_traces(with.as_ref(), "demo", "opentelemetry_traces").await);
    let empty = Fake::new(|_| Ok(vec![json!({"n": 0})]));
    assert!(!has_traces(empty.as_ref(), "demo", "opentelemetry_traces").await);
    // A missing table or database is an error from the server: that counts as empty.
    let missing = Fake::new(|_| Err(grepui::error::ReadError::Db("table not found".into())));
    assert!(!has_traces(missing.as_ref(), "demo", "opentelemetry_traces").await);
    // The guard query is fixed text with a validated table name.
    assert!(
        with.seen.lock().unwrap()[0]
            .sql
            .contains("`demo`.`opentelemetry_traces`")
    );
    assert!(!has_traces(with.as_ref(), "demo", "x; DROP TABLE y").await);
}

#[test]
fn ac_0_2_9_serve_has_no_ingest_path() {
    // Only src/playground/ may write telemetry; no other module may use its HTTP client or encoder.
    fn walk(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
        for e in fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }
    let mut files = vec![];
    walk(Path::new("src"), &mut files);
    assert!(files.len() > 15);
    let mut users = vec![];
    for f in &files {
        let text = fs::read_to_string(f).unwrap();
        if text.contains("playground::http")
            || text.contains("http::post(")
            || text.contains("/v1/otlp/v1/") && !f.ends_with("api/connect.rs")
        {
            users.push(f.to_string_lossy().into_owned());
        }
    }
    users.sort();
    assert!(
        users
            .iter()
            .all(|u| u.starts_with("src/playground/") || u == "src/main.rs"),
        "unexpected writers: {users:?}"
    );
    // `serve` (server.rs) and the API never reference the playground.
    for f in [
        "src/server.rs",
        "src/api/mod.rs",
        "src/api/logs.rs",
        "src/api/streams.rs",
        "src/api/sql.rs",
    ] {
        let t = fs::read_to_string(f).unwrap();
        assert!(
            !t.contains("playground"),
            "{f} must not depend on the playground"
        );
    }
    // The only subcommand that calls prepare() is Playground.
    let main = fs::read_to_string("src/main.rs").unwrap();
    assert_eq!(main.matches("playground::prepare").count(), 1);
}

/// Live: seed a throwaway demo database through the real OTLP endpoints, read it back, and
/// prove a second run does not seed again.
#[tokio::test]
#[ignore = "needs GreptimeDB: make db-up && make test-db"]
async fn ac_0_2_9_playground_seeds_once_live() {
    use grepui::{
        config::Config,
        playground::{Opts, http, prepare},
        query::{Q, Reader, SqlxReader},
    };
    let base = std::env::var("GREPUI_TEST_DB")
        .expect("set GREPUI_TEST_DB=mysql://root@127.0.0.1:4002/public");
    let db = format!("demo_it_{}", std::process::id());
    let mysql = base.replace("/public", &format!("/{db}"));
    let cfg = Config {
        greptime_mysql: mysql.clone(),
        ..Config::default()
    };
    let opts = Opts {
        http: "http://127.0.0.1:4000".into(),
        wait_secs: 20,
        demo_data: true,
        traces: 40,
        minutes: 30,
        seed: 11,
        live: false,
        force: false,
        llm_data: false,
    };

    prepare(&cfg, &opts).await.expect("first run seeds");
    let reader = SqlxReader::lazy(&mysql, std::time::Duration::from_secs(20)).unwrap();
    let count = |t: &'static str| {
        let q = Q::new(
            "t",
            format!("SELECT CAST(count(*) AS BIGINT) AS n FROM `{db}`.`{t}`"),
            vec![],
        );
        let reader = &reader;
        async move { reader.fetch(&q).await.unwrap()[0]["n"].as_i64().unwrap() }
    };
    let (spans, logs) = (
        count("opentelemetry_traces").await,
        count("opentelemetry_logs").await,
    );
    let expected = synth::generate(11, 40, 0, 30).spans.len() as i64;
    assert_eq!(spans, expected, "every generated span arrived");
    assert_eq!(logs, expected);

    // Services are real: the auxiliary table GreptimeDB builds lists all six.
    let svc = reader
        .fetch(&Q::new(
            "t",
            format!("SELECT count(*) AS n FROM `{db}`.`opentelemetry_traces_services`"),
            vec![],
        ))
        .await
        .unwrap();
    assert!(svc[0]["n"].as_i64().unwrap() >= 6);

    prepare(&cfg, &opts).await.expect("second run");
    assert_eq!(
        count("opentelemetry_traces").await,
        spans,
        "a restart must not seed again"
    );

    // The guard refuses a non-demo database before touching anything.
    let prod = Config {
        greptime_mysql: base,
        ..Config::default()
    };
    assert!(prepare(&prod, &opts).await.is_err());

    let (code, _) = http::post(
        "http://127.0.0.1:4000",
        "/v1/sql",
        &[("content-type", "application/x-www-form-urlencoded")],
        format!("sql=DROP DATABASE {db}").as_bytes(),
    )
    .await
    .unwrap();
    assert_eq!(code, 200, "cleanup");
}
