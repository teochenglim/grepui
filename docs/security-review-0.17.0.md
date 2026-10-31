# Security review, v0.17.0

Date: 2026-10-31. Reviewer: the maintainers' coding agent, with the owner's instruction to review the read-only claim, the write allowlist, the RBAC route table, SQL mode and redaction. It is a review of this repository at v0.16.0 plus the v0.17.0 changes, by reading the code, probing it, and pinning what was found in tests. It is not an external audit and not a penetration test.

## Claims checked

| Claim | How it was checked | Result |
|---|---|---|
| The data plane is read-only: telemetry is reached only through SELECT, SHOW and TQL builders | `ac_0_17_1_invariants_still_hold` runs every builder (including the LLM analytics and version statements) and checks tag, verb, no stray statement, no write keyword, one bind per placeholder; `ac_0_1_4_builders_are_read_only` | holds |
| The write allowlist is exactly grepui's own state tables, in grepui's own database | the allowlist, tenant tables, schema version and admin write list are pinned in `ac_0_17_1_invariants_still_hold`; `store::check_write` refuses other tables, other databases, other verbs and a second statement (same test) | holds |
| Every route is classified and denied by default | `tests/golden/routes.txt` (every route, method and access level) must match `access_for` (`ac_0_17_2_route_table_golden`); every `.route(` in the router source must be in `ROUTES` and the other way round (`ac_0_17_2_no_unlisted_route`); `ac_access` walks the roles and two-org isolation | holds |
| Only the public-link data routes, health, sign-in and the OIDC callback are anonymous | read from the golden route table | holds: `GET /healthz`, `/api/v1/health`, `/api/v1/info`, `/api/v1/auth/me`, the OIDC login and callback, `POST /api/v1/auth/login` and `/setup`, and the three `public/{token}` reads |
| SQL mode runs one read statement in the org's database | 56 bypass attempts probed by hand (comments, stacked statements, CTE and EXPLAIN writes, other databases quoted four ways, subqueries and unions into another database, `INTO OUTFILE`, `COPY`, `ADMIN`, `KILL`, `SET`, `USE`, TQL, unicode spaces and bidi controls, string-escape tricks) | holds for everything except finding 1 below; resource abuse (`sleep`, a cross join, `generate_series`) is accepted by the parser and bounded by the query timeout, work groups and cancel (ADR 0013) |
| PromQL is the only request text embedded in SQL, and only through `metrics::check` | `ac_0_6_*` tests (one per spelling of the schema matchers) | unchanged, holds |
| Redaction is applied to every data response, fails closed, and covers exports | `api::redaction::layer`; 503 until the rules are loaded; JSON, NDJSON and CSV; LLM prompt masking (ADR 0016) in the same layer | holds, with the stated limit: a column renamed by a SQL alias is not recognised by name |
| Passwords, sessions, tokens | argon2id with a random salt; constant-time secret comparison (`same_secret`); a password change ends the user's sessions; tokens stored as hashes, shown once; login rate limit 10 per user per minute and 600 overall; API token rate limit | holds |
| CSRF | the `Origin` (else `Sec-Fetch-Site`) of every cookie-authenticated state change is checked in the request layer; the cookie is HttpOnly and SameSite=Strict | holds; `Secure` is the reverse proxy's setting (grepui speaks plain HTTP) |
| Outbound requests (alert webhooks, SMTP) cannot reach cloud metadata | webhook targets are plain http, no credentials, and the resolved addresses are checked again at delivery against unspecified, link-local (169.254.0.0/16, fe80::/10) and multicast | holds; loopback and private ranges are allowed on purpose (a webhook to an internal service is the use case) and destinations are created by org admins only |
| Dependencies | `cargo deny check` and `cargo audit` clean on the lockfile; Trivy and Semgrep in CI | holds on 2026-10-31 |

## Findings

1. **SQL mode could read grepui's own state tables when an org's database was the metadata database** (medium). SQL mode checked the database name of every table it read, and nothing else. If the operator set `--meta-database` equal to the database an org reads (for example `--meta-database public` with the default org on `public`), a member of that org could `SELECT` the users, password hashes, API token hashes, sessions, alert rules and audit rows of every org from the SQL page, from a dashboard SQL panel or from an alert rule. Creating an org on the metadata database is already refused; the default org and a hand-written configuration were not. **Fixed in v0.17.0:** `sqlmode::check` refuses any relation named `grepui_*`, in queries, subqueries, CTEs, unions, `DESC` and `SHOW CREATE TABLE`, whatever the database, with its own message so alert rules are refused at save. Test: `ac_0_17_1_state_tables_unreadable_in_sql_mode`.
2. **The app sent no browser security headers** (low to medium). No Content-Security-Policy, no `nosniff`, no framing protection, no referrer policy. The UI never uses `innerHTML` and text panels are sanitised and sandboxed, so there was no known injection, but a browser should be told. **Fixed in v0.17.0:** every response carries a CSP (`default-src 'self'`, `script-src 'self'`, `connect-src 'self'`, `frame-ancestors 'none'`, `base-uri 'none'`, `object-src 'none'`, inline styles and `https:` images allowed because the UI sets style attributes and text panels may show images), `X-Content-Type-Options: nosniff`, `X-Frame-Options: DENY`, `Referrer-Policy: no-referrer`, a permissions policy and `Cross-Origin-Opener-Policy: same-origin`. Tests: `ac_0_17_4_*`. A browser pass over ten pages, the trace page and a dashboard with markdown and HTML text panels in their sandboxed iframes showed no policy violations.
3. **Found while building 0.16.0 and fixed in it**: the LLM session turn list returned prompt snippets under keys the content masking did not match (`ac_0_16_2_session_turns_masked_for_viewers`).

## Accepted risks and limits

- **Heavy statements.** A member who may use SQL mode can send an expensive read. It is bounded by the query timeout (30 s default), the work-group limits per role, the 1,000-row cap and the window cap, and an admin can cancel it; it is not prevented.
- **`--no-auth`** makes everyone an admin; the CLI refuses it off loopback.
- **Loopback and private webhook targets** are allowed (see above).
- **Redaction by name.** A masked field renamed by a SQL alias, or text inside a free-form log line, is not recognised; prompt text copied into an attribute with another name is not masked until that name is added to the list.
- **TLS** is the reverse proxy's job for the UI and API; grepui itself has a TLS client only for OIDC providers; webhooks and SMTP are plain.
- **Not reviewed:** GreptimeDB itself, the container base image beyond the CI scanners, the browser's own handling of the sandboxed iframe, and anything written after 2026-10-31.

## Repeat

Run `make check`, `cargo deny check` and `cargo audit`; read `tests/golden/routes.txt` and `tests/golden/response_shapes.json` in any diff; and repeat the SQL mode probe (`ac_0_17_1_invariants_still_hold` carries a subset) whenever `src/query/sqlmode.rs` changes.
