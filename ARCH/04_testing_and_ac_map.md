# ARCH 04: Testing and the AC Map

| Layer | Where | Runs in CI |
|---|---|---|
| Unit and builder tests (pure SQL builders, parsers, planners) | `src/**` `#[cfg(test)]`, `tests/` | yes |
| HTTP tests through the real router with a fake `Reader` | `tests/ac_*.rs` | yes |
| Live GreptimeDB tests (`_live` suffix, `#[ignore]`) | `tests/` | yes, with a GreptimeDB service container (`make test-db`) |
| Isolation matrix (every route x every caller kind) | `tests/ac_isolation.rs` (v0.11) | yes |
| Golden API shapes | `tests/golden/` (v1.0) | yes |
| Browser pass (screenshots in `images/`) | recorded per release in the checklist | no, run on a real machine |

## Naming rule (what `ac-map.sh` greps)

`fn ac_<M>_<m>_<n>_<what>` governs `AC<M>.<m>.<n>`. A `_live` suffix marks tests that need GreptimeDB.

## Gate

Per `ARCH/00`: each R-ID is cited by ≥1 AC, each AC cites ≥1 R-ID and has ≥2 distinct tests. A release file whose status starts with `planned` is checked for requirement coverage only.

## Workflow

1. Write or refresh the ACs in `RELEASE/vX.Y.Z.md` first.
2. Red tests, then code, until `make check` is green.
3. `scripts/release-check.sh X.Y.Z` prints the per-AC test count and runs that release's tests.
