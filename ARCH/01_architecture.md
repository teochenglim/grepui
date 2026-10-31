# ARCH 01: Architecture

```
browser (static HTML + ES modules) --/api/v1/orgs/{org}/...--> grepui (Rust, axum, N replicas) --MySQL protocol--> GreptimeDB
                                                                      |                                           (telemetry + grepui_* state)
                                                                      +-- optional extra sources (federation)
```

Diagram source: [architecture.mmd](architecture.mmd).

## Components

- **API layer (`api/`).** Typed REST endpoints, org-scoped. Parses and validates parameters, checks permission (from v0.11), builds a `Q` (statement tag + SQL with `?` + binds), runs it through the `Reader`, and wraps rows with `meta` (statement tag, ms, rows, source).
- **Query layer (`query/`).** Pure builders per feature. Table names are validated config identifiers; values are binds. `SqlxReader` applies timeouts and row caps. A planner chooses raw vs rollup (v0.7).
- **State layer (`store/`).** `grepui_*` tables in a dedicated GreptimeDB database (ADR 0006): schema version, saved views, history now; dashboards, alerts, users and audit later. Every tenant table has `org_id`. `grepui migrate` owns the schema; `AppState::write` is the only write path and `store::check_write` its gate.
- **Schedulers (`jobs/`).** Alerts, reports and housekeeping run in every replica; a leader lease row makes each rule fire once.
- **UI (`ui/`).** Static HTML, CSS and ES modules embedded with `include_bytes!`; org switcher, time range, "Show SQL".
- **Playground (`playground/`).** Demo-only: deterministic synthetic traces and logs, an OTLP protobuf encoder and a tiny HTTP client. Reachable only from `grepui playground --demo-data`, fenced to `demo`/`demo_*` databases (ADR 0005).
- **GreptimeDB.** Ingest, storage and query. Telemetry is written to it directly by collectors and SDKs, never through grepui.

## Tenancy

An org resolves (config, later the metadata store) to `{source, database, table names, quotas}`. The resolved database is never derived from request text. Single-tenant deployments use the built-in `default` org.

## Trust boundary

1. **Authentication and RBAC** (v0.11): deny by default; an exhaustive route table test proves every route checks permission.
2. **Typed API:** clients cannot send SQL, except in SQL mode, where a parser accepts one read-only statement confined to the org's database (`query/sqlmode.rs`).
3. **Write allowlist:** the only writes are `grepui_*` tables and admin-gated TTL, pipeline and flow operations (ARCH 06 D2), plus the playground's demo ingest (ADR 0005).
4. **Database account:** run grepui with a GreptimeDB account that can read telemetry tables and write only `grepui_*`; startup warns if the account can do more (R-O3).

## Request flow

1. UI calls `/api/v1/orgs/{org}/logs?from=&to=&q=...`.
2. Middleware resolves the org and the caller; checks permission and work-group quota; writes the audit row.
3. The builder creates a `Q`; the cache (v0.13) may answer; otherwise the `Reader` runs it with timeout and caps.
4. The response carries rows and `meta`; the UI renders and offers "Show SQL".
