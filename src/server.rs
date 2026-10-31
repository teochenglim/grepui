//! Process wiring: build the state, check the metadata schema, bind, serve, drain on shutdown.

use crate::{
    api::{self, system_clock},
    config::Config,
    query::SqlxReader,
    store::{self, SchemaState},
};
use anyhow::{Result, bail};
use std::{sync::Arc, time::Duration};

fn reader_for(cfg: &Config) -> Result<Arc<SqlxReader>> {
    Ok(Arc::new(SqlxReader::lazy(
        &cfg.greptime_mysql,
        Duration::from_secs(cfg.query_timeout_secs),
    )?))
}

/// What to do about the metadata schema at startup (R-S1). `Err` stops the process.
pub fn startup_decision(state: &Result<SchemaState>) -> Result<Option<String>> {
    Ok(match state {
        Ok(SchemaState::Newer(v)) => bail!(
            "the metadata schema is version {v}, newer than this binary ({}); upgrade grepui",
            store::SCHEMA_VERSION
        ),
        Ok(SchemaState::Current) => None,
        Ok(SchemaState::Missing) => Some("the state store is not initialised: run `grepui migrate` (saved views and history are unavailable until then)".into()),
        Ok(SchemaState::Older(v)) => Some(format!("the metadata schema is version {v}, this binary expects {}: run `grepui migrate`", store::SCHEMA_VERSION)),
        // The database may simply not be up yet: `serve` must still come up (/healthz is DB-free).
        Err(e) => Some(format!("could not check the metadata schema yet: {e}")),
    })
}

pub async fn serve(cfg: Config) -> Result<()> {
    cfg.validate()?;
    let reader = reader_for(&cfg)?;
    let state = store::schema_state(reader.as_ref(), &cfg.meta_database).await;
    if let Some(warning) = startup_decision(&state)? {
        tracing::warn!("{warning}");
    }
    let listen = cfg.listen.clone();
    let app = api::AppState::with_parts(cfg, reader.clone(), reader, system_clock())?;
    // Redaction rules are loaded before the first request is served and kept fresh after.
    api::redaction::spawn_refresh(app.clone());
    if !app.cfg.no_alerts {
        crate::alerts::engine::spawn(app.clone(), app.sender.clone());
    }
    let listener = tokio::net::TcpListener::bind(&listen).await?;
    tracing::info!(version = crate::VERSION, %listen, "grepui listening");
    axum::serve(listener, api::router(app))
        .with_graceful_shutdown(shutdown())
        .await?;
    Ok(())
}

/// `grepui migrate`: create or upgrade the metadata database and tables (idempotent).
pub async fn migrate(cfg: Config, print: bool, wait_secs: u64) -> Result<()> {
    cfg.validate()?;
    if print {
        for s in store::migration_statements(&cfg.meta_database)? {
            println!("{s};");
        }
        return Ok(());
    }
    // Connect through `public`: the metadata database may not exist yet.
    let url = crate::config::admin_url(&cfg.greptime_mysql)?;
    let reader = Arc::new(SqlxReader::lazy(
        &url,
        Duration::from_secs(cfg.query_timeout_secs),
    )?);
    crate::query::wait_for_db(reader.as_ref(), wait_secs).await?;
    let done = store::migrate(reader.as_ref(), reader.as_ref(), &cfg.meta_database).await?;
    println!(
        "metadata schema v{} ready in database {} ({} statements)",
        store::SCHEMA_VERSION,
        cfg.meta_database,
        done.len()
    );
    Ok(())
}

async fn shutdown() {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("shutting down");
}
