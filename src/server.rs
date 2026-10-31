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
    Ok(Arc::new(SqlxReader::lazy_with(
        &cfg.greptime_mysql,
        Duration::from_secs(cfg.query_timeout_secs),
        cfg.pool_max_connections,
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

/// Is `listen` (host:port) a loopback address?
pub fn is_loopback(listen: &str) -> bool {
    let host = listen
        .rsplit_once(':')
        .map_or(listen, |(h, _)| h)
        .trim_matches(['[', ']']);
    host == "localhost"
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

pub async fn serve(cfg: Config) -> Result<()> {
    cfg.validate()?;
    if cfg.no_auth && !is_loopback(&cfg.listen) {
        bail!(
            "--no-auth is only allowed on a loopback address (127.0.0.1, ::1, localhost); {} is reachable by others. Remove --no-auth to use sign-in.",
            cfg.listen
        );
    }
    let reader = reader_for(&cfg)?;
    let state = store::schema_state(reader.as_ref(), &cfg.meta_database).await;
    if let Some(warning) = startup_decision(&state)? {
        tracing::warn!("{warning}");
    }
    let listen = cfg.listen.clone();
    let app = api::AppState::with_parts(cfg, reader.clone(), reader, system_clock())?;
    // Orgs created by platform admins are loaded before the first request and kept fresh.
    if let Err(e) = api::access::reload_orgs(&app).await {
        tracing::warn!(error = %e.1, "could not load orgs yet");
    }
    api::access::spawn_org_refresh(app.clone());
    for spec in &app.cfg.sources {
        let (name, url) = crate::workload::parse_source(spec).map_err(anyhow::Error::msg)?;
        let r = SqlxReader::lazy_with(
            &url,
            Duration::from_secs(app.cfg.query_timeout_secs),
            app.cfg.pool_max_connections,
        )?;
        app.add_source(&name, Arc::new(r));
        tracing::info!(source = %name, "extra source added");
    }
    api::workload::spawn_refresh(app.clone());
    if app.cfg.no_auth {
        tracing::warn!(
            "sign-in is switched off (--no-auth): everyone is an admin and nothing is audited"
        );
    }
    // Redaction rules are loaded before the first request is served and kept fresh after.
    api::redaction::spawn_refresh(app.clone());
    if !app.cfg.no_alerts {
        crate::alerts::engine::spawn(app.clone(), app.sender.clone());
        api::reports::spawn(app.clone(), app.sender.clone());
    }
    if app.cfg.read_only_user {
        match crate::probe::check_read_only(app.writer.as_ref(), &app.cfg.database()?).await {
            Ok(crate::probe::Access::ReadOnly) => {
                tracing::info!("the database account cannot create tables here, as intended")
            }
            Ok(crate::probe::Access::CanWrite) => tracing::warn!(
                "--read-only-user is set but the database account can create tables: give grepui an account without write rights"
            ),
            Ok(crate::probe::Access::Left(m)) => tracing::warn!("{m}"),
            Err(e) => tracing::warn!(error = %e, "could not run the read-only check"),
        }
    }
    let listener = tokio::net::TcpListener::bind(&listen).await?;
    tracing::info!(version = crate::VERSION, %listen, "grepui listening");
    let grace = Duration::from_secs(app.cfg.shutdown_grace_secs);
    run(listener, api::router(app), shutdown(), grace).await
}

/// Serve until `stop` completes, then let running requests finish for at most `grace` (R-O3).
pub async fn run(
    listener: tokio::net::TcpListener,
    app: axum::Router,
    stop: impl std::future::Future<Output = ()> + Send + 'static,
    grace: Duration,
) -> Result<()> {
    use std::future::IntoFuture;
    let (tx, mut rx) = tokio::sync::watch::channel(false);
    let mut late = rx.clone();
    tokio::spawn(async move {
        stop.await;
        let _ = tx.send(true);
    });
    let serving = axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            let _ = rx.wait_for(|v| *v).await;
            tracing::info!("shutting down: letting running requests finish");
        })
        .into_future();
    tokio::select! {
        r = serving => r?,
        _ = async {
            let _ = late.wait_for(|v| *v).await;
            tokio::time::sleep(grace).await;
        } => tracing::warn!("grace period over: closing with requests still running"),
    }
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

/// Ctrl-C, or SIGTERM (what a container runtime sends).
async fn shutdown() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        if let Ok(mut term) = signal(SignalKind::terminate()) {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = term.recv() => {}
            }
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}
