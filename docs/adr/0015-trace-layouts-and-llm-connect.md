# 0015 Trace layouts (flat and JSON2) and the LLM Connect section

Status: Accepted (v0.14.0, LLM track step L1).

## Context

GreptimeDB v1.3.0 offers two trace tables. Trace v1 (`greptime_trace_v1`) makes one column per span attribute (`span_attributes.<key>`), which does not suit LLM spans: OpenInference writes a column per message index and field. Trace v2 (`greptime_trace_v2`) keeps 19 fixed columns and stores attributes as JSON2 objects (ARCH 03, ARCH 08). grepui's trace filters, span panel and detail read the flat columns, so a v2 table gave empty attributes and refused `tool.name=...` as an unknown column.

## Decision

- **Detect, don't configure.** `query::traces::Layout::detect` looks at the table's column names (already read and cached for the filter resolver): a column named exactly `span_attributes` means JSON, otherwise flat. `GET /orgs/{org}/traces/layout` reports it for the UI.
- **One accessor per layout.** Filter keys resolve to `TCol::Attr` (flat, validated against the real column list as before) or `TCol::Json { col, key }` (JSON). A JSON condition is `json_get_string(<col>, ?)` with the path `["key"]` bound ahead of the value; the key is checked only for shape (non-empty, at most 200 characters, no control characters), because any key can be asked for, and it never appears in the statement. `json_get_string` renders numbers and booleans as text, so `llm.token_count.total=510` works like a string match (verified live). The `resource.` prefix selects `resource_attributes`; `span_attributes.` and `attr.` prefixes are accepted as before.
- **Span panel.** `layout::extract` reads attribute objects when the row has them and flat columns when it has those; values keep their JSON type. Strings over 32 KiB are cut with a note of the full size, because an LLM prompt can be hundreds of kilobytes and the detail view sends every attribute of every span.
- **Connect.** The page gains an LLM section: the v2 headers, a table name derived from the org (`<org>_llm_traces`, hyphens to underscores), Python (OpenInference OpenAI instrumentor with the OpenTelemetry exporter), environment and collector snippets, and notes on a separate table, privacy and size. The project is read from the resource attribute `openinference.project.name` (the OpenInference semantic-convention constant, confirmed from the installed package). A test sends a batch with the very headers the page publishes.
- **Demo data.** `playground --llm-data` seeds OpenInference-shaped traces into a v2 table `llm_traces` (AGENT, CHAIN, RETRIEVER, EMBEDDING, LLM with indexed messages and tool calls, TOOL; tokens, cost, session and user).

## Consequences

Flat tables behave exactly as before, including refusing an unknown attribute. An org reads one traces table from its own database, and two orgs cannot share a database, so LLM traces need their own org and database (or `--traces-table`). The Services page works on a v2 table but its request and error rates count server and consumer spans only; OpenInference spans are internal, so they read 0 until the LLM analytics step. Trace v2 is experimental in GreptimeDB, so the layout is isolated behind `Layout` and `TCol` and tested on the pinned version. Not tested: the Phoenix package `arize-phoenix-otel` itself.
