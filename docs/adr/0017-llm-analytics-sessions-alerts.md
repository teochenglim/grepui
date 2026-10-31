# 0017 LLM analytics, sessions and alerts on LLM metrics

Status: Accepted (v0.16.0, LLM track step L3 and the alert part of L6).

## Context

Step L2 (ADR 0016) made one trace readable. Operating an LLM application also needs the aggregate view: how many calls, tokens and dollars, which model fails or is slow, what a session or a user did, and a way to be told when cost or the error rate moves.

## Decision

- **Raw spans, bounded, JSON tables only.** The statements in `query::llm` read the org's trace v2 table (attributes in JSON columns) inside the existing window cap (31 days). A flat table is refused with a pointer to Connect rather than guessed at: its attribute columns are named per key, and a statement over a column that may not exist is a statement that fails at random. No rollup is used; a rollup planner for long ranges stays out of scope.
- **Every attribute name is a bound JSON path** (`json_get_*(span_attributes, ?)`), in the order the placeholders are written, like ADR 0015. The model filter, the session id and the request filter (`q`, service, errors: the Traces page's filter bar) are binds too. A test checks that no attribute name appears in any statement.
- **A model call** is a span with `openinference.span.kind = 'LLM'` or `gen_ai.operation.name` in `chat`, `text_completion`, `generate_content`. Tokens and cost are summed over model calls only, so an agent or chain span that repeats its child's token counts is not counted twice. Latency, errors and calls in a breakdown by span kind or tool count every span of that kind.
- **Cost** is what the spans reported (`llm.cost.total`) plus the price table (ADR 0016) applied to the tokens of calls that reported none, shown apart (`reported`, `estimated`, `total`). The estimate needs a model, so it is computed per model; breakdowns by provider, kind or tool, sessions and users show reported cost only.
- **Latency percentiles** are `approx_percentile_cont` over the span duration: approximate, and not additive, so whole-window p50/p95/p99 come from a totals query, not from per-model rows.
- **Sessions** group by `session.id`, else `gen_ai.conversation.id` (`coalesce`); users by `user.id`. A session opens turn by turn: one row per trace, oldest first, with the root span's input and output cut to 500 characters. Those fields are named `input.value` and `output.value` in the response, so the content rules of ADR 0016 mask them below admin with no handler code. Only spans that carry the session id are counted, which is how instrumentations that set it on every span (OpenInference's context helpers) behave.
- **Alerts.** A rule's query kind `llm` (`metric`: `cost`, `tokens`, `calls`, `error_rate` percent, `latency_p95` ms; optional `model`; `per_model`) runs through `api::llm::alert_values`, which calls the same breakdown and totals statements as the page, so a rule and the page cannot disagree. It is a threshold rule (scheduled or real-time): an anomaly rule over it is refused for now. It uses the lease, dedup, history, cooldown and preview of every other rule; the label `model` is the dedup field for one alert per model. With no model call in the window, a rate or latency has no value (no alert) while cost, tokens and calls are 0.

## Consequences

The LLM page and the alert agree to the last digit and are tested against counts computed from a generated batch on a real GreptimeDB v1.3.0. Aggregates over long ranges read raw spans, so a big org with a 31-day window pays for it; the work-group limits (ADR 0013) and the result cache apply as for any read. Services RED on the same table still counts only server and consumer spans, so internal LLM spans read 0 there; the LLM page is the place for them. No schema change, no new dependency.
