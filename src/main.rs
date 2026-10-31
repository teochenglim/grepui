use anyhow::Result;
use clap::{Parser, Subcommand};
use grepui::{VERSION, config::Config, playground, probe, server};

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
    /// Create or upgrade grepui's own tables (saved views, history) in GreptimeDB. Idempotent.
    Migrate {
        #[command(flatten)]
        cfg: Box<Config>,
        /// Print the SQL and exit; no database connection.
        #[arg(long)]
        print: bool,
        /// Keep retrying for up to this many seconds while GreptimeDB starts.
        #[arg(long, default_value_t = 0)]
        wait: u64,
    },
    /// Wait for GreptimeDB, optionally seed demo data into a `demo` database, then serve.
    Playground {
        #[command(flatten)]
        cfg: Box<Config>,
        /// GreptimeDB HTTP address this process writes demo data to (defaults to --greptime-http-public).
        #[arg(long, env = "GREPTIME_HTTP")]
        greptime_http: Option<String>,
        /// Keep retrying for up to this many seconds while GreptimeDB starts.
        #[arg(long, default_value_t = 0)]
        wait: u64,
        /// Seed demo traces and logs. Writes only to a database named demo or demo_*, and only when it is empty.
        #[arg(long)]
        demo_data: bool,
        #[arg(long, default_value_t = 300)]
        traces: usize,
        /// Spread the seeded traces over this many minutes.
        #[arg(long, default_value_t = 120)]
        minutes: u64,
        #[arg(long, default_value_t = 7)]
        seed: u64,
        /// Do not keep sending a trickle of new demo traffic after seeding.
        #[arg(long)]
        no_live: bool,
        /// Seed even if the database already has traces (doubles the data).
        #[arg(long)]
        force: bool,
        /// Also seed OpenInference LLM traces into a trace v2 table named llm_traces (needs
        /// GreptimeDB v1.3.0+). Create an org with that traces table to browse them.
        #[arg(long)]
        llm_data: bool,
    },
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
        Cmd::Migrate { cfg, print, wait } => server::migrate(*cfg, print, wait).await,
        Cmd::Playground {
            cfg,
            greptime_http,
            wait,
            demo_data,
            traces,
            minutes,
            seed,
            no_live,
            force,
            llm_data,
        } => {
            let http = greptime_http.unwrap_or_else(|| cfg.greptime_http_public.clone());
            let opts = playground::Opts {
                http,
                wait_secs: wait,
                demo_data,
                traces,
                minutes,
                seed,
                live: !no_live,
                force,
                llm_data,
            };
            playground::prepare(&cfg, &opts).await?;
            server::serve(*cfg).await
        }
        Cmd::Healthcheck { addr } => {
            // 0.0.0.0 is a listen address, not somewhere to connect to.
            let addr = addr.replace("0.0.0.0", "127.0.0.1");
            std::process::exit(if probe::healthy(&addr).await { 0 } else { 1 });
        }
    }
}
