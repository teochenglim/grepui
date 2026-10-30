# 0002 Typed API, bound SQL, every statement tagged

**Context.** GreptimeDB exposes SQL, PromQL (via `TQL`) and other APIs for the same data.
**Decision.** Clients never send SQL (except SQL mode). Builders produce `Q { name, sql, binds }`; every statement starts with `/* q:<name> */`; responses carry `meta`.
**Consequences.** Safe by construction, cacheable, greppable in GreptimeDB query logs, and one function per dialect fix.
