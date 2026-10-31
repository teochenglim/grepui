# grepui

**See your logs, traces, metrics and LLM calls in one place, on top of [GreptimeDB](https://greptime.com).**

grepui is a single program with a web page built in. Point it at a GreptimeDB that your applications already send data to, and open your browser. There is nothing else to install: no Node, no second database, no build step. It is free software under the Apache-2.0 licence.

![Logs: search, histogram, fields and records](images/ui_logs.png)

## What you get

| | |
|---|---|
| **Logs** | Search with a simple filter bar (`level=error "upstream timeout"`), see hits over time, open any record, look at what happened just before and after, export CSV or JSON. |
| **Traces** | One row per trace, a latency scatter, a waterfall of every step, a flame graph, and a jump from any span to its logs. |
| **Services** | Rate, errors and latency per service, a service map, and a catalog with owners and runbook links. |
| **Metrics** | PromQL charts you can share by link. |
| **Dashboards** | Build panels, use variables, share a public link, show them on a TV. |
| **Alerts** | Rules over SQL, PromQL or a builder, with history, incidents, webhooks and email. |
| **LLM observability** | Read OpenInference and OpenTelemetry GenAI traces: the conversation, tool calls, tokens, cost, sessions, an agent graph and alerts on cost or errors. Prompts are hidden from people who should not read them. |
| **For teams** | Orgs, roles, single sign-on, API tokens, an audit trail, query limits, scheduled email reports. |

Every chart has a **Show SQL** button. grepui never hides what it asked the database.

## Try it in two minutes

You need Docker and this repository.

```sh
make db-up        # starts GreptimeDB v1.3.0
make playground   # fills a `demo` database with demo data and opens grepui on http://127.0.0.1:8080
```

Sign in as `admin` with the password `grepui-demo-admin`. Prefer one command? `make demo` runs both in Docker.

## A quick tour

**Follow a request.** Find a slow or failing trace, open its waterfall, and click a span to see its attributes and its logs.

![A trace waterfall with the span panel](images/ui_trace_detail.png)

**See how your services fit together.**

![The service map](images/ui_service_map.png)

**Keep what matters on a dashboard.**

![A dashboard](images/ui_dashboard.png)

**Be told when something is wrong.**

![The alert editor with a live preview](images/ui_alert_editor.png)

**Understand your LLM application.** Calls, tokens, cost and errors by model, then sessions turn by turn.

![The LLM overview](images/ui_llm_overview.png)

![An LLM span: conversation, tool call, tokens and cost](images/ui_trace_llm_span.png)

![The agent graph of one trace](images/ui_agent_graph.png)

**Run it for a whole company.** Orgs, roles, API tokens, work groups and an audit trail.

![Administration: members and roles](images/ui_admin_members.png)

![Data health: is my data arriving?](images/ui_health.png)

## Use it with your own data

1. Send telemetry to GreptimeDB with your usual collector. The **Connect** page in grepui shows copy-and-paste snippets for OTLP and Prometheus.

   ![The Connect page](images/ui_connect.png)

2. Create grepui's own tables once, then start it:

   ```sh
   grepui migrate --greptime-mysql mysql://user:pass@greptime:4002/public
   grepui serve   --greptime-mysql mysql://user:pass@greptime:4002/public
   ```

3. Open the address. The first visit asks you to create the administrator.

Or with Docker:

```sh
docker run -p 8080:8080 -e GREPUI_LISTEN=0.0.0.0:8080 \
  -e GREPTIME_MYSQL=mysql://user:pass@greptime:4002/public \
  ghcr.io/teochenglim/grepui:latest
```

Needs **GreptimeDB v1.3.0 or newer**. grepui warns at start-up if yours is older.

## Documentation

| Read this | If you want to |
|---|---|
| [User Guide](docs/guide/user-guide.adoc) | learn every page: searching, traces, dashboards, alerts, LLM views, administration |
| [Technical Guide](docs/guide/technical-guide.adoc) | install, configure, secure and run grepui; every flag; single sign-on; the API |
| [Architecture](ARCH/README.md) and [decisions](docs/adr/README.md) | understand why it is built this way |
| [Release log](RELEASE.md) | see what each version added and what it does not do yet |

The guides are written in AsciiDoc. GitHub shows them as web pages. To make HTML or PDF yourself: `asciidoctor docs/guide/user-guide.adoc` (or `asciidoctor-pdf`).

## Safe by design

* **Read-only on your telemetry.** grepui reads your tables with fixed, parameterised queries. Its own data lives in `grepui_*` tables.
* **SQL mode is fenced.** One read-only statement, only in your org's database, never grepui's own tables.
* **Deny by default.** Every route has a minimum role; the API's shape is frozen by tests.
* **Secrets stay masked.** Redaction rules apply to every answer and export; LLM prompts are masked below admin.
* **Browser protections.** A strict Content-Security-Policy and other security headers on every response.

Details and the latest review: [Technical Guide](docs/guide/technical-guide.adoc#_security_model) and [docs/security-review-0.17.0.md](docs/security-review-0.17.0.md).

## Settings at a glance

| Flag | Environment | Default |
|---|---|---|
| `--listen` | `GREPUI_LISTEN` | `127.0.0.1:8080` |
| `--greptime-mysql` | `GREPTIME_MYSQL` | `mysql://root@127.0.0.1:4002/public` |
| `--greptime-http-public` | `GREPTIME_HTTP_PUBLIC` | `http://127.0.0.1:4000` |
| `--traces-table` / `--logs-table` | `GREPUI_TRACES_TABLE` / `GREPUI_LOGS_TABLE` | `opentelemetry_traces` / `opentelemetry_logs` |
| `--admin-writes` | `GREPUI_ADMIN_WRITES` | off (rollups and retention changes) |

The full list, with single sign-on, email, cache and sources, is in the [Technical Guide](docs/guide/technical-guide.adoc#_configuration).

## Develop

```sh
make check        # format, lint, requirement-to-test map, tests: what CI runs
make test-db      # live tests against GreptimeDB (make db-up first)
```

The UI is plain HTML, CSS and ES modules in [web/static/](web/static/), embedded in the binary. Contributor rules: [AGENTS.md](AGENTS.md). Only the repository owner commits, tags and releases (`make release VERSION=x.y.z`).

## Status

**v0.18.0.** Logs, traces, services, metrics, dashboards, alerts, pipelines, access control, query management, reports, RUM and LLM observability are built. [RELEASE.md](RELEASE.md) lists each release; [ARCH/07_parity_matrix.md](ARCH/07_parity_matrix.md) compares features honestly, gaps included. Version 1.0.0 is planned as the release where every comparison is backed by evidence ([RELEASE/v1.0.0.md](RELEASE/v1.0.0.md)).

## Licence

Apache-2.0. See [LICENSE](LICENSE) and [NOTICE](NOTICE).
