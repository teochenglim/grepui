//! Trace layout (R-R3, R-R4): turn a flat list of spans into a waterfall (depth-first rows with
//! depth and offsets) and a flame graph (identical call paths merged). Pure functions: no
//! database, no clock. All times are nanoseconds in, microseconds out.

use serde_json::{Map, Value, json};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone)]
pub struct SpanIn {
    pub span_id: String,
    pub parent_id: Option<String>,
    pub service: String,
    pub name: String,
    pub kind: String,
    pub status: String,
    pub message: String,
    pub start_ns: i64,
    pub end_ns: i64,
    /// The whole row, for attributes, events and links.
    pub raw: Value,
}

impl SpanIn {
    pub fn is_error(&self) -> bool {
        self.status == "STATUS_CODE_ERROR"
    }
    fn duration_ns(&self) -> i64 {
        (self.end_ns - self.start_ns).max(0)
    }
}

fn text(r: &Value, k: &str) -> String {
    r[k].as_str().unwrap_or_default().to_string()
}

/// Read one `trace_spans` row. `None` when it has no usable id or times.
pub fn span_from_row(r: &Value) -> Option<SpanIn> {
    let span_id = r["span_id"].as_str().filter(|s| !s.is_empty())?.to_string();
    let start_ns: i64 = r["start_ns"].as_str()?.parse().ok()?;
    let end_ns: i64 = r["end_ns"].as_str()?.parse().ok()?;
    let parent_id = r["parent_span_id"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    Some(SpanIn {
        span_id,
        parent_id,
        service: text(r, "service_name"),
        name: text(r, "span_name"),
        kind: text(r, "span_kind"),
        status: text(r, "span_status_code"),
        message: text(r, "span_status_message"),
        start_ns,
        end_ns: end_ns.max(start_ns),
        raw: r.clone(),
    })
}

/// Children by parent index (sorted by start, then id) and the root indexes. A span whose parent
/// is not in the set (a partial trace) is a root.
fn tree(spans: &[SpanIn]) -> (Vec<Vec<usize>>, Vec<usize>) {
    let index: HashMap<&str, usize> = spans
        .iter()
        .enumerate()
        .map(|(i, s)| (s.span_id.as_str(), i))
        .collect();
    let mut children = vec![vec![]; spans.len()];
    let mut roots = vec![];
    for (i, s) in spans.iter().enumerate() {
        match s
            .parent_id
            .as_deref()
            .and_then(|p| index.get(p))
            .filter(|p| **p != i)
        {
            Some(&p) => children[p].push(i),
            None => roots.push(i),
        }
    }
    let key = |i: &usize| (spans[*i].start_ns, spans[*i].span_id.clone());
    roots.sort_by_key(key);
    for c in &mut children {
        c.sort_by_key(key);
    }
    (children, roots)
}

pub struct Waterfall {
    pub rows: Vec<Value>,
    pub start_ns: i64,
    pub end_ns: i64,
}

/// Attributes live in flat columns: `span_attributes.<k>` and `resource_attributes.<k>`.
fn extract(raw: &Value) -> (Map<String, Value>, Map<String, Value>) {
    let (mut attrs, mut res) = (Map::new(), Map::new());
    if let Some(o) = raw.as_object() {
        for (k, v) in o {
            if v.is_null() {
                continue;
            }
            if let Some(n) = k.strip_prefix("span_attributes.") {
                attrs.insert(n.to_string(), v.clone());
            } else if let Some(n) = k.strip_prefix("resource_attributes.") {
                res.insert(n.to_string(), v.clone());
            }
        }
    }
    (attrs, res)
}

/// A JSON column that is present and not an empty array/object.
fn present(v: &Value) -> Option<Value> {
    match v {
        Value::Null => None,
        Value::Array(a) if a.is_empty() => None,
        Value::Object(o) if o.is_empty() => None,
        other => Some(other.clone()),
    }
}

/// Depth-first rows from the roots, children ordered by start. Every span appears exactly once,
/// even in a malformed trace (cycles are appended as extra roots).
pub fn waterfall(spans: &[SpanIn]) -> Waterfall {
    let start = spans.iter().map(|s| s.start_ns).min().unwrap_or(0);
    let end = spans.iter().map(|s| s.end_ns).max().unwrap_or(0);
    let (children, roots) = tree(spans);
    let mut order: Vec<(usize, usize)> = vec![]; // (index, depth)
    let mut seen = HashSet::new();
    let mut stack: Vec<(usize, usize)> = roots.iter().rev().map(|r| (*r, 0)).collect();
    let walk = |stack: &mut Vec<(usize, usize)>,
                seen: &mut HashSet<usize>,
                order: &mut Vec<(usize, usize)>| {
        while let Some((i, d)) = stack.pop() {
            if !seen.insert(i) {
                continue;
            }
            order.push((i, d));
            stack.extend(children[i].iter().rev().map(|c| (*c, d + 1)));
        }
    };
    walk(&mut stack, &mut seen, &mut order);
    // Anything unreachable from a root (a parent cycle) is still shown, at depth 0.
    for i in 0..spans.len() {
        if !seen.contains(&i) {
            let mut st = vec![(i, 0)];
            walk(&mut st, &mut seen, &mut order);
        }
    }
    let rows = order
        .into_iter()
        .map(|(i, depth)| {
            let s = &spans[i];
            let (attributes, resource) = extract(&s.raw);
            let mut row = json!({
                "span_id": s.span_id, "parent_id": s.parent_id, "depth": depth,
                "service": s.service, "name": s.name, "kind": s.kind, "status": s.status, "error": s.is_error(),
                "message": s.message,
                "offset_us": (s.start_ns - start) / 1000, "duration_us": s.duration_ns() / 1000,
                "start_ns": s.start_ns.to_string(),
                "scope": s.raw["scope_name"],
                "attributes": attributes, "resource": resource,
            });
            if let Some(e) = present(&s.raw["span_events"]) {
                row["events"] = e;
            }
            if let Some(l) = present(&s.raw["span_links"]) {
                row["links"] = l;
            }
            row
        })
        .collect();
    Waterfall {
        rows,
        start_ns: start,
        end_ns: end,
    }
}

/// A node of the flame graph: spans with the same service and operation at the same call path.
fn merge(spans: &[SpanIn], children: &[Vec<usize>], group: &[usize], depth: usize) -> Vec<Value> {
    // Group siblings by (service, name), preserving first-seen order, then sort by total time.
    let mut order: Vec<(&str, &str)> = vec![];
    let mut groups: HashMap<(&str, &str), Vec<usize>> = HashMap::new();
    for &i in group {
        let k = (spans[i].service.as_str(), spans[i].name.as_str());
        if !groups.contains_key(&k) {
            order.push(k);
        }
        groups.entry(k).or_default().push(i);
    }
    let mut nodes: Vec<(i64, Value)> = order
        .into_iter()
        .map(|k| {
            let members = &groups[&k];
            let total: i64 = members.iter().map(|&i| spans[i].duration_ns()).sum();
            let kids: Vec<usize> = members
                .iter()
                .flat_map(|&i| children[i].iter().copied())
                .collect();
            let sub = if depth < 256 {
                merge(spans, children, &kids, depth + 1)
            } else {
                vec![]
            };
            let child_total: i64 = sub
                .iter()
                .map(|c| c["total_us"].as_i64().unwrap_or(0) * 1000)
                .sum();
            (
                total,
                json!({
                    "name": k.1, "service": k.0, "count": members.len(),
                    "total_us": total / 1000, "self_us": (total - child_total).max(0) / 1000,
                    "errors": members.iter().filter(|&&i| spans[i].is_error()).count(),
                    "children": sub,
                }),
            )
        })
        .collect();
    nodes.sort_by_key(|(t, _)| std::cmp::Reverse(*t));
    nodes.into_iter().map(|(_, v)| v).collect()
}

/// One root: the merged root span, or a synthetic `(trace)` root over several roots.
pub fn flame(spans: &[SpanIn]) -> Value {
    let (children, roots) = tree(spans);
    let mut merged = merge(spans, &children, &roots, 0);
    if merged.len() == 1 {
        return merged.remove(0);
    }
    let start = spans.iter().map(|s| s.start_ns).min().unwrap_or(0);
    let end = spans.iter().map(|s| s.end_ns).max().unwrap_or(0);
    let total = (end - start) / 1000;
    let child_total: i64 = merged
        .iter()
        .map(|c| c["total_us"].as_i64().unwrap_or(0))
        .sum();
    json!({"name": "(trace)", "service": "", "count": 1, "total_us": total, "self_us": (total - child_total).max(0), "errors": 0, "children": merged})
}
