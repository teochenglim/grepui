//! Deterministic demo telemetry: the same seed always yields the same traces and logs.
//! A small shop: frontend calls cart and checkout; checkout calls payment and inventory;
//! cart and inventory call postgres. About 12% of payment calls fail, and failures
//! propagate to some callers, so error and latency views have something to show.

pub const SERVICES: &[&str] = &[
    "frontend",
    "cart",
    "checkout",
    "payment",
    "inventory",
    "postgres",
];

pub const KIND_SERVER: i32 = 2;
pub const KIND_CLIENT: i32 = 3;

/// xorshift64*: tiny, fast, good enough for demo data, no dependency.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }
    pub fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    /// Uniform in [0, 1).
    pub fn f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
    pub fn range(&mut self, lo: u64, hi: u64) -> u64 {
        lo + self.next_u64() % (hi - lo + 1)
    }
    pub fn chance(&mut self, p: f64) -> bool {
        self.f64() < p
    }
    /// exp(mu + sigma * N(0,1)) by Box-Muller.
    pub fn lognormal(&mut self, mu: f64, sigma: f64) -> f64 {
        let (u1, u2) = (self.f64().max(1e-12), self.f64());
        (mu + sigma * (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()).exp()
    }
    pub fn bytes<const N: usize>(&mut self) -> [u8; N] {
        let mut out = [0u8; N];
        for chunk in out.chunks_mut(8) {
            let v = self.next_u64().to_le_bytes();
            chunk.copy_from_slice(&v[..chunk.len()]);
        }
        out
    }
    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[(self.next_u64() % items.len() as u64) as usize]
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum AttrVal {
    Str(String),
    Int(i64),
}

#[derive(Debug, Clone, PartialEq)]
pub struct SpanRec {
    pub service: &'static str,
    pub trace_id: [u8; 16],
    pub span_id: [u8; 8],
    pub parent: Option<[u8; 8]>,
    pub name: String,
    pub kind: i32,
    pub start_ns: u64,
    pub end_ns: u64,
    pub attrs: Vec<(String, AttrVal)>,
    pub error: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LogRec {
    pub service: &'static str,
    pub ts_ns: u64,
    pub severity_number: i32,
    pub severity_text: &'static str,
    pub body: String,
    pub attrs: Vec<(String, AttrVal)>,
    pub trace_id: [u8; 16],
    pub span_id: [u8; 8],
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Batch {
    pub spans: Vec<SpanRec>,
    pub logs: Vec<LogRec>,
}

fn operations(service: &str) -> &'static [&'static str] {
    match service {
        "frontend" => &["GET /", "GET /product/{id}", "POST /checkout"],
        "cart" => &["GET /cart", "POST /cart/items"],
        "checkout" => &["PlaceOrder", "ValidateCart"],
        "payment" => &["Charge", "Refund"],
        "inventory" => &["Reserve", "Lookup"],
        _ => &["SELECT carts", "INSERT orders", "UPDATE stock"],
    }
}

fn callees(service: &str) -> &'static [&'static str] {
    match service {
        "frontend" => &["cart", "checkout"],
        "checkout" => &["payment", "inventory"],
        "cart" | "inventory" => &["postgres"],
        _ => &[],
    }
}

/// Generate one trace starting at `start_ns` into `out`; returns the end time of the root span.
fn walk(
    rng: &mut Rng,
    out: &mut Batch,
    service: &'static str,
    trace: [u8; 16],
    parent: Option<[u8; 8]>,
    start: u64,
    failing: bool,
) -> u64 {
    let span_id: [u8; 8] = rng.bytes();
    let dur = rng.lognormal(16.5, 0.7) as u64; // median about 15 ms
    let fails = failing || (service == "payment" && rng.chance(0.12));
    let mut t = start + rng.range(0, 2_000_000);
    for child in callees(service) {
        if rng.chance(0.8) {
            let child_fails = fails && rng.chance(0.7);
            // Calls run one after another, so a parent always spans its children.
            t = walk(rng, out, child, trace, Some(span_id), t, child_fails)
                + rng.range(1_000_000, 8_000_000);
        }
    }
    let end = (t + dur).max(start + dur);
    let op = *rng.pick(operations(service));
    out.spans.push(SpanRec {
        service,
        trace_id: trace,
        span_id,
        parent,
        name: op.to_string(),
        kind: if service == "postgres" {
            KIND_CLIENT
        } else {
            KIND_SERVER
        },
        start_ns: start,
        end_ns: end,
        attrs: vec![
            ("http.route".into(), AttrVal::Str(op.to_string())),
            (
                "http.status_code".into(),
                AttrVal::Int(if fails { 500 } else { 200 }),
            ),
        ],
        error: fails,
    });
    let (sev_n, sev_t) = if fails {
        (17, "ERROR")
    } else if rng.chance(0.08) {
        (13, "WARN")
    } else {
        (9, "INFO")
    };
    out.logs.push(LogRec {
        service,
        ts_ns: start + 1_000,
        severity_number: sev_n,
        severity_text: sev_t,
        body: format!(
            "{op} {} in {} ms",
            if fails {
                "failed: upstream timeout"
            } else {
                "completed"
            },
            dur / 1_000_000
        ),
        attrs: vec![(
            "http.method".into(),
            AttrVal::Str(
                op.split(' ')
                    .next()
                    .filter(|_| op.contains(' '))
                    .unwrap_or("RPC")
                    .to_string(),
            ),
        )],
        trace_id: trace,
        span_id,
    });
    end
}

/// `traces` traces spread over the `minutes` before `now_ns`.
pub fn generate(seed: u64, traces: usize, now_ns: u64, minutes: u64) -> Batch {
    let mut rng = Rng::new(seed);
    let mut out = Batch::default();
    for _ in 0..traces {
        let back = rng.range(0, minutes.max(1) * 60) * 1_000_000_000;
        let trace: [u8; 16] = rng.bytes();
        walk(
            &mut rng,
            &mut out,
            "frontend",
            trace,
            None,
            now_ns.saturating_sub(back),
            false,
        );
    }
    out
}

// ---- metrics ----------------------------------------------------------------------------

/// One OTLP data point of a demo metric.
#[derive(Debug, Clone, PartialEq)]
pub struct MetricPoint {
    pub service: &'static str,
    pub name: &'static str,
    /// A cumulative monotonic sum (counter) when true, a gauge otherwise.
    pub counter: bool,
    pub attrs: Vec<(String, AttrVal)>,
    pub ts_ns: u64,
    pub value: f64,
}

pub const COUNTER: &str = "http_server_request_count";
pub const GAUGE: &str = "process_cpu_utilization";
/// Seconds between demo data points.
pub const METRIC_STEP_S: u64 = 15;
const COUNTER_EPOCH_S: f64 = 1_700_000_000.0;

fn base_rate(service: &str) -> f64 {
    match service {
        "frontend" => 20.0,
        "cart" => 12.0,
        "checkout" => 8.0,
        "payment" => 6.0,
        "inventory" => 5.0,
        _ => 15.0,
    }
}

fn error_share(service: &str) -> f64 {
    if service == "payment" { 0.12 } else { 0.02 }
}

/// The counter value at `t_s` seconds: the integral of a rate that wobbles around its base,
/// so it only ever grows and is the same number whenever it is asked for (the live trickle
/// continues the series the seed started).
pub fn counter_at(service: &str, error: bool, t_s: u64) -> f64 {
    let share = error_share(service);
    let base = base_rate(service) * if error { share } else { 1.0 - share };
    let t = t_s as f64 - COUNTER_EPOCH_S;
    let period = 1800.0;
    let w = std::f64::consts::TAU / period;
    (base * (t + 0.3 * (1.0 - (w * t).cos()) / w)).floor()
}

/// CPU utilisation in 0..1 at `t_s`: a slow wave per service plus a small deterministic wobble.
pub fn gauge_at(service: &str, t_s: u64) -> f64 {
    let phase = service.bytes().map(f64::from).sum::<f64>();
    let wobble = (t_s.wrapping_mul(2_654_435_761) % 1000) as f64 / 1000.0 * 0.04;
    let v = 0.35 + 0.2 * (t_s as f64 / 900.0 + phase).sin() + wobble;
    (v * 1000.0).round() / 1000.0
}

/// Data points every [`METRIC_STEP_S`] seconds in `[from_ns, to_ns)`, aligned to the step.
pub fn generate_metrics(from_ns: u64, to_ns: u64) -> Vec<MetricPoint> {
    let step = METRIC_STEP_S * 1_000_000_000;
    let mut out = vec![];
    let mut t = from_ns.div_ceil(step) * step;
    while t < to_ns {
        let t_s = t / 1_000_000_000;
        for svc in SERVICES {
            for (class, error) in [("2xx", false), ("5xx", true)] {
                out.push(MetricPoint {
                    service: svc,
                    name: COUNTER,
                    counter: true,
                    attrs: vec![("http.status_class".into(), AttrVal::Str(class.into()))],
                    ts_ns: t,
                    value: counter_at(svc, error, t_s),
                });
            }
            out.push(MetricPoint {
                service: svc,
                name: GAUGE,
                counter: false,
                attrs: vec![],
                ts_ns: t,
                value: gauge_at(svc, t_s),
            });
        }
        t += step;
    }
    out
}
