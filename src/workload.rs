//! Query management (v0.12.0, ADR 0013): work groups, running queries, statistics and sources.
//!
//! One wrapper, [`Governed`], sits around every reader. A request runs inside a task-local
//! [`Ctx`] (who, which org, which role) that the request layer sets, so no handler has to
//! remember to ask: a read made for a caller is checked against the caller's work group before
//! it runs, listed while it runs, cancellable, and counted when it ends. Reads made outside a
//! request (the alert engine, startup) carry no context and pass straight through, as do reads
//! of grepui's own `grepui_*` tables.

use crate::{
    auth::Role,
    error::ReadError,
    query::{Q, Reader},
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicU64, Ordering},
    },
    time::Instant,
};
use tokio::sync::Notify;

/// The primary source: the one `--greptime-mysql` names. It cannot be disabled.
pub const PRIMARY: &str = "primary";
const MAX_STAT_KEYS: usize = 5000;
const MAX_SQL_SHOWN: usize = 600;

/// Limits for one role in one org. `0` means no limit.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Group {
    #[serde(default)]
    pub max_concurrent: u32,
    #[serde(default)]
    pub timeout_secs: u32,
    #[serde(default)]
    pub max_rows: u32,
    #[serde(default)]
    pub max_window_hours: u32,
}

impl Group {
    pub fn validate(&self) -> Result<(), String> {
        for (v, max, name) in [
            (self.max_concurrent, 1000, "max_concurrent"),
            (self.timeout_secs, 600, "timeout_secs"),
            (self.max_rows, 1_000_000, "max_rows"),
            (self.max_window_hours, 24 * 31, "max_window_hours"),
        ] {
            if v > max {
                return Err(format!("{name} can be at most {max} (0 means no limit)"));
            }
        }
        Ok(())
    }
}

/// Who a read is made for, set by the request layer around the handler.
#[derive(Debug, Clone)]
pub struct Ctx {
    pub user: String,
    pub org: String,
    pub role: Role,
}

tokio::task_local! {
    pub static CTX: Ctx;
}

struct Running {
    user: String,
    org: String,
    tag: &'static str,
    sql: String,
    norm: String,
    start_ms: i64,
    started: Instant,
    cancel: Arc<Notify>,
}

#[derive(Debug, Clone, Default)]
struct Stat {
    count: u64,
    errors: u64,
    total_ms: u64,
    max_ms: u64,
}

/// A running query as shown to the caller.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct RunningView {
    pub id: u64,
    pub user: String,
    pub org: String,
    pub tag: String,
    pub started_ms: i64,
    pub elapsed_ms: u64,
    pub sql: String,
}

/// Work groups, the running list and statistics, shared by every reader of the process.
#[derive(Default)]
pub struct Workload {
    groups: RwLock<HashMap<(String, String), Group>>,
    running: Mutex<HashMap<u64, Running>>,
    stats: Mutex<HashMap<(String, String, String), Stat>>,
    next: AtomicU64,
}

/// Lower-case, comment-free, space-collapsed text without quotes: how a statement from grepui
/// and the same statement in GreptimeDB's process list are compared.
pub fn normalize(sql: &str) -> String {
    let mut out = String::with_capacity(sql.len());
    let b: Vec<char> = sql.chars().collect();
    let mut i = 0;
    while i < b.len() {
        if b[i] == '/' && b.get(i + 1) == Some(&'*') {
            i += 2;
            while i + 1 < b.len() && !(b[i] == '*' && b[i + 1] == '/') {
                i += 1;
            }
            i += 2;
            continue;
        }
        let c = b[i];
        i += 1;
        if c == '`' || c == '"' || c == '\'' || c == '(' || c == ')' || c == ',' {
            continue;
        }
        if c.is_whitespace() {
            if !out.ends_with(' ') && !out.is_empty() {
                out.push(' ');
            }
        } else {
            out.extend(c.to_lowercase());
        }
    }
    out.trim().to_string()
}

impl Workload {
    pub fn set_group(&self, org: &str, role: &str, g: Option<Group>) {
        if let Ok(mut m) = self.groups.write() {
            match g {
                Some(g) => m.insert((org.into(), role.into()), g),
                None => m.remove(&(org.to_string(), role.to_string())),
            };
        }
    }

    /// Replace everything known for `org` (a reload from the store).
    pub fn replace_org(&self, org: &str, groups: Vec<(String, Group)>) {
        if let Ok(mut m) = self.groups.write() {
            m.retain(|(o, _), _| o != org);
            for (r, g) in groups {
                m.insert((org.into(), r), g);
            }
        }
    }

    pub fn group(&self, org: &str, role: Role) -> Group {
        self.groups
            .read()
            .ok()
            .and_then(|m| m.get(&(org.to_string(), role.name().to_string())).copied())
            .unwrap_or_default()
    }

    /// The window limit, checked before a request runs.
    pub fn check_window(&self, org: &str, role: Role, from: i64, to: i64) -> Result<(), String> {
        let g = self.group(org, role);
        if g.max_window_hours > 0 && to - from > g.max_window_hours as i64 * 3_600_000 {
            return Err(format!(
                "work group limit max_window_hours ({}) for {}s in this org: choose a shorter time range",
                g.max_window_hours,
                role.name()
            ));
        }
        Ok(())
    }

    fn admit(&self, c: &Ctx, q: &Q, g: Group, now_ms: i64) -> Result<Permit<'_>, ReadError> {
        let mut run = self
            .running
            .lock()
            .map_err(|_| ReadError::Db("workload lock".into()))?;
        if g.max_concurrent > 0 {
            let mine = run
                .values()
                .filter(|r| r.org == c.org && r.user == c.user)
                .count() as u32;
            if mine >= g.max_concurrent {
                return Err(ReadError::Quota(format!(
                    "work group limit max_concurrent ({}) for {}s in this org: {} queries of yours are already running",
                    g.max_concurrent,
                    c.role.name(),
                    mine
                )));
            }
        }
        let id = self.next.fetch_add(1, Ordering::Relaxed) + 1;
        let cancel = Arc::new(Notify::new());
        let mut sql = q.sql.clone();
        if sql.len() > MAX_SQL_SHOWN {
            let mut cut = MAX_SQL_SHOWN;
            while !sql.is_char_boundary(cut) {
                cut -= 1;
            }
            sql.truncate(cut);
        }
        run.insert(
            id,
            Running {
                user: c.user.clone(),
                org: c.org.clone(),
                tag: q.name,
                norm: normalize(&q.sql),
                sql,
                start_ms: now_ms,
                started: Instant::now(),
                cancel: cancel.clone(),
            },
        );
        Ok(Permit {
            w: self,
            id,
            cancel,
            user: c.user.clone(),
            org: c.org.clone(),
            tag: q.name,
            started: Instant::now(),
            failed: true,
        })
    }

    fn finish(&self, p: &Permit<'_>) {
        if let Ok(mut r) = self.running.lock() {
            r.remove(&p.id);
        }
        let ms = p.started.elapsed().as_millis() as u64;
        if let Ok(mut s) = self.stats.lock() {
            if s.len() > MAX_STAT_KEYS {
                s.clear();
            }
            let e = s
                .entry((p.org.clone(), p.user.clone(), p.tag.to_string()))
                .or_default();
            e.count += 1;
            e.errors += p.failed as u64;
            e.total_ms += ms;
            e.max_ms = e.max_ms.max(ms);
        }
    }

    /// Queries running now, newest last.
    pub fn running(&self) -> Vec<(RunningView, String)> {
        let mut v: Vec<(RunningView, String)> = self
            .running
            .lock()
            .map(|r| {
                r.iter()
                    .map(|(id, x)| {
                        (
                            RunningView {
                                id: *id,
                                user: x.user.clone(),
                                org: x.org.clone(),
                                tag: x.tag.to_string(),
                                started_ms: x.start_ms,
                                elapsed_ms: x.started.elapsed().as_millis() as u64,
                                sql: x.sql.clone(),
                            },
                            x.norm.clone(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        v.sort_by_key(|(r, _)| r.id);
        v
    }

    /// Stop a running query of `org`. `Ok(true)` when it was found and signalled.
    pub fn cancel(&self, org: &str, id: u64, only_user: Option<&str>) -> Result<bool, String> {
        let Ok(r) = self.running.lock() else {
            return Ok(false);
        };
        let Some(x) = r.get(&id).filter(|x| x.org == org) else {
            return Ok(false);
        };
        if only_user.is_some_and(|u| u != x.user) {
            return Err("only an admin can cancel another person's query".into());
        }
        x.cancel.notify_waiters();
        Ok(true)
    }

    /// Slowest and most frequent tags for `org` (`user` narrows), each at most `n`.
    pub fn stats(&self, org: &str, user: Option<&str>, n: usize) -> Value {
        let rows: Vec<(String, String, Stat)> = self
            .stats
            .lock()
            .map(|s| {
                s.iter()
                    .filter(|((o, u, _), _)| o == org && user.is_none_or(|x| x == u))
                    .map(|((_, u, t), s)| (u.clone(), t.clone(), s.clone()))
                    .collect()
            })
            .unwrap_or_default();
        let show = |(u, t, s): &(String, String, Stat)| {
            serde_json::json!({
                "user": u, "tag": t, "count": s.count, "errors": s.errors,
                "avg_ms": s.total_ms.checked_div(s.count).unwrap_or(0), "max_ms": s.max_ms,
                "total_ms": s.total_ms,
            })
        };
        let mut slow = rows.clone();
        slow.sort_by(|a, b| {
            let avg = |s: &Stat| s.total_ms.checked_div(s.count).unwrap_or(0);
            avg(&b.2).cmp(&avg(&a.2)).then(b.2.max_ms.cmp(&a.2.max_ms))
        });
        let mut freq = rows;
        freq.sort_by(|a, b| b.2.count.cmp(&a.2.count).then(a.1.cmp(&b.1)));
        serde_json::json!({
            "slowest": slow.iter().take(n).map(show).collect::<Vec<_>>(),
            "most_frequent": freq.iter().take(n).map(show).collect::<Vec<_>>(),
        })
    }
}

/// A registered running query; ends (and is counted) when dropped.
struct Permit<'a> {
    w: &'a Workload,
    id: u64,
    cancel: Arc<Notify>,
    user: String,
    org: String,
    tag: &'static str,
    started: Instant,
    failed: bool,
}

impl Drop for Permit<'_> {
    fn drop(&mut self) {
        self.w.finish(self);
    }
}

/// The reader every other part of grepui sees.
pub struct Governed {
    inner: Arc<dyn Reader>,
    w: Arc<Workload>,
    clock: crate::api::Clock,
    m: Arc<crate::metrics::Metrics>,
}

impl Governed {
    pub fn new(
        inner: Arc<dyn Reader>,
        w: Arc<Workload>,
        clock: crate::api::Clock,
        m: Arc<crate::metrics::Metrics>,
    ) -> Governed {
        Governed { inner, w, clock, m }
    }

    /// Count a read that reached (or tried to reach) the database; a limit that stopped it first
    /// is not a database read.
    fn count(&self, t: Instant, r: &Result<Vec<Value>, ReadError>) {
        if !matches!(r, Err(ReadError::Quota(_))) {
            self.m.db(
                t.elapsed().as_millis() as u64,
                !matches!(r, Err(ReadError::Db(_) | ReadError::Timeout)),
            );
        }
    }

    async fn go(&self, q: &Q, max: Option<usize>) -> Result<Vec<Value>, ReadError> {
        // Not for a caller, or grepui's own tables, or the process list itself: pass through.
        let ctx = CTX.try_with(|c| c.clone()).ok();
        let own = q.sql.contains("`grepui_") || q.name == "process_list" || q.name == "health";
        let Some(ctx) = ctx.filter(|_| !own) else {
            return match max {
                Some(m) => self.inner.fetch_max(q, m).await,
                None => self.inner.fetch(q).await,
            };
        };
        let g = self.w.group(&ctx.org, ctx.role);
        let mut permit = self.w.admit(&ctx, q, g, (self.clock)())?;
        let cap = if g.max_rows > 0 {
            Some(max.map_or(g.max_rows as usize, |m| m.min(g.max_rows as usize)) + 1)
        } else {
            max
        };
        let run = async {
            match cap {
                Some(m) => self.inner.fetch_max(q, m).await,
                None => self.inner.fetch(q).await,
            }
        };
        let timed = async {
            if g.timeout_secs > 0 {
                tokio::time::timeout(std::time::Duration::from_secs(g.timeout_secs as u64), run)
                    .await
                    .map_err(|_| {
                        ReadError::Quota(format!(
                            "work group limit timeout_secs ({}) for {}s in this org: the query was stopped",
                            g.timeout_secs,
                            ctx.role.name()
                        ))
                    })?
            } else {
                run.await
            }
        };
        let cancelled = permit.cancel.clone();
        let mut rows = tokio::select! {
            r = timed => r?,
            _ = cancelled.notified() => {
                return Err(ReadError::Quota("the query was cancelled".into()));
            }
        };
        if g.max_rows > 0 && rows.len() > g.max_rows as usize {
            return Err(ReadError::Quota(format!(
                "work group limit max_rows ({}) for {}s in this org: narrow the filter or the time range",
                g.max_rows,
                ctx.role.name()
            )));
        }
        if let Some(m) = max {
            rows.truncate(m);
        }
        permit.failed = false;
        Ok(rows)
    }
}

#[async_trait]
impl Reader for Governed {
    async fn fetch(&self, q: &Q) -> Result<Vec<Value>, ReadError> {
        let t = Instant::now();
        let r = self.go(q, None).await;
        self.count(t, &r);
        r
    }
    async fn fetch_max(&self, q: &Q, max: usize) -> Result<Vec<Value>, ReadError> {
        let t = Instant::now();
        let r = self.go(q, Some(max)).await;
        self.count(t, &r);
        r
    }
    async fn kill(&self, id: &str) -> Result<(), ReadError> {
        self.inner.kill(id).await
    }
}

// ---- sources (federation) ------------------------------------------------------------------------

/// `name=mysql://...` of `--source`.
pub fn parse_source(s: &str) -> Result<(String, String), String> {
    let (name, url) = s
        .split_once('=')
        .ok_or("a source is name=mysql://user@host:4002/db")?;
    if !crate::org::valid_name(name) || name == PRIMARY {
        return Err(format!(
            "{name:?} is not a usable source name (lowercase letters, digits, - and _; not \"{PRIMARY}\")"
        ));
    }
    let u = url::Url::parse(url).map_err(|_| format!("the URL of source {name:?} is not valid"))?;
    if u.scheme() != "mysql" {
        return Err(format!("source {name:?} must be a mysql:// URL"));
    }
    Ok((name.to_string(), url.to_string()))
}

pub struct Source {
    pub name: String,
    pub reader: Arc<dyn Reader>,
}

/// Whether a source may be used, and by whom. Kept in the state store, mirrored here.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SourceState {
    pub enabled: bool,
    pub min_role: Role,
}

impl Default for SourceState {
    fn default() -> Self {
        SourceState {
            enabled: true,
            min_role: Role::Viewer,
        }
    }
}

#[derive(Default)]
pub struct Sources {
    list: RwLock<Vec<Arc<Source>>>,
    state: RwLock<HashMap<String, SourceState>>,
}

impl Sources {
    pub fn add(&self, name: &str, reader: Arc<dyn Reader>) {
        if let Ok(mut l) = self.list.write() {
            l.retain(|s| s.name != name);
            l.push(Arc::new(Source {
                name: name.into(),
                reader,
            }));
        }
    }

    pub fn names(&self) -> Vec<String> {
        self.list
            .read()
            .map(|l| l.iter().map(|s| s.name.clone()).collect())
            .unwrap_or_default()
    }

    pub fn get(&self, name: &str) -> Option<Arc<Source>> {
        self.list
            .read()
            .ok()
            .and_then(|l| l.iter().find(|s| s.name == name).cloned())
    }

    pub fn state(&self, name: &str) -> SourceState {
        self.state
            .read()
            .ok()
            .and_then(|m| m.get(name).copied())
            .unwrap_or_default()
    }

    pub fn set_state(&self, name: &str, st: Option<SourceState>) {
        if let Ok(mut m) = self.state.write() {
            match st {
                Some(st) => m.insert(name.into(), st),
                None => m.remove(name),
            };
        }
    }

    pub fn replace_states(&self, all: HashMap<String, SourceState>) {
        if let Ok(mut m) = self.state.write() {
            *m = all;
        }
    }

    /// Can a caller with `role` use `name` right now?
    pub fn permits(&self, name: &str, role: Role) -> bool {
        let st = self.state(name);
        st.enabled && role >= st.min_role
    }
}

/// Merge per-source rows into one list ordered by `key` (a number), newest first, at most `limit`.
pub fn merge_desc(mut rows: Vec<Value>, key: &str, limit: usize) -> Vec<Value> {
    let k = |v: &Value| v.get(key).and_then(Value::as_i64).unwrap_or(i64::MIN);
    rows.sort_by_key(|a| std::cmp::Reverse(k(a)));
    rows.truncate(limit);
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_ignores_comments_quotes_case_and_spacing() {
        assert_eq!(
            normalize("/* q:x */ SELECT `a`,  b\nFROM `d`.`t` WHERE x = 'y'"),
            normalize("select a, b from d.t where x = y")
        );
    }

    #[test]
    fn source_names_are_validated() {
        assert!(parse_source("eu=mysql://root@h:4002/db").is_ok());
        assert!(parse_source("primary=mysql://h/db").is_err());
        assert!(parse_source("Eu=mysql://h/db").is_err());
        assert!(parse_source("eu=http://h/db").is_err());
        assert!(parse_source("nonsense").is_err());
    }

    #[test]
    fn merge_orders_by_key_descending() {
        let r = vec![
            serde_json::json!({"t": 1}),
            serde_json::json!({"t": 3}),
            serde_json::json!({"t": 2}),
        ];
        let m = merge_desc(r, "t", 2);
        assert_eq!(
            m,
            vec![serde_json::json!({"t": 3}), serde_json::json!({"t": 2})]
        );
    }
}
