//! `grepui healthcheck`: a dependency-free HTTP probe for container health checks.

use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    time::timeout,
};

/// True when `GET /healthz` on `addr` answers 200.
pub async fn healthy(addr: &str) -> bool {
    let run = async {
        let mut s = TcpStream::connect(addr).await.ok()?;
        s.write_all(b"GET /healthz HTTP/1.0\r\nHost: grepui\r\n\r\n")
            .await
            .ok()?;
        let mut buf = Vec::new();
        s.read_to_end(&mut buf).await.ok()?;
        Some(buf.starts_with(b"HTTP/1.1 200") || buf.starts_with(b"HTTP/1.0 200"))
    };
    timeout(Duration::from_secs(3), run)
        .await
        .ok()
        .flatten()
        .unwrap_or(false)
}

/// What the read-only check found out about the account behind `--greptime-mysql`.
#[derive(Debug, Clone, PartialEq)]
pub enum Access {
    /// The probe table could not be created: the account cannot write here.
    ReadOnly,
    /// The probe table was created (and removed): the account can write.
    CanWrite,
    /// It was created and could not be removed; the name is in the message.
    Left(String),
}

/// The only DDL of the check (v0.13.0, ADR 0014): one fixed table in the database the caller
/// names, created and dropped at once. The database name is validated, nothing else varies.
pub const PROBE_TABLE: &str = "grepui_write_probe";

pub fn probe_statements(db: &str) -> anyhow::Result<[String; 2]> {
    let t = crate::query::qualified(db, PROBE_TABLE)?;
    Ok([
        format!("CREATE TABLE IF NOT EXISTS {t} (ts TIMESTAMP TIME INDEX, v BIGINT)"),
        format!("DROP TABLE IF EXISTS {t}"),
    ])
}

/// Try to create the probe table in `db`. A read-only account is refused by the database.
pub async fn check_read_only(w: &dyn crate::query::Writer, db: &str) -> anyhow::Result<Access> {
    let [create, drop] = probe_statements(db)?;
    if w.ddl(&create).await.is_err() {
        return Ok(Access::ReadOnly);
    }
    Ok(match w.ddl(&drop).await {
        Ok(()) => Access::CanWrite,
        Err(e) => Access::Left(format!(
            "created {db}.{PROBE_TABLE} but could not drop it ({}); drop it by hand",
            e.detail()
        )),
    })
}
