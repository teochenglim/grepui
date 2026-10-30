# ARCH 06: Positioning

**Goal (decided by the owner):** be on par with the reference all-in-one observability platform: everything it has, grepui has; where it could be better, grepui does it better; including a useful part of its enterprise tier. "Zoom in" interactions are explicitly out of scope. Parity is tracked feature by feature, with gaps stated, in [07_parity_matrix.md](07_parity_matrix.md).

The difference is the foundation. The reference platform is its own storage engine plus UI. grepui is a multi-tenant control and investigation layer over **GreptimeDB**, which already ingests, stores and queries. So grepui does not rebuild ingest or storage, and it keeps exactly one dependency: GreptimeDB holds both the telemetry and grepui's own state (`grepui_*` tables).

## Design choices

| # | Choice | Why it matters |
|---|---|---|
| D1 | **Correlation first.** One window and one filter state across the app; trace IDs, span IDs and service names are links between logs, traces, services and dashboards. | Incidents cross signals; per-signal modules make users retype context. |
| D2 | **Data plane read-only by construction.** Telemetry tables are reached only through SELECT/SHOW/TQL builders. The complete list of writes is one allowlist: `grepui_*` metadata tables, and admin-gated retention (TTL), pipeline and flow operations, each audited. A test asserts the list. | Narrower than "never writes", but exact and testable. Supersedes the earlier "read-only, stateless" wording. |
| D3 | **State lives in GreptimeDB, nothing else.** No second database, no local disk state. Replicas are interchangeable; a leader lease in `grepui_*` makes schedulers fire once. | Horizontal scale is "run more copies"; backup is GreptimeDB's backup. |
| D4 | **Typed REST API, not SQL passthrough.** The client sends parameters, never SQL text; the server builds SQL with bound values, caps windows and rows, sets timeouts. SQL mode is the single, parser-validated exception (R-L5). | Safer, cacheable, scriptable; one user's query cannot starve the cluster. |
| D5 | **Rollup planner.** The server chooses raw spans or a Flow rollup from the window and reports which (`meta.source`). | Long-range metrics without a product-side aggregation engine, and honest about precision. |
| D6 | **Show SQL on every panel.** Each response carries the executed statement tag, DB time and rows; the UI can show it and open it in SQL mode. | Trust, teachability, easy dialect debugging. |
| D7 | **No frontend build step.** Plain HTML, CSS, ES modules embedded in the binary. | One toolchain (Rust); no npm supply chain. |
| D8 | **Apache-2.0, no edition split.** RBAC, SSO, audit trail, query management, work groups, federation and extended retention ship in the same binary. | The reference platform gates these to its Enterprise tier (free up to a daily ingestion limit per its docs). |
| D9 | **Multi-tenant from the first route.** Every route is `/api/v1/orgs/{org}/...`; an org resolves to a source, database and table names; every metadata row carries `org_id`. Single-tenant use is just the `default` org. | Retrofitting tenancy later would break the API and the schema. |

## Where grepui deliberately differs from the reference platform

- **No ingest path in grepui.** Telemetry goes straight to GreptimeDB (OTLP, Prometheus remote write, pipelines). grepui's Connect page tells users exactly how. Consequence: "real-time alerts" are short-interval polling (default 10 s), not an ingest hook, and ingestion tokens are GreptimeDB's concern.
- **SQL functions instead of VRL.** GreptimeDB pipelines are configuration-driven; query-time functions are saved SQL snippets (R-N2).
- **No Custom Charts / user JavaScript panels.** They would run arbitrary code in the viewer's session. The sanitised HTML and markdown panels cover the safe use.
- **Anomaly alerts without ML:** rolling median/MAD only.
- **Reports are HTML/CSV by email.** No PDF rendering.
- **Cipher keys (field encryption at rest) are not implemented;** redaction masks fields on display and export (R-N4).
- **BYOB (bring your own bucket) does not apply:** GreptimeDB owns object storage configuration.

## Non-goals

- Becoming a storage engine or an ingest gateway.
- Session replay and mobile RUM SDKs (GrepRUM, the sibling project, owns RUM data; grepui reads it, R-U1).
- A scripting runtime for alert actions (the reference platform offers custom Python actions; webhooks cover the integration need).

## What we keep from the reference products

Search-first layout, type-aware field sidebar and index summary (Quickwit); the module set, panel catalogue and alert model of the reference platform; trace list, waterfall and span panel (Jaeger-style tools). See [02_reference_uis.md](02_reference_uis.md).
