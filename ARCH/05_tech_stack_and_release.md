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
| csv | Enrichment upload (CSV export is hand-written: one fixed schema) | 0.10 |
| argon2, rand | Password hashing, tokens | 0.11 |
| openidconnect or jsonwebtoken + reqwest (rustls) | OIDC SSO | 0.11 |
| lettre | SMTP for alerts and reports | 0.9 |
| reqwest (rustls) | Webhooks, GreptimeDB pipeline API | 0.9 |
| cron | Cron schedules for alerts and reports | 0.9 |

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


