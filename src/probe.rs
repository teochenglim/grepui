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
