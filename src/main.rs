use anyhow::Result;
use clap::{Parser, Subcommand};
use grepui::{VERSION, config::Config, probe, server};

#[derive(Parser)]
#[command(name = "grepui", version = VERSION, about = "Multi-tenant observability UI for GreptimeDB")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the UI and API.
    Serve(#[command(flatten)] Box<Config>),
    /// Exit 0 when a running grepui answers /healthz (container health check).
    Healthcheck {
        #[arg(long, env = "GREPUI_LISTEN", default_value = "127.0.0.1:8080")]
        addr: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    match Cli::parse().cmd {
        Cmd::Serve(cfg) => server::serve(*cfg).await,
        Cmd::Healthcheck { addr } => {
            // 0.0.0.0 is a listen address, not somewhere to connect to.
            let addr = addr.replace("0.0.0.0", "127.0.0.1");
            std::process::exit(if probe::healthy(&addr).await { 0 } else { 1 });
        }
    }
}
