# ARCH 00: Requirements Traceability

Every requirement has an ID here. Every ID must appear in the `Reqs` column of **at least one AC** in
`RELEASE/vX.Y.Z.md`, and every AC must be governed by **at least two tests** (`scripts/ac-map.sh`, run by
`make ac-map` and `make check`). Chain: **R-ID → AC → tests**.

Format is parsed by the script: keep `| R-XX | … |` rows and the `AC` ids in release files intact.
Tests are Rust (`fn ac_<M>_<m>_<n>_<name>`); a `_live` suffix marks a test that needs GreptimeDB and is `#[ignore]`d (`make test-db`).
UI behaviour is checked by API-level tests plus a recorded manual/browser pass per release (screenshots in `images/`); the frontend has no logic that needs its own test runner (ARCH 06 D7).

Parity with the reference platform is tracked in [07_parity_matrix.md](07_parity_matrix.md); every "planned" row there points at an ID below.

78 requirements, 82 acceptance criteria.

| ID | Area | Requirement |
|---|---|---|
| R-F1 | Foundation | Config from flags and env (`--listen`, `--greptime-mysql`, `--greptime-http`, table names, ...); defaults work against a local GreptimeDB; table names are validated identifiers |
| R-F2 | Foundation | `GET /healthz` answers `ok` without the DB; `GET /api/v1/health` reports DB reachability; `GET /api/v1/info` reports version and configuration; `grepui healthcheck` for container probes |
| R-F3 | Foundation | The UI (HTML, CSS, ES modules) is embedded in the binary; unknown non-API paths fall back to `index.html`; unknown `/api/*` paths return JSON 404; assets carry content type and cache headers |
| R-F4 | Foundation | Data plane is read-only by construction: telemetry tables are reached only through SELECT/SHOW/TQL builders. The complete list of write targets (`grepui_*` metadata tables, and admin-gated retention, pipeline and flow operations) is one allowlist, asserted by tests |
| R-F5 | Foundation | Multi-tenant routing from day one: every data and state route is org-scoped (`/api/v1/orgs/{org}/...`); an org resolves to a GreptimeDB source, database and table names; out of the box there is one `default` org; unknown org is 404; no route can reach another org's database |
| R-Q1 | Query safety | SQL text comes only from fixed builders; table names are validated identifiers; every request-derived value is a bound parameter. The one exception, SQL mode (R-L5), is parsed and allowlisted |
| R-Q2 | Query safety | Every data endpoint requires a time window, caps its length, and caps row limits |
| R-Q3 | Query safety | Queries have a timeout; failures return JSON `{"error"}` with the right status and never leak connection strings or SQL internals |
| R-Q4 | Query safety | Every data response carries `meta`: executed statement tag, DB time in ms, row count, data source ("Show SQL") |
| R-T1 | Streams | Streams list (logs, traces, metrics and other tables) with type, row count, oldest and newest timestamp, plus per-table schema (column, type, role: tag, field, time index) |
| R-I1 | Ingestion | Connect page: OTLP traces, logs and metrics endpoints, Prometheus remote write, required headers and pipeline names, with copy-ready curl, OTel Collector and SDK env snippets built from the real configured address; states that OTLP is protobuf-only |
| R-I2 | Ingestion | Data health: rows per minute over the last hour per stream, newest-row age, last-seen per service, quiet-stream warning |
| R-L1 | Logs | Log search: window, level, free text, `key=value` / `key!=value` / `key~substring` conditions, paging; attribute keys are bound |
| R-L2 | Logs | Log histogram over the same filters; bucket size chosen from the window from a fixed set |
| R-L3 | Logs | Field list with types, interesting-fields (quick) mode, and top values per allowlisted field for the current filters |
| R-L4 | Logs | Source details for one record, and Search Around: N neighbouring records in storage order, ignoring the original filters |
| R-L5 | Logs | SQL mode: exactly one read-only statement (SELECT, WITH, SHOW, DESC, EXPLAIN) validated by a SQL parser and confined to the org's database for every statement variant, with row cap, window macros, timeout. TQL is refused (its `__schema__`/`__database__` matchers reach other databases) |
| R-L6 | Logs | Full-text search over `body` using GreptimeDB's term matching, with a documented fallback when the fulltext index is absent |
| R-L7 | Logs | Export of the current result as CSV or NDJSON, capped at 10 000 rows; spreadsheet formulas in text cells are defused |
| R-G1 | Demo | Playground mode: `grepui playground --demo-data` waits for GreptimeDB, seeds deterministic demo traces and logs through GreptimeDB's OTLP endpoint, and keeps a trickle of live demo traffic going. It writes only to a database named `demo` or `demo_*`, only when it has no traces, and only from this command: `serve` has no ingest path (ADR 0005) |
| R-S1 | State | `grepui migrate` creates and upgrades the `grepui_*` metadata tables idempotently; the binary refuses to start against a newer schema |
| R-S2 | State | Saved views: store, list, rename, delete, share by id; a view restores query, window and columns |
| R-S3 | State | Query history per user with caps and retention; one-click rerun |
| R-S5 | State | Every metadata row carries `org_id` and every metadata builder takes the org as a required, bound argument; a cross-org read or write is impossible by construction and covered by a test over all builders |
| R-S4 | State | Stream details: ingested size, time range, inferred field types, fulltext/partition/index information read from GreptimeDB, TTL shown |
| R-R1 | Traces | Trace list: one row per trace (start, duration, spans, services, errors, root service/operation) with filters and paging |
| R-R2 | Traces | Trace histogram and latency scatter data over the same filters |
| R-R3 | Traces | Trace detail: all spans of one `trace_id` (validated hex), ordered, parent links, waterfall (timeline) layout data |
| R-R4 | Traces | Flame graph layout data for one trace |
| R-R5 | Traces | Span panel: attributes, resource attributes, events and links of one span |
| R-R6 | Traces | Correlation: logs for a trace or span; trace link from a log row; trace page shows its logs |
| R-V1 | Services | Services list with request rate, error rate, avg and p95 latency from server/consumer spans |
| R-V2 | Services | RED time series per service and operation breakdown |
| R-V3 | Services | Service dependencies (caller to callee, calls, errors) and graph layout data for the service map |
| R-V4 | Services | Service catalog: every service seen, first/last seen, RED summary, editable annotations (owner, tier, links) |
| R-M1 | Metrics | PromQL range and instant queries through `TQL EVAL`, with window/step caps; matchers named `__schema__` or `__database__` (and any escaped spelling) are rejected because they select another database |
| R-M2 | Metrics | Metric and label name lists; SQL queries against metric tables; chart-type and query encoded in a shareable URL |
| R-P1 | Rollup | Planner: for windows over the raw limit read the Flow rollup if it exists, otherwise raw with a warning |
| R-P2 | Rollup | Responses state their `source` (`raw` or `rollup_1m`) and precision limits (no p95 from a rollup) |
| R-P3 | Rollup | Rollup management: create, list, backfill and drop Flow rollups (traces RED, metrics downsampling) from vetted templates; admin only and audited |
| R-X1 | Retention | Extended retention: view and set table TTL per stream, tiering policy (raw short, rollup long), preview of rows affected; admin only and audited |
| R-B1 | Dashboards | Folders, dashboards and tabs: create, rename, move, delete; stored in `grepui_*` tables |
| R-B2 | Dashboards | Panel types: area, stacked area, vertical and horizontal bar (plain and stacked), line, scatter, pie, donut, heatmap, metric text, gauge, table (with pivot mode), geo map, sankey, markdown, sanitised HTML |
| R-B3 | Dashboards | Variables and filters (query-backed, multi-select, `$var` substitution as bound parameters) |
| R-B4 | Dashboards | Dashboard time range, auto refresh with interval, manual refresh, comparison against a past period |
| R-B5 | Dashboards | Import and export as JSON; bulk export and move between folders |
| R-B6 | Dashboards | Public dashboards (expiring read-only share link, no login) and TV mode with playlists |
| R-A1 | Alerts | Scheduled alerts: builder, SQL or PromQL query; condition (count or aggregate, operator, threshold); check interval or cron; look-back window; cooldown; a live preview that runs the rule once without firing or notifying |
| R-A2 | Alerts | Real-time alerts: the same rule evaluated on a short interval (default 10 s) with a per-rule cost budget |
| R-A3 | Alerts | Composite alerts: AND / OR / NOT over other alerts' states |
| R-A4 | Alerts | Destinations (email over SMTP, webhook) and message templates with variables; test-send |
| R-A5 | Alerts | Deduplication by fingerprint fields and window, compare-with-past condition, downtime windows, alert history; alert folders and JSON import/export |
| R-A6 | Alerts | Incidents: create from alerts, acknowledge, resolve, timeline |
| R-A7 | Alerts | Scheduler runs in every replica, but a leader lease in `grepui_*` makes each rule fire once |
| R-A8 | Alerts | Anomaly alerts: rolling-median/MAD baseline over a count or metric series, no external ML |
| R-N1 | Pipelines | Pipelines: list, create, version, delete GreptimeDB pipelines through its API; dry-run against sample input; admin only and audited |
| R-N2 | Pipelines | Saved functions: parametrised SQL snippets usable in SQL mode and panels |
| R-N3 | Pipelines | Enrichment tables: upload CSV into a GreptimeDB table, join helper in the query builders |
| R-N4 | Pipelines | Sensitive data redaction: masking rules per field and role applied to every response and export |
| R-C1 | Access | Local users, organizations (mapped to GreptimeDB databases), roles; passwords hashed (argon2); sessions with expiry |
| R-C2 | Access | RBAC enforced in the query layer: per organization, stream and folder permissions, deny by default; every endpoint checks |
| R-C3 | Access | API tokens and service accounts with scoped roles; tokens stored hashed, shown once |
| R-C4 | Access | SSO via OpenID Connect (authorization code + PKCE, JWT verification, role mapping from claims) |
| R-C5 | Access | Audit trail: every request that reads data or changes state is recorded in an append-only table (who, what, window, rows, outcome); searchable in the UI |
| R-C6 | Access | Multi-tenant UI: org switcher listing only permitted orgs and keeping the URL in the org; per-org branding (name, logo, accent colour); org admins manage their own users, roles, tokens, quotas and sources but nothing outside the org; platform admins create and delete orgs (delete cascades metadata and is audited); isolation matrix test across every route |
| R-W1 | Workload | Running queries: list GreptimeDB's own process list with grepui attribution, cancel by id (role-gated, audited) |
| R-W2 | Workload | Work groups: per-role quotas (max concurrent queries, timeout, max rows, max window); enforced before the query runs |
| R-W3 | Workload | Query statistics: slowest and most frequent queries by tag, per user and org (admin view) |
| R-Y1 | Federation | Several GreptimeDB clusters configured as named sources; list and search endpoints fan out, merge by time, and report per-source errors |
| R-Y2 | Federation | Source selector in the UI, per-source health, and RBAC per source |
| R-E1 | Reports | Scheduled reports: a dashboard or saved view rendered to HTML or CSV and emailed on a schedule (PDF is out of scope) |
| R-U1 | RUM | RUM view over GrepRUM tables: page views, web vitals, JS errors with stack traces, sessions list. Session replay is not supported |
| R-O1 | Operate | Short-lived result cache with single-flight, keyed by statement, binds and role; bounded memory |
| R-O2 | Operate | `/metrics` (Prometheus text) for grepui itself: request counts, DB latency, cache hits, alert evaluations |
| R-O3 | Operate | Graceful shutdown, connection pool limits, and `--read-only-user` check at startup that warns if the DB account can write |
| R-AI1 | LLM | The traces table layout (flat trace v1 or JSON2 trace v2) is detected, and filters, trace detail, histogram and scatter work on both; attribute keys are bound parameters |
| R-AI2 | LLM | The Connect page tells LLM SDKs (OpenInference, OTel GenAI) how to send to a trace v2 table: headers, Python, environment and collector snippets, and the notes a user needs (separate table, privacy, size) |
| R-AI3 | LLM | A normalised LLM view of a span is built from OpenInference and `gen_ai.*` attributes: kind, model, provider, tokens, cost, messages reassembled by index, tool calls, documents, embeddings without vectors; a price table fills missing cost |
| R-AI4 | LLM | The trace page shows LLM spans: kind badges and kind filters, conversation transcript with tool calls, token and cost chips, retrieval documents, long values collapsed, a trace summary of calls, tokens and cost |
| R-AI5 | LLM | LLM analytics over bounded windows: calls, tokens, cost, error rate and latency percentiles by model, provider, span kind and tool, with a time series, all on bound JSON paths |
| R-AI6 | LLM | Sessions and users: sessions grouped by `session.id` or `gen_ai.conversation.id` with a turn-by-turn view, and users grouped by `user.id` |
| R-AI7 | LLM | Annotations and evaluations are read from spans (`annotation.*`, `evaluation.*`, `gen_ai.evaluation.result`) and human feedback is stored per org in the state store and shown with the trace |
| R-AI8 | LLM | Content protection: prompts, completions and tool arguments are masked or hidden by role through the redaction layer, and size caps apply to every LLM view and export |
| R-AI9 | LLM | An agent graph is drawn from `graph.node.*`, `agent.name` and span parentage, with calls, errors and latency per node and edge |
| R-AI10 | LLM | Alerts can use LLM metrics (cost, tokens, error rate, latency percentile) as a condition, per model or in total |
| R-D1 | Delivery | `VERSION` is the single source; `Cargo.toml` matches; the binary reports it |
| R-D2 | Delivery | The banned product name appears nowhere in the tree (`make no-banned-names`) |
| R-D3 | Delivery | CI and release workflows pin every action to a commit SHA; release is tag-driven and the tag must equal `VERSION` |
