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

/// The longest attribute value sent to the browser. An LLM prompt can be hundreds of kilobytes;
/// the rest is cut with a note saying how much there was (R-AI8 will let a role see it whole).
pub const ATTR_VALUE_MAX: usize = 32 * 1024;

fn capped(v: &Value) -> Value {
    match v {
        Value::String(s) if s.len() > ATTR_VALUE_MAX => {
            let mut cut = ATTR_VALUE_MAX;
            while !s.is_char_boundary(cut) {
                cut -= 1;
            }
            Value::String(format!("{}… [cut: {} bytes in all]", &s[..cut], s.len()))
        }
        other => other.clone(),
    }
}

/// Attributes of either layout. Flat tables keep one column per attribute
/// (`span_attributes.<k>`, `resource_attributes.<k>`); JSON tables hold them as objects in the
/// `span_attributes` and `resource_attributes` columns, keyed by the dotted name.
fn extract(raw: &Value) -> (Map<String, Value>, Map<String, Value>) {
    let (mut attrs, mut res) = (Map::new(), Map::new());
    if let Some(o) = raw.as_object() {
        for (k, v) in o {
            if v.is_null() {
                continue;
            }
            match (k.as_str(), v) {
                ("span_attributes", Value::Object(m)) => {
                    attrs.extend(
                        m.iter()
                            .filter(|(_, v)| !v.is_null())
                            .map(|(k, v)| (k.clone(), capped(v))),
                    );
                }
                ("resource_attributes", Value::Object(m)) => {
                    res.extend(
                        m.iter()
                            .filter(|(_, v)| !v.is_null())
                            .map(|(k, v)| (k.clone(), capped(v))),
                    );
                }
                _ => {
                    if let Some(n) = k.strip_prefix("span_attributes.") {
                        attrs.insert(n.to_string(), capped(v));
                    } else if let Some(n) = k.strip_prefix("resource_attributes.") {
                        res.insert(n.to_string(), capped(v));
                    }
                }
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

/// A caller to callee edge with its call and error counts (R-V3).
#[derive(Debug, Clone)]
pub struct EdgeIn {
    pub from: String,
    pub to: String,
    pub calls: i64,
    pub errors: i64,
    pub avg_us: f64,
}

pub const GRAPH_COL_W: i64 = 260;
pub const GRAPH_ROW_H: i64 = 90;
const GRAPH_PAD: i64 = 50;

/// Layered layout for the service map: a node's column is the longest call path leading to it, so
/// callers sit left of callees. Edges that close a cycle are kept, flagged `back`, and ignored
/// when layering, so a cyclic call graph still lays out. Self loops are dropped unless asked for.
/// Positions are plain numbers in a fixed grid: the page only draws.
pub fn graph(edges: &[EdgeIn], self_loops: bool) -> Value {
    let edges: Vec<&EdgeIn> = edges
        .iter()
        .filter(|e| self_loops || e.from != e.to)
        .collect();
    let mut names: Vec<&str> = edges
        .iter()
        .flat_map(|e| [e.from.as_str(), e.to.as_str()])
        .collect();
    names.sort_unstable();
    names.dedup();
    let idx: HashMap<&str, usize> = names.iter().enumerate().map(|(i, n)| (*n, i)).collect();
    let n = names.len();
    let mut out: Vec<Vec<(usize, usize)>> = vec![vec![]; n]; // (target, edge index)
    for (ei, e) in edges.iter().enumerate() {
        if e.from != e.to {
            out[idx[e.from.as_str()]].push((idx[e.to.as_str()], ei));
        }
    }
    // Depth-first search in name order; an edge into a node still on the stack closes a cycle.
    let mut back = vec![false; edges.len()];
    let mut state = vec![0u8; n]; // 0 new, 1 on stack, 2 done
    let mut post = Vec::with_capacity(n);
    for root in 0..n {
        if state[root] != 0 {
            continue;
        }
        state[root] = 1;
        let mut stack = vec![(root, 0usize)];
        while let Some((u, next)) = stack.pop() {
            if let Some(&(v, ei)) = out[u].get(next) {
                stack.push((u, next + 1));
                match state[v] {
                    0 => {
                        state[v] = 1;
                        stack.push((v, 0));
                    }
                    1 => back[ei] = true,
                    _ => {}
                }
            } else {
                state[u] = 2;
                post.push(u);
            }
        }
    }
    let mut layer = vec![0i64; n];
    for &u in post.iter().rev() {
        for &(v, ei) in &out[u] {
            if !back[ei] {
                layer[v] = layer[v].max(layer[u] + 1);
            }
        }
    }
    let mut row_in_layer: HashMap<i64, i64> = HashMap::new();
    let mut max_layer = 0;
    let mut max_row = 0;
    let mut calls_in = vec![0i64; n];
    let mut calls_out = vec![0i64; n];
    let mut errors_in = vec![0i64; n];
    for e in &edges {
        calls_out[idx[e.from.as_str()]] += e.calls;
        calls_in[idx[e.to.as_str()]] += e.calls;
        errors_in[idx[e.to.as_str()]] += e.errors;
    }
    let nodes: Vec<Value> = (0..n)
        .map(|i| {
            let row = row_in_layer.entry(layer[i]).or_insert(0);
            let r = *row;
            *row += 1;
            max_layer = max_layer.max(layer[i]);
            max_row = max_row.max(r);
            json!({
                "id": names[i], "layer": layer[i], "row": r,
                "x": GRAPH_PAD + layer[i] * GRAPH_COL_W, "y": GRAPH_PAD + r * GRAPH_ROW_H,
                "calls_in": calls_in[i], "calls_out": calls_out[i], "errors_in": errors_in[i],
            })
        })
        .collect();
    let edge_json: Vec<Value> = edges
        .iter()
        .enumerate()
        .map(|(ei, e)| {
            json!({
                "from": e.from, "to": e.to, "calls": e.calls, "errors": e.errors, "avg_us": e.avg_us,
                "error_pct": if e.calls > 0 { 100.0 * e.errors as f64 / e.calls as f64 } else { 0.0 },
                "back": back[ei], "self": e.from == e.to,
            })
        })
        .collect();
    json!({
        "nodes": nodes, "edges": edge_json,
        "width": GRAPH_PAD * 2 + max_layer * GRAPH_COL_W + 180,
        "height": GRAPH_PAD * 2 + max_row * GRAPH_ROW_H + 40,
    })
}
