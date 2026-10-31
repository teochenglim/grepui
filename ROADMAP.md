# Roadmap

Status per release is in `RELEASE.md` (roll-up) and `RELEASE/` (detail). This file is the order and the reasons. Parity targets and gaps: `ARCH/07_parity_matrix.md`. The LLM track has its own design note: `ARCH/08_llm_observability.md`.

## Release train (planned releases 0.6 to 1.0)

1. **0.1 to 0.5 are built** (foundation, logs, state and data health, traces, services and service map); see `RELEASE.md`.
2. **0.6 Metrics**: finishes the core investigation loop.
3. **0.7 Rollups and extended retention** (D5): long windows and TTL tiers.
4. **0.8 Dashboards, 0.9 Alerts and incidents, 0.10 Pipelines/functions/enrichment/redaction**: the parity modules.
5. **0.11 Access control, multi-tenancy, SSO, audit; 0.12 Query management, work groups, federation; 0.13 Reports, RUM view, operate**: the enterprise tier.
6. **1.0** Freeze the API and prove it.

## Platform: what grepui depends on in GreptimeDB

Pinned to **v1.3.0** (released 2026-10-28; grepui moved to it in v0.4.0). Sources and checks are in `ARCH/08` section 5.

| GreptimeDB | State | grepui action |
|---|---|---|
| Trace v2 pipeline (attributes in JSON2 columns) | **shipped in v1.3.0**, opt-in, experimental | LLM track step L1: read both trace layouts |
| `IN`-subquery, `EXISTS`, joins correctness | fixed (retested; not in release notes) | allowed again for new features with a live proof (ADR 0007) |
| v1.4 "Flow engine enhancements" | planned (Q3 per roadmap #7685) | revisit the rollup design before building 0.7 |
| Vector index, AI functions | dropped from GreptimeDB's roadmap | no embedding similarity search in grepui |
| LLM / GenAI observability | **not on GreptimeDB's roadmap** | grepui's LLM track does not wait on GreptimeDB |
| Query cancel / process list | not found in recent releases | verify live before planning the work-group release (0.12) |

## LLM observability track (roadmap first, no release numbers yet)

Goal: read, search and analyse **OpenInference** (and OTel `gen_ai.*`) traces stored in GreptimeDB, with views no generic trace UI has. grepui still does not ingest; the Connect page tells LLM SDKs how to send to GreptimeDB. The track builds on traces (0.4) and the state store (0.3), and is scheduled between Services and Metrics unless you decide otherwise.

| Step | Theme | What it delivers | Depends on |
|---|---|---|---|
| L0 | Platform | pin v1.3.0 (**done**), spike: OpenInference on trace v2 stored and queried (**done**, `ARCH/03`, `ARCH/08` section 3) | |
| L1 | Trace layouts | detect flat (v1) vs JSON2 (v2) tables; attribute accessor used by filters, span panel, top values, detail; Connect page for LLM SDKs with the v2 headers | L0 |
| L2 | LLM span views | kind badges and filters; chat transcript with tool calls; tokens and cost chips; retrieval documents; embeddings without vectors; long values collapsed; the normalised OpenInference and `gen_ai.*` model | L1 |
| L3 | LLM analytics and sessions | tokens, cost, latency percentiles, error rate by model, tool and span kind; sessions and users; optional per-model price table | L2, rollup planner for long ranges |
| L4 | Evaluations and annotations | read `annotation.*`, `evaluation.*`, `gen_ai.evaluation.result`; human feedback stored in `grepui_*` | L2, state store |
| L5 | Agent graph | from `graph.node.*`, `agent.name` and parentage (Apache-2.0; the reference platform lists its agent graph as Enterprise and Cloud) | L2 |
| L6 | Operate | content protection (redaction by role, size caps), alerts on cost, tokens and error rate | access control (0.11), alerts (0.9), redaction (0.10) |

Unverified and needing a spike before L1 ships: sending from the real Phoenix and OpenInference SDKs to GreptimeDB's OTLP endpoint with the pipeline headers; the Phoenix project-name attribute.

## After 1.0 (not scheduled)

Live tail, log patterns, insights and root-cause analysis, SLOs, annotation sources, alert notification dependencies, Jaeger API as an alternative trace source, session replay (needs GrepRUM support), PDF reports, prompt playground and span replay, datasets and experiments (Phoenix has them; they need a write path to an LLM provider, which is a separate decision).

Explicit non-goals: `ARCH/06`.
