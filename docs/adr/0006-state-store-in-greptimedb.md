# 0006 State store: a metadata database in GreptimeDB, one guarded writer

Status: Accepted

## Context

Saved views, history and (later) dashboards, alerts and users need storage. ADR 0004 decided that state lives in GreptimeDB so that it remains the only dependency. v0.3.0 is the first release that writes, so the design needs to be exact: where the tables live, how a row is updated or deleted on a time-series database, and how to guarantee the binary writes nothing else.

## Decision

- **A dedicated database** (`--meta-database`, default `grepui`) holds `grepui_schema`, `grepui_views`, `grepui_history` and (from v0.5.0, schema version 2) `grepui_services`. Telemetry databases stay free of grepui tables, and the metadata database can be backed up, granted or dropped on its own. `grepui migrate` creates it (the playground does this itself); `serve` only checks it.
- **Every tenant row carries `org_id`**, leading the primary key (R-S5). The version table is the one exemption: it holds no tenant data.
- **Updates are upserts.** A row is identified by its primary key and time index. Views use a constant `ts` (creation time), so re-inserting the same key overwrites it: last write wins (verified live on v1.2.1). **Deletes are soft deletes** (`deleted = true`), which behaves the same on every table mode and keeps the write path to a single statement shape, `INSERT`.
- **History** is append-only with a 30-day TTL on the table; reads also filter to the retention window, and an identical repeat of the newest entry is not stored.
- **One write gate.** `AppState::write` calls `store::check_write`, which admits only `INSERT INTO <meta db>.<allowlisted table> (` built by the store, with a matching `/* q:name */` tag and no `;`. DDL runs only if it is byte-for-byte one of the fixed migration statements. The allowlist (`query::WRITE_TARGETS`) is pinned by tests. A blocked write is logged and returned as an internal error, never executed.
- **Versioning.** `grepui_schema` holds the schema version. A binary refuses to start when the stored version is newer than its own; an older or missing schema starts with a warning that points at `grepui migrate`, and state endpoints answer 503 with the same instruction.
- **Write requests** are JSON only (a cross-site form post is refused with 415), capped at 64 KB, and specs may only contain the allowlisted search keys.
- Until access control (v0.11.0), the owner and history user are the implicit user `local`.

## Consequences

Replicas stay interchangeable: all state is in GreptimeDB. Sharing a view is a link carrying its id. Because deletes are soft, a deleted view still occupies a row until a retention task exists (planned with the housekeeping jobs). Reading the metadata database requires an account that can read and write it; the telemetry databases still only need read access, which the README recommends.
