//! Query management (R-W1 – R-W3) and federation (R-Y1, R-Y2), ADR 0013: running queries and
//! cancel, work groups, statistics, and the sources a read can fan out to.

use super::{Shared, ok};
use crate::{
    auth::{Identity, Role, store as items},
    error::{ApiError, ReadError},
    org::Org,
    query::{self, Meta, Q, Reader, build},
    store::build::PLATFORM_ORG,
    workload::{Group, PRIMARY, RunningView, SourceState, merge_desc, normalize},
};
use axum::{
    Extension, Json,
    extract::{Path, Query, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{collections::HashMap, sync::Arc, time::Duration, time::Instant};

const GROUP_KIND: &str = "workgroup";
const SOURCE_KIND: &str = "source";
const HEALTH_TIMEOUT: Duration = Duration::from_secs(2);

fn meta(name: &str) -> Meta {
    Meta {
        query: name.into(),
        sql: String::new(),
        binds: vec![],
        ms: 0,
        rows: 0,
        source: "raw",
        note: None,
    }
}

fn role_of(id: &Identity, org: &str) -> Role {
    id.role_in(org).unwrap_or(Role::Viewer)
}

// ---- keeping memory and the store in step ----------------------------------------------------

/// Read every org's work groups and the source settings into memory.
pub async fn reload(s: &Shared) {
    for org in s.orgs.list() {
        if let Ok(rows) = items::list(s, &org.name, GROUP_KIND).await {
            let groups = rows
                .iter()
                .filter_map(|r| {
                    serde_json::from_str::<Group>(&r.spec)
                        .ok()
                        .map(|g| (r.item_id.clone(), g))
                })
                .collect();
            s.workload.replace_org(&org.name, groups);
        }
    }
    if let Ok(rows) = items::list(s, PLATFORM_ORG, SOURCE_KIND).await {
        let all: HashMap<String, SourceState> = rows
            .iter()
            .filter_map(|r| {
                serde_json::from_str(&r.spec)
                    .ok()
                    .map(|st| (r.item_id.clone(), st))
            })
            .collect();
        s.sources.replace_states(all);
    }
}

pub fn spawn_refresh(s: Shared) {
    tokio::spawn(async move {
        loop {
            reload(&s).await;
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    });
}

// ---- work groups ---------------------------------------------------------------------------------

pub async fn groups_list(
    State(s): State<Shared>,
    Path(org): Path<String>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let data: Vec<Value> = [Role::Viewer, Role::Editor, Role::Admin]
        .iter()
        .map(|r| json!({"role": r.name(), "limits": s.workload.group(&org.name, *r)}))
        .collect();
    Ok(ok(data, meta("workgroups_list")))
}

fn parse_role(role: &str) -> Result<Role, ApiError> {
    Role::parse(role).ok_or_else(|| ApiError::bad("the role must be viewer, editor or admin"))
}

pub async fn group_put(
    State(s): State<Shared>,
    Path((org, role)): Path<(String, String)>,
    Json(g): Json<Group>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let role = parse_role(&role)?;
    g.validate().map_err(ApiError::bad)?;
    let now = s.now_ms();
    let created = items::get(&s, &org.name, GROUP_KIND, role.name())
        .await?
        .map(|r| r.created_ms)
        .unwrap_or(now);
    let m = items::put(
        &s,
        &org.name,
        GROUP_KIND,
        &items::row(role.name(), role.name(), &g, "admin", created, now),
    )
    .await?;
    s.workload.set_group(&org.name, role.name(), Some(g));
    Ok(ok(json!({"role": role.name(), "limits": g}), m))
}

pub async fn group_delete(
    State(s): State<Shared>,
    Path((org, role)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let role = parse_role(&role)?;
    let Some(r) = items::get(&s, &org.name, GROUP_KIND, role.name()).await? else {
        return Err(ApiError::not_found("no limits set for that role"));
    };
    let m = items::remove(&s, &org.name, GROUP_KIND, r, s.now_ms()).await?;
    s.workload.set_group(&org.name, role.name(), None);
    Ok(ok(json!({"role": role.name(), "removed": true}), m))
}

// ---- running queries and cancel ------------------------------------------------------------------

/// grepui's running queries, each with the GreptimeDB process it matches (by statement text).
/// The second list holds process-list rows grepui did not start.
async fn attribute(s: &Shared) -> (Vec<(RunningView, Option<String>)>, Vec<Value>) {
    let ours = s.workload.running();
    let procs = s
        .reader
        .fetch(&build::process_list())
        .await
        .unwrap_or_default();
    let mut used = vec![false; procs.len()];
    let norms: Vec<String> = procs
        .iter()
        .map(|p| normalize(p["query"].as_str().unwrap_or_default()))
        .collect();
    let mut out = Vec::new();
    for (v, norm) in ours {
        let hit = (0..procs.len()).find(|i| !used[*i] && norms[*i] == norm);
        if let Some(i) = hit {
            used[i] = true;
        }
        out.push((
            v,
            hit.and_then(|i| procs[i]["id"].as_str().map(String::from)),
        ));
    }
    let others = procs
        .iter()
        .enumerate()
        .filter(|(i, p)| {
            !used[*i]
                && !normalize(p["query"].as_str().unwrap_or_default()).contains("process_list")
        })
        .map(|(_, p)| p.clone())
        .collect();
    (out, others)
}

pub async fn queries_list(
    State(s): State<Shared>,
    Extension(id): Extension<Identity>,
    Path(org): Path<String>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let admin = role_of(&id, &org.name) == Role::Admin;
    let (ours, others) = attribute(&s).await;
    let data: Vec<Value> = ours
        .into_iter()
        .filter(|(v, _)| v.org == org.name && (admin || v.user == id.username))
        .map(|(v, g)| {
            // The statement text is for admins; everyone sees what they started and its tag.
            json!({
                "id": v.id, "user": v.user, "tag": v.tag, "started_ms": v.started_ms,
                "elapsed_ms": v.elapsed_ms, "greptime_id": g,
                "sql": if admin { Value::String(v.sql) } else { Value::Null },
                "own": v.user == id.username,
            })
        })
        .collect();
    let n = data.len();
    let mut out = json!({"running": data});
    if id.platform_admin {
        // Rows grepui did not start (other clients, other replicas): a platform admin only.
        out["other"] = Value::Array(others);
    }
    let mut m = meta("queries_list");
    m.rows = n;
    Ok(ok(out, m))
}

pub async fn query_cancel(
    State(s): State<Shared>,
    Extension(id): Extension<Identity>,
    Path((org, qid)): Path<(String, u64)>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let admin = role_of(&id, &org.name) == Role::Admin;
    let (ours, _) = attribute(&s).await;
    let gid = ours
        .iter()
        .find(|(v, _)| v.id == qid && v.org == org.name)
        .and_then(|(_, g)| g.clone());
    let only = (!admin).then_some(id.username.as_str());
    match s.workload.cancel(&org.name, qid, only) {
        Err(m) => Err(ApiError(StatusCode::FORBIDDEN, m)),
        Ok(false) => Err(ApiError::not_found("no such running query in this org")),
        Ok(true) => {
            // The request stops at once; the database is asked to stop its work too.
            let killed = match &gid {
                Some(g) => s.reader.kill(g).await.is_ok(),
                None => false,
            };
            Ok(ok(
                json!({"cancelled": true, "killed": killed}),
                meta("query_cancel"),
            ))
        }
    }
}

#[derive(Deserialize)]
pub struct StatsParams {
    user: Option<String>,
    limit: Option<usize>,
}

pub async fn queries_stats(
    State(s): State<Shared>,
    Path(org): Path<String>,
    Query(p): Query<StatsParams>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let n = p.limit.unwrap_or(10).clamp(1, 100);
    Ok(ok(
        s.workload.stats(&org.name, p.user.as_deref(), n),
        meta("queries_stats"),
    ))
}

// ---- sources ---------------------------------------------------------------------------------------

fn reader_of(s: &Shared, name: &str) -> Option<Arc<dyn Reader>> {
    if name == PRIMARY {
        return Some(s.reader.clone());
    }
    s.sources.get(name).map(|x| x.reader.clone())
}

async fn health_of(r: &Arc<dyn Reader>) -> Value {
    let t = Instant::now();
    let res = tokio::time::timeout(HEALTH_TIMEOUT, r.fetch(&build::health())).await;
    let ms = t.elapsed().as_millis() as u64;
    match res {
        Ok(Ok(_)) => json!({"ok": true, "ms": ms}),
        Ok(Err(e)) => {
            tracing::warn!(detail = %e.detail(), "source health check failed");
            json!({"ok": false, "ms": ms, "error": "the source did not answer"})
        }
        Err(_) => json!({"ok": false, "ms": ms, "error": "the source did not answer in time"}),
    }
}

fn all_names(s: &Shared) -> Vec<String> {
    let mut v = vec![PRIMARY.to_string()];
    v.extend(s.sources.names());
    v
}

/// The sources the caller may use, each with its health (R-Y2).
pub async fn sources_org(
    State(s): State<Shared>,
    Extension(id): Extension<Identity>,
    Path(org): Path<String>,
) -> Result<Response, ApiError> {
    let org = s.org(&org)?;
    let role = role_of(&id, &org.name);
    let mut data = Vec::new();
    for name in all_names(&s) {
        if name != PRIMARY && !s.sources.permits(&name, role) {
            continue;
        }
        let Some(r) = reader_of(&s, &name) else {
            continue;
        };
        data.push(json!({"name": name, "health": health_of(&r).await}));
    }
    Ok(ok(data, meta("sources_list")))
}

pub async fn sources_platform(State(s): State<Shared>) -> Result<Response, ApiError> {
    let mut data = Vec::new();
    for name in all_names(&s) {
        let Some(r) = reader_of(&s, &name) else {
            continue;
        };
        let st = if name == PRIMARY {
            SourceState::default()
        } else {
            s.sources.state(&name)
        };
        data.push(json!({
            "name": name, "enabled": st.enabled, "min_role": st.min_role.name(),
            "health": health_of(&r).await,
        }));
    }
    Ok(ok(data, meta("platform_sources")))
}

#[derive(Deserialize)]
pub struct SourcePatch {
    enabled: Option<bool>,
    min_role: Option<String>,
}

pub async fn source_put(
    State(s): State<Shared>,
    Path(name): Path<String>,
    Json(b): Json<SourcePatch>,
) -> Result<Response, ApiError> {
    if name == PRIMARY {
        return Err(ApiError::bad("the primary source cannot be changed"));
    }
    if s.sources.get(&name).is_none() {
        return Err(ApiError::not_found("no such source"));
    }
    let mut st = s.sources.state(&name);
    if let Some(e) = b.enabled {
        st.enabled = e;
    }
    if let Some(r) = &b.min_role {
        st.min_role = parse_role(r)?;
    }
    let now = s.now_ms();
    let created = items::get(&s, PLATFORM_ORG, SOURCE_KIND, &name)
        .await?
        .map(|r| r.created_ms)
        .unwrap_or(now);
    let m = items::put(
        &s,
        PLATFORM_ORG,
        SOURCE_KIND,
        &items::row(&name, &name, &st, "platform", created, now),
    )
    .await?;
    // Takes effect now, without a restart; other replicas pick it up within seconds.
    s.sources.set_state(&name, Some(st));
    Ok(ok(
        json!({"name": name, "enabled": st.enabled, "min_role": st.min_role.name()}),
        m,
    ))
}

// ---- fan-out -----------------------------------------------------------------------------------------

/// Run `q` on the sources named by `spec` (`all` or `a,b`) and merge by `key`, newest first.
/// A source that fails is reported, not fatal; only when every source fails is the read an error.
pub async fn fan_out(
    s: &Shared,
    id: &Identity,
    org: &Org,
    spec: &str,
    q: Q,
    (key, limit): (&str, usize),
    note: Option<String>,
) -> Result<Response, ApiError> {
    let role = role_of(id, &org.name);
    let names: Vec<String> = if spec == "all" {
        all_names(s)
            .into_iter()
            .filter(|n| n == PRIMARY || s.sources.permits(n, role))
            .collect()
    } else {
        let mut v: Vec<String> = Vec::new();
        for n in spec.split(',').map(str::trim).filter(|n| !n.is_empty()) {
            if reader_of(s, n).is_none() {
                return Err(ApiError::not_found(format!("no such source: {n}")));
            }
            if n != PRIMARY && !s.sources.permits(n, role) {
                return Err(ApiError(
                    StatusCode::FORBIDDEN,
                    format!("you cannot use source {n} (it is disabled or needs a higher role)"),
                ));
            }
            if !v.iter().any(|x| x == n) {
                v.push(n.to_string());
            }
        }
        v
    };
    if names.is_empty() || names.len() > 16 {
        return Err(ApiError::bad("name between 1 and 16 sources"));
    }
    let t = Instant::now();
    let jobs = names.iter().filter_map(|n| {
        let r = reader_of(s, n)?;
        let q = q.clone();
        let n = n.clone();
        Some(async move {
            let t = Instant::now();
            let res = r.fetch(&q).await;
            (n, t.elapsed().as_millis() as u64, res)
        })
    });
    let done = futures_util::future::join_all(jobs).await;
    let (mut rows, mut reports, mut failed) = (Vec::new(), Vec::new(), 0);
    for (name, ms, res) in done {
        match res {
            Ok(mut r) => {
                reports.push(json!({"name": name, "ok": true, "rows": r.len(), "ms": ms}));
                for x in &mut r {
                    if let Some(o) = x.as_object_mut() {
                        o.insert("_source".into(), Value::String(name.clone()));
                    }
                }
                rows.append(&mut r);
            }
            Err(e) => {
                failed += 1;
                let msg = match &e {
                    ReadError::Quota(m) => m.clone(),
                    ReadError::Timeout => "the source took too long".into(),
                    ReadError::Db(d) => {
                        tracing::warn!(source = %name, detail = %d, "source failed");
                        "the source returned an error".into()
                    }
                };
                reports.push(json!({"name": name, "ok": false, "ms": ms, "error": msg}));
            }
        }
    }
    if failed == reports.len() {
        return Err(ApiError(
            StatusCode::BAD_GATEWAY,
            "every source failed; see /sources for their health".into(),
        ));
    }
    let rows = merge_desc(rows, key, limit);
    let m = Meta {
        query: q.name.to_string(),
        sql: q.sql.clone(),
        binds: q.binds.iter().map(query::Bind::show).collect(),
        ms: t.elapsed().as_millis() as u64,
        rows: rows.len(),
        source: "raw",
        note: note.or_else(|| Some(format!("merged from {} sources", reports.len()))),
    };
    let n = m.rows;
    let mut r = Json(json!({"data": rows, "meta": m, "sources": reports})).into_response();
    if let Ok(v) = header::HeaderValue::from_str(&n.to_string()) {
        r.headers_mut().insert("x-grepui-rows", v);
    }
    Ok(r)
}
