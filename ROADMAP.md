# Roadmap

Status per release is in `RELEASE.md` (roll-up) and `RELEASE/` (detail). This file is the order and the reasons. Parity targets and gaps: `ARCH/07_parity_matrix.md`.

1. **0.1 Foundation.** The risky part first: SQL safety, limits, errors, org-scoped routes (multi-tenant from the first route), pipeline, Streams and Connect pages.
2. **0.2 Logs, 0.3 State, 0.4 Traces, 0.5 Services, 0.6 Metrics.** The core investigation loop, with correlation. State arrives in 0.3 so everything later can store things, always tagged with `org_id`.
3. **0.7 Rollups and extended retention.** Long windows and TTL tiers.
4. **0.8 Dashboards, 0.9 Alerts and incidents, 0.10 Pipelines/functions/enrichment/redaction.** The parity modules.
5. **0.11 Access control, multi-tenancy, SSO, audit; 0.12 Query management, work groups, federation; 0.13 Reports, RUM view, operate.** The enterprise tier.
6. **1.0** Freeze the API and prove it.

After 1.0 (not scheduled): live tail, log patterns, insights/RCA, SLOs, annotation sources, alert notification dependencies, Jaeger API as an alternative trace source, `greptime_trace_v2` (JSON2 attributes), session replay (needs GrepRUM support), PDF reports.
Explicit non-goals: ARCH 06.
