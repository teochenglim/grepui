# 0009 Dashboards run saved queries on the server and treat all text as hostile

Status: Accepted (v0.8.0).

## Context

A dashboard is stored by one person and viewed by others, sometimes without logging in (public links, wall displays), and it can arrive from outside as an import. Its panels carry queries, variables, and free text (markdown, HTML).

## Decision

- **The browser never sends SQL for a panel.** `GET /dashboards/{id}/panels/{panel}/data` finds the panel in the stored spec and runs *its* query. A public link therefore reaches exactly the queries the dashboard already holds, with the viewer's time window and variable values as the only inputs, and a public view of the spec omits the query text altogether. SQL panels pass the same parser allowlist as SQL mode (`sqlmode::check`, one read-only statement confined to the org's database); PromQL panels pass `metrics::check` (ADR in v0.6.0 notes).
- **Variables are bound, not pasted.** In SQL, `$name` outside strings, quoted names and comments becomes `?` (several for a multi-select, `NULL` for an empty one) and the values travel as binds; the window macros stay macros. A custom variable accepts only its own listed values. PromQL has no bind parameters, so a value there must be letters, digits and `_ . : / -`, and the substituted expression is checked again. Tests feed quotes, comments, stacked statements, `__schema__` and the window macros as values.
- **Text panels are rendered on the server and shown in a sandboxed frame.** Markdown is escaped first and only then given a few constructs; HTML goes through an allowlist sanitiser (tags, `title`, `href` to http, https or mailto, table spans; scripts, styles, frames, forms, SVG and comments are dropped with their content, links get `rel="noopener noreferrer nofollow"`). The page puts the result in an `<iframe sandbox>` (no scripts, no same-origin) with a `default-src 'none'` policy as a second layer. Custom Charts (arbitrary JavaScript) are not offered.
- **One canonical document.** `dash::canonical` validates a spec (known keys, closed sets, size limits, unique ids, errors naming the path such as `spec.tabs[0].panels[1].type`) and returns the form that is stored, exported and returned; applying it twice changes nothing, so export then import is an identity and ids stay stable.
- **Public links** carry a random 128-bit token; only its SHA-256 is stored. A link has an expiry (at most 90 days) and can be revoked; expired answers 410, unknown or revoked 404. There is no login until v0.11.0, so everything else is open too: public links matter once access control exists, and the tests already pin that nothing under `/public` accepts a write or exposes queries.
- **Playlist rotation depends only on the clock** (`now / interval` modulo the list), so displays that ask at the same moment agree.

## Consequences

Dashboards are four more tables (`grepui_folders`, `grepui_dashboards`, `grepui_shares`, schema version 3), written through the same gate as every state write. The sanitiser is hand written; its allowlist is small and the frame limits the damage of a bug, but it is the first place to look at in a security review. Panel result sizes are capped (2 000 rows, 100 000 PromQL samples). Geo maps draw points on a plain grid because grepui loads no outside tiles.
