# 0008 Admin writes: a flag now, a template gate always

Status: Accepted (v0.7.0). The flag is replaced by the admin role in v0.11.0; the gate stays.

## Context

v0.7.0 is the first release that changes telemetry databases: it creates and drops a Flow rollup (sink table and flow), backfills it, and sets table TTLs. The data plane is otherwise read-only (ARCH 06 D2), and there are no users or roles until v0.11.0.

## Decision

- Every such write needs `--admin-writes` (`GREPUI_ADMIN_WRITES`), off by default. Reads (rollup status, retention, TTL preview) are open. The flag stands in for "admin" until RBAC exists and is not a security boundary against a user who can reach the API: run grepui with it only where everyone who can reach the UI may change retention.
- Statements are generated from fixed templates in `src/admin.rs` with validated identifiers (a sink named `<traces_table>_red_1m`, a flow named `grepui_<db>_red_1m`) and a TTL that is `forever` or n hours, days or weeks, re-rendered from the parsed value. Before the writer runs, `admin::check` regenerates the allowed statements for the org and accepts only an exact match (or an `ALTER TABLE ... SET 'ttl'` on one of the three streams with a re-parsed duration). A test feeds it `DROP TABLE` on telemetry and state tables, stacked statements and odd TTL spellings.
- `ADMIN_WRITES` lists the kinds of statement; a test pins its size. Each write logs an `audit` line (org, actor `local`, action, SQL); the audit table arrives with v0.11.0.
- The rollup sink is created by us, not implicitly by the flow, so its primary key and time index are known; the flow is a batching flow that recomputes dirty windows from the source, so a backfill and the flow agree (verified live: counts match raw spans exactly, re-running a backfill changes nothing).
- The planner (`query::services::plan`) reads the rollup only for windows over 24 hours and only if the sink exists (cached 60 s); otherwise raw spans with a warning in `meta.note`. Rollup answers carry `meta.source = rollup_1m` and no p95.

## Consequences

Per-org flow names include the database, because flows are catalog-wide. Dropping a rollup deletes its data, never the raw spans. TTL changes are applied lazily by GreptimeDB's compaction, so rows older than the cutoff can remain visible for a while; the page says so. Metrics downsampling is a later template through the same gate.
