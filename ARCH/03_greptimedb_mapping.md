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

RED from raw spans is fine for minutes and hours but slow over days. grepui uses GreptimeDB Flow:

1. [`docs/rollup.sql`](../docs/rollup.sql) writes per-minute `requests, errors, sum_ns, max_ns` per service to a small sink table.
2. In the rollup planner (R-P1) finds the sink. For ranges over the raw limit the Services page reads the rollup (the rollup builders).
3. Raw spans can then use a short TTL while the rollup keeps months.

Limits: percentiles cannot be summed across buckets (rollup shows avg and max, not p95; a `uddsketch_state` column is the likely fix); a Flow only sees data written after creation (backfill with one `INSERT ... SELECT`); per-operation breakdown needs raw spans.

## Unverified against a live GreptimeDB

Written from docs, not yet executed (see AGENTS.md rule on testing against the pinned version):

- `approx_percentile_cont(duration_nano, 0.95)` and `date_bin(INTERVAL '...', ...)`.
- `CAST(timestamp AS BIGINT)` returning ns for `TIMESTAMP(9)`.
- `matches_term(body, 'x')` and `json_get_string(log_attributes, 'key')`; dotted keys may need path syntax.
- The parent/child join in `dependencies` on large ranges; a Flow sink may be needed.
- Flow DDL in `docs/rollup.sql`.
- Not supported yet: the `greptime_trace_v2` schema (JSON2 attribute columns).

Each is one function in the query builders, so a dialect fix is a small edit. `_live` tests (`make test-db`) cover each in the release that needs it.
