# ARCH 02: Reference Products

grepui copies no code from any product. It studies interaction design and chooses what fits a stateless, read-only explorer on GreptimeDB.

> Source quality: Quickwit and all-in-one platform docs and READMEs, a DeepWiki summary of Quickwit's UI source, and Quickwit's "joins Datadog" post. **No project's UI source was read directly** (GitHub was not reachable), so UI details come from those summaries. Re-check before relying on one.

## Does Quickwit have a UI?

Yes, in two ways:

- **Embedded web UI** at `http://localhost:7280`, built into the Rust binary. Per the DeepWiki summary: React, TypeScript, Vite, Material UI and a Monaco query editor. Views: search (query editor, index sidebar with field types, results table), an index summary (document count, split states, storage size) and aggregation requests. Separate cluster or node pages could not be confirmed.
- **Traces through other tools.** Quickwit has no native trace UI. It ingests OTLP (gRPC, port 7281) and exposes a Jaeger-compatible gRPC storage API, so traces are viewed in Jaeger UI. A Grafana plugin covers logs and traces too.

## Side by side

| | Quickwit | All-in-one platforms (Parquet + DataFusion class) | grepui on GreptimeDB |
|---|---|---|---|
| Core idea | Search engine for logs and traces on object storage (tantivy) | One product for ingest, storage, UI, dashboards, alerts | A read-only explorer over a database that already ingests and stores |
| Signals in UI | Logs; traces via Jaeger | Logs, metrics, traces, RUM | Logs, traces, services (RED), tables; metrics next |
| Query language | Lucene-style plus aggregations | SQL, PromQL | Filter bar → SQL, raw SQL mode, PromQL |
| API | `GET/POST /api/v1/<index>/search` | `POST /api/{org}/_search` with SQL | Typed REST `/api/v1/*`, no SQL from the client |
| Trace view | Jaeger UI (external) | Built in | Built in: list, latency scatter, timeline, span detail |
| Long-range metrics | Not covered in docs read | Pre-aggregation inside the product | Flow rollup, chosen automatically by the server |
| Own state | Metastore | Metadata store | None (stateless) |
| Licence | AGPLv3 in the repo read; an Apache 2.0 release was promised after the Datadog deal (not checked) | AGPLv3 community edition | Apache-2.0 |

## Taken from Quickwit

- **Field sidebar with types**: each column's type next to its name, from `information_schema.columns`.
- **Index summary** → grepui **Tables** page: tables in the database, columns with role (tag, field, time index), row count, oldest and newest row; opens the logs or traces table in its own page.
- **Plain, search-first layout**: query bar and results above everything else.

## Not taken, and why

- **Monaco editor**: a large bundle and a build step. grepui's SQL mode is a textarea with history and "Show SQL"; revisit if SQL becomes the main way in.
- **Aggregation builder**: in SQL this is `GROUP BY`; a builder is a convenience layer.
- **Trace UI by delegation**: GreptimeDB also serves a Jaeger-compatible API (`/v1/jaeger`), so Jaeger UI already works against it. grepui's reason to exist is the integrated part: pivots between signals, automatic rollups, transparent SQL.
- **Dashboards, alerts, pipelines**: GreptimeDB has a Grafana datasource, Flow and pipelines. See ARCH 06 (non-goals).
