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
    /// Allow admin writes: rollup create/backfill/drop and retention (TTL) changes. Off by default;
    /// the stand-in for an admin role until access control (v0.11.0).
    #[arg(long, env = "GREPUI_ADMIN_WRITES")]
    pub admin_writes: bool,
    /// Do not run the alert scheduler in this process (a UI-only replica).
    #[arg(long, env = "GREPUI_NO_ALERTS")]
    pub no_alerts: bool,
    /// SMTP relay for alert email, host:port. Plain SMTP, no TLS and no login: a relay on a
    /// trusted network (ADR 0010). Without it, email destinations report an error.
    #[arg(long, env = "GREPUI_SMTP")]
    pub smtp: Option<String>,
    /// Sender address of alert email.
    #[arg(
        long,
        env = "GREPUI_SMTP_FROM",
        default_value = "grepui@localhost.localdomain"
    )]
    pub smtp_from: String,
    /// GreptimeDB HTTP address grepui itself calls for pipelines (default: --greptime-http-public).
    #[arg(long, env = "GREPUI_PIPELINE_HTTP")]
    pub pipeline_http: Option<String>,
    /// No sign-in: everyone is an admin. Local use only; refused on a non-loopback address.
    #[arg(long, env = "GREPUI_NO_AUTH")]
    pub no_auth: bool,
    /// How long a sign-in lasts, in hours.
    #[arg(long, env = "GREPUI_SESSION_HOURS", default_value_t = 12)]
    pub session_hours: u64,
    /// Requests per minute one API token may make.
    #[arg(long, env = "GREPUI_TOKEN_RATE", default_value_t = 600)]
    pub token_rate_per_min: u32,
    /// OpenID Connect: the issuer (`iss` of the ID tokens).
    #[arg(long, env = "GREPUI_OIDC_ISSUER")]
    pub oidc_issuer: Option<String>,
    #[arg(long, env = "GREPUI_OIDC_CLIENT_ID")]
    pub oidc_client_id: Option<String>,
    #[arg(long, env = "GREPUI_OIDC_CLIENT_SECRET")]
    pub oidc_client_secret: Option<String>,
    /// The provider's authorization endpoint.
    #[arg(long, env = "GREPUI_OIDC_AUTH_URL")]
    pub oidc_auth_url: Option<String>,
    /// The provider's token endpoint (http:// or https://).
    #[arg(long, env = "GREPUI_OIDC_TOKEN_URL")]
    pub oidc_token_url: Option<String>,
    /// The provider's JWKS endpoint.
    #[arg(long, env = "GREPUI_OIDC_JWKS_URL")]
    pub oidc_jwks_url: Option<String>,
    /// This grepui's callback, e.g. https://grepui.example.com/api/v1/auth/oidc/callback.
    #[arg(long, env = "GREPUI_OIDC_REDIRECT_URL")]
    pub oidc_redirect_url: Option<String>,
    #[arg(
        long,
        env = "GREPUI_OIDC_SCOPES",
        default_value = "openid profile email"
    )]
    pub oidc_scopes: String,
    /// The claim that carries groups or roles.
    #[arg(long, env = "GREPUI_OIDC_ROLE_CLAIM", default_value = "groups")]
    pub oidc_role_claim: String,
    /// `value=role@org` (viewer, editor, admin) or `value=platform_admin`; repeat or separate with `;`.
    #[arg(long, env = "GREPUI_OIDC_ROLE_MAP", value_delimiter = ';')]
    pub oidc_role_map: Vec<String>,
    /// Extra GreptimeDB sources as `name=mysql://...` (v0.12.0); repeat or separate with `;`.
    /// The org databases are expected to exist on each one.
    #[arg(long = "source", env = "GREPUI_SOURCES", value_delimiter = ';')]
    pub sources: Vec<String>,
    /// Per-query timeout in seconds.
    #[arg(long, env = "GREPUI_QUERY_TIMEOUT_SECS", default_value_t = 30)]
    pub query_timeout_secs: u64,
    /// Result cache lifetime in seconds; 0 turns the cache off (v0.13.0).
    #[arg(long, env = "GREPUI_CACHE_TTL_SECS", default_value_t = 5)]
    pub cache_ttl_secs: u64,
    /// Most entries the result cache keeps.
    #[arg(long, env = "GREPUI_CACHE_MAX_ENTRIES", default_value_t = 1000)]
    pub cache_max_entries: usize,
    /// Most memory (an estimate, in MiB) the result cache keeps.
    #[arg(long, env = "GREPUI_CACHE_MAX_MB", default_value_t = 64)]
    pub cache_max_mb: u64,
    /// Connections to GreptimeDB per source.
    #[arg(long, env = "GREPUI_POOL_MAX_CONNECTIONS", default_value_t = 8)]
    pub pool_max_connections: u32,
    /// Seconds to let running requests finish after SIGTERM or Ctrl-C.
    #[arg(long, env = "GREPUI_SHUTDOWN_GRACE_SECS", default_value_t = 30)]
    pub shutdown_grace_secs: u64,
    /// The account in --greptime-mysql is meant to be read-only: at startup, try to create a
    /// probe table in the default org's database and warn if that works.
    #[arg(long, env = "GREPUI_READ_ONLY_USER")]
    pub read_only_user: bool,
    /// Bearer token that may read /metrics (otherwise a platform admin is needed).
    #[arg(long, env = "GREPUI_METRICS_TOKEN")]
    pub metrics_token: Option<String>,
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
            admin_writes: false,
            no_alerts: false,
            // Library and test default: no sign-in. The command line defaults to sign-in required.
            no_auth: true,
            session_hours: 12,
            token_rate_per_min: 600,
            oidc_issuer: None,
            oidc_client_id: None,
            oidc_client_secret: None,
            oidc_auth_url: None,
            oidc_token_url: None,
            oidc_jwks_url: None,
            oidc_redirect_url: None,
            oidc_scopes: "openid profile email".into(),
            oidc_role_claim: "groups".into(),
            oidc_role_map: vec![],
            sources: vec![],
            pipeline_http: None,
            smtp: None,
            smtp_from: "grepui@localhost.localdomain".into(),
            query_timeout_secs: 30,
            // Library and test default: no cache, so every read reaches the reader.
            cache_ttl_secs: 0,
            cache_max_entries: 1000,
            cache_max_mb: 64,
            pool_max_connections: 8,
            shutdown_grace_secs: 30,
            read_only_user: false,
            metrics_token: None,
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
        let oidc = [
            &self.oidc_issuer,
            &self.oidc_client_id,
            &self.oidc_auth_url,
            &self.oidc_token_url,
            &self.oidc_jwks_url,
            &self.oidc_redirect_url,
        ];
        if oidc.iter().any(|o| o.is_some()) {
            if oidc.iter().any(|o| o.is_none()) {
                anyhow::bail!(
                    "--oidc-issuer, --oidc-client-id, --oidc-auth-url, --oidc-token-url, --oidc-jwks-url and --oidc-redirect-url must be set together"
                );
            }
            for u in [
                &self.oidc_auth_url,
                &self.oidc_token_url,
                &self.oidc_jwks_url,
                &self.oidc_redirect_url,
            ]
            .into_iter()
            .flatten()
            {
                let p = url::Url::parse(u)
                    .map_err(|_| anyhow::anyhow!("an --oidc URL is not valid: {u}"))?;
                if !matches!(p.scheme(), "http" | "https") {
                    anyhow::bail!("--oidc URLs must be http:// or https://");
                }
            }
            for r in &self.oidc_role_map {
                crate::auth::oidc::parse_rule(r)
                    .map_err(|e| anyhow::anyhow!("--oidc-role-map {r:?}: {e}"))?;
            }
        }
        if let Some(a) = &self.smtp {
            crate::alerts::notify::Smtp::parse(a, &self.smtp_from)
                .map_err(|e| anyhow::anyhow!("--smtp: {e}"))?;
        }
        let mut seen = std::collections::HashSet::new();
        for r in &self.sources {
            let (n, _) =
                crate::workload::parse_source(r).map_err(|e| anyhow::anyhow!("--source: {e}"))?;
            if !seen.insert(n) {
                anyhow::bail!("--source: a name is used twice");
            }
        }
        if self.pool_max_connections == 0 || self.pool_max_connections > 100 {
            anyhow::bail!("--pool-max-connections must be between 1 and 100");
        }
        if self.cache_ttl_secs > 3600 {
            anyhow::bail!("--cache-ttl-secs must be at most 3600 (0 turns the cache off)");
        }
        if self.cache_max_entries == 0 || self.cache_max_mb == 0 || self.cache_max_mb > 4096 {
            anyhow::bail!(
                "--cache-max-entries must be at least 1 and --cache-max-mb between 1 and 4096"
            );
        }
        if self.shutdown_grace_secs > 600 {
            anyhow::bail!("--shutdown-grace-secs must be at most 600");
        }
        if self.metrics_token.as_ref().is_some_and(|t| t.len() < 16) {
            anyhow::bail!("--metrics-token must be at least 16 characters");
        }
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
