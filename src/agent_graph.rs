//! The agent graph of a trace (R-AI9, ADR 0016): nodes are agents, tools, model calls, retrievers
//! and other LLM steps; edges are "this step was run by that one". Pure: it reads the span rows
//! the waterfall builds (each with its normalised `llm` view and attributes). Built from
//! `graph.node.id` / `graph.node.parent_id` when the instrumentation sets them, else from
//! `agent.name` / `gen_ai.agent.name`, the span kind and span parentage. Cycles and missing
//! parents are tolerated: an unknown parent means "no edge", never a failure.

use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, HashSet};

const MAX_HOPS: usize = 256;

struct Node {
    label: String,
    kind: String,
    durations: Vec<i64>,
    errors: u64,
    tokens: i64,
    cost: f64,
}

/// `(node id, label, kind)` of a span row, or `None` when the span is not a graph step.
fn identify(row: &Value) -> Option<(String, String, String)> {
    let a = &row["attributes"];
    let v = &row["llm"];
    let kind = v["kind"].as_str().unwrap_or_default().to_string();
    let text = |x: &Value| x.as_str().filter(|s| !s.is_empty()).map(str::to_string);
    if let Some(id) = text(&a["graph.node.id"]) {
        let label = text(&a["graph.node.name"]).unwrap_or_else(|| id.clone());
        return Some((
            id,
            label,
            if kind.is_empty() { "NODE".into() } else { kind },
        ));
    }
    let agent = text(&v["agent"]);
    if kind == "AGENT" || agent.is_some() {
        let name = agent.or_else(|| text(&row["name"]))?;
        return Some((format!("agent:{name}"), name, "AGENT".into()));
    }
    let name = match kind.as_str() {
        "TOOL" => text(&v["tool"]["name"]).or_else(|| text(&row["name"])),
        "LLM" => text(&v["model"]).or_else(|| text(&row["name"])),
        "" => return None,
        _ => text(&row["name"]),
    }?;
    Some((format!("{}:{name}", kind.to_ascii_lowercase()), name, kind))
}

fn p95(v: &mut [i64]) -> i64 {
    if v.is_empty() {
        return 0;
    }
    v.sort_unstable();
    v[((v.len() as f64 * 0.95).ceil() as usize).clamp(1, v.len()) - 1]
}

/// The graph of the span rows of one trace.
pub fn build(rows: &[Value]) -> Value {
    let by_id: HashMap<&str, usize> = rows
        .iter()
        .enumerate()
        .filter_map(|(i, r)| r["span_id"].as_str().map(|s| (s, i)))
        .collect();
    let ids: Vec<Option<(String, String, String)>> = rows.iter().map(identify).collect();
    let mut nodes: BTreeMap<String, Node> = BTreeMap::new();
    let mut edges: BTreeMap<(String, String), (u64, u64)> = BTreeMap::new();
    for (i, row) in rows.iter().enumerate() {
        let Some((id, label, kind)) = &ids[i] else {
            continue;
        };
        let n = nodes.entry(id.clone()).or_insert_with(|| Node {
            label: label.clone(),
            kind: kind.clone(),
            durations: vec![],
            errors: 0,
            tokens: 0,
            cost: 0.0,
        });
        n.durations.push(row["duration_us"].as_i64().unwrap_or(0));
        let error = row["error"].as_bool().unwrap_or(false);
        n.errors += u64::from(error);
        if kind == "LLM" {
            n.tokens += row["llm"]["tokens"]["total"].as_i64().unwrap_or(0);
            n.cost += row["llm"]["cost"]["total"].as_f64().unwrap_or(0.0);
        }
        // The parent step: an explicit `graph.node.parent_id`, else the nearest ancestor that is a step.
        let explicit = row["attributes"]["graph.node.parent_id"]
            .as_str()
            .filter(|s| !s.is_empty());
        let parent = match explicit {
            Some(p) => Some(p.to_string()),
            None => {
                let (mut cur, mut hops, mut seen) = (row["parent_id"].as_str(), 0, HashSet::new());
                let mut found = None;
                while let Some(pid) = cur {
                    if hops >= MAX_HOPS || !seen.insert(pid) {
                        break;
                    }
                    let Some(&pi) = by_id.get(pid) else { break };
                    if let Some((nid, ..)) = &ids[pi] {
                        found = Some(nid.clone());
                        break;
                    }
                    cur = rows[pi]["parent_id"].as_str();
                    hops += 1;
                }
                found
            }
        };
        if let Some(p) = parent {
            let e = edges.entry((p, id.clone())).or_default();
            e.0 += 1;
            e.1 += u64::from(error);
        }
    }
    // An explicit parent id that names no node still gets a bare node, so no edge dangles.
    let missing: Vec<String> = edges
        .keys()
        .filter(|(p, _)| !nodes.contains_key(p))
        .map(|(p, _)| p.clone())
        .collect();
    for p in missing {
        nodes.entry(p.clone()).or_insert_with(|| Node {
            label: p,
            kind: "NODE".into(),
            durations: vec![],
            errors: 0,
            tokens: 0,
            cost: 0.0,
        });
    }
    let nodes: Vec<Value> = nodes
        .into_iter()
        .map(|(id, mut n)| {
            let total: i64 = n.durations.iter().sum();
            json!({"id": id, "label": n.label, "kind": n.kind, "calls": n.durations.len(),
                   "errors": n.errors, "total_us": total, "p95_us": p95(&mut n.durations),
                   "tokens": n.tokens, "cost": n.cost})
        })
        .collect();
    let edges: Vec<Value> = edges
        .into_iter()
        .map(|((from, to), (calls, errors))| {
            json!({"from": from, "to": to, "calls": calls, "errors": errors, "self": from == to})
        })
        .collect();
    json!({"nodes": nodes, "edges": edges})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, parent: Option<&str>, kind: &str, name: &str, extra: Value) -> Value {
        let mut r = json!({"span_id": id, "parent_id": parent, "name": name, "duration_us": 100, "error": false,
            "attributes": {}, "llm": {"kind": kind}});
        if let Value::Object(m) = extra {
            for (k, v) in m {
                r[&k] = v;
            }
        }
        r
    }

    #[test]
    fn steps_connect_through_plain_spans() {
        let rows = vec![
            row("a", None, "AGENT", "support", json!({})),
            row("b", Some("a"), "", "http client", json!({"llm": null})),
            row(
                "c",
                Some("b"),
                "LLM",
                "chat",
                json!({"llm": {"kind": "LLM", "model": "m", "tokens": {"total": 7}, "cost": {"total": 0.5}}}),
            ),
            row(
                "d",
                Some("c"),
                "TOOL",
                "t",
                json!({"llm": {"kind": "TOOL", "tool": {"name": "lookup"}}, "error": true}),
            ),
        ];
        let g = build(&rows);
        let edges = g["edges"].as_array().unwrap();
        assert_eq!(edges.len(), 2);
        assert!(
            edges
                .iter()
                .any(|e| e["from"] == "agent:support" && e["to"] == "llm:m")
        );
        assert!(
            edges
                .iter()
                .any(|e| e["from"] == "llm:m" && e["to"] == "tool:lookup" && e["errors"] == 1)
        );
        let llm = g["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|n| n["id"] == "llm:m")
            .unwrap();
        assert_eq!(
            (llm["tokens"].clone(), llm["cost"].clone()),
            (json!(7), json!(0.5))
        );
    }

    #[test]
    fn graph_node_attributes_win_and_orphans_do_not_fail() {
        let rows = vec![
            row(
                "a",
                Some("gone"),
                "AGENT",
                "x",
                json!({"attributes": {"graph.node.id": "planner", "graph.node.name": "Planner"}}),
            ),
            row(
                "b",
                Some("a"),
                "AGENT",
                "y",
                json!({"attributes": {"graph.node.id": "worker", "graph.node.parent_id": "planner"}}),
            ),
        ];
        let g = build(&rows);
        assert_eq!(g["nodes"].as_array().unwrap().len(), 2);
        assert_eq!(g["edges"][0]["from"], "planner");
        assert_eq!(g["nodes"][0]["label"], "Planner");
    }

    #[test]
    fn cycles_and_repeats_terminate_and_count() {
        let rows = vec![
            row("a", Some("b"), "TOOL", "t", json!({})),
            row("b", Some("a"), "TOOL", "t", json!({})),
            row("c", Some("c"), "TOOL", "u", json!({})),
            row("d", None, "AGENT", "ag", json!({})),
            row("e", Some("d"), "AGENT", "ag", json!({})),
        ];
        let g = build(&rows);
        let t = g["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|n| n["id"] == "tool:t")
            .unwrap();
        assert_eq!(t["calls"], 2);
        assert!(
            g["edges"]
                .as_array()
                .unwrap()
                .iter()
                .any(|e| e["self"] == true)
        );
    }
}
