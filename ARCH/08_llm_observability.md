# ARCH 08: LLM observability (OpenInference and OTel GenAI)

Status: **steps L1 (v0.14.0, ADR 0015) and L2, L4, L5 with masking from L6 (v0.15.0, ADR 0016) built; L3 and the L6 alerts (v0.16.0, ADR 0017). The LLM track is complete.** This document records what was verified (2026-10-31), the design that follows from it, candidate requirements and the risks. It adds no requirement IDs to ARCH 00 yet: `make ac-map` requires every ID to be covered by an AC, so IDs are added when a release is written.

## 1. What "store OpenInference traces" means for grepui

grepui does not ingest (ARCH 06 non-goal). OpenInference spans are ordinary OpenTelemetry spans with conventional attributes, so they reach **GreptimeDB** over OTLP like any trace, and grepui must be able to **read, search and analyse** them. The work is therefore: (1) understand both storage layouts GreptimeDB offers, (2) a Connect page for LLM SDKs, (3) LLM-aware views, (4) analytics, sessions and evaluations on top.

## 2. The conventions (sources read)

**OpenInference** ([spec](https://raw.githubusercontent.com/Arize-ai/openinference/main/spec/semantic_conventions.md), [traces](https://raw.githubusercontent.com/Arize-ai/openinference/main/spec/traces.md), [README](https://raw.githubusercontent.com/Arize-ai/openinference/main/README.md)):

| Area | Attributes (verbatim names) |
|---|---|
| Span kind (required on every span) | `openinference.span.kind` = `LLM`, `EMBEDDING`, `CHAIN`, `RETRIEVER`, `RERANKER`, `TOOL`, `AGENT`, `GUARDRAIL`, `EVALUATOR`, `PROMPT`, `DECISION` (`traces.md` lists 10 and omits `DECISION`) |
| LLM call | `llm.model_name`, `llm.request.model_name`, `llm.response.model_name`, `llm.provider`, `llm.system`, `llm.invocation_parameters` (JSON), `llm.finish_reason`, `llm.function_call` |
| Tokens | `llm.token_count.prompt`, `.completion`, `.total`; details `llm.token_count.prompt_details.cache_read`/`.cache_write`/`.audio`, `llm.token_count.completion_details.reasoning`/`.audio` |
| Cost (USD) | `llm.cost.prompt`, `llm.cost.completion`, `llm.cost.total` and detail variants |
| Messages | flattened: `llm.input_messages.<i>.message.role`/`.content`, `llm.output_messages.<i>...`; tool calls `llm.output_messages.<i>.message.tool_calls.<j>.tool_call.function.name`/`.arguments`/`.id`; multimodal `message_content.*` |
| Prompt template | `llm.prompt_template.template`, `.variables`, `.version` |
| Input/output | `input.value`, `input.mime_type`, `output.value`, `output.mime_type` |
| Retrieval / reranker | `retrieval.documents` (items: `document.id`, `.score`, `.content`, `.metadata`, `.url`), `reranker.query`, `.model_name`, `.top_k`, `.input_documents`, `.output_documents` |
| Tool | `tool.name`, `tool.description`, `tool.parameters`, `tool.json_schema`, `tool.id`, `tool_call.function.name`/`.arguments` |
| Embedding | `embedding.model_name`, `embedding.embeddings.*` (`embedding.text`, `embedding.vector`) |
| Session, user, tags, metadata | `session.id`, `user.id`, `tag.tags`, `metadata` (JSON string) |
| Agent | `agent.name`, `graph.node.id`, `graph.node.name`, `graph.node.parent_id` |
| Evaluations / annotations | `annotation.name`/`.label`/`.score`/`.explanation`/`.annotator_kind`, also `evaluation.*` and `trace.`/`session.` scoped lists |
| Size guidance | none in the spec; `traces.md` says attribute values are strings, booleans, numbers or arrays of them |

Instrumentations (README): Python (OpenAI, Anthropic, LangChain, LlamaIndex, DSPy, Bedrock, CrewAI, Haystack, LiteLLM, Groq, Mistral, Vertex, Google GenAI/ADK, AutoGen, Pydantic AI, smolagents, Strands, Agno, MCP, and more), JavaScript (OpenAI, Anthropic, LangChain, Vercel AI SDK, Bedrock, MCP, ...), Java (LangChain4j, Spring AI), Go (OpenAI, Anthropic). Span processors convert OpenLIT and OpenLLMetry traces to OpenInference.

**OpenTelemetry GenAI** (now in [semantic-conventions-genai](https://github.com/open-telemetry/semantic-conventions-genai), status **Development**): `gen_ai.operation.name` (`chat`, `embeddings`, `execute_tool`, `invoke_agent`, `retrieval`, ...), `gen_ai.provider.name`, `gen_ai.request.model`, `gen_ai.response.model`, `gen_ai.usage.input_tokens`, `gen_ai.usage.output_tokens`, `gen_ai.response.finish_reasons`, `gen_ai.conversation.id`, `gen_ai.agent.*`, `gen_ai.tool.*`; content is opt-in (`gen_ai.input.messages`, `gen_ai.output.messages`, `gen_ai.system_instructions`, `gen_ai.tool.definitions`) and flagged as likely sensitive. An evaluation event `gen_ai.evaluation.result` exists. Two conventions will coexist in the wild, so grepui normalises both into one "LLM call" model, OpenInference first.

**Phoenix (the OpenInference reference backend)** ([config](https://arizeai-433a7140.mintlify.site/docs/phoenix/self-hosting/configuration.md)): OTLP over HTTP at `:6006/v1/traces` (protobuf) and gRPC `:4317`; `PHOENIX_COLLECTOR_ENDPOINT`, `PHOENIX_CLIENT_HEADERS`, `PHOENIX_PROJECT`. Its features: tracing, sessions, evaluations and annotations, prompt management and playground, span replay, datasets and experiments.

## 3. How GreptimeDB stores them (verified locally on v1.2.1 and v1.3.0)

| Option | Layout | Verified result |
|---|---|---|
| **Trace v1** (`greptime_trace_v1`, only option on v1.2.1) | every attribute becomes a column `span_attributes.<key>` | An OpenInference LLM span creates a column **per message index and per field** (`llm.input_messages.0.message.content`, `.1.`, ...). Columns grow with the longest conversation and with tool-call and document indexes: schema bloat, a wide sparse table. Works, but does not scale to agent conversations |
| **Trace v2** (`greptime_trace_v2`, **v1.3.0**, opt-in, separate table) | 19 fixed columns; `span_attributes`, `scope_attributes`, `resource_attributes` are JSON2; events and links are JSON | 181 OpenInference spans (308 KB request) stored with **no new columns**, including one span with 300 indexed messages and a 200 KB prompt; point read of message 299 in ~30 ms. Tokens and cost by model, span-kind mix, sessions, p95 and the project name all queried with `json_get_*(col, '["dotted.key"]')` |
| Collector-side flattening (OTTL `transform`) | fold `llm.input_messages.N.*` into one JSON array attribute before export | not tried; an option for v1 tables |

GreptimeDB's [data-model doc](https://docs.greptime.com/user-guide/traces/data-model) calls the trace pipeline section experimental. v1.3.0 notes: v2 uses a separate table ("Use a separate destination table from Trace V1"), existing v1 tables are not migrated, a v2 write to a v1 table is rejected, events and links stay JSON (`ARRAY(JSON2)` deferred). The JSON2 [deep dive](https://greptime.com/blogs/2026-09-23-greptimedb-json2-deep-dive.html) states bounded automatic shredding (`max_auto_expanded_paths`, default 100) with type hints settable only at CREATE time in 1.2. **Recommendation for LLM traces: trace v2 on GreptimeDB v1.3.0 or newer.**

What grepui did not handle on v2 before v0.14.0 (verified; fixed by step L1, ADR 0015): the trace page's span panel returns empty attributes (it reads flat columns) and `q=tool.name=x` is rejected as "not a column". Everything else (list, scatter, histogram, waterfall, flame graph, logs pivot, SQL mode, JSON2 decoding) works on a v2 table.

## 4. Where this stands against the reference platform (from its own docs, not tested)

The two LLM pages of its public documentation that were read (the "LLM Applications" and "LLM Monitoring" pages under its AI integrations section, 2026-10-31) describe LLM telemetry as ordinary OpenTelemetry with `llm.usage.*` / `gen_ai.*` attributes, computed token and cost fields (cost only when pricing is enabled), Insights (cost, tokens, traces, p95, error rate), Sessions with turn-by-turn view, and an Agent Graph noted as Enterprise and Cloud only. Neither page mentions OpenInference. So **OpenInference-native views and an Apache-2.0 agent graph are differentiators as far as those pages show**; the product itself was not run, and no claim of being "more advanced" is made or needed. Re-read the pages before publishing any comparison.

## 5. GreptimeDB roadmap items that matter (sources: [roadmap #7685](https://github.com/GreptimeTeam/greptimedb/issues/7685), [mid-year review](https://greptime.com/blogs/2026-09-18-greptimedb-roadmap-2026-midyear-review.html), [v1.3.0 release](https://github.com/GreptimeTeam/greptimedb/releases/tag/v1.3.0))

| GreptimeDB item | State | Effect on grepui |
|---|---|---|
| v1.3.0, trace v2 with JSON2 attributes | **shipped 2026-10-28** | LLM track depends on it; grepui is pinned to v1.3.0 |
| Semi-join / `IN` / `EXISTS` correctness | works on v1.3.0 (our retest), not in release notes | joins allowed again for new features, with live proof (ADR 0007) |
| v1.4 "Flow engine enhancements" (roadmap, Q3) | planned | revisit the rollup design (RELEASE plan 0.7) then |
| Vector index and AI functions | **dropped** from the 2026 roadmap; v1.4 plans to remove the vector index | no embedding similarity in grepui; embedding spans are displayed, not searched |
| LLM / GenAI observability feature | **none found** in roadmap, releases or blog (a March 2026 post shows OTel GenAI data stored as flattened columns plus a conversation table) | grepui's LLM work is not waiting on GreptimeDB |
| Semantic entity graph with a `gen_ai` naming pack | experimental in v1.3.0 | watch; could feed an agent graph later |
| Jaeger query endpoints for Jaeger UI v2 | nightly | an alternative read path, not needed |
| Query cancel / process list | not found in the last six releases | the work-group release (query management) needs a live check before it is planned |
| JSON2 limits | path cap 100, hints at CREATE only, bulk insert unsupported | document in Connect; recommend hints for high-cardinality prefixes |

## 6. Design

1. **Attribute model abstraction.** One accessor per trace table kind: `Flat` (v1: `CAST(col AS STRING)` over validated column names) and `Json` (v2: `json_get_string(span_attributes, ?)` with the key bound as the path `["key"]`). Detected from the table's DDL (JSON2 attribute columns mean v2). Builders, the filter resolver, the span panel and the top-values endpoint go through it; keys are bound parameters in both.
2. **Normalised LLM view.** A pure mapping module turns OpenInference and `gen_ai.*` attributes into `{kind, model, provider, tokens{prompt,completion,total,cache,reasoning}, cost, messages[], tool_calls[], documents[], input, output, session, user, tags}`. Flattened `llm.input_messages.N.*` are reassembled by index; nested JSON (as in `traces.md`) is also accepted. Pure functions, tested with fixtures from the spec.
3. **LLM-aware UI.** Span kind badges and filters, a chat-style transcript with tool calls, token and cost chips, retrieval documents with scores, embeddings shown without vectors, long values collapsed with size shown, content hidden by redaction rules for roles without permission.
4. **Analytics** run as bounded aggregates over the JSON columns (tokens, cost, latency percentiles, error rate by model, tool and span kind); long ranges use the rollup planner. Cost uses `llm.cost.*` when present, else an optional per-model price table kept in the state store.
5. **Sessions** group by `session.id` (and `gen_ai.conversation.id`), showing the conversation turn by turn; users by `user.id`.
6. **Evaluations and annotations**: read `annotation.*`/`evaluation.*` and `gen_ai.evaluation.result`; human feedback is stored in `grepui_*` tables (state store, org scoped).
7. **Projects.** Phoenix groups traces by project. grepui maps a project to a resource attribute filter (the attribute name is to be confirmed against Phoenix; the spike sent `openinference.project.name` in `resource_attributes` and could read it back), inside an org.

## 7. Candidate requirements (not yet in ARCH 00)

| Candidate | Statement |
|---|---|
| R-AI1 | The trace table kind (flat v1 or JSON2 v2) is detected and every trace feature (filters, span panel, top values, detail) works on both |
| R-AI2 | Connect page for OpenInference and OTel GenAI SDKs: Python/JS snippets, the v2 pipeline and table headers, protobuf-only, content-size and privacy notes |
| R-AI3 | Normalised LLM call model from OpenInference and `gen_ai.*`, with indexed-message reassembly |
| R-AI4 | LLM span views: kind badges and filters, transcript, tool calls, token and cost, retrieval documents, embeddings without vectors |
| R-AI5 | LLM analytics: tokens, cost, latency percentiles, error rate by model, tool and kind, over bounded windows |
| R-AI6 | Sessions and users: conversation view by `session.id` / `gen_ai.conversation.id` |
| R-AI7 | Annotations and evaluations: read from spans, write human feedback to the state store |
| R-AI8 | Content protection: redaction by role, size caps and collapsed display for prompts and completions |
| R-AI9 | Agent graph from `graph.node.*`, `agent.name` and span parentage |
| R-AI10 | Alerts on LLM metrics (cost, tokens, error rate) |

## 8. Risks and open questions

- **Trace v2 is experimental** in GreptimeDB (its doc says so): table layout may change in later versions. Mitigate by isolating attribute access behind one accessor and testing on the pinned version in CI.
- **No migration** from v1 to v2 tables: users with existing flat tables keep them; both must keep working.
- **Sensitive content.** Prompts and completions contain user data (the GenAI conventions say so). Redaction, RBAC per org or project, and TTL per table are prerequisites for recommending this in production.
- **Value size.** No size guidance exists in the spec; a 200 KB value stored and read fine in the spike, but UI collapse, API payload caps and an export cap are needed. A hard limit for OTLP request size on GreptimeDB was not found in its docs.
- **Phoenix SDK to GreptimeDB.** Whether `arize-phoenix-otel` can send to GreptimeDB's `/v1/otlp/v1/traces` with the pipeline headers (standard `OTEL_EXPORTER_OTLP_*` variables are the likely route) has **not been tested**; it needs a spike with the real instrumentor packages.
- **Project concept.** The Phoenix resource attribute for a project name was not confirmed from its docs.
- **Two conventions.** OpenInference and `gen_ai.*` overlap (model, tokens, conversation). The normaliser must define precedence, and the GenAI conventions are still in Development status.
- **Pinned version drift.** v1.4 nightly builds exist; any upgrade needs the ADR 0007 table and the full live suite rerun.
