//! Process wiring: build the state, bind, serve, drain on shutdown.

use crate::{api, config::Config, query::SqlxReader};
use std::{sync::Arc, time::Duration};

pub async fn serve(cfg: Config) -> anyhow::Result<()> {
    cfg.validate()?;
    let reader = Arc::new(SqlxReader::lazy(
        &cfg.greptime_mysql,
        Duration::from_secs(cfg.query_timeout_secs),
    )?);
    let listen = cfg.listen.clone();
    let state = api::AppState::new(cfg, reader)?;
    let listener = tokio::net::TcpListener::bind(&listen).await?;
    tracing::info!(version = crate::VERSION, %listen, "grepui listening");
    axum::serve(listener, api::router(state))
        .with_graceful_shutdown(shutdown())
        .await?;
    Ok(())
}

async fn shutdown() {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("shutting down");
}
