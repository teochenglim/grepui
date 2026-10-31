//! LLM analytics statements (R-AI5, R-AI6, ADR 0017): calls, errors, latency percentiles, tokens
//! and cost by model, provider, span kind and tool; sessions and users. They read raw spans of a
//! JSON-layout traces table (attributes in JSON columns), inside the window cap.
//!
//! Every attribute name is a bound JSON path (`json_get_*(col, ?)`), written in the order the
//! placeholders appear. Tokens and cost are summed over model-call spans only, so a parent span
//! that repeats its child's token counts is not counted twice. A request filter (`q`, service,
//! errors) narrows which spans are counted, like on the Traces page.

use super::{
    Bind, Q, Window, logs, qualified,
    traces::{self, TraceFilter},
    ts_string,
};
use crate::llm::{
    COMPLETION_TOKENS, COST_TOTAL, KIND, MODEL, OP, PROMPT_TOKENS, PROVIDER, SESSION, TOOL_NAME,
    USER,
};

const SA: &str = "`span_attributes`";
const TS: &str = traces::TS;
const DUR: &str = "(CAST(`timestamp_end` AS BIGINT) - CAST(`timestamp` AS BIGINT))";
const ERR: &str = "`span_status_code` = 'STATUS_CODE_ERROR'";
/// Rows a breakdown returns at most.
pub const TOP: u32 = 100;
pub const MAX_OFFSET: u32 = 10_000;
/// Longest piece of a prompt or answer shown in a session's turn list.
pub const SNIPPET: u32 = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum By {
    Model,
    Provider,
    Kind,
    Tool,
}

impl By {
    pub fn parse(s: &str) -> Option<By> {
        Some(match s {
            "model" => By::Model,
            "provider" => By::Provider,
            "kind" => By::Kind,
            "tool" => By::Tool,
            _ => return None,
        })
    }
}

/// Binds in the order their placeholders are written.
#[derive(Default)]
struct B {
    binds: Vec<Bind>,
}

impl B {
    fn path(&mut self, key: &str) {
        self.binds.push(Bind::Str(logs::json_path(key)));
    }
    fn s(&mut self, key: &str) -> String {
        self.path(key);
        format!("json_get_string({SA}, ?)")
    }
    fn i(&mut self, key: &str) -> String {
        self.path(key);
        format!("json_get_int({SA}, ?)")
    }
    fn f(&mut self, key: &str) -> String {
        self.path(key);
        format!("json_get_float({SA}, ?)")
    }
    fn first_s(&mut self, keys: &[&str]) -> String {
        let parts: Vec<String> = keys.iter().map(|k| self.s(k)).collect();
        format!("coalesce({})", parts.join(", "))
    }
    fn first_i(&mut self, keys: &[&str]) -> String {
        let parts: Vec<String> = keys.iter().map(|k| self.i(k)).collect();
        format!("coalesce({})", parts.join(", "))
    }
    /// A model call: OpenInference kind LLM, or a GenAI chat-like operation.
    fn is_llm(&mut self) -> String {
        let k = self.s(KIND);
        let o = self.s(OP);
        format!("({k} = 'LLM' OR {o} IN ('chat', 'text_completion', 'generate_content'))")
    }
    fn is_tool(&mut self) -> String {
        let k = self.s(KIND);
        let o = self.s(OP);
        format!("({k} = 'TOOL' OR {o} = 'execute_tool')")
    }
}

#[derive(Clone, Copy)]
enum Sum {
    Prompt,
    Completion,
    Cost,
    UncostedPrompt,
    UncostedCompletion,
}

/// `sum(...)` of tokens or cost over model-call spans (zero for the rest).
fn llm_sum(b: &mut B, alias: &str, what: Sum) -> String {
    let l = b.is_llm();
    let body = match what {
        Sum::Prompt => format!("coalesce({}, 0)", b.first_i(PROMPT_TOKENS)),
        Sum::Completion => format!("coalesce({}, 0)", b.first_i(COMPLETION_TOKENS)),
        Sum::Cost => format!("coalesce({}, 0)", b.f(COST_TOTAL)),
        Sum::UncostedPrompt | Sum::UncostedCompletion => {
            let c = b.f(COST_TOTAL);
            let n = if matches!(what, Sum::UncostedPrompt) {
                b.first_i(PROMPT_TOKENS)
            } else {
                b.first_i(COMPLETION_TOKENS)
            };
            return format!(
                "sum(CASE WHEN {l} AND {c} IS NULL THEN coalesce({n}, 0) ELSE 0 END) AS `{alias}`"
            );
        }
    };
    format!("sum(CASE WHEN {l} THEN {body} ELSE 0 END) AS `{alias}`")
}

/// calls, errors, latency and the token and cost sums: the columns every breakdown has.
fn measures(b: &mut B) -> String {
    let mut cols = vec![
        "count(*) AS `calls`".to_string(),
        format!("sum(CASE WHEN {ERR} THEN 1 ELSE 0 END) AS `errors`"),
        format!("approx_percentile_cont({DUR}, 0.5) AS `p50_ns`"),
        format!("approx_percentile_cont({DUR}, 0.95) AS `p95_ns`"),
        format!("approx_percentile_cont({DUR}, 0.99) AS `p99_ns`"),
        format!("avg({DUR}) AS `avg_ns`"),
    ];
    for (alias, what) in [
        ("prompt_tokens", Sum::Prompt),
        ("completion_tokens", Sum::Completion),
        ("cost", Sum::Cost),
        ("uncosted_prompt", Sum::UncostedPrompt),
        ("uncosted_completion", Sum::UncostedCompletion),
    ] {
        cols.push(llm_sum(b, alias, what));
    }
    cols.join(", ")
}

/// Window, then the restriction, the optional model and the request filter.
fn where_sql(
    b: &mut B,
    f: &TraceFilter,
    w: Window,
    model: Option<&str>,
    restrict: impl FnOnce(&mut B) -> String,
) -> String {
    let mut parts = vec![format!("{TS} >= ?"), format!("{TS} < ?")];
    b.binds.push(Bind::Str(ts_string(w.from_ms)));
    b.binds.push(Bind::Str(ts_string(w.to_ms)));
    parts.push(restrict(b));
    if let Some(m) = model {
        let e = b.first_s(MODEL);
        parts.push(format!("{e} = ?"));
        b.binds.push(Bind::Str(m.to_string()));
    }
    if let Some((p, pb)) = traces::span_pred(f) {
        parts.push(p);
        b.binds.extend(pb);
    }
    parts.join(" AND ")
}

/// Calls, errors, latency, tokens and cost per model, provider, span kind or tool.
pub fn breakdown(
    db: &str,
    table: &str,
    by: By,
    f: &TraceFilter,
    model: Option<&str>,
    w: Window,
) -> anyhow::Result<Q> {
    let t = qualified(db, table)?;
    let mut b = B::default();
    let grp = match by {
        By::Model => format!("coalesce({}, '(unknown)')", b.first_s(MODEL)),
        By::Provider => format!("coalesce({}, '(unknown)')", b.first_s(PROVIDER)),
        By::Kind => {
            let k = b.s(KIND);
            let o = b.s(OP);
            format!("coalesce({k}, {o}, '(none)')")
        }
        By::Tool => format!("coalesce({}, `span_name`)", b.first_s(TOOL_NAME)),
    };
    let cols = measures(&mut b);
    let wh = where_sql(&mut b, f, w, model, |b| match by {
        By::Model | By::Provider => b.is_llm(),
        By::Kind => {
            let k = b.s(KIND);
            let o = b.s(OP);
            format!("({k} IS NOT NULL OR {o} IS NOT NULL)")
        }
        By::Tool => b.is_tool(),
    });
    Ok(Q::new(
        "llm_breakdown",
        format!(
            "SELECT {grp} AS `grp`, {cols} FROM {t} WHERE {wh} GROUP BY 1 ORDER BY `calls` DESC LIMIT {TOP}"
        ),
        b.binds,
    ))
}

/// One row over every model call in the window: for whole-window latency and error rate, which
/// cannot be added up from per-model rows.
pub fn totals(
    db: &str,
    table: &str,
    f: &TraceFilter,
    model: Option<&str>,
    w: Window,
) -> anyhow::Result<Q> {
    let t = qualified(db, table)?;
    let mut b = B::default();
    let cols = measures(&mut b);
    let wh = where_sql(&mut b, f, w, model, |b| b.is_llm());
    Ok(Q::new(
        "llm_totals",
        format!("SELECT {cols} FROM {t} WHERE {wh}"),
        b.binds,
    ))
}

/// Model calls over time: calls, errors, tokens, reported cost and p95 per bucket.
pub fn series(
    db: &str,
    table: &str,
    f: &TraceFilter,
    model: Option<&str>,
    w: Window,
) -> anyhow::Result<Q> {
    let t = qualified(db, table)?;
    let (ms, _) = logs::bucket_for(w.to_ms - w.from_ms);
    let mut b = B::default();
    let cols = [
        "count(*) AS `calls`".to_string(),
        format!("sum(CASE WHEN {ERR} THEN 1 ELSE 0 END) AS `errors`"),
        format!("approx_percentile_cont({DUR}, 0.95) AS `p95_ns`"),
        llm_sum(&mut b, "prompt_tokens", Sum::Prompt),
        llm_sum(&mut b, "completion_tokens", Sum::Completion),
        llm_sum(&mut b, "cost", Sum::Cost),
    ]
    .join(", ");
    let wh = where_sql(&mut b, f, w, model, |b| b.is_llm());
    Ok(Q::new(
        "llm_series",
        format!(
            "SELECT (CAST({TS} AS BIGINT) / {}) * {ms} AS bucket_ms, {cols} FROM {t} WHERE {wh} GROUP BY 1 ORDER BY 1",
            ms * 1_000_000
        ),
        b.binds,
    ))
}

/// The aggregates a session or a user row carries.
fn who_columns(b: &mut B, with_user: bool) -> String {
    let mut cols = vec![
        "count(DISTINCT `trace_id`) AS `traces`".to_string(),
        "count(*) AS `spans`".to_string(),
    ];
    let l = b.is_llm();
    cols.push(format!(
        "sum(CASE WHEN {l} THEN 1 ELSE 0 END) AS `llm_calls`"
    ));
    cols.push(format!(
        "sum(CASE WHEN {ERR} THEN 1 ELSE 0 END) AS `errors`"
    ));
    for (alias, what) in [
        ("prompt_tokens", Sum::Prompt),
        ("completion_tokens", Sum::Completion),
        ("cost", Sum::Cost),
        ("uncosted_prompt", Sum::UncostedPrompt),
        ("uncosted_completion", Sum::UncostedCompletion),
    ] {
        cols.push(llm_sum(b, alias, what));
    }
    cols.push(format!("min(CAST({TS} AS BIGINT)) / 1000000 AS `first_ms`"));
    cols.push("max(CAST(`timestamp_end` AS BIGINT)) / 1000000 AS `last_ms`".to_string());
    if with_user {
        let u = b.first_s(USER);
        cols.push(format!("count(DISTINCT {u}) AS `users`"));
        let u = b.first_s(USER);
        cols.push(format!("max({u}) AS `user`"));
    } else {
        let s = b.first_s(SESSION);
        cols.push(format!("count(DISTINCT {s}) AS `sessions`"));
    }
    cols.join(", ")
}

/// Sessions (`session.id`, or `gen_ai.conversation.id`), most recent first.
pub fn sessions(
    db: &str,
    table: &str,
    f: &TraceFilter,
    w: Window,
    offset: u32,
) -> anyhow::Result<Q> {
    let t = qualified(db, table)?;
    let mut b = B::default();
    let sid = b.first_s(SESSION);
    let cols = who_columns(&mut b, true);
    let wh = where_sql(&mut b, f, w, None, |b| {
        format!("{} IS NOT NULL", b.first_s(SESSION))
    });
    Ok(Q::new(
        "llm_sessions",
        format!(
            "SELECT {sid} AS `session_id`, {cols} FROM {t} WHERE {wh} GROUP BY 1 ORDER BY `last_ms` DESC LIMIT {} OFFSET {}",
            w.limit,
            offset.min(MAX_OFFSET)
        ),
        b.binds,
    ))
}

/// Users (`user.id`), most recently active first.
pub fn users(db: &str, table: &str, f: &TraceFilter, w: Window, offset: u32) -> anyhow::Result<Q> {
    let t = qualified(db, table)?;
    let mut b = B::default();
    let uid = b.first_s(USER);
    let cols = who_columns(&mut b, false);
    let wh = where_sql(&mut b, f, w, None, |b| {
        format!("{} IS NOT NULL", b.first_s(USER))
    });
    Ok(Q::new(
        "llm_users",
        format!(
            "SELECT {uid} AS `user_id`, {cols} FROM {t} WHERE {wh} GROUP BY 1 ORDER BY `last_ms` DESC LIMIT {} OFFSET {}",
            w.limit,
            offset.min(MAX_OFFSET)
        ),
        b.binds,
    ))
}

/// The turns of one session: one row per trace, oldest first, with the root span's input and
/// output cut to a snippet.
pub fn session_turns(db: &str, table: &str, session: &str, w: Window) -> anyhow::Result<Q> {
    let t = qualified(db, table)?;
    let mut b = B::default();
    let l = b.is_llm();
    let calls = format!("sum(CASE WHEN {l} THEN 1 ELSE 0 END) AS `llm_calls`");
    let mut cols = vec![
        format!("min(CAST({TS} AS BIGINT)) / 1000000 AS `start_ms`"),
        format!(
            "(max(CAST(`timestamp_end` AS BIGINT)) - min(CAST({TS} AS BIGINT))) / 1000 AS `duration_us`"
        ),
        "count(*) AS `spans`".to_string(),
        format!("sum(CASE WHEN {ERR} THEN 1 ELSE 0 END) AS `errors`"),
        calls,
    ];
    for (alias, what) in [
        ("prompt_tokens", Sum::Prompt),
        ("completion_tokens", Sum::Completion),
        ("cost", Sum::Cost),
        ("uncosted_prompt", Sum::UncostedPrompt),
        ("uncosted_completion", Sum::UncostedCompletion),
    ] {
        cols.push(llm_sum(&mut b, alias, what));
    }
    for (alias, key) in [("input", "input.value"), ("output", "output.value")] {
        let v = b.s(key);
        cols.push(format!(
            "max(CASE WHEN {} THEN substr(coalesce({v}, ''), 1, {SNIPPET}) END) AS `{alias}`",
            traces::ROOT_SQL
        ));
    }
    let cols = cols.join(", ");
    let wh = where_sql(&mut b, &TraceFilter::default(), w, None, |b| {
        format!("{} = ?", b.first_s(SESSION))
    });
    b.binds.push(Bind::Str(session.to_string()));
    Ok(Q::new(
        "llm_session_turns",
        format!(
            "SELECT `trace_id`, {cols} FROM {t} WHERE {wh} GROUP BY `trace_id` ORDER BY `start_ms` ASC LIMIT {}",
            w.limit
        ),
        b.binds,
    ))
}
