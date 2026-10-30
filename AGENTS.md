# AGENTS.md

grepui: a multi-tenant observability UI for GreptimeDB (logs, traces, metrics, dashboards, alerts, RBAC and more). One Rust binary (axum) embeds a no-build frontend and queries GreptimeDB over its MySQL protocol; GreptimeDB is the only dependency and holds both telemetry and grepui's own state (`grepui_*` tables). Goal: parity with the reference platform plus useful enterprise features, tracked in `ARCH/07_parity_matrix.md`.

Docs: `ROADMAP.md` = plan · `RELEASE.md` = release log + summary (keep its status table and changelog current) · `RELEASE/vX.Y.Z.md` = release notes + ACs · `ARCH/` = architecture and why · `docs/adr/` = decisions · `README.md` = end users.

## Commands

- `make check` (= `fmt-check` + `clippy` + `ac-map` + `test` + `version-check` + `no-banned-names`): must pass before claiming anything works. ACs live as `ac<M>_<m>_<n>_*` Rust tests; a new AC needs 2 tests and a row in `RELEASE/vX.Y.Z.md`. Tests ending `_live` need GreptimeDB (`make db-up`, `make test-db`).
- `make build`, `make run`, `make demo` (docker compose), `make db-up` / `db-down`, `python3 scripts/seed.py` (demo OTLP data).
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
- **Data plane read-only, writes allowlisted:** telemetry tables are touched only by SELECT/SHOW/TQL builders. Writes go to `grepui_*` metadata tables or to admin-gated TTL, pipeline and flow operations, and the full list lives in one allowlist asserted by a test (R-F4). Adding a write target needs an AC and an `ARCH/01` entry.
- **Multi-tenant:** every route is `/api/v1/orgs/{org}/...`; every metadata builder takes the org as a bound argument; every metadata row has `org_id` (R-F5, R-S5). Never resolve a database or table from request text.
- Every statement starts with `/* q:<name> */`; every data response carries `meta` (R-Q4).
- Changed behaviour means updating the `ARCH/` or `RELEASE/` doc it touches, unprompted, plus `RELEASE.md` status and `ARCH/07_parity_matrix.md`. A new implementation choice that changes the decision log means a new file in `docs/adr/`.
- Test every GreptimeDB statement against the pinned version (`GREPTIME_VERSION` in Makefile), not just by reading docs. Statements not yet run live are listed in `RELEASE.md` section 5.

## Gotchas

- Timestamps are `TIMESTAMP(9)` (ns): convert to ms/us in SQL before they reach JSON/JS.
- Quote reserved words in GreptimeDB SQL.
- Span attributes are flat columns named `span_attributes.<key>`; log and resource attributes are JSON (verified on v1.2.1).
- GreptimeDB's OTLP endpoint accepts protobuf only.
- Flow sink tables get an extra `update_at` column; do not `SELECT *` from them.
- Append-mode tables reject `DELETE`; erasure relies on TTL.
- macOS `sed -i` needs an empty suffix argument; prefer editing files directly.
