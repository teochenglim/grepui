# Release plan

The roll-up and changelog live in [`../RELEASE.md`](../RELEASE.md). One file per release here: `RELEASE/vX.Y.Z.md` holds scope, acceptance criteria (each mapped to named tests), notes and the release checklist. Positioning for each theme is in `ARCH/06_positioning.md`; feature parity is in `ARCH/07_parity_matrix.md`.

| Release | Theme | Design choices | Exit gate (summary) | Depends on |
|---|---|---|---|---|
| [v0.1.0](v0.1.0.md) | Foundation | D2 D4 D6 D7 D8 | `make check` green; Streams and Connect pages work against the pinned GreptimeDB; three workflows pinned | - |
| [v0.2.0](v0.2.0.md) | Logs and playground | D1 D4 D6 | filter bar → bound SQL; SQL mode refuses non-read statements; live tests on seeded OTLP logs; the playground seeds a demo database once | 0.1 |
| [v0.3.0](v0.3.0.md) | State, saved views, stream health and the SQL-mode fix | D2 D3 D6 | `grepui migrate` idempotent on the pinned DB; saved views and history live in GreptimeDB and are read back through a second app instance; data-health page works; SQL mode confines every statement variant to the org's database | 0.2 |
| [v0.4.0](v0.4.0.md) | Traces and the v0.3.0 CI fix | D1 | one row per trace; waterfall, flame graph and span panel data exactly match generated traces on a fresh database; log pivot works both ways; the CI-parity run (fresh unflushed database) is green | 0.3 |
| [v0.5.0](v0.5.0.md) | Services and service map | D1 | RED matches numbers computed from the seed; service map and catalog render from API data | 0.4 |
| [v0.6.0](v0.6.0.md) | Metrics | D4 | PromQL via TQL works on seeded metrics; shareable metric URLs round-trip | 0.5 |
| [v0.7.0](v0.7.0.md) | Rollups and extended retention | D5 | 7-day window answered from the Flow sink; TTL changes are previewed, applied and audited-ready | 0.6 |
| [v0.8.0](v0.8.0.md) | Dashboards | D1 D4 | build, save, reload and share a dashboard with every panel type on seeded data | 0.7 |
| [v0.9.0](v0.9.0.md) | Alerts and incidents | D2 | an alert fires once on seeded data, notifies a webhook, dedups, and a replica restart does not double-fire | 0.8 |
| [v0.10.0](v0.10.0.md) | Pipelines, functions, enrichment, redaction | D2 | a pipeline is created, dry-run and listed; a CSV enrichment join works; redaction hides a field for a role | 0.9 |
| [v0.11.0](v0.11.0.md) | Access control, multi-tenancy, SSO and audit | D2 D3 | a viewer cannot read another org's streams or write state; SSO login maps claims to a role; every action appears in the audit table | 0.10 |
| [v0.12.0](v0.12.0.md) | Query management, work groups and federation | D3 | a quota stops an over-limit query before it runs; a running query is cancelled; two sources merge by time | 0.11 |
| [v0.13.0](v0.13.0.md) | Reports, RUM view and operate | D3 D7 | a scheduled report arrives by email; RUM page renders from GrepRUM tables; two identical concurrent queries hit the DB once | 0.12 |
| [v0.17.0](v0.17.0.md) | Hardening | all | API shapes frozen by golden files; GreptimeDB v1.3.0 is the one supported version, checked at start-up; security review recorded | 0.16 |
| [v0.18.0](v0.18.0.md) | Documentation | none | README with screenshots, User Guide and Technical Guide (AsciiDoc); a test keeps flags, pages, routes, images and links true | 0.17 |
| [v1.0.0](v1.0.0.md) | Better than the reference platform, shown by evidence | all | every comparison backed by a repo test or measurement; gap list closed; every ADR re-read (vision only) | 0.17 |

## Per-release workflow

1. Write/refresh `RELEASE/vX.Y.Z.md` ACs **before** code (red tests first, see ARCH 04).
2. Implement until `make check` (and `make test-db` where listed) is green.
3. Tick the checklist in the release file; update `RELEASE.md`, `ROADMAP.md` and any `ARCH/` / `docs/adr/` touched.
4. Agent hands over a one-line commit message. **User** commits, then runs `make release VERSION=x.y.z`
   (bumps `VERSION` + `Cargo.toml`, pushes, tags `vX.Y.Z`; the tag push triggers CI `release.yml` and `docker.yml`).
5. CI gate (`make check`, tag == `VERSION`) → 4-target build → GitHub Release using `RELEASE/vX.Y.Z.md` as notes.

## Versioning

SemVer. `0.x` minors map to the table above; patches (`0.x.y`) are fixes with no AC change and get a short `RELEASE/v0.x.y.md`. `0.17.0` freezes the `/api/v1/*` shapes with golden files. `1.0.0` is the evidence release (vision only for now).
