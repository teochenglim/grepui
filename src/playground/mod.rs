//! Playground (ADR 0005): `grepui playground --demo-data` waits for GreptimeDB, seeds demo
//! traces and logs when the demo database is empty, then serves. greprum has the same idea.
//!
//! This is the only code in the binary that writes telemetry, and it is fenced three ways:
//! it runs only from the `playground` command (never `serve`), only when `--demo-data` is
//! given, only against a database named `demo` or `demo_*`, and only when that database has
//! no traces yet (append-mode tables reject DELETE, so a re-seed would double every count).

pub mod http;
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
            }
        });
    }
    Ok(())
}
