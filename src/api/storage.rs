//! Rollups (R-P3) and retention (R-X1): the admin-gated writes of v0.7.0, and their read side.
//! Reads are open; every write needs `--admin-writes` and passes `admin::check`.

use super::{Shared, ok};
use crate::{
    admin::{self, Plan, Stream, Ttl},
    error::ApiError,
    query::{Window, build},
};
use axum::{
    Json,
    extract::{Path, Query, State},
    response::Response,
};
use serde::Deserialize;
use serde_json::{Value, json};

fn bad_table<T>(r: anyhow::Result<T>) -> Result<T, ApiError> {
    r.map_err(|_| ApiError::bad("unsupported table name"))
}

/// The planner caches whether the sink exists; a create or drop must not wait for it to expire.
fn forget_sink(s: &Shared, org: &str, plan: &Plan) {
    if let Ok(mut m) = s.cols_cache.lock() {
        m.remove(&format!("{org}/{}", plan.sink));
    }
}

pub async fn rollups(
    State(s): State<Shared>,
    Path(org): Path<String>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let mut out = vec![];
    let mut last = None;
    for t in admin::TEMPLATES {
        let plan = Plan::for_org(org, t)?;
        let (cols, meta) = s.run(build::stream_schema(&plan.db, &plan.sink)).await?;
        let exists = !cols.is_empty();
        let (flow, _) = s.run(admin::rollup_flow(&plan.flow)).await?;
        let (mut rows, mut newest) = (Value::Null, Value::Null);
        if exists
            && let Ok((st, _)) = s
                .run(bad_table(build::stream_stats(
                    &plan.db,
                    &plan.sink,
                    "time_window",
                ))?)
                .await
            && let Some(r) = st.first()
        {
            rows = r["total"].clone();
            newest = r["newest"].clone();
        }
        out.push(json!({
            "template": plan.template, "title": "Traces RED, one row per service per minute",
            "source": plan.source, "sink": plan.sink, "flow": plan.flow,
            "sink_exists": exists, "flow_exists": !flow.is_empty(),
            "created_ms": flow.first().map(|r| r["created_time"].clone()),
            "rows": rows, "newest_ms": newest,
        }));
        last = Some(meta);
    }
    let meta = last.ok_or_else(|| ApiError::bad("no templates"))?;
    Ok(ok(
        json!({"templates": out, "admin_writes": s.cfg.admin_writes}),
        meta,
    ))
}

pub async fn rollup_create(
    State(s): State<Shared>,
    Path((org, template)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let plan = Plan::for_org(org, &template)?;
    let mut meta = None;
    for sql in plan.create() {
        meta = Some(s.admin_ddl(org, "rollup_create", &sql).await?);
    }
    forget_sink(&s, &org.name, &plan);
    let meta = meta.ok_or_else(|| ApiError::bad("nothing to run"))?;
    Ok(ok(
        json!({"created": true, "sink": plan.sink, "flow": plan.flow,
               "next": "Rows written from now on are rolled up by the flow; backfill the history you already have."}),
        meta,
    ))
}

#[derive(Deserialize)]
pub struct Range {
    from: Option<i64>,
    to: Option<i64>,
}

pub async fn rollup_backfill(
    State(s): State<Shared>,
    Path((org, template)): Path<(String, String)>,
    Query(p): Query<Range>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let plan = Plan::for_org(org, &template)?;
    let w = Window::parse(p.from, p.to, None).map_err(|_| {
        ApiError::bad("from and to are required (epoch ms) and at most 31 days apart")
    })?;
    let chunks = admin::chunks(w.from_ms, w.to_ms)?;
    let mut rows = 0;
    let mut meta = None;
    for (a, b) in &chunks {
        let m = s.admin_exec(org, plan.backfill(*a, *b)).await?;
        rows += m.rows;
        meta = Some(m);
    }
    let meta = meta.ok_or_else(|| ApiError::bad("nothing to backfill"))?;
    Ok(ok(
        json!({"chunks": chunks.len(), "from_ms": chunks[0].0, "to_ms": chunks[chunks.len() - 1].1, "rows": rows}),
        meta,
    ))
}

pub async fn rollup_drop(
    State(s): State<Shared>,
    Path((org, template)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let plan = Plan::for_org(org, &template)?;
    let mut meta = None;
    for sql in plan.drop() {
        meta = Some(s.admin_ddl(org, "rollup_drop", &sql).await?);
    }
    let meta = meta.ok_or_else(|| ApiError::bad("nothing to run"))?;
    Ok(ok(
        json!({"dropped": true, "flow": plan.flow, "sink": plan.sink}),
        meta,
    ))
}

// ---- retention ----------------------------------------------------------------------

pub async fn retention(
    State(s): State<Shared>,
    Path(org): Path<String>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let mut tables = vec![];
    for st in Stream::ALL {
        tables.push(st.table(org)?.0);
    }
    let (rows, meta) = s
        .run(admin::retention_options(&org.database, &tables))
        .await?;
    let streams: Vec<Value> = Stream::ALL
        .iter()
        .zip(&tables)
        .map(|(st, t)| {
            let row = rows.iter().find(|r| r["table_name"] == t.as_str());
            let opts = row
                .and_then(|r| r["create_options"].as_str())
                .unwrap_or_default();
            json!({
                "stream": st.name(), "table": t, "exists": row.is_some(),
                "ttl": admin::ttl_text_from_options(opts).unwrap_or_else(|| "none".into()),
                "ttl_secs": admin::ttl_secs_from_options(opts),
            })
        })
        .collect();
    // Tiering advice: raw short, rollup long.
    let secs = |n: &str| {
        streams.iter().find(|x| x["stream"] == n).map(|x| {
            (
                x["exists"].as_bool().unwrap_or(false),
                x["ttl_secs"].as_i64(),
            )
        })
    };
    let advice = match (secs("traces"), secs("rollup")) {
        (Some((_, None)), Some((true, _))) => Some(
            "Raw spans are kept forever. With a rollup in place, a short raw TTL (for example 7d) saves the most storage.",
        ),
        (Some((_, Some(raw))), Some((true, roll))) if roll.is_some_and(|r| raw > r) => Some(
            "The rollup is kept shorter than raw spans. Keep raw spans short (for example 7d) and the rollup long (for example 180d).",
        ),
        _ => None,
    };
    Ok(ok(
        json!({"streams": streams, "admin_writes": s.cfg.admin_writes, "advice": advice}),
        meta,
    ))
}

#[derive(Deserialize)]
pub struct PreviewParams {
    ttl: Option<String>,
}

/// What a TTL would expire: rows already older than `now - ttl`. Read-only.
pub async fn retention_preview(
    State(s): State<Shared>,
    Path((org, stream)): Path<(String, String)>,
    Query(p): Query<PreviewParams>,
) -> Result<Response, ApiError> {
    let stream = Stream::parse(&stream)?;
    let ttl = Ttl::parse(p.ttl.as_deref().unwrap_or_default())?;
    let org = s.org(&org)?;
    let (table, col) = stream.table(org)?;
    let cutoff = ttl.secs().map_or(0, |secs| s.now_ms() - secs * 1000);
    let q = bad_table(admin::retention_counts(&org.database, &table, col, cutoff))?;
    let (rows, meta) = s
        .run(q)
        .await
        .map_err(|_| ApiError::not_found("that stream's table does not exist"))?;
    let r = rows.first();
    let total = r.and_then(|r| r["total"].as_i64()).unwrap_or(0);
    let older = if ttl == Ttl::Forever {
        0
    } else {
        r.and_then(|r| r["older"].as_i64()).unwrap_or(0)
    };
    Ok(ok(
        json!({"stream": stream.name(), "table": table, "ttl": ttl.text(), "cutoff_ms": cutoff,
               "total": total, "would_expire": older, "would_keep": total - older}),
        meta,
    ))
}

#[derive(Deserialize)]
pub struct SetTtl {
    ttl: String,
}

pub async fn retention_set(
    State(s): State<Shared>,
    Path((org, stream)): Path<(String, String)>,
    Json(b): Json<SetTtl>,
) -> Result<Response, ApiError> {
    let stream = Stream::parse(&stream)?;
    let ttl = Ttl::parse(&b.ttl)?;
    let org = s.org(&org)?;
    let sql = admin::ttl_ddl(org, stream, &ttl)?;
    let meta = s.admin_ddl(org, "retention_set", &sql).await?;
    Ok(ok(
        json!({"stream": stream.name(), "ttl": ttl.text()}),
        meta,
    ))
}
