//! A short-lived result cache with single-flight (R-O1, ADR 0014), in front of the governed reader.
//!
//! Only reads made for a signed-in caller are cached, keyed by source, org, role, statement and
//! binds (so a viewer never gets a result made for an admin). Identical reads that arrive while
//! one is running wait for it instead of running again. Memory is bounded by an entry count and an
//! estimate of bytes; the oldest entries go first. Reads of grepui's own tables, the process list
//! and health probes are never cached.

use crate::{
    error::ReadError,
    metrics::Metrics,
    query::{Bind, Q, Reader},
    workload::CTX,
};
use async_trait::async_trait;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex, atomic::Ordering::Relaxed},
    time::{Duration, Instant},
};
use tokio::sync::watch;

type Rows = Arc<Vec<Value>>;

#[derive(Clone)]
enum Fail {
    Timeout,
    Quota(String),
    Db(String),
}

impl From<&ReadError> for Fail {
    fn from(e: &ReadError) -> Fail {
        match e {
            ReadError::Timeout => Fail::Timeout,
            ReadError::Quota(m) => Fail::Quota(m.clone()),
            ReadError::Db(d) => Fail::Db(d.clone()),
        }
    }
}

impl From<Fail> for ReadError {
    fn from(f: Fail) -> ReadError {
        match f {
            Fail::Timeout => ReadError::Timeout,
            Fail::Quota(m) => ReadError::Quota(m),
            Fail::Db(d) => ReadError::Db(d),
        }
    }
}

type Outcome = Option<Result<Rows, Fail>>;

struct Entry {
    at: Instant,
    rows: Rows,
    bytes: usize,
}

#[derive(Default)]
struct Store {
    map: HashMap<String, Entry>,
    order: VecDeque<String>,
    bytes: usize,
}

pub struct ResultCache {
    ttl: Duration,
    max_entries: usize,
    max_bytes: usize,
    store: Mutex<Store>,
    flights: Mutex<HashMap<String, watch::Sender<Outcome>>>,
    m: Arc<Metrics>,
}

/// A rough size of a result: string bytes plus a fixed cost per value.
fn approx(v: &Value) -> usize {
    match v {
        Value::String(s) => 24 + s.len(),
        Value::Array(a) => 24 + a.iter().map(approx).sum::<usize>(),
        Value::Object(o) => 48 + o.iter().map(|(k, v)| k.len() + approx(v)).sum::<usize>(),
        _ => 16,
    }
}

fn key(ns: &str, org: &str, role: &str, q: &Q, max: Option<usize>) -> String {
    let mut h = Sha256::new();
    for part in [ns, org, role, &format!("{max:?}"), &q.sql] {
        h.update(part.as_bytes());
        h.update([0]);
    }
    for b in &q.binds {
        let tag = match b {
            Bind::Str(_) => 's',
            Bind::Int(_) => 'i',
            Bind::Bool(_) => 'b',
            Bind::Float(_) => 'f',
            Bind::Null => 'n',
        };
        h.update([tag as u8]);
        h.update(b.show().as_bytes());
        h.update([0]);
    }
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

impl ResultCache {
    pub fn new(
        ttl: Duration,
        max_entries: usize,
        max_bytes: usize,
        m: Arc<Metrics>,
    ) -> ResultCache {
        ResultCache {
            ttl,
            max_entries,
            max_bytes,
            store: Mutex::new(Store::default()),
            flights: Mutex::new(HashMap::new()),
            m,
        }
    }

    pub fn enabled(&self) -> bool {
        !self.ttl.is_zero()
    }

    pub fn len(&self) -> usize {
        self.store.lock().map(|s| s.map.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn bytes(&self) -> usize {
        self.store.lock().map(|s| s.bytes).unwrap_or(0)
    }

    fn get(&self, k: &str) -> Option<Rows> {
        let mut s = self.store.lock().ok()?;
        match s.map.get(k) {
            Some(e) if e.at.elapsed() < self.ttl => Some(e.rows.clone()),
            Some(_) => {
                if let Some(e) = s.map.remove(k) {
                    s.bytes -= e.bytes;
                }
                None
            }
            None => None,
        }
    }

    fn put(&self, k: String, rows: Rows) {
        let bytes: usize = rows.iter().map(approx).sum::<usize>() + 64;
        // A result that would crowd out the rest is not worth keeping.
        if bytes > self.max_bytes / 8 {
            return;
        }
        let Ok(mut s) = self.store.lock() else { return };
        if let Some(old) = s.map.remove(&k) {
            s.bytes -= old.bytes;
        } else {
            s.order.push_back(k.clone());
        }
        s.bytes += bytes;
        s.map.insert(
            k,
            Entry {
                at: Instant::now(),
                rows,
                bytes,
            },
        );
        while s.map.len() > self.max_entries || s.bytes > self.max_bytes {
            let Some(old) = s.order.pop_front() else {
                break;
            };
            if let Some(e) = s.map.remove(&old) {
                s.bytes -= e.bytes;
                self.m.cache_evictions.fetch_add(1, Relaxed);
            }
        }
        // Keep `order` from growing with keys that were replaced or expired.
        if s.order.len() > s.map.len() * 2 + 16 {
            let live: VecDeque<String> = s
                .order
                .iter()
                .filter(|k| s.map.contains_key(*k))
                .cloned()
                .collect();
            s.order = live;
        }
    }
}

/// Sends a failure to the waiting followers if the leader goes away without an answer.
struct Lead<'a> {
    cache: &'a ResultCache,
    k: String,
    tx: Option<watch::Sender<Outcome>>,
}

impl Lead<'_> {
    fn finish(&mut self, r: Result<Rows, Fail>) {
        if let Ok(mut f) = self.cache.flights.lock() {
            f.remove(&self.k);
        }
        if let Some(tx) = self.tx.take() {
            let _ = tx.send(Some(r));
        }
    }
}

impl Drop for Lead<'_> {
    fn drop(&mut self) {
        if self.tx.is_some() {
            self.finish(Err(Fail::Db(
                "the read this one was waiting for was cancelled".into(),
            )));
        }
    }
}

pub struct Cached {
    inner: Arc<dyn Reader>,
    cache: Arc<ResultCache>,
    ns: String,
}

impl Cached {
    pub fn new(inner: Arc<dyn Reader>, cache: Arc<ResultCache>, ns: &str) -> Cached {
        Cached {
            inner,
            cache,
            ns: ns.into(),
        }
    }

    async fn go(&self, q: &Q, max: Option<usize>) -> Result<Vec<Value>, ReadError> {
        let ctx = CTX.try_with(|c| c.clone()).ok();
        let skip = q.sql.contains("`grepui_") || matches!(q.name, "process_list" | "health");
        let Some(ctx) = ctx.filter(|_| self.cache.enabled() && !skip) else {
            return self.run(q, max).await;
        };
        let k = key(&self.ns, &ctx.org, ctx.role.name(), q, max);
        if let Some(rows) = self.cache.get(&k) {
            self.cache.m.cache_hits.fetch_add(1, Relaxed);
            return Ok(rows.as_ref().clone());
        }
        // Join a read already running for the same key, or become the one that runs it. The lock
        // is released before anything is awaited.
        enum Role {
            Lead(watch::Sender<Outcome>),
            Join(watch::Receiver<Outcome>),
            Alone,
        }
        let role = match self.cache.flights.lock() {
            Ok(mut f) => match f.get(&k) {
                Some(tx) => Role::Join(tx.subscribe()),
                None => {
                    let (tx, _) = watch::channel(None);
                    f.insert(k.clone(), tx.clone());
                    Role::Lead(tx)
                }
            },
            Err(_) => Role::Alone,
        };
        match role {
            Role::Alone => self.run(q, max).await,
            Role::Lead(tx) => {
                let mut lead = Lead {
                    cache: &self.cache,
                    k: k.clone(),
                    tx: Some(tx),
                };
                self.cache.m.cache_misses.fetch_add(1, Relaxed);
                let res = self.run(q, max).await;
                let shared = res
                    .as_ref()
                    .map(|r| Arc::new(r.clone()))
                    .map_err(Fail::from);
                if let Ok(rows) = &shared {
                    self.cache.put(k, rows.clone());
                }
                lead.finish(shared);
                res
            }
            Role::Join(mut rx) => {
                self.cache.m.cache_joined.fetch_add(1, Relaxed);
                let seen = rx
                    .wait_for(|v| v.is_some())
                    .await
                    .map_err(|_| ReadError::Db("the read this one was waiting for ended".into()))?
                    .clone();
                match seen {
                    Some(Ok(rows)) => Ok(rows.as_ref().clone()),
                    Some(Err(f)) => Err(f.into()),
                    None => Err(ReadError::Db("no result".into())),
                }
            }
        }
    }

    async fn run(&self, q: &Q, max: Option<usize>) -> Result<Vec<Value>, ReadError> {
        match max {
            Some(m) => self.inner.fetch_max(q, m).await,
            None => self.inner.fetch(q).await,
        }
    }
}

#[async_trait]
impl Reader for Cached {
    async fn fetch(&self, q: &Q) -> Result<Vec<Value>, ReadError> {
        self.go(q, None).await
    }
    async fn fetch_max(&self, q: &Q, max: usize) -> Result<Vec<Value>, ReadError> {
        self.go(q, Some(max)).await
    }
    async fn kill(&self, id: &str) -> Result<(), ReadError> {
        self.inner.kill(id).await
    }
}
