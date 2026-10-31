# AGENTS.md

grepui: a multi-tenant observability UI for GreptimeDB (logs, traces, services, metrics, dashboards, rollups, alerts; pipelines, RBAC, SSO next). One Rust binary (axum) embeds a no-build frontend (`web/static`, ES modules) and queries GreptimeDB over its MySQL protocol. GreptimeDB is the only dependency: it holds telemetry and grepui's own state (`grepui_*` tables). Goal: parity with the reference platform plus its enterprise tier, tracked in `ARCH/07_parity_matrix.md`.

## Where things stand (update this block each release)

- **Built, `ready`:** v0.1.0 to v0.10.0 (foundation, logs, state and views, traces, services and service map, metrics, rollups and retention, dashboards, alerts and incidents, pipelines/functions/enrichment/redaction). The user commits and releases; check `git log` for what is committed.
- **Next:** v0.11.0 access control (`RELEASE/v0.11.0.md`: RBAC, SSO, audit; replaces the `--admin-writes` and viewer/admin stand-ins), or the LLM/OpenInference track L1 (`ROADMAP.md`, `ARCH/08`). Ask which if not said. Then v0.12, v0.13, v1.0.
- **Docs:** `RELEASE.md` roll-up (status table, changelog newest first, section 5 = verified/unverified ledger) · `RELEASE/vX.Y.Z.md` per release (scope, ACs with named tests, notes, checklist) · `ROADMAP.md` · `ARCH/` (00 requirements, 01 architecture, 03 GreptimeDB facts, 07 parity, 08 LLM) · `docs/adr/0001-0011` · `README.md`.
- **Code map:** `src/api/*` handlers + `ROUTES`/`WRITE_ROUTES`/router in `api/mod.rs` · `src/query/*` pure SQL builders (`Q` + binds) · `src/store/*` state tables and gate · `src/dash/*` dashboard spec, panels, variables, time · `src/admin.rs` admin DDL gate · `src/enrich.rs` CSV enrichment (spec, inference, gate, join helper) · `src/functions.rs` saved functions · `src/pipelines.rs` + `src/http.rs` pipeline API client · `src/redact.rs` + `src/api/redaction.rs` masks and the response layer · `src/csv.rs` · `src/alerts/*` rule spec, engine (tick, lease, gates), conditions/MAD, cron, templates, webhook/SMTP delivery · `src/api/alerts.rs` · `src/layout.rs` waterfall/flame/service graph · `src/playground/*` demo data (synth, OTLP encoder, demo dashboard) · `web/static/js/{pages,components,charts,util}` · `tests/ac_*.rs` (shared fakes in `tests/common/mod.rs`: `Fake`, `MemStore`).

## Commands

- `make check` (= fmt-check, clippy `-D warnings`, `ac-map`, tests, version-check, no-banned-names): must be green before claiming anything works. `make test-db` runs `_live` tests (need `make db-up`). `make playground` (migrates, seeds `demo`, serves :8080, `--admin-writes` on), `make demo` (compose), `make migrate`, `python3 scripts/seed.py [--db X]`.
- ACs are tests named `ac_<M>_<m>_<n>_*` mapped to rows in `RELEASE/vX.Y.Z.md`; `scripts/ac-map.sh` needs 2+ tests per AC and every requirement covered. `_live` tests are `#[ignore]` and build a **throwaway database** from `playground::synth` and assert exact numbers; never rely on shared `public` data or flush state.
- **Before declaring a release ready:** `make check`; `cargo deny check` and `cargo audit`; CI parity `make db-down db-up`, `python3 scripts/seed.py`, `make test-db` (must be 0 failed; it has caught real bugs); browser pass with Playwright (see below) and screenshots in `images/`; docs updated.

## Git & release (important)

- **The user does ALL git commits, pushes, tags and releases. The agent never runs git commit/push/tag, `make bump` or `make release`.** Finish a release by giving a one-line commit message; the user commits, then runs `make release VERSION=x.y.z`. `VERSION` and `Cargo.toml` must match (the user's release bumps it).
- The agent cannot run `/compact`; tell the user to.

## Rules

- Rust 2024, `#![forbid(unsafe_code)]`, no `unwrap()`/`expect()` outside tests and `main`. New dependency = row in `ARCH/05`. No Node, no JS build step. Frontend never uses `innerHTML` (DOM helper `h()` in `util/dom.js`).
- **Naming:** never write the reference product's name anywhere (code, docs, comments, tests, URLs); say "the reference platform". `no-banned-names` test enforces it (it caught the agent once via a pasted URL).
- **SQL safety:** table names from validated identifiers or fixed matches; every request value is a bound parameter; windows and limits capped (31 days, 1000 rows); every statement starts `/* q:<name> */` and every data response carries `meta` ("Show SQL"). User-written SQL runs only in SQL mode and dashboard SQL panels via `query::sqlmode::check` (parser allowlist, one statement, confined to the org DB, every statement variant checked, TQL refused). Extend it with tests, never regexes.
- **PromQL (v0.6.0)** is the one place request text is embedded in SQL (`TQL EVAL` has no binds): only through `query::metrics::check` (no `;`/`#`, no backslash outside strings, strings decoded, `__schema__`/`__database__` refused case-insensitively in every spelling) and only when the org's database equals the connection's. Any change needs a test per spelling.
- **State store (ADR 0006):** writes only via `AppState::write` → `store::check_write` (INSERT into `WRITE_TARGETS`: schema, views, history, services, folders, dashboards, shares, alert_items, alert_history); DDL only from `store::migration_statements`; `SCHEMA_VERSION` is 4. New table = migration + `WRITE_TARGETS` + `TENANT_TABLES` + `org_id` leading the PK + AC + MemStore support in `tests/common`. Upserts at constant `ts`, soft deletes, `TIMESTAMP(3)` casts to ms, quote `name`.
- **Admin writes (ADR 0008):** rollup create/drop/backfill and TTL only in `src/admin.rs`, behind `--admin-writes`, templates re-checked by `admin::check`, logged as `audit`; new kinds need `ADMIN_WRITES`, a gate case and a refusal test.
- **Dashboards (ADR 0009):** panels run only their stored query on the server; variables bind as `?` (`dash::vars`); text panels sanitised in `dash::panels` and shown in a sandboxed iframe; every spec goes through `dash::canonical`. Never add a route that takes SQL for a panel.
- **Alerts (ADR 0010):** rules run only their saved query (SQL via `sqlmode::check`, PromQL via `metrics::check`, builder from validated columns); a preview is a GET and writes nothing; delivery is plain `http://` webhooks and plain SMTP (no TLS client), destination URLs are validated in `alerts::webhook_target`; replicas coordinate by a write-then-read lease per rule, not compare-and-set. Test with `alert_app`/`Rec` in `tests/common`; logs keep the service in `resource_attributes` (`json_get_string(.., '["service.name"]')`), not a `service_name` column.
- **v0.10 rules (ADR 0011):** pipeline names are `<org>.<name>` and every call to GreptimeDB's pipeline API passes `pipelines::check_path`; function arguments are literals sent as binds, bodies cannot call functions; enrichment tables are `enrich_*` created only by generated statements that `admin::check` re-derives; redaction is applied by `api::redaction::layer` to every `/api/v1/orgs/` data response, roles are a stand-in (`admin` = `--admin-writes`, public links always `viewer`), rules live in memory (refreshed every 5 s; 503 until first load). New data routes need no redaction code, but their JSON must be `{data, meta}` or plain data. `grepui_alert_items` also holds `function` and `redaction` kinds.
- **Multi-tenant:** every route is `/api/v1/orgs/{org}/...`; never resolve a database or table from request text. Only the `default` org exists until v0.11.0 (so TQL, which cannot be qualified, is guarded).
- Test statements against the real MySQL protocol and the pinned GreptimeDB (**v1.3.0**, `GREPTIME_VERSION` in Makefile). Joins/`IN (subquery)` are correct on 1.3.0 (wrong on 1.2.x, ADR 0007); a new join needs a live test against an independently computed answer. A repeated aggregate expression in one select list is a planner error: compute each once.
- Changed behaviour = update the `ARCH/`/`RELEASE/` doc, `RELEASE.md`, `ARCH/07`, and add an ADR for decisions, in the same change. Read-only routes vs `WRITE_ROUTES` are pinned by `tests/ac_foundation.rs` (new write-only routes need its `fill()` placeholders and `write_only` list).

## Working method that worked

1. Read `RELEASE/vX.Y.Z.md`, the related `ARCH` section and the neighbouring code; probe GreptimeDB live first (HTTP `:4000/v1/sql?db=`; the local `mysql` client cannot authenticate, so use curl or a Rust test) before designing SQL.
2. Backend builders + handlers, then tests (offline with `Fake`/`MemStore`, plus `_live`), then frontend, then browser pass, then docs, then the full gate.
3. The embedded UI is compiled in: after editing `web/static`, restart the server **on a new port** (browser caches ES modules). Playwright: `playground --listen 127.0.0.1:81xx --demo-data --admin-writes --wait 30 --greptime-mysql mysql://root@127.0.0.1:4002/demo`; stop it with `pkill -f "grepui playground"` when done.
4. Scratch files go in the session scratchpad, not the repo. Deleting directories (`rm -rf`) is blocked in this environment; leftover scratch dirs are the user's to delete.
5. Report honestly: say what was not exercised in a browser and any limits; record them in the release notes and `RELEASE.md` section 5.

## Pitfalls already paid for

- `cargo fmt` reflows code, so scripted text replacements on previously formatted files often miss; check the replacement landed (or use direct edits). Heredocs inside heredocs break: write a script file instead.
- macOS `sed -i` needs `''`. GreptimeDB `TIMESTAMP(9)` is ns (convert in SQL), `TIMESTAMP(3)` ms; integer division on BIGINT truncates (cast to DOUBLE); `name` is reserved; `INTERVAL` literals fail in prepared statements (use integer arithmetic); the OTLP endpoint takes protobuf only; v1.3.0 flows are the batching kind (sink recomputed from source; flow only sees writes after creation; `ADMIN FLUSH_FLOW('name')` forces a run); OTLP counters create `<name>_total` tables on the metric engine; Flow sinks get `update_at`; append-mode tables reject DELETE; TTL echoes as `ttl=7days`, `2months 29days ...`.
- Foundation test: a 404 with a JSON `error` body counts as routed; a bare 404 means unrouted.
- Clippy `type_complexity` and `useless format!` fail the gate; run clippy before live runs.
- Docs generated early on (`RELEASE.md`, `RELEASE/*.md`) are now hand-maintained; edit them directly, newest first.

## Supply chain

`.github/workflows/security.yml` (Semgrep, Trivy fs + image, SARIF, weekly, pinned SHAs). Runtime image is `distroless/base-nossl` plus `libgcc_s` (no TLS in grepui); if TLS to GreptimeDB is added, change the base and re-scan. Dependencies are checked with `cargo deny` and `cargo audit`.
