# ARCH 05: Tech Stack and Release

## Dependencies (planned; each row is added in the release that introduces it)

| Dependency | Why | From |
|---|---|---|
| axum 0.8, tokio, tower | HTTP server and middleware | 0.1 |
| sqlx (MySQL, no default features) | Talk to GreptimeDB over its MySQL protocol with bound parameters | 0.1 |
| clap | CLI and env config | 0.1 |
| serde, serde_json, thiserror, anyhow, tracing | Plumbing | 0.1 |
| sqlparser (visitor) | Validate SQL mode input by AST, find table references (R-L5) | 0.2 |
| futures-util | Read result rows as a stream so SQL mode stops at the row cap | 0.2 |
| ~~csv~~ | Not adopted in 0.10: `src/csv.rs` (RFC 4180 reader and writer, about 100 lines, tested) serves enrichment uploads and CSV redaction | |
| rand | View ids (random 64-bit), public link tokens (OS random) | 0.3 |
| sha2 | SHA-256 of public dashboard link tokens (already in the tree through sqlx) | 0.8 |
| argon2 | Password hashing, tokens | 0.11 |
| openidconnect or jsonwebtoken + reqwest (rustls) | OIDC SSO | 0.11 |
| ~~lettre~~ | Not adopted in 0.9: alert email is a 60-line plain-SMTP client (relay on a trusted network); revisit with TLS | |
| reqwest (rustls) | HTTPS webhooks, GreptimeDB pipeline API (0.9 webhooks are plain http written in the crate) | 0.10 |
| ~~cron~~ | Not adopted in 0.9: a five-field UTC parser is in `alerts::cron` (about 100 lines, tested) | |

### Considered and not adopted

| Option | Why not (evaluated 2026-10-31) | Revisit when |
|---|---|---|
| `greptimedb-ingester` 0.19, GreptimeDB's official Rust SDK (Apache-2.0) | gRPC on port 4001 and ingestion-focused: insert and bulk streaming, no SQL query API in its source. It adds about 106 crates on top of grepui's 168 (Arrow, tonic, prost). Reads and the small state writes are served by `sqlx` over the MySQL protocol, GreptimeDB's recommended query path, with bound parameters. For the playground it would write raw rows into tables of our own design, losing the real OTLP-created tables and the `_services`/`_operations` tables the demo wants | grepui needs high-volume writes (alert history at scale, an ingest proxy): none is planned |
| `reqwest` / hyper client | The playground needs one plain-HTTP POST; the 40-line client in `playground/http.rs` avoids a TLS dependency | something needs HTTPS or redirects (webhooks in v0.9) |
| `opentelemetry-otlp` SDK for demo data | Heavy, and cannot set past timestamps; a hand-written protobuf encoder is small and tested | never likely |

No Node, no npm, no JS framework: the UI is hand-written ES modules (ARCH 06 D7). A chart renderer is written in the repo (SVG/canvas) unless a vendored, licence-checked library is added with an ADR.

## Versioning

SemVer. `VERSION` is the single source; `Cargo.toml` must equal it (`make version-check`); the binary reports it. `0.x` minors map to `RELEASE/README.md`; patches get a short note.

## Pipeline

- `ci.yml`: on PRs and `v*` tags: `make check`; GreptimeDB integration job; `cargo audit` and `cargo deny`.
- `release.yml`: on `v*` tags: gate (tag must equal `VERSION`, `make check`) → four native builds (linux/darwin × amd64/arm64) → GitHub Release using `RELEASE/<tag>.md`.
- `docker.yml`: on `v*` tags: native amd64 and arm64 builds pushed by digest to GHCR, merged into one multi-arch tag set. Non-root image on `distroless/base-nossl` (no OpenSSL; `libgcc_s` copied from the build stage).
- `security.yml`: on push/PR to main and weekly: Semgrep (Rust, JS, Python, SQL injection, secrets, OWASP, Dockerfile, GitHub Actions), Trivy filesystem scan (Cargo.lock), Trivy image scan (builds the image, no push). SARIF goes to the Security tab; CRITICAL/HIGH fails the job.
- All actions are pinned to full commit SHAs; `make github-action-bump` re-pins them (`scripts/pin-actions.py`).
- Only the user commits, tags and releases (AGENTS.md).


