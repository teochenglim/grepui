//! Delivery (R-A4): a webhook is one HTTP POST and an email one SMTP conversation, both over
//! plain TCP with short timeouts (grepui has no TLS client: ADR 0010). Nothing here decides
//! *whether* to send; the engine does. A failure is returned as text for the history and the
//! test-send, never panicked on.

use super::Destination;
use async_trait::async_trait;
use serde_json::Value;
use std::time::Duration;
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::TcpStream,
    time::timeout,
};

const IO_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_RESPONSE: u64 = 64 * 1024;

/// What is delivered: the rendered text and the structured payload a webhook carries.
#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    pub subject: String,
    pub body: String,
    pub payload: Value,
    /// The body is an HTML page (reports); alerts send plain text.
    pub html: bool,
}

#[async_trait]
pub trait Sender: Send + Sync {
    async fn send(&self, to: &Destination, m: &Message) -> Result<(), String>;
}

#[derive(Debug, Clone, PartialEq)]
pub struct Smtp {
    pub host: String,
    pub port: u16,
    pub from: String,
}

impl Smtp {
    /// `host:port` plus a sender address.
    pub fn parse(addr: &str, from: &str) -> Result<Smtp, String> {
        let (host, port) = addr
            .rsplit_once(':')
            .ok_or("the SMTP address must be host:port")?;
        let port = port.parse::<u16>().map_err(|_| "bad SMTP port")?;
        if host.is_empty() || host.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return Err("bad SMTP host".into());
        }
        if !super::valid_email(from) {
            return Err("--smtp-from must be an email address".into());
        }
        Ok(Smtp {
            host: host.into(),
            port,
            from: from.into(),
        })
    }
}

#[derive(Default)]
pub struct NetSender {
    pub smtp: Option<Smtp>,
}

impl NetSender {
    /// Email works only when `--smtp` is set (the config was validated at startup).
    pub fn from_config(cfg: &crate::config::Config) -> NetSender {
        NetSender {
            smtp: cfg
                .smtp
                .as_deref()
                .and_then(|a| Smtp::parse(a, &cfg.smtp_from).ok()),
        }
    }
}

#[async_trait]
impl Sender for NetSender {
    async fn send(&self, to: &Destination, m: &Message) -> Result<(), String> {
        match to.kind.as_str() {
            "webhook" => webhook(&to.url, &m.payload).await,
            "email" => match &self.smtp {
                Some(s) => email_as(s, &to.to, &m.subject, &m.body, m.html).await,
                None => Err(
                    "email is not configured: start grepui with --smtp host:port and --smtp-from"
                        .into(),
                ),
            },
            _ => Err("unknown destination kind".into()),
        }
    }
}

async fn connect(host: &str, port: u16) -> Result<TcpStream, String> {
    timeout(IO_TIMEOUT, TcpStream::connect((host, port)))
        .await
        .map_err(|_| format!("connecting to {host}:{port} timed out"))?
        .map_err(|e| format!("cannot connect to {host}:{port}: {e}"))
}

pub async fn webhook(url: &str, payload: &Value) -> Result<(), String> {
    let u = super::webhook_target(url).map_err(|e| e.1)?;
    let host = u.host_str().unwrap_or_default().to_string();
    let port = u.port_or_known_default().unwrap_or(80);
    let body = payload.to_string();
    let mut path = u.path().to_string();
    if let Some(q) = u.query() {
        path.push('?');
        path.push_str(q);
    }
    let req = format!(
        "POST {path} HTTP/1.1\r\nHost: {host}:{port}\r\nUser-Agent: grepui\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    // Resolve first, refuse blocked addresses, and connect to the address that was checked, so a
    // name cannot answer differently between the check and the connection.
    let addrs: Vec<std::net::SocketAddr> =
        timeout(IO_TIMEOUT, tokio::net::lookup_host((host.as_str(), port)))
            .await
            .map_err(|_| format!("resolving {host} timed out"))?
            .map_err(|e| format!("cannot resolve {host}: {e}"))?
            .collect();
    if addrs.is_empty() || addrs.iter().any(|a| super::blocked_ip(a.ip())) {
        return Err(format!("{host} resolves to an address that is not allowed"));
    }
    let mut s = timeout(IO_TIMEOUT, TcpStream::connect(addrs[0]))
        .await
        .map_err(|_| format!("connecting to {host}:{port} timed out"))?
        .map_err(|e| format!("cannot connect to {host}:{port}: {e}"))?;
    timeout(IO_TIMEOUT, s.write_all(req.as_bytes()))
        .await
        .map_err(|_| "sending the webhook timed out".to_string())?
        .map_err(|e| format!("sending the webhook failed: {e}"))?;
    let mut buf = vec![];
    timeout(IO_TIMEOUT, s.take(MAX_RESPONSE).read_to_end(&mut buf))
        .await
        .map_err(|_| "the webhook did not answer in time".to_string())?
        .map_err(|e| format!("reading the webhook answer failed: {e}"))?;
    let head = String::from_utf8_lossy(&buf);
    let status = head
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|c| c.parse::<u16>().ok())
        .ok_or("the webhook answered with something that is not HTTP")?;
    if (200..300).contains(&status) {
        Ok(())
    } else {
        Err(format!("the webhook answered HTTP {status}"))
    }
}

fn b64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16
            | (*c.get(1).unwrap_or(&0) as u32) << 8
            | *c.get(2).unwrap_or(&0) as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if c.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// The message as it goes after DATA: headers, a blank line, the body with CRLF line ends and
/// dot-stuffing. The subject is one line and encoded if it is not ASCII; addresses were validated
/// to hold no whitespace, so no header can be injected.
pub fn mail_text(from: &str, to: &[String], subject: &str, body: &str, date: &str) -> String {
    mail_text_as(from, to, subject, body, date, false)
}

/// [`mail_text`] with the body as `text/html` when `html` is set.
pub fn mail_text_as(
    from: &str,
    to: &[String],
    subject: &str,
    body: &str,
    date: &str,
    html: bool,
) -> String {
    let kind = if html { "text/html" } else { "text/plain" };
    let subject: String = subject.chars().filter(|c| !c.is_control()).collect();
    let subject = if subject.is_ascii() {
        subject
    } else {
        format!("=?UTF-8?B?{}?=", b64(subject.as_bytes()))
    };
    // SMTP lines are limited to 998 characters. A page of HTML (or a CSV row with a long field)
    // can be longer, so such a body travels as base64, which is wrapped at 76.
    let encode = html || body.lines().any(|l| l.len() > 900);
    let mut out = format!(
        "From: <{from}>\r\nTo: {}\r\nSubject: {subject}\r\nDate: {date}\r\nMIME-Version: 1.0\r\nContent-Type: {kind}; charset=utf-8\r\nContent-Transfer-Encoding: {}\r\n\r\n",
        to.iter()
            .map(|a| format!("<{a}>"))
            .collect::<Vec<_>>()
            .join(", "),
        if encode { "base64" } else { "8bit" }
    );
    if encode {
        let enc = b64(body.as_bytes());
        for chunk in enc.as_bytes().chunks(76) {
            out.push_str(std::str::from_utf8(chunk).unwrap_or_default());
            out.push_str("\r\n");
        }
        return out;
    }
    for line in body.replace("\r\n", "\n").replace('\r', "\n").split('\n') {
        if line.starts_with('.') {
            out.push('.');
        }
        out.push_str(line);
        out.push_str("\r\n");
    }
    out
}

async fn reply(r: &mut BufReader<TcpStream>) -> Result<(u16, String), String> {
    let mut text = String::new();
    loop {
        let mut line = String::new();
        let n = timeout(IO_TIMEOUT, r.read_line(&mut line))
            .await
            .map_err(|_| "the mail server did not answer in time".to_string())?
            .map_err(|e| format!("reading from the mail server failed: {e}"))?;
        if n == 0 || line.len() > 1000 {
            return Err("the mail server closed the connection".into());
        }
        text.push_str(line.trim_end());
        let code = line.get(..3).and_then(|c| c.parse::<u16>().ok());
        match (code, line.as_bytes().get(3)) {
            (Some(_), Some(b'-')) => text.push('\n'),
            (Some(c), _) => return Ok((c, text)),
            _ => return Err("the mail server sent something that is not SMTP".into()),
        }
    }
}

async fn expect(r: &mut BufReader<TcpStream>, ok: &[u16], what: &str) -> Result<(), String> {
    let (code, text) = reply(r).await?;
    if ok.contains(&code) {
        Ok(())
    } else {
        Err(format!("the mail server refused {what}: {text}"))
    }
}

async fn cmd(
    r: &mut BufReader<TcpStream>,
    line: &str,
    ok: &[u16],
    what: &str,
) -> Result<(), String> {
    timeout(
        IO_TIMEOUT,
        r.get_mut().write_all(format!("{line}\r\n").as_bytes()),
    )
    .await
    .map_err(|_| "writing to the mail server timed out".to_string())?
    .map_err(|e| format!("writing to the mail server failed: {e}"))?;
    expect(r, ok, what).await
}

/// One message to all recipients over plain SMTP (no STARTTLS, no AUTH: a relay on a trusted
/// network, ADR 0010).
pub async fn email(smtp: &Smtp, to: &[String], subject: &str, body: &str) -> Result<(), String> {
    email_as(smtp, to, subject, body, false).await
}

pub async fn email_as(
    smtp: &Smtp,
    to: &[String],
    subject: &str,
    body: &str,
    html: bool,
) -> Result<(), String> {
    let mut r = BufReader::new(connect(&smtp.host, smtp.port).await?);
    expect(&mut r, &[220], "the connection").await?;
    cmd(&mut r, "EHLO grepui", &[250], "EHLO").await?;
    cmd(
        &mut r,
        &format!("MAIL FROM:<{}>", smtp.from),
        &[250],
        "the sender",
    )
    .await?;
    for a in to {
        cmd(
            &mut r,
            &format!("RCPT TO:<{a}>"),
            &[250, 251],
            "a recipient",
        )
        .await?;
    }
    cmd(&mut r, "DATA", &[354], "DATA").await?;
    let date = chrono::Utc::now()
        .format("%a, %d %b %Y %H:%M:%S +0000")
        .to_string();
    let text = mail_text_as(&smtp.from, to, subject, body, &date, html);
    cmd(&mut r, &format!("{text}."), &[250], "the message").await?;
    let _ = cmd(&mut r, "QUIT", &[221], "QUIT").await;
    Ok(())
}
