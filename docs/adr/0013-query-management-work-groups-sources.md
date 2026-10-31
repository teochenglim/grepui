# 0013 Query management: work groups, running queries, statistics and sources

Status: Accepted (v0.12.0).

## Context

With sign-in (ADR 0012) grepui knows who runs a query. v0.12.0 has to let an admin see what is running, stop it, limit what each role may ask for, rank the expensive queries, and read from more than one GreptimeDB without every handler learning about it.

## Decision

- **One wrapper around every reader.** `workload::Governed` wraps the reader (and each extra source). The request layer (`api::authz`) sets a task-local `Ctx` (user, org, role) around the handler, so any read made for a caller is governed without handler code. Reads with no context (the alert engine, startup), reads of grepui's own `grepui_*` tables, the process list and the health probe pass straight through.
- **Work groups** are per org and per role (`viewer`, `editor`, `admin`): `max_concurrent`, `timeout_secs`, `max_rows`, `max_window_hours`, 0 meaning no limit. They are kind `workgroup` rows of `grepui_alert_items` (key = role), set by org admins, mirrored in memory and reloaded every 5 s. The window is checked by the request layer before anything runs (`from`/`to` of a GET); concurrency is checked before the statement is sent; the timeout stops the read; rows are read with a cap of one more than the limit and an overrun is an error. Every error names the limit and the role (`work group limit max_concurrent (1) for viewers in this org ...`, status 429).
- **Running queries** are grepui's own registry (id, user, org, tag, statement, start), not a guess from the database: attribution is exact. GreptimeDB's `information_schema.process_list` strips comments and rewrites the text, so a statement is tied to its process by a normalised comparison of its text (`workload::normalize`); the match gives the process id shown as `greptime_id`. Rows grepui did not start (other clients, other replicas) are listed to platform admins only, because their text can belong to another org.
- **Cancel** is by grepui id. The waiting request ends at once (a `Notify` races the read), and when the process is known the database is asked to stop with `KILL '<id>'`. `KILL` cannot be prepared, so `Reader::kill` accepts only digits, `.`, `:` and `/` in the id, which comes from the process list. Editors may cancel their own queries; admins any query of the org. The route is a write route, so it is audited like any mutation.
- **Statistics** are in memory per process (count, errors, total and max ms per org, user and tag), bounded at 5000 keys, read by org admins (`/queries/stats`). They reset on restart and cover one replica; the audit trail is the durable record.
- **Sources.** `--source name=mysql://...` (repeatable) adds GreptimeDB servers; `primary` is the one `--greptime-mysql` names and cannot be disabled. Org databases are expected to exist on each source. A source has a state row (`source` kind under the platform pseudo-org): `enabled` and `min_role`. Logs search and the trace list take `sources=all` or `sources=a,b`; the handler runs the same statement on each permitted source concurrently, tags rows with `_source`, merges by time (`timestamp`, `start_ms`) newest first, applies the row limit to the merge and returns a `sources` array of per-source outcome. A failed source is reported with a generic message, not fatal; only when all fail is the request an error. Offsets cannot be combined with `sources`. Extra sources are governed like the primary.

## Consequences

Quotas are per org and role, not per user or per stream. Statistics are not shared between replicas and not persisted. Attribution of a process depends on text equality, so two identical statements running at once are matched in start order. Federation covers two list/search endpoints, not every route; histograms and field lists stay on the primary. Source URLs live in flags (they hold credentials), their on/off and minimum role in the store.
