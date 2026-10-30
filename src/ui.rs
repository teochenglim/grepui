//! The embedded UI: every file under `web/static/` (R-F3). Unknown extensionless paths fall
//! back to `index.html` (hash routing makes this rarely needed); missing assets are 404.

use axum::{
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};

include!(concat!(env!("OUT_DIR"), "/static_files.rs"));

pub fn mime(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or_default() {
        "html" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "json" | "map" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    }
}

fn find(name: &str) -> Option<&'static [u8]> {
    FILES.iter().find(|(n, _)| *n == name).map(|(_, b)| *b)
}

/// FNV-1a: a stable validator for `ETag`, no extra dependency.
fn etag(bytes: &[u8]) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("\"{h:016x}\"")
}

pub fn serve(path: &str, if_none_match: Option<&str>) -> Response {
    let rel = path.trim_start_matches('/');
    let name = if rel.is_empty() { "index.html" } else { rel };
    let (name, bytes) = match find(name) {
        Some(b) => (name, b),
        None if !name.rsplit('/').next().unwrap_or_default().contains('.') => {
            match find("index.html") {
                Some(b) => ("index.html", b),
                None => return unbuilt(),
            }
        }
        None => return StatusCode::NOT_FOUND.into_response(),
    };
    // Everything is revalidated on each load: the ETag makes that a cheap 304, and an upgrade
    // never leaves a browser running old JS against a new API.
    let cache = if name.ends_with(".html") {
        "no-cache"
    } else {
        "public, max-age=0, must-revalidate"
    };
    let tag = etag(bytes);
    if if_none_match == Some(tag.as_str()) {
        let mut r = StatusCode::NOT_MODIFIED.into_response();
        if let Ok(v) = HeaderValue::from_str(&tag) {
            r.headers_mut().insert(header::ETAG, v);
        }
        return r;
    }
    let mut r = bytes.into_response();
    let h = r.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(mime(name)));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    if let Ok(v) = HeaderValue::from_str(&tag) {
        h.insert(header::ETAG, v);
    }
    r
}

fn unbuilt() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        "UI missing: web/static/index.html was not embedded",
    )
        .into_response()
}
