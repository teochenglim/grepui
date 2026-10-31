//! A minimal protobuf encoder for the OTLP messages GreptimeDB accepts (it takes protobuf
//! only). Field numbers follow opentelemetry-proto: ExportTraceServiceRequest and
//! ExportLogsServiceRequest.

use super::synth::{AttrVal, Batch, LogRec, SERVICES, SpanRec};

pub fn varint(mut n: u64) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let b = (n & 0x7F) as u8;
        n >>= 7;
        out.push(if n == 0 { b } else { b | 0x80 });
        if n == 0 {
            return out;
        }
    }
}

pub fn f_bytes(num: u32, b: &[u8]) -> Vec<u8> {
    let mut out = varint(u64::from(num) << 3 | 2);
    out.extend(varint(b.len() as u64));
    out.extend_from_slice(b);
    out
}

pub fn f_str(num: u32, s: &str) -> Vec<u8> {
    f_bytes(num, s.as_bytes())
}

pub fn f_var(num: u32, n: u64) -> Vec<u8> {
    let mut out = varint(u64::from(num) << 3);
    out.extend(varint(n));
    out
}

pub fn f_fix64(num: u32, n: u64) -> Vec<u8> {
    let mut out = varint(u64::from(num) << 3 | 1);
    out.extend_from_slice(&n.to_le_bytes());
    out
}

/// KeyValue { key = 1, value = 2 AnyValue { string_value = 1, int_value = 3 } }
fn attr(k: &str, v: &AttrVal) -> Vec<u8> {
    let any = match v {
        AttrVal::Str(s) => f_str(1, s),
        AttrVal::Int(i) => f_var(3, *i as u64),
    };
    [f_str(1, k), f_bytes(2, &any)].concat()
}

fn resource(service: &str) -> Vec<u8> {
    let attrs = [
        f_bytes(1, &attr("service.name", &AttrVal::Str(service.into()))),
        f_bytes(1, &attr("service.version", &AttrVal::Str("1.4.2".into()))),
    ]
    .concat();
    f_bytes(1, &attrs)
}

fn scope() -> Vec<u8> {
    f_bytes(1, &f_str(1, "demo"))
}

fn span(s: &SpanRec) -> Vec<u8> {
    [
        f_bytes(1, &s.trace_id),
        f_bytes(2, &s.span_id),
        f_bytes(4, s.parent.as_ref().map(|p| p.as_slice()).unwrap_or(&[])),
        f_str(5, &s.name),
        f_var(6, s.kind as u64),
        f_fix64(7, s.start_ns),
        f_fix64(8, s.end_ns),
        s.attrs
            .iter()
            .flat_map(|(k, v)| f_bytes(9, &attr(k, v)))
            .collect::<Vec<u8>>(),
        f_bytes(15, &f_var(3, if s.error { 2 } else { 1 })),
    ]
    .concat()
}

fn log(l: &LogRec) -> Vec<u8> {
    [
        f_fix64(1, l.ts_ns),
        f_var(2, l.severity_number as u64),
        f_str(3, l.severity_text),
        f_bytes(5, &f_str(1, &l.body)),
        l.attrs
            .iter()
            .flat_map(|(k, v)| f_bytes(6, &attr(k, v)))
            .collect::<Vec<u8>>(),
        f_bytes(9, &l.trace_id),
        f_bytes(10, &l.span_id),
    ]
    .concat()
}

/// ExportTraceServiceRequest: one ResourceSpans per service.
pub fn encode_traces(b: &Batch) -> Vec<u8> {
    SERVICES
        .iter()
        .filter_map(|svc| {
            let spans: Vec<u8> = b
                .spans
                .iter()
                .filter(|s| s.service == *svc)
                .flat_map(|s| f_bytes(2, &span(s)))
                .collect();
            (!spans.is_empty()).then(|| {
                f_bytes(
                    1,
                    &[resource(svc), f_bytes(2, &[scope(), spans].concat())].concat(),
                )
            })
        })
        .flatten()
        .collect()
}

/// ExportLogsServiceRequest: one ResourceLogs per service.
pub fn encode_logs(b: &Batch) -> Vec<u8> {
    SERVICES
        .iter()
        .filter_map(|svc| {
            let recs: Vec<u8> = b
                .logs
                .iter()
                .filter(|l| l.service == *svc)
                .flat_map(|l| f_bytes(2, &log(l)))
                .collect();
            (!recs.is_empty()).then(|| {
                f_bytes(
                    1,
                    &[resource(svc), f_bytes(2, &[scope(), recs].concat())].concat(),
                )
            })
        })
        .flatten()
        .collect()
}
