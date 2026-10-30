# 0004 State in GreptimeDB, writes limited to an allowlist

**Context.** Dashboards, alerts, users and audit need storage; a second database would add a dependency. The earlier "read-only, stateless" claim cannot hold with those features.
**Decision.** grepui stores its state in `grepui_*` tables in GreptimeDB. The only writes the binary can issue are to those tables and to admin-gated TTL, pipeline and flow operations. The list is a single allowlist, asserted by tests (R-F4, R-S1).
**Consequences.** One dependency, replicas are interchangeable (leader lease for schedulers). Append-mode tables reject `DELETE`, so metadata tables are designed for update-by-key and TTL.
