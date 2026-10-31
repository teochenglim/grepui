# ARCH 03: GreptimeDB Mapping

| UI need | GreptimeDB feature |
|---|---|
| Run a query | `POST /v1/sql?db=...` (form body `sql=`) |
| Time histogram | `date_bin(INTERVAL '5 minutes', timestamp)` |
| Nanosecond timestamps | `timestamp` is `TIMESTAMP(9)`; converted in SQL to us/ms because ns does not fit a JS number |
| Trace list (one row per trace) | `GROUP BY trace_id` over `opentelemetry_traces` (the trace-list builder) |
| Span detail | `WHERE trace_id = ...`; the table is partitioned by `trace_id` |
| Service list | `opentelemetry_traces_services` (created by GreptimeDB) |
| Service map and RED | Self-join of parent/child spans; aggregates over server spans |
| PromQL | `/v1/prometheus/api/v1/query_range`, `/label/__name__/values` |
| Log full text search | `matches_term(body, '...')` (fulltext index in the default OTLP log table) |
| Orgs | An org maps to a source, database and table names (R-F5) |
| Access | grepui users and RBAC (v0.11); a DB account scoped to telemetry reads + `grepui_*` writes |

## Long-range trace metrics

RED from raw spans is fine for minutes and hours but slow over days. grepui uses GreptimeDB Flow (verified on v1.3.0, ADR 0008):

1. The Rollups page (or `POST /rollups/traces_red`) creates the sink `<traces_table>_red_1m` (service_name tag, `time_window` TIMESTAMP(3), requests, errors, sum_ns, max_ns) and a flow `grepui_<db>_red_1m` that fills it per minute from server and consumer spans. `scripts/rollup.sql` shows the same DDL for use outside grepui.
2. The planner (R-P1) reads the sink for windows over 24 hours when it exists (Services list, catalog, per-service summary and series), reports `meta.source = rollup_1m`, and falls back to raw spans with a warning when it does not. Operations and dependencies always read raw spans.
3. Raw spans can then use a short TTL while the rollup keeps months (Retention page).

Facts found live: v1.3.0 flows are the batching kind and recompute the windows that received writes from the source table, so a backfill (`INSERT INTO sink SELECT ... GROUP BY`, prepared with bound time limits) and the flow agree and a backfill is idempotent; the flow only reacts to writes after its creation, so history needs the backfill; `ADMIN FLUSH_FLOW('name')` forces a run; `ALTER TABLE t SET 'ttl' = '7d'` is echoed in `create_options` as `ttl=7days` and a 90 day TTL as `ttl=2months 29days 2h 52m 48s`; `'forever'` is accepted. Limits: percentiles cannot be summed across buckets (the rollup shows avg and max, not p95; a `uddsketch_state` column is the likely fix); per-operation breakdown needs raw spans.

## Unverified against a live GreptimeDB

Written from docs, not yet executed (see AGENTS.md rule on testing against the pinned version):

- `approx_percentile_cont(duration_nano, 0.95)` and `date_bin(INTERVAL '...', ...)`.
- `CAST(timestamp AS BIGINT)` returning ns for `TIMESTAMP(9)`.
- `matches_term(body, 'x')` and `json_get_string(log_attributes, 'key')`; dotted keys may need path syntax.
- The parent/child join in `dependencies` on large ranges; a Flow sink may be needed.
- Flow DDL in `scripts/rollup.sql`.
- Not supported yet: the `greptime_trace_v2` schema (JSON2 attribute columns).

Each is one function in the query builders, so a dialect fix is a small edit. `_live` tests (`make test-db`) cover each in the release that needs it.

## Verified live (GreptimeDB v1.2.1, MySQL protocol)

| Item | Result |
|---|---|
| Bound `TIMESTAMP` comparison (`timestamp >= ?` with `'YYYY-MM-DD HH:MM:SS.mmm'`) | works |
| `matches_term(body, ?)` with a bound term | works (fulltext index present on the OTLP logs table) |
| `json_get_string(col, ?)` with a flat key containing dots | works only in path form `["http.method"]`; the plain dotted key returns NULL |
| `LIKE ?` with escapes | only the backslash escape is supported (`ESCAPE '!'` is rejected) |
| `CAST(CAST(timestamp AS BIGINT) AS VARCHAR)` | exact nanosecond key as a string (a number would lose precision in JS) |
| `date_bin(INTERVAL '10 minutes', ts)` | works over HTTP, **fails in prepared statements** (the histogram uses `(CAST(ts AS BIGINT) / bucket_ns) * bucket_ms`) |
| `SHOW CREATE TABLE` | identifiers are backticked over the MySQL protocol (double-quoted over HTTP); the `body` line carries `FULLTEXT INDEX` |
| `UNION ALL` of two parenthesised `ORDER BY ... LIMIT` selects | works (Search Around) |
| `TIMESTAMP(9)` / `JSON` result columns | sqlx needs `DateTime<Utc>` and its `json` feature |
| `DESC TABLE t`, `SHOW TABLES`, `SHOW CREATE TABLE` through SQL mode | work |
| OTLP ingest | protobuf only; `x-greptime-db-name` selects the database |
| Upsert by primary key + time index (`INSERT` the same `(org_id, view_id, ts)` again) | works: one row, latest write wins; `deleted = false` filtering hides soft-deleted rows |
| Reserved words | `name` must be quoted as a column name (`Cannot use keyword 'name'`) |
| `CAST(ts AS BIGINT)` for `TIMESTAMP(3)` | milliseconds (for `TIMESTAMP(9)` it is nanoseconds) |
| Missing table / database error text | `Table not found: ...` / `Database not found: ...` (recognised as "run `grepui migrate`") |
| `information_schema.tables` | `data_length`, `index_length`, `table_rows`, `engine`, `create_options`; sizes are 0 until a table is flushed to storage |
| `WITH (ttl = '30d')` | stored and echoed by `SHOW CREATE TABLE` as `ttl = '30days'` |
| Backtick-quoted column list `PARTITION ON COLUMNS`, `FULLTEXT INDEX`, `SKIPPING INDEX`, `INVERTED INDEX` | parsed from `SHOW CREATE TABLE` (the fulltext line verified live; the others from the documented syntax) |
| `IN (SELECT ...)`, `EXISTS`, self-`JOIN` on a telemetry table | **wrong and nondeterministic results** on v1.2.1 (4 / 6 / 5 / 0 for a true 33); never used (ADR 0007) |
| Conditional aggregation `sum(CASE WHEN pred THEN 1 ELSE 0 END)` inside one `GROUP BY`, filtered in a derived table | correct and deterministic; the basis of the trace filters |
| Same aggregate expression twice in one select list | planner error "duplicate unqualified field name"; compute once in a derived table with aliases |
| OTLP span columns | `timestamp`/`timestamp_end` are `TIMESTAMP(9)`, `parent_span_id` is NULL for roots (not an empty string), status `STATUS_CODE_OK`/`STATUS_CODE_ERROR`, kind `SPAN_KIND_SERVER`/`SPAN_KIND_CLIENT`, attributes are flat `span_attributes.<key>` and `resource_attributes.<key>` columns, `span_events`/`span_links` are JSON |
| `SELECT *, CAST(CAST(timestamp AS BIGINT) AS VARCHAR) AS start_ns` | works: exact nanoseconds as strings alongside every flat attribute column |

## Re-verified on GreptimeDB v1.3.0 (the pinned version since v0.4.0)

| Item | Result on v1.3.0 |
|---|---|
| grepui v0.1 to v0.4 against a fresh v1.3.0 (streams, logs, traces and filters, ingest health, migrate, playground, all 15 live tests) | works unchanged |
| `IN (subquery)`, `EXISTS`, self-`JOIN` | **correct and stable** (32 of 32 on repeats); fixed since v1.2.1, not mentioned in the release notes read (ADR 0007) |
| Same aggregate twice in a select list | still a planner error |
| `greptime_trace_v2` pipeline (`x-greptime-pipeline-name`, separate table, `x-greptime-trace-table-name`) | accepted; **19 fixed columns**: `span_attributes`, `scope_attributes`, `resource_attributes` as JSON2, `span_events`/`span_links` as JSON, `service_name` tag, `timestamp` TIMESTAMP(9). v1.2.1 rejected it ("Unsupported pipeline for trace") |
| JSON2 attribute access | `json_get_string(span_attributes, '["llm.model_name"]')`, `json_get_int`, `json_get_float`; the bare dotted key returns NULL (same path rule as v1 JSON); `span_attributes['k']` and `span_attributes.k` do not work for dotted keys; `CAST(span_attributes AS STRING)` is unsupported |
| JSON2 column through grepui's MySQL reader | decodes to a JSON object (SQL mode and the API) |
| OpenInference-shaped spans (181 spans, 308 KB OTLP request) | stored; includes a span with 300 indexed messages and a 200 KB prompt: no new columns, point read of message 299 in about 30 ms |
| Aggregations over JSON2 | tokens and cost by model, span-kind mix, sessions, p95 latency, project name from `resource_attributes` all work |
| Existing v1 tables | not migrated automatically ("Existing Trace V1 tables are not automatically migrated"); a v2 write to a v1 table is rejected |
| grepui trace detail and attribute filters on a v2 table | **do not work yet**: the span panel returns empty attributes (it reads flat columns) and `q=tool.name=...` is rejected as an unknown column. Roadmap: LLM track, step L1 |
