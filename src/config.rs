//! Flags and environment. Defaults work against a local GreptimeDB (R-F1).

use crate::query::ident;
use clap::Args;

#[derive(Debug, Clone, Args)]
pub struct Config {
    /// Address to listen on.
    #[arg(long, env = "GREPUI_LISTEN", default_value = "127.0.0.1:8080")]
    pub listen: String,
    /// GreptimeDB MySQL-protocol URL; the path is the default org's database.
    #[arg(
        long,
        env = "GREPTIME_MYSQL",
        default_value = "mysql://root@127.0.0.1:4002/public"
    )]
    pub greptime_mysql: String,
    /// GreptimeDB HTTP address as clients (collectors, SDKs) reach it; shown on the Connect page.
    #[arg(
        long,
        env = "GREPTIME_HTTP_PUBLIC",
        default_value = "http://127.0.0.1:4000"
    )]
    pub greptime_http_public: String,
    /// Traces table of the default org.
    #[arg(
        long,
        env = "GREPUI_TRACES_TABLE",
        default_value = "opentelemetry_traces"
    )]
    pub traces_table: String,
    /// Logs table of the default org.
    #[arg(long, env = "GREPUI_LOGS_TABLE", default_value = "opentelemetry_logs")]
    pub logs_table: String,
    /// Database holding grepui's own state (saved views, history); created by `grepui migrate`.
    #[arg(long, env = "GREPUI_META_DATABASE", default_value = "grepui")]
    pub meta_database: String,
    /// Per-query timeout in seconds.
    #[arg(long, env = "GREPUI_QUERY_TIMEOUT_SECS", default_value_t = 30)]
    pub query_timeout_secs: u64,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            listen: "127.0.0.1:8080".into(),
            greptime_mysql: "mysql://root@127.0.0.1:4002/public".into(),
            greptime_http_public: "http://127.0.0.1:4000".into(),
            traces_table: "opentelemetry_traces".into(),
            logs_table: "opentelemetry_logs".into(),
            meta_database: "grepui".into(),
            query_timeout_secs: 30,
        }
    }
}

impl Config {
    /// Database name from the MySQL URL path (`public` when absent).
    pub fn database(&self) -> anyhow::Result<String> {
        let u = url::Url::parse(&self.greptime_mysql)
            .map_err(|_| anyhow::anyhow!("--greptime-mysql is not a valid URL"))?;
        let db = u.path().trim_start_matches('/');
        let db = if db.is_empty() { "public" } else { db };
        ident(db)?;
        Ok(db.to_string())
    }

    /// Reject anything that would later be placed in SQL text unvalidated.
    pub fn validate(&self) -> anyhow::Result<()> {
        ident(&self.traces_table).map_err(|_| {
            anyhow::anyhow!(
                "--traces-table {:?} is not a valid identifier",
                self.traces_table
            )
        })?;
        ident(&self.logs_table).map_err(|_| {
            anyhow::anyhow!(
                "--logs-table {:?} is not a valid identifier",
                self.logs_table
            )
        })?;
        self.database()?;
        ident(&self.meta_database).map_err(|_| {
            anyhow::anyhow!(
                "--meta-database {:?} is not a valid identifier",
                self.meta_database
            )
        })?;
        if self.query_timeout_secs == 0 || self.query_timeout_secs > 600 {
            anyhow::bail!("--query-timeout-secs must be between 1 and 600");
        }
        Ok(())
    }
}

/// The same server with `/public` as the database: it always exists, so it works before the
/// org or metadata database has been created (migrate, playground).
pub fn admin_url(mysql: &str) -> anyhow::Result<String> {
    let mut u = url::Url::parse(mysql)?;
    u.set_path("/public");
    Ok(u.to_string())
}
