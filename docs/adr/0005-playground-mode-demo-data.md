# 0005 Playground mode: demo data from one command, fenced against production

Status: Accepted

## Context

A demo needs data. Until now that meant running `scripts/seed.py` against a running GreptimeDB, a second step and a Python dependency. greprum solved the same problem with `greprum playground` (its ADR 0053): one process that initialises, seeds guarded demo data and serves. grepui's design says the data plane is read-only (ADR 0004), so a binary that writes telemetry needs an explicit, narrow exception.

## Decision

`grepui playground --demo-data` waits for GreptimeDB, seeds deterministic demo traces and logs through GreptimeDB's own OTLP endpoints, optionally keeps a trickle of live demo traffic going, then serves exactly like `serve`. The write is fenced four ways:

1. It exists only in the `playground` subcommand. `serve`, the API and the query layer never reference it; a test scans the source to prove it (`ac_0_2_9_serve_has_no_ingest_path`).
2. It needs the explicit `--demo-data` flag. Without it, `playground` only waits and serves.
3. It refuses any database not named `demo` or `demo_*`, even with `--force`, so a production configuration pointed at the playground cannot receive synthetic rows.
4. It seeds only when the demo database has no traces yet (append-mode tables reject `DELETE`, so a re-seed would double every count). `--force` overrides this one guard, never the database-name guard.

It writes through GreptimeDB's ingest API (OTLP over HTTP, protobuf) and one `CREATE DATABASE IF NOT EXISTS`, not through grepui's SQL builders, so the write allowlist (R-F4) for the SQL layer is unchanged. The demo data is a small shop (frontend, cart, checkout, payment, inventory, postgres) with realistic latencies, a payment failure rate and error propagation, so the views have something to show. The generator is seeded and a test pins determinism.

The HTTP client is about 40 lines over a TCP stream (plain `http://` only), which avoids a TLS-capable HTTP dependency for a feature that talks to a local or compose-internal database.

## Consequences

`docker compose up` is a populated demo: one container, no seed step. The first start takes about a second to seed before the listener binds. `scripts/seed.py` remains for CI and for seeding other databases (`--db`). The Connect page and the Show SQL drawer work the same in playground and in production. Any new write path in the binary needs its own ADR and an entry in the write allowlist.
