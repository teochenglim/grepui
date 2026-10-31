//! Traces (R-R1 – R-R6): list, histogram, scatter, detail (waterfall), flame graph, logs.

use super::{Shared, ok};
use crate::{
    error::ApiError,
    layout,
    org::Org,
    query::{
        Window,
        filter::parse,
        logs::bucket_for,
        traces::{self, TraceFilter},
    },
};
use axum::{
    extract::{Path, Query, State},
    response::Response,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

const COLS_TTL: Duration = Duration::from_secs(60);

#[derive(Deserialize)]
pub struct TraceParams {
    from: Option<i64>,
    to: Option<i64>,
    limit: Option<u32>,
    offset: Option<u32>,
    q: Option<String>,
    service: Option<String>,
    errors: Option<bool>,
    min_ms: Option<i64>,
    span: Option<String>,
    /// `all` or `a,b`: read from these sources and merge by time (v0.12.0).
    sources: Option<String>,
}

/// Columns of the traces table (including the flat attribute columns), cached briefly.
async fn columns(s: &Shared, org: &Org) -> Result<Arc<Vec<String>>, ApiError> {
    let key = format!("{}/{}", org.name, org.traces_table);
    if let Some((at, c)) = s.cols_cache.lock().ok().and_then(|m| m.get(&key).cloned())
        && at.elapsed() < COLS_TTL
    {
        return Ok(c);
    }
    let (rows, _) = s
        .run(crate::query::build::stream_schema(
            &org.database,
            &org.traces_table,
        ))
        .await?;
    if rows.is_empty() {
        return Err(ApiError::not_found(
            "the traces table does not exist yet; see Connect",
        ));
    }
    let cols: Arc<Vec<String>> = Arc::new(
        rows.iter()
            .filter_map(|r| r["column_name"].as_str().map(str::to_string))
            .collect(),
    );
    if let Ok(mut m) = s.cols_cache.lock() {
        m.insert(key, (Instant::now(), cols.clone()));
    }
    Ok(cols)
}

async fn filter(s: &Shared, org: &Org, p: &TraceParams) -> Result<TraceFilter, ApiError> {
    let parsed = parse(p.q.as_deref().unwrap_or_default())?;
    let service = p
        .service
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string);
    if let Some(v) = &service
        && (v.chars().count() > 200 || v.chars().any(char::is_control))
    {
        return Err(ApiError::bad("invalid service"));
    }
    let conds = if parsed.conds.is_empty() {
        vec![]
    } else {
        traces::resolve(&parsed, &columns(s, org).await?)?
    };
    Ok(TraceFilter {
        conds,
        terms: parsed.terms,
        service,
        errors: p.errors.unwrap_or(false),
        min_ms: p.min_ms.map(|m| m.clamp(0, 86_400_000)),
    })
}

fn bad_table<T>(r: anyhow::Result<T>) -> Result<T, ApiError> {
    r.map_err(|_| ApiError::bad("unsupported traces table name"))
}

/// Which attribute layout the org's traces table has (R-AI1): `flat` (trace v1) or `json`
/// (trace v2). The UI shows it, and the filter bar adapts its hint.
pub async fn layout_info(
    State(s): State<Shared>,
    Path(org): Path<String>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let cols = columns(&s, org).await?;
    Ok(ok(
        json!({"layout": traces::Layout::detect(&cols), "table": org.traces_table, "columns": cols.len()}),
        crate::query::Meta {
            query: "traces_layout".into(),
            sql: String::new(),
            binds: vec![],
            ms: 0,
            rows: 0,
            source: "raw",
            note: None,
        },
    ))
}

pub async fn list(
    State(s): State<Shared>,
    axum::Extension(id): axum::Extension<crate::auth::Identity>,
    Path(org): Path<String>,
    Query(p): Query<TraceParams>,
) -> Result<Response, ApiError> {
    let w = Window::parse(p.from, p.to, p.limit)?;
    let org = s.org(&org)?;
    let f = filter(&s, org, &p).await?;
    let q = bad_table(traces::list(
        &org.database,
        &org.traces_table,
        &f,
        w,
        p.offset.unwrap_or(0),
    ))?;
    if let Some(spec) = p.sources.as_deref() {
        if p.offset.unwrap_or(0) > 0 {
            return Err(ApiError::bad("offset cannot be combined with sources"));
        }
        return super::workload::fan_out(
            &s,
            &id,
            org,
            spec,
            q,
            ("start_ms", w.limit as usize),
            None,
        )
        .await;
    }
    let (rows, meta) = s.run(q).await?;
    Ok(ok(rows, meta))
}

pub async fn scatter(
    State(s): State<Shared>,
    Path(org): Path<String>,
    Query(p): Query<TraceParams>,
) -> Result<Response, ApiError> {
    let w = Window::parse(p.from, p.to, None)?;
    let org = s.org(&org)?;
    let f = filter(&s, org, &p).await?;
    let (rows, meta) = s
        .run(bad_table(traces::scatter(
            &org.database,
            &org.traces_table,
            &f,
            w,
        ))?)
        .await?;
    let capped = rows.len() as u32 >= traces::SCATTER_MAX;
    Ok(ok(
        json!({"points": rows, "capped": capped, "max": traces::SCATTER_MAX}),
        meta,
    ))
}

pub async fn histogram(
    State(s): State<Shared>,
    Path(org): Path<String>,
    Query(p): Query<TraceParams>,
) -> Result<Response, ApiError> {
    let w = Window::parse(p.from, p.to, None)?;
    let org = s.org(&org)?;
    let f = filter(&s, org, &p).await?;
    let (rows, meta) = s
        .run(bad_table(traces::histogram(
            &org.database,
            &org.traces_table,
            &f,
            w,
        ))?)
        .await?;
    let buckets: Vec<Value> = rows
        .iter()
        .map(|r| json!({"t": r["bucket_ms"], "count": r["n"]}))
        .collect();
    let total: i64 = rows.iter().filter_map(|r| r["n"].as_i64()).sum();
    let (bucket_ms, _) = bucket_for(w.to_ms - w.from_ms);
    Ok(ok(
        json!({"bucket_ms": bucket_ms, "buckets": buckets, "total": total}),
        meta,
    ))
}

pub async fn services(
    State(s): State<Shared>,
    Path(org): Path<String>,
    Query(p): Query<TraceParams>,
) -> Result<Response, ApiError> {
    let w = Window::parse(p.from, p.to, None)?;
    let org = s.org(&org)?;
    let (rows, meta) = s
        .run(bad_table(traces::services(
            &org.database,
            &org.traces_table,
            w,
        ))?)
        .await?;
    let names: Vec<&str> = rows
        .iter()
        .filter_map(|r| r["service_name"].as_str())
        .collect();
    Ok(ok(names, meta))
}

/// The window for a single trace: the one given, or the last week.
fn trace_window(s: &Shared, p: &TraceParams) -> Result<Window, ApiError> {
    if p.from.is_none() && p.to.is_none() {
        return Ok(traces::default_window(s.now_ms()));
    }
    Window::parse(p.from, p.to, None)
}

/// Fetch and parse the spans of one trace; `(spans, truncated)`.
async fn load(
    s: &Shared,
    org: &Org,
    id: &str,
    w: Window,
) -> Result<(Vec<layout::SpanIn>, bool, crate::query::Meta), ApiError> {
    let (rows, meta) = s
        .run(bad_table(traces::spans(
            &org.database,
            &org.traces_table,
            id,
            w,
        ))?)
        .await?;
    let truncated = rows.len() as u32 > traces::SPANS_MAX;
    let spans: Vec<layout::SpanIn> = rows
        .iter()
        .take(traces::SPANS_MAX as usize)
        .filter_map(layout::span_from_row)
        .collect();
    if spans.is_empty() {
        return Err(ApiError::not_found(
            "no spans for that trace in this time range; widen the range",
        ));
    }
    Ok((spans, truncated, meta))
}

/// Fill missing costs from the price table and summarise the LLM spans of a trace; `(has_llm,
/// summary)`. A price table that cannot be read leaves costs as reported.
async fn llm_views(s: &Shared, org: &Org, rows: &mut [Value]) -> (bool, Value) {
    if !rows.iter().any(|r| r.get("llm").is_some()) {
        return (false, Value::Null);
    }
    let prices = super::llm::prices(s, &org.name).await.unwrap_or_default();
    for r in rows.iter_mut() {
        if let Some(v) = r.get_mut("llm") {
            crate::llm::estimate_cost(v, &prices);
        }
    }
    let views: Vec<&Value> = rows.iter().filter_map(|r| r.get("llm")).collect();
    (true, crate::llm::trace_summary(&views))
}

pub async fn detail(
    State(s): State<Shared>,
    Path((org, id)): Path<(String, String)>,
    Query(p): Query<TraceParams>,
) -> Result<Response, ApiError> {
    let id = traces::trace_id(&id)?;
    let w = trace_window(&s, &p)?;
    let org = s.org(&org)?;
    let (spans, truncated, meta) = load(&s, org, &id, w).await?;
    let mut wf = layout::waterfall(&spans);
    let (llm, summary) = llm_views(&s, org, &mut wf.rows).await;
    let mut services: Vec<&str> = spans.iter().map(|x| x.service.as_str()).collect();
    services.sort_unstable();
    services.dedup();
    let trace = json!({
        "trace_id": id, "start_ms": wf.start_ns / 1_000_000, "start_ns": wf.start_ns.to_string(),
        "duration_us": (wf.end_ns - wf.start_ns) / 1000, "spans": spans.len(),
        "services": services, "errors": spans.iter().filter(|x| x.is_error()).count(), "truncated": truncated,
        "llm": llm.then_some(summary),
    });
    Ok(ok(json!({"trace": trace, "spans": wf.rows}), meta))
}

/// The agent graph of one trace (R-AI9): agents, tools and model calls with the edges between them.
pub async fn agent_graph(
    State(s): State<Shared>,
    Path((org, id)): Path<(String, String)>,
    Query(p): Query<TraceParams>,
) -> Result<Response, ApiError> {
    let id = traces::trace_id(&id)?;
    let w = trace_window(&s, &p)?;
    let org = s.org(&org)?;
    let (spans, truncated, meta) = load(&s, org, &id, w).await?;
    let mut wf = layout::waterfall(&spans);
    llm_views(&s, org, &mut wf.rows).await;
    let mut g = crate::agent_graph::build(&wf.rows);
    let edges: Vec<layout::EdgeIn> = g["edges"]
        .as_array()
        .map(|l| {
            l.iter()
                .map(|e| layout::EdgeIn {
                    from: e["from"].as_str().unwrap_or_default().to_string(),
                    to: e["to"].as_str().unwrap_or_default().to_string(),
                    calls: e["calls"].as_i64().unwrap_or(0),
                    errors: e["errors"].as_i64().unwrap_or(0),
                    avg_us: 0.0,
                })
                .collect()
        })
        .unwrap_or_default();
    // Positions for the drawing (the service map's layered layout); a lone step has no edge, so
    // the page also lists every node in a table.
    g["map"] = layout::graph(&edges, true);
    g["trace_id"] = json!(id);
    g["truncated"] = json!(truncated);
    Ok(ok(g, meta))
}

pub async fn flame(
    State(s): State<Shared>,
    Path((org, id)): Path<(String, String)>,
    Query(p): Query<TraceParams>,
) -> Result<Response, ApiError> {
    let id = traces::trace_id(&id)?;
    let w = trace_window(&s, &p)?;
    let org = s.org(&org)?;
    let (spans, truncated, meta) = load(&s, org, &id, w).await?;
    Ok(ok(
        json!({"trace_id": id, "root": layout::flame(&spans), "truncated": truncated}),
        meta,
    ))
}

/// Logs of one trace, or of one span of it (`?span=`).
pub async fn logs(
    State(s): State<Shared>,
    Path((org, id)): Path<(String, String)>,
    Query(p): Query<TraceParams>,
) -> Result<Response, ApiError> {
    let id = traces::trace_id(&id)?;
    let span = p.span.as_deref().map(traces::span_id).transpose()?;
    let base = trace_window(&s, &p)?;
    let w = Window {
        limit: p.limit.unwrap_or(500).clamp(1, crate::query::MAX_LIMIT),
        ..base
    };
    let org = s.org(&org)?;
    let q = bad_table(traces::trace_logs(
        &org.database,
        &org.logs_table,
        &id,
        span.as_deref(),
        w,
    ))?;
    let (rows, meta) = s.run(q).await?;
    Ok(ok(rows, meta))
}
