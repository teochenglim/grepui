# grepui

A multi-tenant observability UI for [GreptimeDB](https://greptime.com), in one Rust binary. The UI is plain HTML, CSS and ES modules embedded in the binary: no Node, no build step.

grepui reads what your collectors already send to GreptimeDB. It does not ingest telemetry itself. Every panel can show the SQL it ran ("Show SQL"), and every route is scoped to an org (a tenant mapped to a GreptimeDB database).

**Status: v0.1.0 (foundation).** Available now: Streams (every table, its schema, exact stats and newest rows), Connect (how to send OTLP and Prometheus data, with copy-ready snippets), org switcher, time range, Show SQL. Logs, traces, services, metrics, dashboards, alerts and the enterprise tier are planned: see [RELEASE.md](RELEASE.md) for the plan and [ARCH/07_parity_matrix.md](ARCH/07_parity_matrix.md) for the feature comparison and gaps.

## Try it

```sh
make db-up                 # GreptimeDB v1.2.1 in docker (HTTP 4000, MySQL 4002)
make seed                  # demo traces + logs over OTLP (ORG=tenant_a seeds another database)
make run                   # http://127.0.0.1:8080
```

Or everything in docker: `make demo`.

## Configuration

Flags, with environment variables for containers:

| Flag | Env | Default |
|---|---|---|
| `--listen` | `GREPUI_LISTEN` | `127.0.0.1:8080` |
| `--greptime-mysql` | `GREPTIME_MYSQL` | `mysql://root@127.0.0.1:4002/public` (the path is the default org's database) |
| `--greptime-http-public` | `GREPTIME_HTTP_PUBLIC` | `http://127.0.0.1:4000` (shown on Connect; use the address your collectors reach) |
| `--traces-table` / `--logs-table` | `GREPUI_TRACES_TABLE` / `GREPUI_LOGS_TABLE` | `opentelemetry_traces` / `opentelemetry_logs` |
| `--query-timeout-secs` | `GREPUI_QUERY_TIMEOUT_SECS` | `30` |

`grepui healthcheck` exits 0 when a running instance answers `/healthz` (used by the container health check).

## API

All data routes are `GET /api/v1/orgs/{org}/...` and return `{ "data": ..., "meta": { query, sql, binds, ms, rows, source } }`. Windows are required (`from` and `to` in epoch milliseconds, at most 31 days) and row limits are capped. Errors are always JSON `{ "error": "..." }`.

| Route | Purpose |
|---|---|
| `/healthz`, `/api/v1/health`, `/api/v1/info`, `/api/v1/orgs` | liveness, readiness, version, orgs |
| `/streams` | tables with kind and row estimate |
| `/streams/{name}`, `/stats`, `/sample?from&to&limit` | schema with roles, exact count and time range, newest rows |
| `/connect` | endpoints, headers and snippets for sending telemetry |

## Safety

The data plane is read-only by construction: telemetry tables are reached only through fixed SELECT/SHOW builders with bound parameters, identifiers are validated, and the list of things grepui may ever write is a single allowlist that tests pin (empty in v0.1.0). Run it with a GreptimeDB account that can read your telemetry tables. See [ARCH/01_architecture.md](ARCH/01_architecture.md).

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
