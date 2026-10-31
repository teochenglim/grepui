# 0007 No joins, IN-subqueries or EXISTS on telemetry tables

Status: Accepted. Narrowed by the v1.3.0 retest (see "Update" below): the bug is fixed upstream; the conditional-aggregation design stays.

## Context

The first version of the trace list found "traces with at least one matching span" with `trace_id IN (SELECT trace_id FROM <same table> WHERE <filter>)`. On a fresh GreptimeDB v1.2.1 it returned the wrong rows. Isolating it with literal SQL against a table where 33 traces have a `cart` span (the plain `GROUP BY` answered 33, correctly):

| Formulation | Result |
|---|---|
| `trace_id IN (SELECT trace_id ... service_name = 'cart')` | 4, then 6, then 5, then 0 on repeats |
| `... IN (SELECT DISTINCT trace_id ...)` | 6 |
| `EXISTS (SELECT 1 ... i.trace_id = o.trace_id ...)` | 1 |
| self `JOIN` on a `SELECT DISTINCT trace_id` derived table | 2, then 12 |

The same statement returned different answers on repeat, before and after `ADMIN flush_table`. The conditional aggregate `sum(CASE WHEN service_name = 'cart' THEN 1 ELSE 0 END) ... HAVING > 0` returned 33 every time. A second planner limitation: the same aggregate expression twice in one select list (`min(x)` used for two outputs) fails with "Schema contains duplicate unqualified field name".

## Decision

No query on a telemetry table may use a join, an `IN (subquery)` or `EXISTS` against a table (including itself). Row-level relations are expressed as conditional aggregation in one `GROUP BY` (`matches` counters in a derived table with explicit aliases), and anything that needs a join is computed in Rust from rows fetched with a plain scan (for example the service dependency edges in v0.5.0, which are parent/child span pairs). The builder tests assert the generated SQL contains none of ` JOIN `, `EXISTS`, ` IN (SELECT`. Literal `IN (?, ?, ...)` lists are not subqueries and remain allowed.

## Consequences

Filters are evaluated per span inside the aggregate, so all conditions must match the same span (`service=cart` with `errors=true` means "a cart span that errored"). This is documented in the UI help and tested live against generated data. The trace list scans the window's spans once, which it needed to do for the totals anyway. When GreptimeDB fixes the semi-join behaviour this ADR can be revisited, with the table above as the regression test.

## Update: retested on GreptimeDB v1.3.0 (2026-10-31)

grepui is now pinned to v1.3.0 (released 2026-10-28). The same queries on a fresh v1.3.0 with the same shape of data (true answer 32):

| Formulation | v1.2.1 | v1.3.0 |
|---|---|---|
| plain `GROUP BY` aggregate | correct | 32 |
| `IN (SELECT ...)` | 4, 6, 5, 0 on repeats | 32, 32, 32, 32 |
| `EXISTS (SELECT 1 ...)` | 1 | 32 |
| self-`JOIN` on `SELECT DISTINCT` | 2, 12 | 32 |
| same aggregate twice in one select list | planner error | planner error (unchanged) |

The v1.3.0 release notes that were read do not mention this fix (no semi-join or `IN`-subquery entry was found in the six most recent releases), so it is established by this test, not by a changelog. The wrong results are therefore a v1.2.x bug and grepui supports only v1.3.0 and newer.

Consequences: the trace filters keep conditional aggregation because it is correct on both versions, needs one scan and keeps whole-trace aggregates exact, so there is no reason to rewrite it. New features may use joins and `IN (subquery)` where they are clearly simpler (the service dependency edges in the roadmap), provided a live test compares the result to an independently computed answer. The duplicate-aggregate rule still applies. If the pinned version moves again, rerun the table above first.

## Update: first use of a join (v0.5.0)

Service dependencies (`query/services.rs`) join each span to its parent span in one self-join and aggregate per caller and callee. It is the only join in grepui. `ac_0_5_3_dependencies_live` compares every edge with the answer computed from the generator's records and repeats the query to catch nondeterminism; the demo database gave the same edge counts as an independent pairing. The trace builders are unchanged and still avoid joins.
