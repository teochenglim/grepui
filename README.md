# grepui

A multi-tenant observability UI for [GreptimeDB](https://greptime.com), in one Rust binary. The UI is plain HTML, CSS and ES modules embedded in the binary: no Node, no build step.

grepui reads what your collectors already send to GreptimeDB. It does not ingest telemetry itself. Every panel can show the SQL it ran ("Show SQL"), and every route is scoped to an org (a tenant mapped to a GreptimeDB database).

**Status: v0.4.0 (traces).** Available now: Traces (search spans, traces over time, latency scatter, one row per trace, waterfall, flame graph, span panel with attributes and events, and the trace-to-logs pivot both ways), saved views (shareable by link) and recent searches, Data health (is data arriving, how fast, which service went quiet), stream details (size, TTL, indexes), and Logs (filter bar, histogram, fields with types and top values, expandable records with Search Around, SQL mode, CSV/NDJSON export), Streams, Connect (how to send OTLP and Prometheus data, with copy-ready snippets), org switcher, time range, Show SQL, and a playground mode with demo data. Traces, services, metrics, dashboards, alerts and the enterprise tier are planned: see [RELEASE.md](RELEASE.md) for the plan and [ARCH/07_parity_matrix.md](ARCH/07_parity_matrix.md) for the feature comparison and gaps.

## Try it

```sh
make db-up                 # GreptimeDB v1.3.0 in docker (HTTP 4000, MySQL 4002)
make playground           # seeds a `demo` database with demo traces + logs, keeps live traffic going, serves http://127.0.0.1:8080
```

Or everything in docker: `make demo` (one grepui container in playground mode, no seed step).

Playground writes demo data **only** to a database named `demo` or `demo_*`, only when it is empty, and only from `grepui playground --demo-data`; `grepui serve` never writes telemetry ([ADR 0005](docs/adr/0005-playground-mode-demo-data.md)). grepui keeps its own state (saved views, history) in a `grepui` database in the same GreptimeDB. Create it once with `grepui migrate` (idempotent; the playground does it for you); `serve` warns and the state endpoints answer 503 until you do. The telemetry databases still only need read access; the account needs write access to the `grepui` database ([ADR 0006](docs/adr/0006-state-store-in-greptimedb.md)).

To use your own data: `make migrate && make run` against a GreptimeDB that your collectors write to (see the Connect page).

## Configuration

Flags, with environment variables for containers:

| Flag | Env | Default |
|---|---|---|
| `--listen` | `GREPUI_LISTEN` | `127.0.0.1:8080` |
| `--greptime-mysql` | `GREPTIME_MYSQL` | `mysql://root@127.0.0.1:4002/public` (the path is the default org's database) |
| `--greptime-http-public` | `GREPTIME_HTTP_PUBLIC` | `http://127.0.0.1:4000` (shown on Connect; use the address your collectors reach) |
| `--traces-table` / `--logs-table` | `GREPUI_TRACES_TABLE` / `GREPUI_LOGS_TABLE` | `opentelemetry_traces` / `opentelemetry_logs` |
| `--query-timeout-secs` | `GREPUI_QUERY_TIMEOUT_SECS` | `30` |

`grepui playground --demo-data [--wait 90] [--traces 300] [--minutes 120] [--seed 7] [--no-live] [--greptime-http http://host:4000]` seeds and serves; `GREPTIME_HTTP` is where it writes (defaults to `--greptime-http-public`).

`grepui migrate [--print] [--wait 30] [--meta-database grepui]` creates or upgrades the state store (`--print` shows the SQL without connecting).

`grepui healthcheck` exits 0 when a running instance answers `/healthz` (used by the container health check).

## API

All data routes are `GET /api/v1/orgs/{org}/...` and return `{ "data": ..., "meta": { query, sql, binds, ms, rows, source } }`. Windows are required (`from` and `to` in epoch milliseconds, at most 31 days) and row limits are capped. Errors are always JSON `{ "error": "..." }`.

| Route | Purpose |
|---|---|
| `/healthz`, `/api/v1/health`, `/api/v1/info`, `/api/v1/orgs` | liveness, readiness, version, orgs |
| `/streams` | tables with kind and row estimate |
| `/streams/{name}`, `/stats`, `/sample?from&to&limit` | schema with roles, exact count and time range, newest rows |
| `/views` (GET, POST), `/views/{id}` (GET, PUT, DELETE) | saved views: `{name, kind: logs\|sql, spec: {q, level, range, mode, sql}}`; JSON bodies only, 64 KB max |
| `/history` (GET, POST) | the last 100 searches (30 days); an identical repeat is not stored |
| `/ingest/freshness`, `/ingest/rate`, `/ingest/services` | newest row per stream and quiet warning, rows per bucket, last span per service |
| `/streams/{name}/info`, `/streams/{name}/ddl` | size and engine; indexes, partitions, TTL and keys from `SHOW CREATE TABLE` |
| `/traces?from&to&q&service&errors&min_ms&limit&offset` | one row per trace; filters must match the same span |
| `/traces/histogram`, `/traces/scatter`, `/traces/services` | traces over time, latency points (capped at 500), service names |
| `/traces/{id}` (`?from&to`, default last 7 days), `/traces/{id}/flame`, `/traces/{id}/logs?span=` | spans as a waterfall with depth and offsets and the span panel data; merged flame graph; logs of the trace or one span |
| `/connect` | endpoints, headers and snippets for sending telemetry |
| `/logs?from&to&q&level&limit&offset` | search; `q` is the filter bar (`key=value`, `key!=value`, `key~substring`, `"quoted text"`) |
| `/logs/histogram`, `/logs/fields`, `/logs/fields/{field}/values` | hits over time, columns with types, top values |
| `/logs/record?ts_ns`, `/logs/around?ts_ns&n` | one record by nanosecond key; neighbours in storage order |
| `/logs/export?...&format=csv\|ndjson` | download (capped at 10 000 rows) |
| `/sql?sql&from&to&limit` | SQL mode: one read-only statement, `$__from`/`$__to` macros |

## Safety

The data plane is read-only by construction: telemetry tables are reached only through fixed SELECT/SHOW builders with bound parameters, identifiers are validated, and the list of things grepui may ever write is a single allowlist that tests pin: its own `grepui_schema`, `grepui_views` and `grepui_history` tables (plus the playground's demo data into a `demo` database). SQL mode is confined to the org's database for every statement variant, and TQL is not available in it. Run it with a GreptimeDB account that can read your telemetry tables. See [ARCH/01_architecture.md](ARCH/01_architecture.md).

## Develop

```sh
make check        # fmt, clippy, ac-map, tests, version: what CI runs
make test-db      # live tests against GreptimeDB (make db-up && make seed first)
```

The UI lives in [web/static/](web/static/) (edit and reload; `make run` embeds it at build time, so rebuild to see changes). Architecture and decisions: [ARCH/](ARCH/README.md), [docs/adr/](docs/adr/README.md). Contributor and agent rules: [AGENTS.md](AGENTS.md).

## Releases

[RELEASE.md](RELEASE.md) is the log and summary; [RELEASE/](RELEASE/README.md) has one file per release with acceptance criteria mapped to tests. Only the repository owner commits, tags and releases (`make release VERSION=x.y.z`).

## Licence

Apache-2.0. See [LICENSE](LICENSE) and [NOTICE](NOTICE).
