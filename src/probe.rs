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

/// The oldest GreptimeDB grepui supports: trace v2 tables, the JSON2 functions the LLM views read
/// and the statements the RUM view uses arrived in 1.3.0 (a run of the live suite on 1.2.1 failed
/// 11 of 44 tests).
pub const MIN_VERSION: (u32, u32) = (1, 3);

/// `(major, minor)` of a version string such as `1.3.0`, `v1.4.0-nightly`, `1.3` or the MySQL
/// protocol's `8.4.2-GreptimeDB-1.3.0` (the leading 8.4.2 is the MySQL version GreptimeDB
/// claims; the GreptimeDB version follows `GreptimeDB-`).
pub fn parse_version(v: &str) -> Option<(u32, u32)> {
    let v = v.trim();
    let v = v.split_once("GreptimeDB-").map_or(v, |(_, rest)| rest);
    let v = v.trim_start_matches(['v', 'V']);
    let mut it = v.split(['.', '-', '+']);
    let major = it.next()?.parse().ok()?;
    let minor = it.next()?.parse().ok()?;
    Some((major, minor))
}

/// Whether a database reporting `v` is supported; an unreadable version is given the benefit of the doubt.
pub fn version_supported(v: &str) -> bool {
    parse_version(v).is_none_or(|got| got >= MIN_VERSION)
}

#[cfg(test)]
mod version_tests {
    use super::*;

    #[test]
    fn versions_are_read_and_compared() {
        assert_eq!(parse_version("1.3.0"), Some((1, 3)));
        assert_eq!(parse_version("v1.4.0-nightly-20261031"), Some((1, 4)));
        assert_eq!(parse_version("1.2.1"), Some((1, 2)));
        assert_eq!(parse_version("garbage"), None);
        // What the MySQL protocol reports: the MySQL version first, GreptimeDB's after the tag.
        assert_eq!(parse_version("8.4.2-GreptimeDB-1.3.0"), Some((1, 3)));
        assert_eq!(parse_version("8.4.2-GreptimeDB-v1.2.1"), Some((1, 2)));
        assert!(version_supported("8.4.2-GreptimeDB-1.3.0"));
        assert!(!version_supported("8.4.2-GreptimeDB-1.2.1"));
        assert!(
            version_supported("1.3.0") && version_supported("1.4.2") && version_supported("2.0.0")
        );
        assert!(!version_supported("1.2.1") && !version_supported("0.9.5"));
        assert!(
            version_supported("unknown"),
            "an unreadable version is not a reason to refuse"
        );
    }
}
