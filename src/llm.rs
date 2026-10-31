//! The normalised LLM view of a span (R-AI3, R-AI4, ADR 0016). Pure functions: attributes in,
//! JSON out; no database, no clock.
//!
//! Two conventions meet here. OpenInference (`llm.*`, `retrieval.*`, `tool.*`, ...) is read
//! first; OpenTelemetry GenAI (`gen_ai.*`, status Development) fills what OpenInference left
//! out. Flattened indexed attributes (`llm.input_messages.3.message.role`) are put back together
//! by index; nested JSON values (as in the OpenInference traces spec) are accepted too.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;

pub const KIND: &str = "openinference.span.kind";
pub const OP: &str = "gen_ai.operation.name";

/// The OpenInference span kinds, for the filter chips and the kind breakdown.
pub const KINDS: &[&str] = &[
    "LLM",
    "AGENT",
    "CHAIN",
    "TOOL",
    "RETRIEVER",
    "RERANKER",
    "EMBEDDING",
    "GUARDRAIL",
    "EVALUATOR",
    "PROMPT",
    "DECISION",
];

/// The attribute names, first match wins (OpenInference before GenAI).
pub const MODEL: &[&str] = &[
    "llm.model_name",
    "llm.response.model_name",
    "llm.request.model_name",
    "gen_ai.response.model",
    "gen_ai.request.model",
    "embedding.model_name",
];
pub const PROVIDER: &[&str] = &[
    "llm.provider",
    "llm.system",
    "gen_ai.provider.name",
    "gen_ai.system",
];
pub const PROMPT_TOKENS: &[&str] = &["llm.token_count.prompt", "gen_ai.usage.input_tokens"];
pub const COMPLETION_TOKENS: &[&str] =
    &["llm.token_count.completion", "gen_ai.usage.output_tokens"];
pub const COST_TOTAL: &str = "llm.cost.total";
pub const SESSION: &[&str] = &["session.id", "gen_ai.conversation.id"];
pub const USER: &[&str] = &["user.id"];
pub const TOOL_NAME: &[&str] = &["tool.name", "gen_ai.tool.name", "tool_call.function.name"];
/// `gen_ai.operation.name` values that are model calls.
pub const CALL_OPS: &[&str] = &["chat", "text_completion", "generate_content"];

/// Longest message or document text sent in the normalised view; the raw attribute is capped
/// separately by `layout::ATTR_VALUE_MAX`.
const TEXT_MAX: usize = 32 * 1024;

type Attrs = Map<String, Value>;

// ---- scalar readers ---------------------------------------------------------------------------

fn text_of(v: &Value) -> Option<String> {
    match v {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn first_str(a: &Attrs, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|k| a.get(*k).and_then(text_of))
}

fn int_of(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn float_of(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn first_int(a: &Attrs, keys: &[&str]) -> Option<i64> {
    keys.iter().find_map(|k| a.get(*k).and_then(int_of))
}

fn first_float(a: &Attrs, keys: &[&str]) -> Option<f64> {
    keys.iter().find_map(|k| a.get(*k).and_then(float_of))
}

fn clip(s: &str) -> String {
    if s.len() <= TEXT_MAX {
        return s.to_string();
    }
    let mut cut = TEXT_MAX;
    while !s.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}… [cut: {} bytes in all]", &s[..cut], s.len())
}

/// A string value, or a structured value written out as JSON.
fn display(v: &Value) -> Option<String> {
    match v {
        Value::Null => None,
        Value::String(s) => (!s.is_empty()).then(|| clip(s)),
        Value::Number(_) | Value::Bool(_) => Some(v.to_string()),
        other => Some(clip(&other.to_string())),
    }
}

/// Drop empty values so the response carries only what the span has.
fn compact(m: Map<String, Value>) -> Value {
    Value::Object(
        m.into_iter()
            .filter(|(_, v)| match v {
                Value::Null => false,
                Value::Array(a) => !a.is_empty(),
                Value::Object(o) => !o.is_empty(),
                _ => true,
            })
            .collect(),
    )
}

// ---- indexed attributes -----------------------------------------------------------------------

type Group<'a> = BTreeMap<usize, BTreeMap<String, &'a Value>>;

/// `prefix.<n>.<rest>` attributes grouped by `n`.
fn group<'a>(a: impl Iterator<Item = (&'a String, &'a Value)>, prefix: &str) -> Group<'a> {
    let mut out: Group = BTreeMap::new();
    let start = format!("{prefix}.");
    for (k, v) in a {
        let Some(rest) = k.strip_prefix(&start) else {
            continue;
        };
        let Some((idx, tail)) = rest.split_once('.') else {
            continue;
        };
        let Ok(i) = idx.parse::<usize>() else {
            continue;
        };
        out.entry(i).or_default().insert(tail.to_string(), v);
    }
    out
}

fn get<'a>(m: &BTreeMap<String, &'a Value>, k: &str) -> Option<&'a Value> {
    m.get(k).copied()
}

fn pick_text(m: &BTreeMap<String, &Value>, k: &str) -> Option<String> {
    get(m, k).and_then(display)
}

fn tool_call(m: &BTreeMap<String, &Value>) -> Value {
    let mut o = Map::new();
    o.insert(
        "id".into(),
        pick_text(m, "tool_call.id").map_or(Value::Null, Value::String),
    );
    o.insert(
        "name".into(),
        pick_text(m, "tool_call.function.name").map_or(Value::Null, Value::String),
    );
    o.insert(
        "arguments".into(),
        pick_text(m, "tool_call.function.arguments").map_or(Value::Null, Value::String),
    );
    compact(o)
}

/// One flattened message: the map holds the keys after `llm.input_messages.<n>.`.
fn flat_message(m: &BTreeMap<String, &Value>) -> Value {
    let mut o = Map::new();
    let set = |o: &mut Map<String, Value>, k: &str, v: Option<String>| {
        if let Some(v) = v {
            o.insert(k.into(), Value::String(v));
        }
    };
    set(&mut o, "role", pick_text(m, "message.role"));
    set(&mut o, "name", pick_text(m, "message.name"));
    set(&mut o, "tool_call_id", pick_text(m, "message.tool_call_id"));
    let mut content = pick_text(m, "message.content");
    // Multimodal: `message.contents.<k>.message_content.{type,text,image.image.url}`.
    let parts = group(m.iter().map(|(k, v)| (k, *v)), "message.contents");
    let mut part_list = vec![];
    for (_, p) in parts {
        let kind = pick_text(&p, "message_content.type").unwrap_or_else(|| "text".into());
        let text = pick_text(&p, "message_content.text");
        let image = pick_text(&p, "message_content.image.image.url");
        part_list.push(compact(
            json!({"type": kind, "text": text, "image_url": image})
                .as_object()
                .cloned()
                .unwrap_or_default(),
        ));
    }
    if content.is_none() && !part_list.is_empty() {
        let joined: Vec<String> = part_list
            .iter()
            .filter_map(|p| p["text"].as_str().map(str::to_string))
            .collect();
        if !joined.is_empty() {
            content = Some(joined.join("\n"));
        }
    }
    set(&mut o, "content", content);
    if part_list.iter().any(|p| p["type"] != "text") {
        o.insert("parts".into(), Value::Array(part_list));
    }
    let calls = group(m.iter().map(|(k, v)| (k, *v)), "message.tool_calls");
    let calls: Vec<Value> = calls.values().map(tool_call).collect();
    if !calls.is_empty() {
        o.insert("tool_calls".into(), Value::Array(calls));
    }
    compact(o)
}

/// A message written as nested JSON: `{message: {role, content, tool_calls: [...]}}` or the
/// message itself, as OpenInference's traces spec and the GenAI conventions show.
fn nested_message(v: &Value) -> Value {
    let m = v.get("message").unwrap_or(v);
    let mut o = Map::new();
    let role = m.get("role").and_then(text_of);
    o.insert("role".into(), role.map_or(Value::Null, Value::String));
    // GenAI: `parts: [{type: text, content}, {type: tool_call, id, name, arguments}, ...]`.
    let mut content = m.get("content").and_then(display);
    let mut calls: Vec<Value> = vec![];
    if let Some(parts) = m.get("parts").and_then(Value::as_array) {
        let mut texts = vec![];
        for p in parts {
            match p.get("type").and_then(Value::as_str).unwrap_or("text") {
                "text" => {
                    if let Some(t) = p.get("content").or_else(|| p.get("text")).and_then(display) {
                        texts.push(t);
                    }
                }
                "tool_call" => calls.push(compact(
                    json!({"id": p.get("id"), "name": p.get("name"),
                           "arguments": p.get("arguments").and_then(display)})
                    .as_object()
                    .cloned()
                    .unwrap_or_default(),
                )),
                "tool_call_response" => {
                    if let Some(t) = p.get("response").and_then(display) {
                        texts.push(t);
                    }
                }
                _ => {}
            }
        }
        if content.is_none() && !texts.is_empty() {
            content = Some(texts.join("\n"));
        }
    }
    o.insert("content".into(), content.map_or(Value::Null, Value::String));
    if let Some(arr) = m.get("tool_calls").and_then(Value::as_array) {
        for c in arr {
            let c = c.get("tool_call").unwrap_or(c);
            let f = c.get("function").unwrap_or(c);
            calls.push(compact(
                json!({"id": c.get("id"), "name": f.get("name"),
                       "arguments": f.get("arguments").and_then(display)})
                .as_object()
                .cloned()
                .unwrap_or_default(),
            ));
        }
    }
    if !calls.is_empty() {
        o.insert("tool_calls".into(), Value::Array(calls));
    }
    compact(o)
}

/// Messages under `prefix` (`llm.input_messages`): flattened first, then nested JSON, then the
/// GenAI JSON attribute `gen_ai_key` (a JSON string or an already-parsed array).
fn messages(a: &Attrs, prefix: &str, gen_ai_key: &str) -> Vec<Value> {
    let flat = group(a.iter(), prefix);
    if !flat.is_empty() {
        return flat.values().map(flat_message).collect();
    }
    let as_list = |v: &Value| -> Option<Vec<Value>> {
        match v {
            Value::Array(l) => Some(l.clone()),
            Value::String(s) => serde_json::from_str::<Value>(s)
                .ok()
                .and_then(|p| p.as_array().cloned()),
            _ => None,
        }
    };
    for key in [prefix, gen_ai_key] {
        if let Some(l) = a.get(key).and_then(as_list) {
            return l.iter().map(nested_message).collect();
        }
    }
    vec![]
}

/// Retrieved or reranked documents: `prefix.<n>.document.{id,score,content,url,metadata}`.
fn documents(a: &Attrs, prefix: &str) -> Vec<Value> {
    let flat = group(a.iter(), prefix);
    if !flat.is_empty() {
        return flat
            .values()
            .map(|m| {
                let mut o = Map::new();
                o.insert(
                    "id".into(),
                    pick_text(m, "document.id").map_or(Value::Null, Value::String),
                );
                o.insert(
                    "score".into(),
                    get(m, "document.score")
                        .and_then(float_of)
                        .map_or(Value::Null, |f| json!(f)),
                );
                o.insert(
                    "content".into(),
                    pick_text(m, "document.content").map_or(Value::Null, Value::String),
                );
                o.insert(
                    "url".into(),
                    pick_text(m, "document.url").map_or(Value::Null, Value::String),
                );
                o.insert(
                    "metadata".into(),
                    pick_text(m, "document.metadata").map_or(Value::Null, Value::String),
                );
                compact(o)
            })
            .collect();
    }
    let list = match a.get(prefix) {
        Some(Value::Array(l)) => l.clone(),
        Some(Value::String(s)) => serde_json::from_str::<Value>(s)
            .ok()
            .and_then(|p| p.as_array().cloned())
            .unwrap_or_default(),
        _ => vec![],
    };
    list.iter()
        .map(|d| {
            let d = d.get("document").unwrap_or(d);
            compact(
                json!({"id": d.get("id").and_then(text_of), "score": d.get("score").and_then(float_of),
                       "content": d.get("content").and_then(display), "url": d.get("url").and_then(text_of),
                       "metadata": d.get("metadata").and_then(display)})
                .as_object()
                .cloned()
                .unwrap_or_default(),
            )
        })
        .collect()
}

/// Embedded texts, without the vectors: only the vector length is kept.
fn embeddings(a: &Attrs) -> Vec<Value> {
    group(a.iter(), "embedding.embeddings")
        .values()
        .map(|m| {
            let dims = get(m, "embedding.vector").and_then(vector_len);
            compact(
                json!({"text": pick_text(m, "embedding.text"), "dims": dims})
                    .as_object()
                    .cloned()
                    .unwrap_or_default(),
            )
        })
        .collect()
}

/// Length of a vector stored as an array or as text (`[0.1, 0.2]` or `0.1,0.2`).
pub fn vector_len(v: &Value) -> Option<usize> {
    match v {
        Value::Array(a) => Some(a.len()),
        Value::String(s) => {
            let t = s.trim();
            (!t.is_empty()).then(|| t.matches(',').count() + 1)
        }
        _ => None,
    }
}

// ---- the span view ----------------------------------------------------------------------------

/// The span kind: OpenInference's, else derived from the GenAI operation.
pub fn span_kind(a: &Attrs) -> Option<String> {
    if let Some(k) = a
        .get(KIND)
        .and_then(Value::as_str)
        .filter(|k| !k.is_empty())
    {
        return Some(k.to_ascii_uppercase());
    }
    let op = a.get(OP).and_then(Value::as_str)?;
    Some(
        match op {
            o if CALL_OPS.contains(&o) => "LLM",
            "embeddings" => "EMBEDDING",
            "execute_tool" => "TOOL",
            "invoke_agent" | "create_agent" => "AGENT",
            "retrieval" => "RETRIEVER",
            _ => "CHAIN",
        }
        .into(),
    )
}

fn value_pair(a: &Attrs, key: &str) -> Value {
    let mut o = Map::new();
    if let Some(v) = a.get(&format!("{key}.value")).and_then(display) {
        o.insert("value".into(), Value::String(v));
    }
    if let Some(m) = a.get(&format!("{key}.mime_type")).and_then(text_of) {
        o.insert("mime".into(), Value::String(m));
    }
    compact(o)
}

/// The LLM view of one span's attributes; `None` when the span carries no LLM attribute at all.
pub fn normalise(a: &Attrs) -> Option<Value> {
    let kind = span_kind(a)?;
    let mut o = Map::new();
    o.insert("kind".into(), json!(kind));
    o.insert(
        "model".into(),
        first_str(a, MODEL).map_or(Value::Null, Value::String),
    );
    o.insert(
        "provider".into(),
        first_str(a, PROVIDER).map_or(Value::Null, Value::String),
    );
    o.insert(
        "session".into(),
        first_str(a, SESSION).map_or(Value::Null, Value::String),
    );
    o.insert(
        "user".into(),
        first_str(a, USER).map_or(Value::Null, Value::String),
    );
    o.insert(
        "agent".into(),
        first_str(a, &["agent.name", "gen_ai.agent.name"]).map_or(Value::Null, Value::String),
    );
    // Tokens and cost.
    let prompt = first_int(a, PROMPT_TOKENS);
    let completion = first_int(a, COMPLETION_TOKENS);
    let total = first_int(a, &["llm.token_count.total"]).or_else(|| match (prompt, completion) {
        (None, None) => None,
        (p, c) => Some(p.unwrap_or(0) + c.unwrap_or(0)),
    });
    o.insert(
        "tokens".into(),
        compact(
            json!({"prompt": prompt, "completion": completion, "total": total,
                   "cache_read": first_int(a, &["llm.token_count.prompt_details.cache_read"]),
                   "cache_write": first_int(a, &["llm.token_count.prompt_details.cache_write"]),
                   "reasoning": first_int(a, &["llm.token_count.completion_details.reasoning"])})
            .as_object()
            .cloned()
            .unwrap_or_default(),
        ),
    );
    let cost_total = first_float(a, &[COST_TOTAL]);
    let (cp, cc) = (
        first_float(a, &["llm.cost.prompt"]),
        first_float(a, &["llm.cost.completion"]),
    );
    let cost_total = cost_total
        .or_else(|| (cp.is_some() || cc.is_some()).then(|| cp.unwrap_or(0.0) + cc.unwrap_or(0.0)));
    o.insert(
        "cost".into(),
        compact(
            json!({"prompt": cp, "completion": cc, "total": cost_total})
                .as_object()
                .cloned()
                .unwrap_or_default(),
        ),
    );
    // Conversation.
    let mut input = messages(a, "llm.input_messages", "gen_ai.input.messages");
    if let Some(sys) = a.get("gen_ai.system_instructions").and_then(display)
        && !input.iter().any(|m| m["role"] == "system")
    {
        input.insert(0, json!({"role": "system", "content": sys}));
    }
    let output = messages(a, "llm.output_messages", "gen_ai.output.messages");
    if !input.is_empty() || !output.is_empty() {
        o.insert("messages".into(), json!({"input": input, "output": output}));
    }
    o.insert(
        "documents".into(),
        Value::Array(documents(a, "retrieval.documents")),
    );
    o.insert(
        "reranked".into(),
        Value::Array(documents(a, "reranker.output_documents")),
    );
    o.insert("embeddings".into(), Value::Array(embeddings(a)));
    // Tool.
    let tool_name = first_str(a, TOOL_NAME);
    if tool_name.is_some() || kind == "TOOL" {
        o.insert(
            "tool".into(),
            compact(
                json!({"name": tool_name,
                       "description": first_str(a, &["tool.description", "gen_ai.tool.description"]),
                       "arguments": a.get("tool_call.function.arguments")
                           .or_else(|| a.get("gen_ai.tool.call.arguments"))
                           .or_else(|| a.get("tool.parameters")).and_then(display),
                       "result": a.get("gen_ai.tool.call.result").and_then(display)})
                .as_object()
                .cloned()
                .unwrap_or_default(),
            ),
        );
    }
    o.insert("input".into(), value_pair(a, "input"));
    o.insert("output".into(), value_pair(a, "output"));
    o.insert(
        "finish_reason".into(),
        first_str(a, &["llm.finish_reason"])
            .or_else(|| {
                a.get("gen_ai.response.finish_reasons")
                    .and_then(|v| match v {
                        Value::Array(l) => {
                            Some(l.iter().filter_map(text_of).collect::<Vec<_>>().join(", "))
                        }
                        other => text_of(other),
                    })
            })
            .map_or(Value::Null, Value::String),
    );
    o.insert(
        "parameters".into(),
        a.get("llm.invocation_parameters")
            .and_then(display)
            .map_or(Value::Null, Value::String),
    );
    let template = compact(
        json!({"template": first_str(a, &["llm.prompt_template.template"]),
               "version": first_str(a, &["llm.prompt_template.version"]),
               "variables": a.get("llm.prompt_template.variables").and_then(display)})
        .as_object()
        .cloned()
        .unwrap_or_default(),
    );
    o.insert("template".into(), template);
    o.insert(
        "tags".into(),
        match a.get("tag.tags") {
            Some(Value::Array(l)) => {
                Value::Array(l.iter().filter_map(text_of).map(Value::String).collect())
            }
            Some(v) => text_of(v).map_or(Value::Null, |t| json!([t])),
            None => Value::Null,
        },
    );
    o.insert("evaluations".into(), Value::Array(evaluations(a)));
    o.insert(
        "metadata".into(),
        a.get("metadata")
            .and_then(display)
            .map_or(Value::Null, Value::String),
    );
    Some(compact(o))
}

/// Replace embedding vectors in an attribute map by a short note: a vector is thousands of
/// numbers nobody reads, and the span panel would send them all.
pub fn summarise_vectors(a: &mut Attrs) {
    for (k, v) in a.iter_mut() {
        if (k.ends_with(".embedding.vector") || k.ends_with(".embedding_vector"))
            && let Some(n) = vector_len(v)
        {
            *v = Value::String(format!("[vector, {n} dimensions]"));
        }
    }
}

// ---- evaluations ------------------------------------------------------------------------------

fn evaluation(
    name: Option<String>,
    label: Option<String>,
    score: Option<f64>,
    explanation: Option<String>,
    source: Option<String>,
) -> Option<Value> {
    if name.is_none() && label.is_none() && score.is_none() {
        return None;
    }
    Some(compact(
        json!({"name": name, "label": label, "score": score, "explanation": explanation, "source": source})
            .as_object()
            .cloned()
            .unwrap_or_default(),
    ))
}

/// Evaluations carried as span attributes: `annotation.{name,label,score,explanation,annotator_kind}`
/// and `evaluation.*`, single or indexed (`annotation.<n>.name`).
pub fn evaluations(a: &Attrs) -> Vec<Value> {
    let one = |m: &BTreeMap<String, &Value>| {
        evaluation(
            pick_text(m, "name"),
            pick_text(m, "label"),
            get(m, "score").and_then(float_of),
            pick_text(m, "explanation"),
            pick_text(m, "annotator_kind"),
        )
    };
    let mut out = vec![];
    for p in ["annotation", "evaluation"] {
        let start = format!("{p}.");
        let single: BTreeMap<String, &Value> = a
            .iter()
            .filter_map(|(k, v)| k.strip_prefix(&start).map(|r| (r.to_string(), v)))
            .collect();
        out.extend(one(&single));
        for g in group(a.iter(), p).values() {
            out.extend(one(g));
        }
    }
    out
}

/// Evaluations carried as span events named `gen_ai.evaluation.result`. Event attributes may be
/// an object or a list of `{key, value}` pairs.
pub fn event_evaluations(events: &Value) -> Vec<Value> {
    let Some(list) = events.as_array() else {
        return vec![];
    };
    list.iter()
        .filter(|e| e["name"] == "gen_ai.evaluation.result")
        .filter_map(|e| {
            let mut m = Map::new();
            match &e["attributes"] {
                Value::Object(o) => m.extend(o.clone()),
                Value::Array(l) => {
                    for kv in l {
                        if let Some(k) = kv["key"].as_str() {
                            m.insert(k.to_string(), kv["value"].clone());
                        }
                    }
                }
                _ => {}
            }
            evaluation(
                first_str(&m, &["gen_ai.evaluation.name"]),
                first_str(&m, &["gen_ai.evaluation.score.label"]),
                first_float(&m, &["gen_ai.evaluation.score.value"]),
                m.get("gen_ai.evaluation.explanation").and_then(display),
                Some("event".into()),
            )
        })
        .collect()
}

// ---- content protection -----------------------------------------------------------------------

/// What a role below admin sees of prompts, completions, tool arguments and document text
/// (R-AI8). `Masked` is the default: the text is replaced by its length; `Hidden` removes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Hidden,
    #[default]
    Masked,
    Visible,
}

/// Attribute names (equal, or the end of a dotted key) that hold content.
const CONTENT_SUFFIXES: &[&str] = &[
    // Containers written as nested JSON instead of flattened indexed attributes: the whole value.
    "llm.input_messages",
    "llm.output_messages",
    "llm.function_call",
    "retrieval.documents",
    "reranker.input_documents",
    "reranker.output_documents",
    "embedding.embeddings",
    "message.content",
    "message_content.text",
    "document.content",
    "document.metadata",
    "embedding.text",
    "function.arguments",
    "input.value",
    "output.value",
    "tool.parameters",
    "llm.prompt_template.template",
    "llm.prompt_template.variables",
    "gen_ai.input.messages",
    "gen_ai.output.messages",
    "gen_ai.system_instructions",
    "gen_ai.tool.call.arguments",
    "gen_ai.tool.call.result",
    "annotation.explanation",
    "evaluation.explanation",
];

/// Keys that hold content inside the normalised view (`llm` in a span row).
const VIEW_KEYS: &[&str] = &[
    "content",
    "arguments",
    "text",
    "value",
    "result",
    "template",
    "variables",
    "metadata",
    "explanation",
];

fn is_content_key(k: &str) -> bool {
    let k = k.to_ascii_lowercase();
    CONTENT_SUFFIXES
        .iter()
        .any(|s| k == *s || k.ends_with(&format!(".{s}")))
}

fn mask_value(v: &Value) -> Value {
    match v {
        Value::String(s) => Value::String(format!("[masked: {} characters]", s.chars().count())),
        _ => Value::String("[masked]".into()),
    }
}

fn protect_view(v: &mut Value, mode: Mode) -> bool {
    let mut changed = false;
    match v {
        Value::Object(o) => {
            let keys: Vec<String> = o.keys().cloned().collect();
            for k in keys {
                if VIEW_KEYS.contains(&k.as_str())
                    && !o[&k].is_null()
                    && !o[&k].is_object()
                    && !o[&k].is_array()
                {
                    changed = true;
                    if mode == Mode::Hidden {
                        o.remove(&k);
                    } else {
                        let m = mask_value(&o[&k]);
                        o.insert(k, m);
                    }
                } else if let Some(x) = o.get_mut(&k) {
                    changed |= protect_view(x, mode);
                }
            }
        }
        Value::Array(a) => {
            for x in a {
                changed |= protect_view(x, mode);
            }
        }
        _ => {}
    }
    changed
}

/// Mask or remove content in a response body, at any depth: attribute keys that hold content,
/// and everything content-like inside a normalised `llm` view. Returns whether anything changed.
pub fn protect(v: &mut Value, mode: Mode) -> bool {
    if mode == Mode::Visible {
        return false;
    }
    let mut changed = false;
    match v {
        Value::Object(o) => {
            let keys: Vec<String> = o.keys().cloned().collect();
            for k in keys {
                if k == "llm" && o[&k].is_object() {
                    changed |= protect_view(&mut o[&k], mode);
                } else if is_content_key(&k) && !o[&k].is_null() {
                    changed = true;
                    if mode == Mode::Hidden {
                        o.remove(&k);
                    } else {
                        let m = mask_value(&o[&k]);
                        o.insert(k, m);
                    }
                } else if let Some(x) = o.get_mut(&k) {
                    changed |= protect(x, mode);
                }
            }
        }
        Value::Array(a) => {
            for x in a {
                changed |= protect(x, mode);
            }
        }
        _ => {}
    }
    changed
}

/// The same protection for a CSV body: a column whose header names content is masked, and a
/// cell holding JSON is parsed and protected like any other body.
pub fn protect_csv(text: &str, mode: Mode) -> String {
    if mode == Mode::Visible {
        return text.to_string();
    }
    let Ok(mut rows) = crate::csv::parse(text, usize::MAX, 10_000) else {
        return "[masked]".to_string();
    };
    let Some(head) = rows.first().cloned() else {
        return text.to_string();
    };
    for row in rows.iter_mut().skip(1) {
        for (i, cell) in row.iter_mut().enumerate() {
            if cell.is_empty() {
                continue;
            }
            if head.get(i).is_some_and(|h| is_content_key(h)) {
                *cell = if mode == Mode::Hidden {
                    String::new()
                } else {
                    format!("[masked: {} characters]", cell.chars().count())
                };
            } else if cell.starts_with('{') || cell.starts_with('[') {
                match serde_json::from_str::<Value>(cell) {
                    Ok(mut v) => {
                        if protect(&mut v, mode) {
                            *cell = v.to_string();
                        }
                    }
                    // JSON that does not parse could still carry content: never passed unchecked.
                    Err(_) => *cell = "[masked]".to_string(),
                }
            }
        }
    }
    crate::csv::write(&rows)
}

// ---- cost -------------------------------------------------------------------------------------

/// Price of a model in US dollars per million tokens (stored per org, ADR 0016).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Price {
    pub prompt_per_1m: f64,
    pub completion_per_1m: f64,
}

impl Price {
    pub fn validate(&self) -> Result<(), String> {
        for (n, v) in [
            ("prompt_per_1m", self.prompt_per_1m),
            ("completion_per_1m", self.completion_per_1m),
        ] {
            if !v.is_finite() || !(0.0..=1_000_000.0).contains(&v) {
                return Err(format!(
                    "{n} must be between 0 and 1000000 dollars per million tokens"
                ));
            }
        }
        Ok(())
    }

    pub fn cost(&self, prompt: i64, completion: i64) -> f64 {
        prompt.max(0) as f64 * self.prompt_per_1m / 1e6
            + completion.max(0) as f64 * self.completion_per_1m / 1e6
    }
}

/// A model name is a plain label: not empty, not long, no control characters.
pub fn valid_model(m: &str) -> bool {
    !m.trim().is_empty() && m.chars().count() <= 128 && !m.chars().any(char::is_control)
}

/// Fill `cost.total` of a normalised span from the price table when the span reports none.
pub fn estimate_cost(view: &mut Value, prices: &std::collections::HashMap<String, Price>) {
    if view["cost"]["total"].is_number() {
        return;
    }
    let Some(price) = view["model"].as_str().and_then(|m| prices.get(m)) else {
        return;
    };
    let t = &view["tokens"];
    let (p, c) = (t["prompt"].as_i64(), t["completion"].as_i64());
    if p.is_none() && c.is_none() {
        return;
    }
    let total = price.cost(p.unwrap_or(0), c.unwrap_or(0));
    view["cost"]["total"] = json!(total);
    view["cost"]["estimated"] = json!(true);
}

/// Trace-level numbers over the normalised spans of one trace: model calls, tokens, cost, models,
/// tools, the session and the user.
pub fn trace_summary(views: &[&Value]) -> Value {
    let (mut calls, mut prompt, mut completion) = (0u64, 0i64, 0i64);
    let (mut cost, mut estimated) = (0.0f64, false);
    let (mut models, mut tools): (Vec<String>, Vec<String>) = (vec![], vec![]);
    let (mut session, mut user) = (None, None);
    for v in views {
        let kind = v["kind"].as_str().unwrap_or_default();
        if kind == "LLM" {
            calls += 1;
            prompt += v["tokens"]["prompt"].as_i64().unwrap_or(0);
            completion += v["tokens"]["completion"].as_i64().unwrap_or(0);
            cost += v["cost"]["total"].as_f64().unwrap_or(0.0);
            estimated |= v["cost"]["estimated"].as_bool().unwrap_or(false);
            if let Some(m) = v["model"].as_str()
                && !models.iter().any(|x| x == m)
            {
                models.push(m.into());
            }
        }
        if kind == "TOOL"
            && let Some(t) = v["tool"]["name"].as_str()
            && !tools.iter().any(|x| x == t)
        {
            tools.push(t.into());
        }
        session = session.or_else(|| v["session"].as_str().map(str::to_string));
        user = user.or_else(|| v["user"].as_str().map(str::to_string));
    }
    json!({"llm_calls": calls, "prompt_tokens": prompt, "completion_tokens": completion,
           "total_tokens": prompt + completion, "cost": cost, "cost_estimated": estimated,
           "models": models, "tools": tools, "session": session, "user": user})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attrs(v: Value) -> Attrs {
        v.as_object().cloned().unwrap_or_default()
    }

    #[test]
    fn flat_messages_are_put_back_in_order() {
        let a = attrs(json!({
            "openinference.span.kind": "LLM",
            "llm.input_messages.1.message.content": "hi",
            "llm.input_messages.1.message.role": "user",
            "llm.input_messages.0.message.role": "system",
            "llm.input_messages.0.message.content": "be brief",
            "llm.input_messages.10.message.role": "user",
            "llm.output_messages.0.message.role": "assistant",
            "llm.output_messages.0.message.tool_calls.0.tool_call.function.name": "lookup",
            "llm.output_messages.0.message.tool_calls.0.tool_call.function.arguments": "{\"a\":1}",
        }));
        let v = normalise(&a).unwrap();
        let input = v["messages"]["input"].as_array().unwrap();
        assert_eq!(input.len(), 3);
        assert_eq!(input[0]["role"], "system");
        assert_eq!(input[1]["content"], "hi");
        assert_eq!(
            input[2]["role"], "user",
            "index 10 sorts after 1, not after 0"
        );
        assert_eq!(
            v["messages"]["output"][0]["tool_calls"][0]["name"],
            "lookup"
        );
    }

    #[test]
    fn genai_fills_what_openinference_left_out() {
        let a = attrs(json!({
            "gen_ai.operation.name": "chat",
            "gen_ai.request.model": "gpt-4o",
            "gen_ai.response.model": "gpt-4o-2024",
            "gen_ai.provider.name": "openai",
            "gen_ai.usage.input_tokens": 10,
            "gen_ai.usage.output_tokens": 5,
            "gen_ai.conversation.id": "c-1",
            "gen_ai.input.messages": "[{\"role\":\"user\",\"parts\":[{\"type\":\"text\",\"content\":\"hello\"}]}]",
            "gen_ai.output.messages": [{"role":"assistant","parts":[{"type":"tool_call","id":"t1","name":"f","arguments":{"x":1}}]}],
            "gen_ai.system_instructions": "be kind",
        }));
        let v = normalise(&a).unwrap();
        assert_eq!(v["kind"], "LLM");
        assert_eq!(
            v["model"], "gpt-4o-2024",
            "the response model wins over the request model"
        );
        assert_eq!(v["tokens"]["total"], 15);
        assert_eq!(v["session"], "c-1");
        assert_eq!(v["messages"]["input"][0]["role"], "system");
        assert_eq!(v["messages"]["input"][1]["content"], "hello");
        assert_eq!(v["messages"]["output"][0]["tool_calls"][0]["name"], "f");
    }

    #[test]
    fn nested_json_messages_are_accepted() {
        let a = attrs(json!({
            "openinference.span.kind": "LLM",
            "llm.input_messages": [{"message": {"role": "user", "content": "q"}}],
        }));
        assert_eq!(
            normalise(&a).unwrap()["messages"]["input"][0]["content"],
            "q"
        );
    }

    #[test]
    fn embedding_vectors_are_counted_not_shown() {
        let a = attrs(json!({
            "openinference.span.kind": "EMBEDDING",
            "embedding.embeddings.0.embedding.text": "t",
            "embedding.embeddings.0.embedding.vector": [0.1, 0.2, 0.3],
        }));
        let v = normalise(&a).unwrap();
        assert_eq!(v["embeddings"][0]["dims"], 3);
        assert!(!v.to_string().contains("0.2"));
        let mut raw = a.clone();
        summarise_vectors(&mut raw);
        assert_eq!(
            raw["embedding.embeddings.0.embedding.vector"],
            "[vector, 3 dimensions]"
        );
    }

    #[test]
    fn documents_keep_score_and_order() {
        let a = attrs(json!({
            "openinference.span.kind": "RETRIEVER",
            "retrieval.documents.1.document.id": "b",
            "retrieval.documents.0.document.id": "a",
            "retrieval.documents.0.document.score": 0.9,
        }));
        let d = normalise(&a).unwrap()["documents"].clone();
        assert_eq!(d[0]["id"], "a");
        assert_eq!(d[0]["score"], 0.9);
        assert_eq!(d[1]["id"], "b");
    }

    #[test]
    fn a_span_without_llm_attributes_has_no_view() {
        assert!(normalise(&attrs(json!({"http.route": "/x"}))).is_none());
    }

    #[test]
    fn cost_is_reported_or_estimated_from_prices() {
        let mut a = attrs(
            json!({"openinference.span.kind": "LLM", "llm.model_name": "m",
            "llm.token_count.prompt": 1000, "llm.token_count.completion": 500}),
        );
        let mut v = normalise(&a).unwrap();
        let mut prices = std::collections::HashMap::new();
        prices.insert(
            "m".to_string(),
            Price {
                prompt_per_1m: 2.0,
                completion_per_1m: 4.0,
            },
        );
        estimate_cost(&mut v, &prices);
        assert!((v["cost"]["total"].as_f64().unwrap() - 0.004).abs() < 1e-12);
        assert_eq!(v["cost"]["estimated"], true);
        a.insert("llm.cost.total".into(), json!(0.5));
        let mut v = normalise(&a).unwrap();
        estimate_cost(&mut v, &prices);
        assert_eq!(v["cost"]["total"], 0.5, "a reported cost is never replaced");
        assert!(v["cost"]["estimated"].is_null());
    }

    #[test]
    fn protect_masks_hides_or_leaves_content() {
        let body = json!({"spans": [{"name": "chat", "attributes": {
            "llm.input_messages.0.message.content": "secret prompt",
            "llm.model_name": "gpt-4o", "input.value": "q"},
            "llm": {"model": "gpt-4o", "messages": {"input": [{"role": "user", "content": "secret prompt"}]},
                    "tokens": {"prompt": 3}, "input": {"value": "q", "mime": "text/plain"}}}]});
        let mut m = body.clone();
        assert!(protect(&mut m, Mode::Masked));
        let t = m.to_string();
        assert!(!t.contains("secret prompt"));
        assert!(t.contains("[masked: 13 characters]"));
        assert!(
            t.contains("gpt-4o") && t.contains("\"prompt\":3") && t.contains("\"role\":\"user\"")
        );
        let mut h = body.clone();
        assert!(protect(&mut h, Mode::Hidden));
        assert!(!h.to_string().contains("secret") && !h.to_string().contains("[masked"));
        let mut v = body.clone();
        assert!(!protect(&mut v, Mode::Visible));
        assert_eq!(v, body);
    }

    #[test]
    fn evaluations_from_attributes_and_events() {
        let a = attrs(
            json!({"openinference.span.kind": "EVALUATOR", "annotation.name": "correctness",
            "annotation.label": "correct", "annotation.score": 1, "annotation.explanation": "matches",
            "annotation.annotator_kind": "LLM"}),
        );
        let v = normalise(&a).unwrap();
        assert_eq!(v["evaluations"][0]["name"], "correctness");
        assert_eq!(v["evaluations"][0]["score"], 1.0);
        let idx = attrs(
            json!({"openinference.span.kind": "CHAIN", "evaluation.0.name": "a", "evaluation.1.name": "b", "evaluation.1.score": "0.5"}),
        );
        assert_eq!(
            normalise(&idx).unwrap()["evaluations"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        let ev = json!([{"name": "gen_ai.evaluation.result", "attributes": {"gen_ai.evaluation.name": "relevance", "gen_ai.evaluation.score.value": 0.8, "gen_ai.evaluation.score.label": "good"}},
                        {"name": "other", "attributes": {}},
                        {"name": "gen_ai.evaluation.result", "attributes": [{"key": "gen_ai.evaluation.name", "value": "tone"}]}]);
        let out = event_evaluations(&ev);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0]["score"], 0.8);
        assert_eq!(out[1]["name"], "tone");
    }

    #[test]
    fn nested_containers_and_csv_are_protected() {
        let mut v = json!({"attributes": {
            "llm.input_messages": [{"message": {"role": "user", "content": "secret nested"}}],
            "retrieval.documents": [{"document": {"content": "secret doc"}}],
            "llm.model_name": "m"}});
        assert!(protect(&mut v, Mode::Masked));
        let t = v.to_string();
        assert!(
            !t.contains("secret") && t.contains("\"llm.model_name\":\"m\""),
            "{t}"
        );
        let csv = "name,input.value,attrs\nchat,top secret,\"{\"\"output.value\"\":\"\"also secret\"\",\"\"n\"\":1}\"\n";
        for mode in [Mode::Masked, Mode::Hidden] {
            let out = protect_csv(csv, mode);
            assert!(!out.contains("secret"), "{mode:?}: {out}");
            assert!(out.contains("chat"));
        }
        assert_eq!(protect_csv(csv, Mode::Visible), csv);
    }

    #[test]
    fn price_limits() {
        assert!(
            Price {
                prompt_per_1m: -1.0,
                completion_per_1m: 0.0
            }
            .validate()
            .is_err()
        );
        assert!(
            Price {
                prompt_per_1m: f64::NAN,
                completion_per_1m: 0.0
            }
            .validate()
            .is_err()
        );
        assert!(
            Price {
                prompt_per_1m: 3.0,
                completion_per_1m: 15.0
            }
            .validate()
            .is_ok()
        );
        assert!(valid_model("gpt-4o") && !valid_model("") && !valid_model("a\nb"));
    }

    #[test]
    fn long_text_is_cut_with_its_size() {
        let a = attrs(
            json!({"openinference.span.kind": "CHAIN", "input.value": "x".repeat(TEXT_MAX + 10)}),
        );
        let v = normalise(&a).unwrap();
        let s = v["input"]["value"].as_str().unwrap();
        assert!(s.contains("[cut: ") && s.len() < TEXT_MAX + 60);
    }

    #[test]
    fn trace_summary_adds_up_model_calls_only() {
        let a = normalise(&attrs(
            json!({"openinference.span.kind": "LLM", "llm.model_name": "m",
            "llm.token_count.prompt": 10, "llm.token_count.completion": 5, "llm.cost.total": 0.25,
            "session.id": "s"}),
        ))
        .unwrap();
        let b = normalise(&attrs(
            json!({"openinference.span.kind": "TOOL", "tool.name": "t"}),
        ))
        .unwrap();
        let c = normalise(&attrs(
            json!({"openinference.span.kind": "CHAIN", "llm.token_count.prompt": 999}),
        ))
        .unwrap();
        let s = trace_summary(&[&a, &b, &c]);
        assert_eq!(s["llm_calls"], 1);
        assert_eq!(
            s["total_tokens"], 15,
            "tokens on a chain span are not double counted"
        );
        assert_eq!(s["tools"][0], "t");
        assert_eq!(s["session"], "s");
    }
}
