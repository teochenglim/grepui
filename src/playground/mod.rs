//! Playground (ADR 0005): `grepui playground --demo-data` waits for GreptimeDB, seeds demo
//! traces and logs when the demo database is empty, then serves. greprum has the same idea.
//!
//! This is the only code in the binary that writes telemetry, and it is fenced three ways:
//! it runs only from the `playground` command (never `serve`), only when `--demo-data` is
//! given, only against a database named `demo` or `demo_*`, and only when that database has
//! no traces yet (append-mode tables reject DELETE, so a re-seed would double every count).

pub use crate::http;
pub mod proto;
pub mod synth;

use crate::{
    config::Config,
    query::{Q, Reader, SqlxReader, ident, qualified},
};
use anyhow::{Result, bail};
use std::{sync::Arc, time::Duration};

/// Databases the playground may write to.
pub fn is_demo_database(db: &str) -> bool {
    db == "demo"
        || db
            .strip_prefix("demo_")
            .is_some_and(|rest| !rest.is_empty())
}

/// The seeding decision: `Ok(true)` seed, `Ok(false)` skip, `Err` refuse.
pub fn plan_seed(db: &str, already_has_data: bool, force: bool) -> Result<bool> {
    if !is_demo_database(db) {
        bail!(
            "--demo-data only writes to a database named demo or demo_*, but this org uses {db:?}; refusing to add synthetic rows"
        );
    }
    Ok(force || !already_has_data)
}

/// Does the demo database already hold traces? A missing table counts as empty.
pub async fn has_traces(reader: &dyn Reader, db: &str, table: &str) -> bool {
    let Ok(t) = qualified(db, table) else {
        return false;
    };
    let q = Q::new(
        "playground_seeded",
        format!("SELECT CAST(count(*) AS BIGINT) AS n FROM {t}"),
        vec![],
    );
    reader
        .fetch(&q)
        .await
        .ok()
        .and_then(|r| r.first().and_then(|r| r["n"].as_i64()))
        .unwrap_or(0)
        > 0
}

#[derive(Debug, Clone)]
pub struct Opts {
    /// Where the playground writes: GreptimeDB's HTTP address as reachable from this process.
    pub http: String,
    pub wait_secs: u64,
    pub demo_data: bool,
    pub traces: usize,
    pub minutes: u64,
    pub seed: u64,
    pub live: bool,
    pub force: bool,
    /// Also seed OpenInference LLM traces into a trace v2 table called `llm_traces`.
    pub llm_data: bool,
}

fn now_ns() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

/// Send a batch to GreptimeDB's OTLP endpoints, into `db`.
pub async fn send(
    http: &str,
    db: &str,
    traces_table: &str,
    logs_table: &str,
    b: &synth::Batch,
) -> Result<()> {
    let (code, body) = http::post(
        http,
        "/v1/otlp/v1/traces",
        &[
            ("content-type", "application/x-protobuf"),
            ("x-greptime-db-name", db),
            ("x-greptime-pipeline-name", "greptime_trace_v1"),
            ("x-greptime-trace-table-name", traces_table),
        ],
        &proto::encode_traces(b),
    )
    .await?;
    if code != 200 {
        bail!(
            "GreptimeDB rejected the demo traces (HTTP {code}): {}",
            body.chars().take(200).collect::<String>()
        );
    }
    let (code, body) = http::post(
        http,
        "/v1/otlp/v1/logs",
        &[
            ("content-type", "application/x-protobuf"),
            ("x-greptime-db-name", db),
            ("x-greptime-log-table-name", logs_table),
        ],
        &proto::encode_logs(b),
    )
    .await?;
    if code != 200 {
        bail!(
            "GreptimeDB rejected the demo logs (HTTP {code}): {}",
            body.chars().take(200).collect::<String>()
        );
    }
    Ok(())
}

/// Send spans to a **trace v2** table (`greptime_trace_v2`, JSON2 attribute columns; needs
/// GreptimeDB v1.3.0 or newer), with an OpenInference project name on the resource.
pub async fn send_llm_traces(
    http: &str,
    db: &str,
    table: &str,
    project: &str,
    b: &synth::Batch,
) -> Result<()> {
    let (code, body) = http::post(
        http,
        "/v1/otlp/v1/traces",
        &[
            ("content-type", "application/x-protobuf"),
            ("x-greptime-db-name", db),
            ("x-greptime-pipeline-name", "greptime_trace_v2"),
            ("x-greptime-trace-table-name", table),
        ],
        &proto::encode_spans(
            b,
            &[(
                "openinference.project.name",
                synth::AttrVal::Str(project.into()),
            )],
        ),
    )
    .await?;
    if code != 200 {
        bail!(
            "GreptimeDB rejected the LLM traces (HTTP {code}): {}",
            body.chars().take(300).collect::<String>()
        );
    }
    Ok(())
}

/// Send demo metric points to GreptimeDB's OTLP metrics endpoint, into `db`.
pub async fn send_metrics(http: &str, db: &str, points: &[synth::MetricPoint]) -> Result<()> {
    if points.is_empty() {
        return Ok(());
    }
    let (code, body) = http::post(
        http,
        "/v1/otlp/v1/metrics",
        &[
            ("content-type", "application/x-protobuf"),
            ("x-greptime-db-name", db),
        ],
        &proto::encode_metrics(points),
    )
    .await?;
    if code != 200 {
        bail!(
            "GreptimeDB rejected the demo metrics (HTTP {code}): {}",
            body.chars().take(200).collect::<String>()
        );
    }
    Ok(())
}

/// The dashboard the playground adds to an empty state store (it is plain JSON, validated like
/// any import, and a test keeps it valid).
pub const DEMO_DASHBOARD: &str = include_str!("demo_dashboard.json");

/// Create the demo dashboard if the org has none. State-store writes only, never telemetry.
pub async fn seed_demo_dashboard(reader: &SqlxReader, meta_db: &str, org: &str) -> Result<()> {
    use crate::store::{build, check_write};
    let (rows, _) = crate::query::run(reader, build::dash_list(meta_db, org, None, None)?).await?;
    if !rows.is_empty() {
        return Ok(());
    }
    let spec = crate::dash::canonical(&serde_json::from_str(DEMO_DASHBOARD)?)
        .map_err(|e| anyhow::anyhow!("demo dashboard: {}", e.1))?;
    let now = (now_ns() / 1_000_000) as i64;
    let row = build::DashRow {
        dash_id: format!("{:016x}", rand::random::<u64>()),
        title: "Demo overview".into(),
        kind: "dashboard".into(),
        spec: spec.to_string(),
        owner: crate::store::LOCAL_USER.into(),
        created_ms: now,
        updated_ms: now,
        ..Default::default()
    };
    let q = build::dash_put(meta_db, org, &row)?;
    check_write(&q, meta_db)?;
    crate::query::Writer::exec(reader, &q)
        .await
        .map_err(|e| anyhow::anyhow!("could not create the demo dashboard: {}", e.detail()))?;
    Ok(())
}

/// The alert the playground adds: it counts error logs per service over the last 10 minutes and
/// has no destination, so it only shows in the list, the history and as an incident.
pub const DEMO_ALERT: &str = r#"{
  "name": "Error logs per service", "kind": "scheduled", "folder": "demo", "open_incident": true,
  "description": "More than 3 ERROR logs from one service in 10 minutes",
  "query": {"kind": "sql", "text": "SELECT json_get_string(resource_attributes, '[\"service.name\"]') AS service_name, count(*) AS n FROM opentelemetry_logs WHERE severity_text = 'ERROR' AND timestamp >= $__from AND timestamp < $__to GROUP BY 1 ORDER BY 1"},
  "condition": {"op": ">", "threshold": 3}, "lookback_s": 600, "schedule": {"every_s": 30},
  "dedup": {"fields": ["service_name"], "window_s": 900}
}"#;

/// Create the demo alert if the org has no alerts. State-store writes only.
pub async fn seed_demo_alert(reader: &SqlxReader, meta_db: &str, org: &str) -> Result<()> {
    use crate::store::{build, check_write};
    let (rows, _) = crate::query::run(reader, build::item_list(meta_db, org, "alert")?).await?;
    if !rows.is_empty() {
        return Ok(());
    }
    let rule = crate::alerts::parse_rule(&serde_json::from_str(DEMO_ALERT)?)
        .map_err(|e| anyhow::anyhow!("demo alert: {}", e.1))?;
    let now = (now_ns() / 1_000_000) as i64;
    let row = build::ItemRow {
        item_id: format!("{:016x}", rand::random::<u64>()),
        name: rule.name.clone(),
        spec: serde_json::to_string(&rule)?,
        owner: crate::store::LOCAL_USER.into(),
        created_ms: now,
        updated_ms: now,
        deleted: false,
    };
    let q = build::item_put(meta_db, org, "alert", &row)?;
    check_write(&q, meta_db)?;
    crate::query::Writer::exec(reader, &q)
        .await
        .map_err(|e| anyhow::anyhow!("could not create the demo alert: {}", e.detail()))?;
    Ok(())
}

/// A saved function for the playground: spans slower than a number of milliseconds.
pub const DEMO_FUNCTION: &str = r#"{"name": "slower_than", "description": "Spans slower than the given milliseconds", "params": ["ms"], "body": "((CAST(`timestamp_end` AS BIGINT) - CAST(`timestamp` AS BIGINT)) / 1000000 > $1)"}"#;

/// Create the demo function if the org has none. State-store writes only.
pub async fn seed_demo_function(reader: &SqlxReader, meta_db: &str, org: &str) -> Result<()> {
    use crate::store::{build, check_write};
    let (rows, _) = crate::query::run(reader, build::item_list(meta_db, org, "function")?).await?;
    if !rows.is_empty() {
        return Ok(());
    }
    let f = serde_json::from_str::<crate::functions::Function>(DEMO_FUNCTION)?
        .canonical()
        .map_err(|e| anyhow::anyhow!("demo function: {}", e.1))?;
    let now = (now_ns() / 1_000_000) as i64;
    let row = build::ItemRow {
        item_id: f.name.clone(),
        name: f.name.clone(),
        spec: serde_json::to_string(&f)?,
        owner: crate::store::LOCAL_USER.into(),
        created_ms: now,
        updated_ms: now,
        deleted: false,
    };
    let q = build::item_put(meta_db, org, "function", &row)?;
    check_write(&q, meta_db)?;
    crate::query::Writer::exec(reader, &q)
        .await
        .map_err(|e| anyhow::anyhow!("could not create the demo function: {}", e.detail()))?;
    Ok(())
}

/// The demo sign-in: user `admin`, password from `GREPUI_DEMO_ADMIN_PASSWORD` or a fixed demo one.
/// Only the playground creates it, and only when there are no users yet.
pub async fn seed_demo_admin(reader: &SqlxReader, meta_db: &str) -> Result<()> {
    use crate::store::{build, check_write};
    let (rows, _) = crate::query::run(
        reader,
        build::item_list(meta_db, build::PLATFORM_ORG, "user")?,
    )
    .await?;
    if !rows.is_empty() {
        return Ok(());
    }
    let pw =
        std::env::var("GREPUI_DEMO_ADMIN_PASSWORD").unwrap_or_else(|_| "grepui-demo-admin".into());
    let u = crate::auth::User {
        username: "admin".into(),
        display: "demo admin".into(),
        password_hash: crate::auth::password::hash(&pw).map_err(|e| anyhow::anyhow!(e))?,
        platform_admin: true,
        source: "local".into(),
        ..Default::default()
    };
    let now = (now_ns() / 1_000_000) as i64;
    let row = build::ItemRow {
        item_id: "admin".into(),
        name: "admin".into(),
        spec: serde_json::to_string(&u)?,
        owner: "platform".into(),
        created_ms: now,
        updated_ms: now,
        deleted: false,
    };
    let q = build::item_put(meta_db, build::PLATFORM_ORG, "user", &row)?;
    check_write(&q, meta_db)?;
    crate::query::Writer::exec(reader, &q)
        .await
        .map_err(|e| anyhow::anyhow!("could not create the demo admin: {}", e.detail()))?;
    tracing::info!("playground: sign in as admin / {pw} (demo only)");
    Ok(())
}

/// Everything before serving: wait, guard, seed, start the live trickle.
pub async fn prepare(cfg: &Config, o: &Opts) -> Result<()> {
    cfg.validate()?;
    // Connect to `public`, which always exists: the demo database may not exist yet.
    let reader = Arc::new(SqlxReader::lazy(
        &crate::config::admin_url(&cfg.greptime_mysql)?,
        Duration::from_secs(cfg.query_timeout_secs),
    )?);
    crate::query::wait_for_db(reader.as_ref(), o.wait_secs).await?;
    // The playground owns its stack, so it also creates the state store (saved views, history).
    crate::store::migrate(reader.as_ref(), reader.as_ref(), &cfg.meta_database).await?;
    if !o.demo_data {
        return Ok(());
    }
    if let Err(e) =
        seed_demo_dashboard(reader.as_ref(), &cfg.meta_database, crate::org::DEFAULT_ORG).await
    {
        tracing::warn!(error = %e, "playground: no demo dashboard");
    }
    if !cfg.no_auth
        && let Err(e) = seed_demo_admin(reader.as_ref(), &cfg.meta_database).await
    {
        tracing::warn!(error = %e, "playground: no demo admin");
    }
    if let Err(e) =
        seed_demo_alert(reader.as_ref(), &cfg.meta_database, crate::org::DEFAULT_ORG).await
    {
        tracing::warn!(error = %e, "playground: no demo alert");
    }
    if let Err(e) =
        seed_demo_function(reader.as_ref(), &cfg.meta_database, crate::org::DEFAULT_ORG).await
    {
        tracing::warn!(error = %e, "playground: no demo function");
    }
    let db = cfg.database()?;
    let present = has_traces(reader.as_ref(), &db, &cfg.traces_table).await;
    if !plan_seed(&db, present, o.force)? {
        tracing::info!(%db, "playground: demo data already present, not seeding");
    } else {
        // The database must exist before OTLP can create tables in it.
        ident(&db)?;
        let (code, _) = http::post(
            &o.http,
            "/v1/sql",
            &[("content-type", "application/x-www-form-urlencoded")],
            format!("sql=CREATE DATABASE IF NOT EXISTS {db}").as_bytes(),
        )
        .await?;
        if code != 200 {
            bail!("could not create the demo database (HTTP {code})");
        }
        let batch = synth::generate(o.seed, o.traces, now_ns(), o.minutes);
        tracing::info!(%db, traces = o.traces, spans = batch.spans.len(), logs = batch.logs.len(), "playground: seeding demo data");
        send(&o.http, &db, &cfg.traces_table, &cfg.logs_table, &batch).await?;
        let to = now_ns();
        let from = to.saturating_sub(o.minutes * 60 * 1_000_000_000);
        send_metrics(&o.http, &db, &synth::generate_metrics(from, to)).await?;
    }
    if o.llm_data {
        let db = cfg.database()?;
        if has_traces(reader.as_ref(), &db, "llm_traces").await {
            tracing::info!(%db, "playground: LLM traces already present, not seeding");
        } else {
            ident(&db)?;
            let b = synth::generate_llm(o.seed, 80, now_ns(), o.minutes);
            tracing::info!(%db, spans = b.spans.len(), "playground: seeding LLM traces into llm_traces (trace v2)");
            send_llm_traces(&o.http, &db, "llm_traces", synth::LLM_PROJECT, &b).await?;
        }
    }
    if o.live {
        let (http, db, tt, lt, seed) = (
            o.http.clone(),
            db,
            cfg.traces_table.clone(),
            cfg.logs_table.clone(),
            o.seed,
        );
        tokio::spawn(async move {
            let mut n = 0u64;
            loop {
                tokio::time::sleep(Duration::from_secs(5)).await;
                n += 1;
                let b = synth::generate(seed.wrapping_add(n), 2, now_ns(), 1);
                if let Err(e) = send(&http, &db, &tt, &lt, &b).await {
                    tracing::warn!(error = %e, "playground: live demo traffic failed");
                }
                let to = now_ns();
                let pts = synth::generate_metrics(to.saturating_sub(5_000_000_000), to);
                if let Err(e) = send_metrics(&http, &db, &pts).await {
                    tracing::warn!(error = %e, "playground: live demo metrics failed");
                }
            }
        });
    }
    Ok(())
}
