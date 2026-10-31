//! A tiny HTTP/1.1 client: one request, `Connection: close`. Used by the playground (demo data),
//! by the pipeline manager (GreptimeDB's pipeline API, admin writes only) and by OIDC sign-in
//! (the identity provider's token and key endpoints). `http://` is plain TCP; `https://` goes
//! through rustls with the Mozilla root certificates (ADR 0012). Chunked responses are decoded.

use anyhow::{Context, Result, bail};
use std::{sync::Arc, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::TcpStream,
    time::timeout,
};

const MAX_RESPONSE: usize = 8 << 20;

/// POST `body` to `base` + `path`; returns the status code and the response text. Plain http
/// only: the playground and GreptimeDB talk on a local address.
pub async fn post(
    base: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> Result<(u16, String)> {
    if !base.starts_with("http://") {
        bail!("the playground only supports http:// GreptimeDB addresses");
    }
    request("POST", base, path, headers, body).await
}

fn tls() -> Result<Arc<rustls::ClientConfig>> {
    let roots = rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let cfg = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .context("TLS is not available")?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(Arc::new(cfg))
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
    let u = url::Url::parse(base).context("invalid HTTP address")?;
    let https = match u.scheme() {
        "http" => false,
        "https" => true,
        _ => bail!("only http:// and https:// addresses are supported"),
    };
    let host = u
        .host_str()
        .context("the HTTP address has no host")?
        .to_string();
    let port = u
        .port_or_known_default()
        .unwrap_or(if https { 443 } else { 80 });
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
        let tcp = TcpStream::connect((host.as_str(), port)).await?;
        if https {
            let name = rustls::pki_types::ServerName::try_from(host.clone())?;
            let s = tokio_rustls::TlsConnector::from(tls()?)
                .connect(name, tcp)
                .await?;
            exchange(s, req.as_bytes(), body).await
        } else {
            exchange(tcp, req.as_bytes(), body).await
        }
    };
    let buf = timeout(Duration::from_secs(60), run)
        .await
        .context("the server did not answer in time")??;
    parse(&buf)
}

async fn exchange<S: AsyncRead + AsyncWrite + Unpin>(
    mut s: S,
    head: &[u8],
    body: &[u8],
) -> Result<Vec<u8>> {
    s.write_all(head).await?;
    s.write_all(body).await?;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 16 * 1024];
    loop {
        match s.read(&mut chunk).await {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if buf.len() > MAX_RESPONSE {
                    bail!("the response is too large");
                }
            }
            // Many servers close a TLS connection without close_notify; what we have is the answer.
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof && !buf.is_empty() => break,
            Err(e) => return Err(e.into()),
        }
    }
    Ok(buf)
}

fn parse(buf: &[u8]) -> Result<(u16, String)> {
    let split = buf
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .context("malformed HTTP response")?;
    let head = String::from_utf8_lossy(&buf[..split]).to_string();
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .context("malformed HTTP response")?;
    let chunked = head.lines().any(|l| {
        l.to_ascii_lowercase().starts_with("transfer-encoding:")
            && l.to_ascii_lowercase().contains("chunked")
    });
    let raw = &buf[split + 4..];
    let body = if chunked { dechunk(raw)? } else { raw.to_vec() };
    Ok((status, String::from_utf8_lossy(&body).into_owned()))
}

fn dechunk(mut b: &[u8]) -> Result<Vec<u8>> {
    let mut out = vec![];
    loop {
        let eol = b
            .windows(2)
            .position(|w| w == b"\r\n")
            .context("bad chunked body")?;
        let size_text = String::from_utf8_lossy(&b[..eol]);
        let size =
            usize::from_str_radix(size_text.split(';').next().unwrap_or_default().trim(), 16)
                .context("bad chunk size")?;
        b = &b[eol + 2..];
        if size == 0 {
            return Ok(out);
        }
        if b.len() < size + 2 {
            bail!("a chunk is cut short");
        }
        out.extend_from_slice(&b[..size]);
        b = &b[size + 2..];
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn chunked_and_plain_bodies() {
        let (s, b) = super::parse(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n3\r\n\r\n!\r\n0\r\n\r\n").unwrap();
        assert_eq!((s, b.as_str()), (200, "hello\r\n!"));
        let (s, b) =
            super::parse(b"HTTP/1.1 404 X\r\nContent-Length: 9\r\n\r\na\r\n\r\nb").unwrap();
        assert_eq!((s, b.as_str()), (404, "a\r\n\r\nb"));
    }
}
