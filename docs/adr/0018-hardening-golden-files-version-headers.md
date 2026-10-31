# 0018 Hardening: golden files, one supported database version, browser headers, state tables

Status: Accepted (v0.17.0).

## Context

By v0.16.0 grepui had 148 routes, an enterprise tier and an LLM track, with its safety rules spread over tests written release by release. Before anything is called stable, the API and the permission table need to be frozen so a change is a visible decision, the supported database needs to be stated, and the safety claims need a review that is written down (`docs/security-review-0.17.0.md`).

## Decision

- **Golden files freeze the contract.** `tests/golden/response_shapes.json` holds, for every route, the status and the structure (keys and types, never values) of its response from a scripted database, plus seven representative writes; `tests/golden/routes.txt` holds every route, method and access level. The tests fail with the first differing line; an intended change is made with `UPDATE_GOLDEN=1 cargo test --test ac_hardening` and the diff is reviewed like an API change. Arrays are the merge of their elements, a key missing from some is `key?`, null beside a type is `type?`. `ac_0_17_2_no_unlisted_route` reads the router source for every `.route(` and compares with `ROUTES`, so a route cannot be added without being classified. The shapes are frozen for 0.17.x; a later 1.0.0 may freeze them as a promise to users.
- **One supported GreptimeDB: v1.3.0 and newer (owner decision).** A run of the live suite on v1.2.1 failed 11 of 44 tests (trace v2 tables, the JSON2 functions the LLM views read, RUM). grepui asks the database for its version at start (`SELECT version()`), logs it, and warns once if it is older than 1.3; it does not refuse to start, because an unreadable or unusual version string should not take the UI down. The pinned version in the Makefile must satisfy the rule (a test reads the Makefile). A newer minor is expected to work and is checked by moving the pin.
- **Browser security headers on every response**, set in one layer unless a handler set its own: a Content-Security-Policy (`default-src 'self'`, `script-src 'self'` with no inline script and no eval, `connect-src 'self'`, `frame-ancestors 'none'`, `base-uri 'none'`, `object-src 'none'`; `style-src` keeps `'unsafe-inline'` because the DOM helper sets style attributes; `img-src` allows `data:` and `https:` for icons and text panels), `nosniff`, `X-Frame-Options: DENY`, `Referrer-Policy: no-referrer`, a permissions policy and `Cross-Origin-Opener-Policy`. No HSTS: grepui speaks plain HTTP and TLS is the proxy's business.
- **SQL mode never reads grepui's own tables.** Any relation named `grepui_*` is refused whatever the database, because the database-name check alone fails when an org's database is the metadata database (security review finding 1). Alert rules are refused at save with the same check.

## Consequences

An intended API change now costs a golden-file diff, which is the point. The shapes freeze structure, not values, so a changed unit or meaning is still caught only by feature tests. The CSP keeps `'unsafe-inline'` for styles, which is weaker than a nonce-based policy; moving the UI off inline style attributes is possible but was not worth a rewrite here. A configuration that points an org at the metadata database is still unwise (other state could be read through the telemetry query paths of that org's own tables), but SQL mode, dashboards and alert rules can no longer read the state tables through it.
