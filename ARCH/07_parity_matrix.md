# ARCH 07: Parity Matrix

Feature-by-feature comparison with the reference all-in-one platform, built from its **public documentation read on 2026-10-31**.

**Evidence tags.** `read` = stated on a docs page that was actually fetched. `nav` = only a navigation title or link was seen; the content was not read. `unconfirmed` = not found in the pages read. Pages fetched: docs home, features (overview, logs, metrics, distributed tracing, frontend, enterprise), user-guide index, logs exploration, search around, alerts overview, dashboards overview, pipelines, functions, IAM overview, streams, RUM setup. Everything marked `nav` or `unconfirmed` must be re-checked before its release is built.

**Edition.** `ent` = labelled enterprise-only in the docs (RBAC, SSO, extended retention, federated search, BYOB, cipher keys, query management, workload management, audit trail, anomaly-detection alerts). Everything else is unlabelled (not confirmed as open source). grepui ships all of its features under Apache-2.0.

Status: `planned` (requirement ID and release; a row whose release is `ready` or `shipped` in [RELEASE.md](../RELEASE.md) is built), `differs` (planned, but deliberately different), `gap` (not planned, with the reason), `n/a`.

## Logs

| Feature | Evidence | Edition | grepui | Notes |
|---|---|---|---|---|
| Ingestion over HTTP, Fluent Bit, Vector, OTLP, curl; syslog | read | | n/a, covered by GreptimeDB | Connect page (R-I1, 0.1) gives endpoints and snippets |
| Search with absolute/relative time, field filters | read | | planned R-L1, 0.2 | Filter bar `key=value`, `!=`, `~` |
| SQL mode | read | | planned R-L5, 0.2 | Parser-validated, one read-only statement |
| Full-text search (per-stream full-text keys) | read | | planned R-L6, 0.2 | Term matching on `body`; index state shown (R-S4) |
| Field detection, schema settings, field types | read | | planned R-L3, R-S4, 0.2 and 0.3 | Types from `information_schema` |
| Quick mode and interesting fields | nav | | planned R-L3, 0.2 | |
| Search Around (N events before and after, ignores filters) | read | | planned R-L4, 0.2 | |
| Save views, scheduled recurring searches | read (phrase only) | | planned R-S2 (0.3), R-A1 (0.9), R-E1 (0.13) | |
| Export | read (phrase only) | | planned R-L7, 0.2 | CSV and NDJSON |
| Auto Run (re-run on change) | read | | planned, UI behaviour of R-L1 | Recorded in the 0.2 browser pass |
| Query history | unconfirmed | | planned R-S3, 0.3 | |
| Explain and Analyze query | nav | | planned via R-L5 (`EXPLAIN`) | |
| Refresh cache / run query | nav | | planned R-O1, 0.13 | Cache bypass parameter |
| Cross-linking between signals | nav | | built R-R6, 0.4 | D1 |
| VRL functions on logs | read | | built (differs) R-N2, 0.10 | Saved SQL functions, bound arguments, no recursion, no VRL |
| Insights (root-cause analysis over anomaly periods) | nav | | gap | Revisit after 1.0 |
| Log patterns | nav | | gap | Revisit after 1.0 |
| Live tail | unconfirmed | | gap | Polling model; revisit after 1.0 |

## Metrics

| Feature | Evidence | Edition | grepui | Notes |
|---|---|---|---|---|
| Prometheus remote write, OTLP, JSON, scraping | read | | n/a, GreptimeDB | Connect page |
| PromQL and SQL queries | read | | built R-M1, R-M2, 0.6 | `TQL EVAL` |
| Share URL encoding chart type, query, stream, range, refresh | read | | built R-M2, 0.6 | |
| Downsampling | nav | | planned R-P3, 0.7 | Flow templates |
| Retention settings | read | | built R-X1, 0.7 | |
| Alerting on metrics | read | | planned R-A1, 0.9 | PromQL query mode |

## Traces

| Feature | Evidence | Edition | grepui | Notes |
|---|---|---|---|---|
| OTLP, Jaeger, Zipkin ingestion | read | | n/a, GreptimeDB | Connect page; Jaeger read API possible later |
| Search by service, operation, duration, errors, attributes | read | | built R-R1, 0.4 | |
| Hierarchical trace view, waterfall/Gantt | read | | built R-R3, 0.4 | |
| Span attributes and tags | read | | built R-R5, 0.4 | |
| Service map with performance and error rates | read | | built R-V3, 0.5 | |
| Flame graph | unconfirmed | | built R-R4, 0.4 | Beyond what the docs show |
| RED metrics | unconfirmed | | built R-V1, R-V2, 0.5 | |
| Trace-to-logs correlation | unconfirmed | | built R-R6, 0.4 | D1 |
| Service catalog | unconfirmed | | built R-V4, 0.5 | |

## Dashboards

| Feature | Evidence | Edition | grepui | Notes |
|---|---|---|---|---|
| Folders, dashboards, tabs; move; manage | read | | planned R-B1, R-B5, 0.8 | |
| Panel types: area, stacked area, bars (v/h, stacked), line, scatter, pie, donut, heatmap, metric text, gauge, table, geo map, maps, sankey, markdown, HTML | read | | built R-B2, 0.8 | Table has pivot mode |
| Custom Charts | read | | gap, deliberate | Would run arbitrary user JavaScript |
| Time range, auto run/refresh, refresh key | read | | built R-B4, 0.8 | |
| Import dashboard from JSON | read | | built R-B5, 0.8 | |
| Variables and filters | nav | | built R-B3, 0.8 | Values bound as parameters |
| Public dashboards, TV mode, TV playlist | nav | | built R-B6, 0.8 | |
| Comparison against past period | nav | | built R-B4, 0.8 | |
| Histogram caching | nav | | planned R-O1, 0.13 | |
| Annotation sources, folder icons, drilldowns | nav / unconfirmed | | gap | Re-check docs; revisit after 1.0 |
| Grafana dashboard import | unconfirmed | | gap | |

## Alerts

| Feature | Evidence | Edition | grepui | Notes |
|---|---|---|---|---|
| Scheduled and real-time alerts | read | | built R-A1, R-A2, 0.9 | Real-time = short-interval polling (differs) |
| Composite alerts (AND/OR/NOT) | read | | built R-A3, 0.9 | |
| Anomaly detection | read | ent (self-hosted) | built R-A8, 0.9 | MAD baseline, no ML |
| Builder, SQL, PromQL query modes | read | | built R-A1, 0.9 | |
| Check interval or cron, look-back, cooldown | read | | built R-A1, 0.9 | |
| Email (SMTP) and webhook destinations, templates, variables | read | | built R-A4, 0.9 (plain http and SMTP, no TLS) | |
| Actions (custom Python scripts) | read | | gap, deliberate | Arbitrary code execution; webhooks instead |
| Creates incident, incidents page | read | | built R-A6, 0.9 | |
| Live preview and summary | read | | built R-A1, 0.9 | Preview never notifies |
| Deduplication, compare with past, additional settings | read | | built R-A5, 0.9 | |
| Folders, import/export, downtimes, history | nav | | built R-A5, 0.9 | Folders are a label on the rule |
| Notification dependencies, advanced alert configuration | nav | | gap | Re-check docs |
| AI-assisted SQL in alert builder | read | | gap | |
| SLOs | nav | | gap | Revisit after 1.0 |

## Pipelines, functions, enrichment

| Feature | Evidence | Edition | grepui | Notes |
|---|---|---|---|---|
| Real-time pipelines (transform, filter, route at ingest) | read | | built (differs) R-N1, 0.10 | Manages GreptimeDB pipelines (YAML), with dry-run; per-org names |
| Scheduled pipelines | read (title) | | differs R-P3, 0.7 | Flow templates |
| Remote destinations, workflows, import/export of pipelines | nav | | gap | |
| VRL functions at ingest or query time | read | | built (differs) R-N2, 0.10 | Saved SQL functions, bound arguments, no recursion |
| Enrichment tables | nav | | built R-N3, 0.10 | CSV upload with join helper |
| Sensitive data redaction | unconfirmed (use case only) | | built R-N4, 0.10 | One response layer; roles are a stand-in until 0.11 |

## Streams and retention

| Feature | Evidence | Edition | grepui | Notes |
|---|---|---|---|---|
| Stream list with type, document count, size | read | | planned R-T1, R-S4, 0.1 and 0.3 | |
| Time range, inferred field types | read | | planned R-S4, 0.3 | |
| Full-text and partition key settings | read | | planned (view) R-S4; editing is a gap | Indexes are DDL in GreptimeDB |
| Retention | read | | built R-X1, 0.7 | TTL per table |
| Extended retention | read | ent | differs (built) R-X1, 0.7 | TTL policy and rollup tiering, not object-store tiering |
| Bring your own bucket | read | ent | n/a | GreptimeDB owns object storage |

## RUM

| Feature | Evidence | Edition | grepui | Notes |
|---|---|---|---|---|
| Browser RUM: page load, Core Web Vitals, errors, resources, user interactions | read | | planned R-U1, 0.13 | Reads GrepRUM tables |
| Session replay | read | | gap | GrepRUM does not record replays |
| Mobile RUM (React Native, Android, iOS; beta) | read | | gap | |

## Reports

| Feature | Evidence | Edition | grepui | Notes |
|---|---|---|---|---|
| Scheduled reports | nav | | planned R-E1, 0.13 | HTML and CSV by email |
| PDF output | unconfirmed | | gap | |

## Identity, tenancy and enterprise tier

| Feature | Evidence | Edition | grepui | Notes |
|---|---|---|---|---|
| Organizations | read (IAM list) | | built R-F5, R-S5, R-C6 (0.1, 0.3, 0.11) | Multi-tenant UI; org = database mapping; platform admins create and delete orgs |
| Users, roles, invites | read | | planned R-C1, 0.11 | |
| RBAC | read | ent | built R-C2, 0.11 | Deny by default over every route; per-org roles (no stream or folder grain yet) |
| SSO (OIDC, Keycloak) | read | ent | built R-C4, 0.11 | OIDC code + PKCE; endpoints configured by flags; tested with a local stand-in provider |
| Service accounts, tokens | read (IAM list) | | built R-C3, 0.11 | |
| Ingestion tokens | read (IAM list) | | n/a | Ingest goes to GreptimeDB, not grepui |
| Audit trail | read | ent | built R-C5, 0.11 | |
| Query management | read | ent | built R-W1, 0.12 | Process list and cancel |
| Workload management / work groups | read | ent | built R-W2, R-W3, 0.12 | Quotas per org and role; statistics per process |
| Quotas | read (IAM list) | | built R-W2, 0.12 | |
| Federated search / super cluster | read | ent | built R-Y1, R-Y2, 0.12 | Fan-out over `--source` servers for logs search and the trace list |
| Cipher keys | read | ent | gap | Redaction only (R-N4) |
| AI credits, cloud usage, AutoObserve, Kubernetes explorer, synthetics, incidents RCA | nav | | gap | Cloud or AI features; out of scope |

## Where grepui aims to be better

| Area | Improvement | Release |
|---|---|---|
| Correlation | One state across signals, trace/log/service links on every row | 0.4 |
| Transparency | "Show SQL" and `meta` (source, ms, rows) on every panel | 0.1 |
| Long ranges | Automatic rollup planner that reports its `source` | 0.7 |
| Safety | Typed API with caps, parser-validated SQL mode, write allowlist test, dashboards with bound variables | 0.1 to 0.8 |
| Enterprise tier | RBAC, SSO, audit, work groups, federation in the Apache-2.0 binary | 0.11, 0.12 |
| Operations | One dependency (GreptimeDB), leader lease, no second datastore | 0.9, 0.13 |
