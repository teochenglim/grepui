//! A tiny HTTP/1.1 client over a TCP stream: one request, `Connection: close`. Used by the
//! playground (demo data) and by the pipeline manager (GreptimeDB's pipeline API, admin writes
//! only). No TLS: GreptimeDB is reached on a local or compose-internal address.

use anyhow::{Context, Result, bail};
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    time::timeout,
};

/// POST `body` to `base` + `path`; returns the status code and the response text.
pub async fn post(
    base: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> Result<(u16, String)> {
    request("POST", base, path, headers, body).await
}

/// Any method; `path` includes the query string, which the caller has percent-encoded.
pub async fn request(
    method: &str,
    base: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> Result<(u16, String)> {
    if !["GET", "POST", "PUT", "DELETE"].contains(&method) || path.contains(['\r', '\n', ' ']) {
        bail!("invalid request");
    }
    let u = url::Url::parse(base).context("invalid GreptimeDB HTTP address")?;
    if u.scheme() != "http" {
        bail!("the playground only supports http:// GreptimeDB addresses");
    }
    let host = u
        .host_str()
        .context("the GreptimeDB HTTP address has no host")?;
    let port = u.port_or_known_default().unwrap_or(80);
    let mut req = format!(
        "{method} {path} HTTP/1.1\r\nHost: {host}:{port}\r\nConnection: close\r\nContent-Length: {}\r\n",
        body.len()
    );
    for (k, v) in headers {
        if k.contains(['\r', '\n']) || v.contains(['\r', '\n']) {
            bail!("invalid header");
        }
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str("\r\n");
    let run = async {
        let mut s = TcpStream::connect((host, port)).await?;
        s.write_all(req.as_bytes()).await?;
        s.write_all(body).await?;
        let mut buf = Vec::new();
        s.read_to_end(&mut buf).await?;
        anyhow::Ok(buf)
    };
    let buf = timeout(Duration::from_secs(60), run)
        .await
        .context("GreptimeDB did not answer in time")??;
    let text = String::from_utf8_lossy(&buf).into_owned();
    let status = text
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .context("malformed HTTP response")?;
    Ok((
        status,
        text.split("\r\n\r\n")
            .nth(1)
            .unwrap_or_default()
            .to_string(),
    ))
}
