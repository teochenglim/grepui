# grepui release log and summary

The single place to see where grepui stands: what is shipped, what is planned, what each release proves, and how a release is cut. Per-release detail (scope, acceptance criteria, named tests, checklist) lives in [`RELEASE/vX.Y.Z.md`](RELEASE/README.md); this file is the readable roll-up and the changelog.

> **Current state:** v0.1.0 to v0.3.0 are **ready** (code, tests and docs done; v0.1.0 and v0.2.0 are committed, v0.3.0 is not; tags and releases are the owner's step). No tag has been pushed, so nothing is published. v0.4.0 onward are planned and unstarted. Statuses below are updated when a release changes state, in the same change as the code.

## 1. Summary

grepui is a multi-tenant observability UI for GreptimeDB: one Rust binary (axum) with an embedded no-build frontend (plain HTML, CSS, ES modules). The goal is **parity with the reference all-in-one platform** (logs, metrics, traces, dashboards, alerts, pipelines, functions, streams, RUM view, reports, IAM) **plus its enterprise tier** (RBAC, SSO, audit trail, query management and work groups, federated search, extended retention, anomaly alerts), under Apache-2.0 with no edition split. Where it can be better: correlation across signals, typed API with caps, automatic rollup planner, "Show SQL" on every panel, and a single dependency (GreptimeDB holds data and state). Design choices D1–D9 are in [`ARCH/06_positioning.md`](ARCH/06_positioning.md); the feature-by-feature comparison, with gaps stated, is [`ARCH/07_parity_matrix.md`](ARCH/07_parity_matrix.md).

| Fact | Value |
|---|---|
| Language / runtime | Rust (edition 2024), axum 0.8, sqlx (MySQL protocol to GreptimeDB), no Node anywhere |
| Pinned GreptimeDB | `v1.2.1` (`GREPTIME_VERSION` in the Makefile) |
| Tenancy | Multi-tenant from v0.1.0: org-scoped routes; orgs map to GreptimeDB databases |
| Releases planned | 14 (v0.1.0 to v0.13.0, then v1.0.0) |
| Requirements | 78 (`ARCH/00_requirements_traceability.md`) |
| Acceptance criteria | 82, each governed by at least two named tests (`make ac-map`) |
| Licence | Apache-2.0 |
| Artifacts per tag | tarballs for linux and macOS on amd64 and arm64 (with `.sha256`), multi-arch image on GHCR, GitHub Release with the notes from `RELEASE/<tag>.md` |

## 2. Release status

Latest built: **v0.3.0: State, saved views, stream health and the SQL-mode fix** (ready). Newest first.

| Release | Theme | Status | ACs | Requirements covered |
|---|---|---|---|---|
| [v1.0.0](RELEASE/v1.0.0.md) | Launch hardening | planned | 2 | R-C2 R-F4 R-Q1 |
| [v0.13.0](RELEASE/v0.13.0.md) | Reports, RUM view and operate | planned | 5 | R-E1 R-O1 R-O2 R-O3 R-U1 |
| [v0.12.0](RELEASE/v0.12.0.md) | Query management, work groups and federation | planned | 5 | R-W1 R-W2 R-W3 R-Y1 R-Y2 |
| [v0.11.0](RELEASE/v0.11.0.md) | Access control, multi-tenancy, SSO and audit | planned | 7 | R-C1 R-C2 R-C3 R-C4 R-C5 R-C6 |
| [v0.10.0](RELEASE/v0.10.0.md) | Pipelines, functions, enrichment, redaction | planned | 4 | R-F4 R-N1 R-N2 R-N3 R-N4 |
| [v0.9.0](RELEASE/v0.9.0.md) | Alerts and incidents | planned | 8 | R-A1 R-A2 R-A3 R-A4 R-A5 R-A6 R-A7 R-A8 |
| [v0.8.0](RELEASE/v0.8.0.md) | Dashboards | planned | 6 | R-B1 R-B2 R-B3 R-B4 R-B5 R-B6 |
| [v0.7.0](RELEASE/v0.7.0.md) | Rollups and extended retention | planned | 4 | R-F4 R-P1 R-P2 R-P3 R-X1 |
| [v0.6.0](RELEASE/v0.6.0.md) | Metrics | planned | 2 | R-M1 R-M2 |
| [v0.5.0](RELEASE/v0.5.0.md) | Services and service map | planned | 4 | R-V1 R-V2 R-V3 R-V4 |
| [v0.4.0](RELEASE/v0.4.0.md) | Traces | planned | 6 | R-R1 R-R2 R-R3 R-R4 R-R5 R-R6 |
| [v0.3.0](RELEASE/v0.3.0.md) | State, saved views, stream health and the SQL-mode fix | ready | 7 | R-F4 R-I2 R-L5 R-Q1 R-S1 R-S2 R-S3 R-S4 R-S5 |
| [v0.2.0](RELEASE/v0.2.0.md) | Logs and playground | ready | 9 | R-G1 R-L1 R-L2 R-L3 R-L4 R-L5 R-L6 R-L7 R-Q1 |
| [v0.1.0](RELEASE/v0.1.0.md) | Foundation | ready | 13 | R-D1 R-D2 R-D3 R-F1 R-F2 R-F3 R-F4 R-F5 R-I1 R-Q1 R-Q2 R-Q3 R-Q4 R-T1 |

Status vocabulary: `planned` (ACs written, no code) → `in progress` (tests being written, gate counts them) → `ready` (`make check` green locally, tag run pending, user releases) → `shipped` (tag pushed, CI green, release published).

## 3. Roadmap in one page

1. **0.1 Foundation.** The risky part first: SQL safety, limits, errors, org-scoped routes, delivery pipeline, Streams and Connect pages.
2. **0.2 Logs, 0.3 State, 0.4 Traces, 0.5 Services, 0.6 Metrics.** The core investigation loop with correlation (D1). State (saved views, history, org-tagged metadata) arrives in 0.3 so everything after can store things.
3. **0.7 Rollups and extended retention.** Long windows and TTL tiers (first DDL writes).
4. **0.8 Dashboards, 0.9 Alerts and incidents, 0.10 Pipelines/functions/enrichment/redaction.** The parity modules.
5. **0.11 Access control, multi-tenancy, SSO, audit; 0.12 Query management, work groups, federation; 0.13 Reports, RUM view, operate.** The enterprise tier.
6. **1.0 Hardening.** API frozen by golden files, compatibility on two GreptimeDB versions, security review.

Not scheduled: Jaeger API as an alternative trace source, `greptime_trace_v2` (JSON2 attributes), anomaly ML beyond MAD, session replay, PDF reports. See [`ROADMAP.md`](ROADMAP.md) and the gaps in `ARCH/07`.

## 4. Changelog

Newest first. An entry is written when a release becomes `ready`; the same text becomes the GitHub Release notes (`RELEASE/<tag>.md`).

### Unreleased

- Requirements, architecture, positioning and parity matrix (ARCH 00–07), release plan with ACs for v0.1.0–v1.0.0, `scripts/seed.py` (demo traces and logs over OTLP/protobuf, `--db` per org).

### v0.3.0: State, saved views, stream health and the SQL-mode fix (ready)

Added: `grepui migrate` and the `grepui_*` metadata database and tables (schema version, saved views, history), with one guarded writer (the write allowlist is now exactly those tables); Saved views (create, rename, delete, share by link) and recent-search history in the Logs toolbar; Stream details (size, TTL, indexes, partitions, keys) and a Data health page (freshness, rate, last seen per service, quiet-stream warning); Security fix carried from v0.2.0: SQL mode checks every SHOW/DESC variant against the org's database and refuses TQL; Playground also creates the state store; `make migrate`.

First release that writes: only to the `grepui` metadata database, through `AppState::write`, which admits nothing but `INSERT INTO` the allowlisted tables (ADR 0006). Updates are upserts on (primary key, time index) and deletes are soft deletes: both verified live. Until access control (v0.11.0) the owner and history user are the implicit user `local`. Evaluated and not adopted: GreptimeDB's official Rust ingester crate (gRPC, ingestion-focused, about 106 extra crates); `sqlx` over the MySQL protocol is the recommended client for reads and for these small writes (ARCH 05). Found by live tests and fixed: `name` is a reserved word in GreptimeDB (quoted everywhere); `TIMESTAMP(3)` casts to milliseconds, not nanoseconds (an earlier divisor made history look 20 000 days old); the relation visitor misreads `SHOW TABLES FROM greptime.public`, so SHOW/DESC are checked by their own fields. Data size reads 0 B until GreptimeDB flushes a table to storage. Not yet exercised in a browser: the quiet-stream banner (covered by tests only).

### v0.2.0: Logs and playground (ready)

Added: Logs page: filter bar, histogram, fields sidebar with types and quick mode, top values, expandable rows, paging; Source details and Search Around; SQL mode (parser-validated, window macros, row cap), CSV/NDJSON export; Playground mode (`grepui playground --demo-data`, `make demo`) with deterministic demo data and a live traffic trickle; Security workflow (Semgrep, Trivy filesystem and image scans) and a base image without OpenSSL.

Verified live on GreptimeDB v1.2.1 (found and fixed): `INTERVAL '10 minutes'` parses over HTTP but not in MySQL prepared statements (the histogram uses integer arithmetic); `SHOW CREATE TABLE` backticks identifiers over the MySQL protocol and double-quotes them over HTTP; dotted attribute keys need the JSON path form `["http.method"]`; `LIKE` supports only the backslash escape; `DESC TABLE x` needs normalising for the SQL parser. Export is fetched then written (capped at 10 000 rows), not streamed from the database. SQL mode is confined to the org's database by parsing table references; `information_schema` is refused until access control (v0.11.0). **Known issue in v0.2.0 as tagged (fixed in v0.3.0, AC0.3.7):** SQL mode did not apply the database check to `SHOW TABLES FROM <db>` and `SHOW CREATE TABLE <db>.t`, and accepted TQL, whose `__schema__`/`__database__` matchers select another database. On a server with several databases, v0.2.0 SQL mode can read another database's table names and DDL. Upgrade to v0.3.0; until then run grepui with a database account that can read only the intended database.

### v0.1.0: Foundation (ready)

Added: Crate `grepui` (axum 0.8, sqlx MySQL protocol, clap): `serve`, `healthcheck`; `query/`: `Q`, `Bind`, `Reader`, `SqlxReader`, pure builders; `api/` health, info, streams, connect; `ui/`: shell, router, time range, "Show SQL" drawer, Streams page, Connect page; Pipeline: Makefile, workflows, `deny.toml`, Dockerfile, compose, scripts.

Verified so far: GreptimeDB OTLP is protobuf-only; table columns of the two OTLP tables (see RELEASE.md section 5).

**Planned, not built yet (newest first):**

### v1.0.0: Launch hardening (planned)

Will add: Golden files for every response shape; Compatibility run on the pinned GreptimeDB and the previous minor; Security review: read-only claim, write allowlist, RBAC route table, SQL mode, redaction; Load test on seeded 10 M spans / 10 M logs recorded in `docs/`.

No new requirement IDs.

### v0.13.0: Reports, RUM view and operate (planned)

Will add: Scheduled reports (HTML/CSV by email); RUM view over GrepRUM tables; Result cache with single-flight, `/metrics`, graceful shutdown, read-only-user check.

Session replay and PDF reports are not supported; ARCH 07 lists them as gaps.

### v0.12.0: Query management, work groups and federation (planned)

Will add: Running queries and cancel; Work groups and quotas per role; Query statistics; Several GreptimeDB sources with fan-out and merge.

### v0.11.0: Access control, multi-tenancy, SSO and audit (planned)

Will add: Local users, organizations (tenants), roles, sessions; multi-tenant UI with org switcher, branding and org admins; RBAC enforced in the query layer for every endpoint; API tokens and service accounts; OpenID Connect SSO; Audit trail with search.

Replaces the earlier plan for a single `--basic-auth` flag. `--no-auth` exists for local use and is refused when listening on a non-loopback address.

### v0.10.0: Pipelines, functions, enrichment, redaction (planned)

Will add: Pipelines manager over GreptimeDB's pipeline API with dry-run; Saved SQL functions; Enrichment tables from CSV; Redaction rules per field and role.

GreptimeDB pipelines are YAML-configured, not VRL scripts; this is a documented difference. Cipher-key field encryption is not implemented (redaction only).

### v0.9.0: Alerts and incidents (planned)

Will add: Rule engine: builder/SQL/PromQL, interval or cron, look-back, cooldown; Real-time (short interval) and composite alerts; anomaly alerts; Destinations, templates, test-send; dedup, compare-with-past, downtimes, history; Incidents; leader lease for HA.

Real-time here means short-interval polling of GreptimeDB (default 10 s), not an ingest hook; ARCH 07 states the difference.

### v0.8.0: Dashboards (planned)

Will add: Folders, dashboards, tabs; All panel types except Custom Charts (arbitrary user JS is excluded on purpose); Variables, filters, time range, auto refresh, compare-with-past; JSON import/export, move; public links; TV mode and playlists.

Custom Charts are excluded: they would execute arbitrary JavaScript. The HTML panel is the safe subset.

### v0.7.0: Rollups and extended retention (planned)

Will add: Planner choosing raw or rollup with `meta.source`; Rollup management from vetted templates (traces RED, metrics downsampling) with backfill; Retention view and edit per stream; tiering policy.

`docs/rollup.sql` Flow DDL verified here. First release with GreptimeDB DDL writes.

### v0.6.0: Metrics (planned)

Will add: PromQL range and instant queries; Metric and label lists, SQL on metric tables; Shareable URL encoding chart type and query.

To verify live: `TQL EVAL` output columns and label handling over the MySQL protocol.

### v0.5.0: Services and service map (planned)

Will add: Services list with RED; Per-service series and operations; Dependencies and service map graph; Service catalog with annotations (metadata table).

### v0.4.0: Traces (planned)

Will add: Trace list with histogram and latency scatter; Trace detail: waterfall, flame graph, span panel; Trace ↔ logs pivot.

Schema finding (live, v1.2.1): span attributes are flat columns named `span_attributes.<key>`; log and resource attributes are JSON.

## 5. Verified and unverified

Honest ledger of what has actually been run. Updated as releases land.

| Item | State |
|---|---|
| The reference platform's feature inventory (modules, enterprise labels) | read from its public docs on 2026-10-31; pages actually read are listed in ARCH 07; many sub-pages were navigation titles only |
| GreptimeDB OTLP accepts only protobuf (`application/x-protobuf`) | verified live on v1.2.1 (JSON returns 400) |
| `scripts/seed.py` writes traces and logs; `opentelemetry_traces`, `opentelemetry_logs` and the `_services` / `_operations` tables appear | verified live on v1.2.1 |
| Column names, types and roles of the two OTLP tables (span attributes are flat `span_attributes.*` columns) | verified live on v1.2.1 |
| Bound parameters over the MySQL protocol for `approx_percentile_cont`, `TQL EVAL`, process list, `ALTER TABLE ... SET ttl`, pipeline API, Flow DDL | **not yet run**: first exercised in the release that needs each |
| v0.1.0 to v0.3.0 code | `make check` green (offline tests + `ac-map`); `make test-db` green on GreptimeDB v1.2.1 (live tests for streams, search, histogram, search around, fulltext, playground); `cargo deny` and `cargo audit` clean; UI checked in a real browser (screenshots in `images/`) |
| Bound timestamp parameters, `TIMESTAMP(9)` and `JSON` decoding, `information_schema` | verified live (v0.1.0): sqlx needs `DateTime<Utc>` for `TIMESTAMP` and its `json` feature for `JSON` |
| `matches_term` with a bound term, `json_get_string` with dotted keys (path form `["k.x"]`), bound `LIKE` with backslash escapes, `CAST(CAST(ts AS BIGINT) AS VARCHAR)` nanosecond keys, `UNION ALL` of ordered limited selects, `SHOW CREATE TABLE`, `DESC TABLE`, SQL-mode window macros | verified live (v0.2.0); details in ARCH/03 |
| State store on GreptimeDB (v0.3.0): upsert by key + time index, soft delete, org isolation, `TIMESTAMP(3)` casts to ms, reserved word `name`, migration idempotent, views read back from a second app instance | verified live (`ac_0_3_*_live`) and in a browser (save, share link, recent, data health, stream details) |
| Quiet-stream banner on the Data health page | logic covered by tests; the banner itself **not yet seen in a browser** (needs 5 minutes of silence) |
| SQL-mode tenant-isolation fix (SHOW/DESC variants, TQL) | reproduced against a live server, fixed, regression tests, re-verified live; **v0.2.0 as committed still has the gap** (see its release notes) |
| `date_bin(INTERVAL '..')` in prepared statements | **fails** over the MySQL protocol (works over HTTP): the histogram uses integer arithmetic instead (found by the live test) |
| Playground: seed through OTLP, exactly-once on restart, refusal outside `demo`/`demo_*` | verified live (`ac_0_2_9_playground_seeds_once_live`) and in a browser (live traffic trickle visible in counts) |
| Security workflow tools | Semgrep (Rust, JS, Python, secrets, GitHub Actions, Dockerfile packs) clean on the tree; Trivy filesystem clean; Trivy image found a HIGH `libssl3` CVE in the `distroless/cc` base, **fixed** by switching to `base-nossl` + `libgcc_s` (rescanned clean, container runs); the workflow itself has not run on GitHub |
| CI, release and docker workflows | written for cargo; **not yet run on GitHub** (needs a pushed tag); the Docker build and container health were run locally |
| Quickwit UI details (React, TypeScript, Vite, MUI, Monaco) | from a documentation summary; UI source was not read ([ARCH 02](ARCH/02_reference_uis.md)) |

## 6. How a release is cut

**Only the human runs git commit, push, tag and release. The agent never does** ([`AGENTS.md`](AGENTS.md)).

1. ACs for the release are written or refreshed in `RELEASE/vX.Y.Z.md` before any code; the gate counts tests only once the status leaves `planned`.
2. Implement until `make check` is green (`fmt-check`, `clippy`, `ac-map`, `test`, `version-check`, `no-banned-names`) and `make test-db` is green against the pinned GreptimeDB for the `_live` tests.
3. Tick the release checklist, set the status to `ready`, update this file (sections 2 and 4), `ROADMAP.md`, `ARCH/07_parity_matrix.md` and any `ARCH/` or `docs/adr/` file the change touches.
4. The agent hands over a one-line commit message.
5. The user commits, then runs `make release VERSION=x.y.z` (space, then `VERSION=`). It bumps `VERSION` and `Cargo.toml`, amends the bump into that commit, pushes `HEAD`, tags `vX.Y.Z` and pushes the tag. Run it once per commit; it needs at least one commit and an `origin` remote.
6. The tag triggers three workflows:
   - `ci.yml`: `make check` plus the dependency audit.
   - `release.yml`: gate (tag must equal `VERSION`, `make check`) → four native builds → GitHub Release with `RELEASE/<tag>.md` as the notes (generated notes if the file is missing).
   - `docker.yml`: native amd64 and arm64 builds pushed by digest, merged into a multi-arch tag set (`X.Y.Z`, `X.Y`, `latest`, short sha) on `ghcr.io/teochenglim/grepui`.
7. After CI is green, set the status to `shipped` in the next change.

**Patches** (`0.x.y`) are fixes with no AC change; they get a short `RELEASE/v0.x.y.md` and a changelog line here.

**Actions are pinned** to full commit SHAs. `make github-action-bump` re-pins them to the newest release of the same major (`MAJOR=1` moves majors); review the diff and run `make check` before committing.

## 7. Release checklist template

```
- [ ] ACs have >=2 passing tests (`make ac-map`, `scripts/release-check.sh X.Y.Z`)
- [ ] `make check` green
- [ ] `make test-db` green against GREPTIME_VERSION (tests ending _live)
- [ ] `cargo deny check` green
- [ ] Screenshots updated in `images/`; browser pass recorded (UI releases)
- [ ] Status -> ready; RELEASE.md sections 2 and 4, ROADMAP.md, ARCH/07, ARCH/ and docs/adr/ updated
- [ ] One-line commit message handed to the user
```

## 8. Where things live

| Need | File |
|---|---|
| Why the product looks like this | [`ARCH/06_positioning.md`](ARCH/06_positioning.md), [`ARCH/02_reference_uis.md`](ARCH/02_reference_uis.md) |
| Feature parity and gaps | [`ARCH/07_parity_matrix.md`](ARCH/07_parity_matrix.md) |
| Requirements and the AC→test gate | [`ARCH/00_requirements_traceability.md`](ARCH/00_requirements_traceability.md), [`ARCH/04_testing_and_ac_map.md`](ARCH/04_testing_and_ac_map.md) |
| GreptimeDB feature mapping and unverified SQL | [`ARCH/03_greptimedb_mapping.md`](ARCH/03_greptimedb_mapping.md) |
| Dependencies and pipeline | [`ARCH/05_tech_stack_and_release.md`](ARCH/05_tech_stack_and_release.md) |
| Per-release scope, ACs, tests | [`RELEASE/`](RELEASE/README.md) |
| Decisions | [`docs/adr/`](docs/adr/README.md) |
| Rules for agents and the human-only git policy | [`AGENTS.md`](AGENTS.md) |
