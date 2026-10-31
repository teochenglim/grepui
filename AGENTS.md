# AGENTS.md

grepui: a multi-tenant observability UI for GreptimeDB (logs, traces, metrics, dashboards, alerts, RBAC and more). One Rust binary (axum) embeds a no-build frontend and queries GreptimeDB over its MySQL protocol; GreptimeDB is the only dependency and holds both telemetry and grepui's own state (`grepui_*` tables). Goal: parity with the reference platform plus useful enterprise features, tracked in `ARCH/07_parity_matrix.md`.

Docs: `ROADMAP.md` = plan · `RELEASE.md` = release log + summary, **newest first** (status table and changelog; keep both current) · `RELEASE/vX.Y.Z.md` = release notes + ACs · `ARCH/` = architecture and why · `docs/adr/` = decisions · `README.md` = end users.

## Commands

- `make check` (= `fmt-check` + `clippy` + `ac-map` + `test` + `version-check` + `no-banned-names`): must pass before claiming anything works. ACs live as `ac<M>_<m>_<n>_*` Rust tests; a new AC needs 2 tests and a row in `RELEASE/vX.Y.Z.md`. Tests ending `_live` need GreptimeDB (`make db-up`, `make test-db`).
- `make build`, `make run`, `make migrate` (create the state store), `make playground` (migrates, seeds a `demo` database and serves), `make demo` (docker compose, playground mode), `make db-up` / `db-down`, `python3 scripts/seed.py` (demo OTLP data for CI and other databases).
- `make release VERSION=x.y.z`: user only (see below).
- The Makefile still has the earlier Go/Node targets until v0.1.0 is built; they are replaced by cargo targets in that release.

## Git & release caveat (important)

- **The user does ALL git commits, pushes, tags, and releases manually. The agent never runs git commit/push/tag, `make bump`, or `make release`.**
- The release command is `make release VERSION=x.y.z` (space + `VERSION=`).
- Workflow: **commit first, then `make release VERSION=x.y.z`**. It amends the bump into that fresh commit, pushes `HEAD`, tags `vX.Y.Z` and pushes the tag. Run it once per commit; on an already-pushed commit it would rewrite history and the push is rejected.
- When an implementation is finished, give a 1-line commit-message summary; the user commits and releases.
- `make release` fails unless the repo has at least one commit AND an `origin` remote.
- `VERSION` is the source of truth; `Cargo.toml` version must match (`make version-check`, enforced in CI); the binary reads it at compile time. Tags are `vX.Y.Z`; CI is tag-driven (`v*`) plus PRs.

## Rules

- Rust stable, edition 2024. `#![forbid(unsafe_code)]`. No `unwrap()`/`expect()` outside tests and `main`.
- **Naming:** never mention the reference product we compare against anywhere (code, docs, comments, tests, commit text); call it "the reference platform". `make no-banned-names` enforces it. Other products (Quickwit, Jaeger, Grafana, GrepRUM) may be named.
- New dependency means a row in `ARCH/05_tech_stack_and_release.md`. Prefer std and what is already there. No Node, no JS build step (ARCH 06 D7).
- **SQL safety:** table names come from validated config identifiers or a fixed `match`; every request-derived value is a bound parameter; time ranges and limits are capped. Never `format!` request text into SQL. Only SQL mode (R-L5) runs user SQL, and only after the parser allowlist.
- **State store (ADR 0006):** every write goes through `AppState::write` → `store::check_write` (INSERT into the three `grepui_*` tables, nothing else; DDL only from `store::migration_statements`). New table = new migration statement + an entry in `WRITE_TARGETS` + `org_id` leading its primary key + an AC. Updates are upserts at a constant `ts`; deletes are soft. Quote `name` and other reserved words. `TIMESTAMP(3)` casts to milliseconds.
- **Data plane read-only, writes allowlisted:** telemetry tables are touched only by SELECT/SHOW/TQL builders. Writes go to `grepui_*` metadata tables or to admin-gated TTL, pipeline and flow operations, and the full list lives in one allowlist asserted by a test (R-F4). Adding a write target needs an AC and an `ARCH/01` entry. The one existing exception is playground mode (ADR 0005): demo data into a `demo`/`demo_*` database via OTLP, only from `grepui playground --demo-data`; `serve` must never reference it.
- **Multi-tenant:** every route is `/api/v1/orgs/{org}/...`; every metadata builder takes the org as a bound argument; every metadata row has `org_id` (R-F5, R-S5). Never resolve a database or table from request text.
- SQL mode (`sqlmode.rs`) is the only place user-written SQL runs: parser allowlist, one statement, confined to the org's database. Extend the deny list or the AST checks there, with tests, never with regexes.
- **Every statement variant needs the database check**, not only `FROM` clauses: `SHOW TABLES FROM x`, `SHOW CREATE TABLE x.t`, `DESC x.t` and TQL's `__schema__`/`__database__` matchers all reach other databases (found by security review, reproduced live). TQL stays out of SQL mode until a typed endpoint rejects those matchers (v0.6.0).
- Test every query against the real MySQL protocol, not only HTTP: the prepared-statement parser differs (e.g. `INTERVAL '10 minutes'` fails, `SHOW CREATE TABLE` backticks identifiers).
- Every statement starts with `/* q:<name> */`; every data response carries `meta` (R-Q4).
- Changed behaviour means updating the `ARCH/` or `RELEASE/` doc it touches, unprompted, plus `RELEASE.md` status and `ARCH/07_parity_matrix.md`. A new implementation choice that changes the decision log means a new file in `docs/adr/`.
- Test every GreptimeDB statement against the pinned version (`GREPTIME_VERSION` in Makefile), not just by reading docs. Statements not yet run live are listed in `RELEASE.md` section 5.

- **GreptimeDB is pinned to v1.3.0** (`GREPTIME_VERSION` in the Makefile, the compose file, CI via `make db-up`); older versions are unsupported. Joins, `IN (subquery)` and `EXISTS` returned wrong, nondeterministic rows on v1.2.x and are correct on v1.3.0 (retested, ADR 0007); the trace filters keep conditional aggregation anyway. Any new join needs a live test against an independently computed answer. Compute each aggregate once: a repeated aggregate expression in one select list is still a planner error.
- **Live tests must not depend on flush state or shared data.** A freshly seeded GreptimeDB has `data_length = 0` (CI failed v0.3.0 on this), and the shared `public` database accumulates duplicate seed runs. Build a throwaway database from the deterministic generator and assert exact numbers (see `tests/ac_traces.rs`). Before tagging, reproduce CI: `make db-down db-up`, `python3 scripts/seed.py`, `make test-db`.

## Supply chain

- `.github/workflows/security.yml` (modelled on teochenglim/cogent): Semgrep SAST, Trivy filesystem and image scans, SARIF to the Security tab, weekly. Run the same scans locally with docker before a release.
- The runtime image is `distroless/base-nossl` plus `libgcc_s` (grepui does no TLS), which keeps OpenSSL CVEs out. If TLS to GreptimeDB is added, change the base and re-scan.

## Gotchas

- Timestamps are `TIMESTAMP(9)` (ns): convert to ms/us in SQL before they reach JSON/JS.
- Quote reserved words in GreptimeDB SQL.
- Span attributes are flat columns named `span_attributes.<key>`; log and resource attributes are JSON (trace pipeline v1; verified on v1.2.1 and v1.3.0; the opt-in `greptime_trace_v2` stores attributes in JSON2 columns, see ARCH/08).
- GreptimeDB's OTLP endpoint accepts protobuf only.
- Flow sink tables get an extra `update_at` column; do not `SELECT *` from them.
- Append-mode tables reject `DELETE`; erasure relies on TTL.
- macOS `sed -i` needs an empty suffix argument; prefer editing files directly.
