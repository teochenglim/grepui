//! `/metrics` (R-O2): Prometheus text for grepui itself. Counters are atomics or small maps, so
//! counting never waits on a lock held across an await.

use std::{
    collections::BTreeMap,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering::Relaxed},
    },
    time::Instant,
};

/// Upper bounds of the database latency histogram, in milliseconds.
pub const BUCKETS_MS: [u64; 10] = [5, 10, 25, 50, 100, 250, 500, 1000, 2500, 5000];

pub struct Metrics {
    started: Instant,
    requests: Mutex<BTreeMap<(String, u16), u64>>,
    db_buckets: [AtomicU64; BUCKETS_MS.len()],
    db_count: AtomicU64,
    db_errors: AtomicU64,
    db_sum_ms: AtomicU64,
    pub cache_hits: AtomicU64,
    pub cache_misses: AtomicU64,
    pub cache_joined: AtomicU64,
    pub cache_evictions: AtomicU64,
    alert_ok: AtomicU64,
    alert_err: AtomicU64,
}

impl Default for Metrics {
    fn default() -> Self {
        Metrics {
            started: Instant::now(),
            requests: Mutex::new(BTreeMap::new()),
            db_buckets: Default::default(),
            db_count: Default::default(),
            db_errors: Default::default(),
            db_sum_ms: Default::default(),
            cache_hits: Default::default(),
            cache_misses: Default::default(),
            cache_joined: Default::default(),
            cache_evictions: Default::default(),
            alert_ok: Default::default(),
            alert_err: Default::default(),
        }
    }
}

fn label(v: &str) -> String {
    v.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

impl Metrics {
    /// One finished request: the matched route template (never the raw path) and the status.
    pub fn request(&self, template: &str, status: u16) {
        let t = if template.is_empty() {
            "unmatched"
        } else {
            template
        };
        if let Ok(mut m) = self.requests.lock() {
            // Routes and statuses are few; the cap only guards against a surprise.
            if m.len() < 5000 || m.contains_key(&(t.to_string(), status)) {
                *m.entry((t.to_string(), status)).or_insert(0) += 1;
            }
        }
    }

    pub fn db(&self, ms: u64, ok: bool) {
        self.db_count.fetch_add(1, Relaxed);
        self.db_sum_ms.fetch_add(ms, Relaxed);
        if !ok {
            self.db_errors.fetch_add(1, Relaxed);
        }
        if let Some(i) = BUCKETS_MS.iter().position(|b| ms <= *b) {
            self.db_buckets[i].fetch_add(1, Relaxed);
        }
    }

    pub fn alert_evaluation(&self, ok: bool) {
        if ok { &self.alert_ok } else { &self.alert_err }.fetch_add(1, Relaxed);
    }

    pub fn render(&self) -> String {
        let mut o = String::new();
        let mut put = |s: String| {
            o.push_str(&s);
            o.push('\n');
        };
        put("# HELP grepui_build_info grepui version.".into());
        put("# TYPE grepui_build_info gauge".into());
        put(format!(
            "grepui_build_info{{version=\"{}\"}} 1",
            label(crate::VERSION)
        ));
        put("# HELP grepui_uptime_seconds Seconds since the process started.".into());
        put("# TYPE grepui_uptime_seconds gauge".into());
        put(format!(
            "grepui_uptime_seconds {}",
            self.started.elapsed().as_secs()
        ));
        put("# HELP grepui_http_requests_total Requests by route template and status.".into());
        put("# TYPE grepui_http_requests_total counter".into());
        if let Ok(m) = self.requests.lock() {
            for ((t, c), n) in m.iter() {
                put(format!(
                    "grepui_http_requests_total{{route=\"{}\",code=\"{c}\"}} {n}",
                    label(t)
                ));
            }
        }
        put("# HELP grepui_db_query_duration_seconds Reads sent to GreptimeDB.".into());
        put("# TYPE grepui_db_query_duration_seconds histogram".into());
        let mut cum = 0;
        for (i, b) in BUCKETS_MS.iter().enumerate() {
            cum += self.db_buckets[i].load(Relaxed);
            put(format!(
                "grepui_db_query_duration_seconds_bucket{{le=\"{}\"}} {cum}",
                *b as f64 / 1000.0
            ));
        }
        let n = self.db_count.load(Relaxed);
        put(format!(
            "grepui_db_query_duration_seconds_bucket{{le=\"+Inf\"}} {n}"
        ));
        put(format!(
            "grepui_db_query_duration_seconds_sum {}",
            self.db_sum_ms.load(Relaxed) as f64 / 1000.0
        ));
        put(format!("grepui_db_query_duration_seconds_count {n}"));
        put("# HELP grepui_db_query_errors_total Reads that failed.".into());
        put("# TYPE grepui_db_query_errors_total counter".into());
        put(format!(
            "grepui_db_query_errors_total {}",
            self.db_errors.load(Relaxed)
        ));
        for (name, help, v) in [
            (
                "grepui_cache_hits_total",
                "Reads answered from the result cache.",
                &self.cache_hits,
            ),
            (
                "grepui_cache_misses_total",
                "Reads that went to the database.",
                &self.cache_misses,
            ),
            (
                "grepui_cache_joined_total",
                "Identical concurrent reads that waited for one run (single-flight).",
                &self.cache_joined,
            ),
            (
                "grepui_cache_evictions_total",
                "Entries dropped to stay in bounds.",
                &self.cache_evictions,
            ),
        ] {
            put(format!("# HELP {name} {help}"));
            put(format!("# TYPE {name} counter"));
            put(format!("{name} {}", v.load(Relaxed)));
        }
        put("# HELP grepui_alert_evaluations_total Alert rule evaluations.".into());
        put("# TYPE grepui_alert_evaluations_total counter".into());
        put(format!(
            "grepui_alert_evaluations_total{{result=\"ok\"}} {}",
            self.alert_ok.load(Relaxed)
        ));
        put(format!(
            "grepui_alert_evaluations_total{{result=\"error\"}} {}",
            self.alert_err.load(Relaxed)
        ));
        o
    }
}
