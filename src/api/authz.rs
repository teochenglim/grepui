//! The request layer for access control (ADR 0012): identify the caller, classify the route,
//! decide, apply the CSRF and rate checks, and append the audit row. It is a `route_layer`, so
//! it sees the matched route template, never a spelling of the path.

use super::Shared;
use crate::{
    audit,
    auth::{self, Access, Identity},
    store::build::{AuditRow, PLATFORM_ORG},
    workload::{CTX, Ctx},
};
use axum::{
    Json,
    extract::{MatchedPath, Request},
    http::{HeaderMap, Method, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use serde_json::json;

/// Percent-decode a path segment the way the router does before a handler sees it.
pub fn decode(seg: &str) -> Option<String> {
    let b = seg.as_bytes();
    let (mut out, mut i) = (Vec::with_capacity(b.len()), 0);
    while i < b.len() {
        if b[i] == b'%' {
            let h = std::str::from_utf8(b.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(h, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn deny(code: StatusCode, msg: &str) -> Response {
    (code, Json(json!({"error": msg}))).into_response()
}

fn cookie_of(h: &HeaderMap) -> Option<String> {
    h.get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .find_map(|p| {
            p.trim()
                .strip_prefix(&format!("{}=", auth::COOKIE))
                .map(String::from)
        })
}

/// Who is calling. `None` means credentials were sent and are not valid.
pub async fn authenticate(s: &Shared, h: &HeaderMap) -> Option<Identity> {
    if let Some(v) = h.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()) {
        let tok = v.strip_prefix("Bearer ")?;
        return auth::from_bearer(s, tok.trim()).await.ok().flatten();
    }
    if let Some(c) = cookie_of(h) {
        return auth::from_cookie(s, &c).await.ok().flatten();
    }
    Some(Identity::anonymous())
}

/// A request that changes state and was made with the browser's own cookie must come from this
/// site: the `Origin` (or, failing that, `Sec-Fetch-Site`) is checked.
fn same_site(h: &HeaderMap) -> bool {
    let host = h
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    match h.get(header::ORIGIN).and_then(|v| v.to_str().ok()) {
        Some(o) => o.split("://").nth(1).is_some_and(|a| a == host),
        None => h
            .get("sec-fetch-site")
            .and_then(|v| v.to_str().ok())
            .is_none_or(|v| v == "same-origin" || v == "none"),
    }
}

fn audited(template: &str, method: &Method) -> bool {
    if matches!(
        template,
        "/healthz"
            | "/metrics"
            | "/api/v1/health"
            | "/api/v1/info"
            | "/api/v1/auth/me"
            | "/api/v1/orgs"
    ) {
        return false;
    }
    *method != Method::OPTIONS && *method != Method::HEAD
}

fn opt_param(q: Option<&str>, key: &str) -> Option<i64> {
    q?.split('&')
        .find_map(|p| p.strip_prefix(&format!("{key}=")))
        .and_then(|v| v.parse().ok())
}

fn params(q: Option<&str>, key: &str) -> i64 {
    q.and_then(|q| {
        q.split('&')
            .find_map(|p| p.strip_prefix(&format!("{key}=")))
    })
    .and_then(|v| v.parse().ok())
    .unwrap_or(0)
}

/// Count every answer by matched route (R-O2), then do the access work.
pub async fn layer(
    axum::extract::State(s): axum::extract::State<Shared>,
    req: Request,
    next: Next,
) -> Response {
    let template = req
        .extensions()
        .get::<MatchedPath>()
        .map(|m| m.as_str().to_string())
        .unwrap_or_default();
    let res = access(axum::extract::State(s.clone()), req, next).await;
    s.metrics.request(&template, res.status().as_u16());
    res
}

/// Equal text without stopping at the first difference.
fn same_secret(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |d, (x, y)| d | (x ^ y)) == 0
}

async fn access(
    axum::extract::State(s): axum::extract::State<Shared>,
    mut req: Request,
    next: Next,
) -> Response {
    let method = req.method().clone();
    let template = req
        .extensions()
        .get::<MatchedPath>()
        .map(|m| m.as_str().to_string())
        .unwrap_or_default();
    let path = req.uri().path().to_string();
    let query = req.uri().query().map(String::from);
    let access = auth::access_for(&method, &template);
    // A method the table does not know for this route is not allowed for anyone.
    if access == Access::Deny {
        return deny(StatusCode::METHOD_NOT_ALLOWED, "method not allowed");
    }
    // A scraper may present the metrics token instead of a platform admin's session.
    if template == "/metrics"
        && let Some(want) = s.cfg.metrics_token.as_deref()
        && req
            .headers()
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .is_some_and(|t| same_secret(t.trim(), want))
    {
        req.extensions_mut().insert(Identity::anonymous());
        return next.run(req).await;
    }
    let mut identity = if s.cfg.no_auth {
        Identity::local()
    } else {
        match authenticate(&s, req.headers()).await {
            Some(i) => i,
            None if access == Access::Anonymous => Identity::anonymous(),
            None => {
                return deny(
                    StatusCode::UNAUTHORIZED,
                    "sign in again: the session or token is not valid",
                );
            }
        }
    };
    let org = if template.starts_with("/api/v1/orgs/{org}") {
        path.split('/').nth(4).and_then(decode)
    } else {
        None
    };
    if !auth::allows(&identity, access, org.as_deref()) {
        if identity.is_anonymous() {
            return deny(StatusCode::UNAUTHORIZED, "sign in first");
        }
        // The same answer for an org that does not exist and one the caller may not see.
        let r = deny(StatusCode::FORBIDDEN, "you do not have access to this");
        record(
            &s,
            &identity,
            &template,
            &path,
            query.as_deref(),
            org.as_deref(),
            StatusCode::FORBIDDEN,
            0,
            &method,
        )
        .await;
        return r;
    }
    if !s.cfg.no_auth {
        if identity.cookie
            && !(method == Method::GET || method == Method::HEAD)
            && !same_site(req.headers())
        {
            return deny(StatusCode::FORBIDDEN, "cross-site request refused");
        }
        if let Some(k) = identity.token_key.clone()
            && let Err(wait) = s.auth.hit(&format!("tok:{k}"), s.cfg.token_rate_per_min)
        {
            let mut r = auth::too_many(wait).into_response();
            if let Ok(v) = header::HeaderValue::from_str(&wait.max(1).to_string()) {
                r.headers_mut().insert(header::RETRY_AFTER, v);
            }
            return r;
        }
    }
    // Public links are anonymous callers of one org's data.
    if access == Access::Anonymous && identity.is_anonymous() {
        identity.username = "anonymous".into();
    }
    // Work groups (ADR 0013): the window is checked before anything runs, and the rest of the
    // limits ride on the task so every read made for this request is held to them.
    let ctx = org.as_ref().map(|o| Ctx {
        user: identity.username.clone(),
        org: o.clone(),
        role: identity.role_in(o).unwrap_or(auth::Role::Viewer),
    });
    if let Some(c) = &ctx
        && (method == Method::GET || method == Method::HEAD)
        && let (Some(from), Some(to)) = (
            opt_param(query.as_deref(), "from"),
            opt_param(query.as_deref(), "to"),
        )
        && let Err(m) = s.workload.check_window(&c.org, c.role, from, to)
    {
        return deny(StatusCode::TOO_MANY_REQUESTS, &m);
    }
    req.extensions_mut().insert(identity.clone());
    let res = match ctx {
        Some(c) => CTX.scope(c, next.run(req)).await,
        None => next.run(req).await,
    };
    if !s.cfg.no_auth && audited(&template, &method) {
        let rows = res
            .headers()
            .get("x-grepui-rows")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        record(
            &s,
            &identity,
            &template,
            &path,
            query.as_deref(),
            org.as_deref(),
            res.status(),
            rows,
            &method,
        )
        .await;
    }
    res
}

#[allow(clippy::too_many_arguments)]
async fn record(
    s: &Shared,
    id: &Identity,
    template: &str,
    path: &str,
    query: Option<&str>,
    org: Option<&str>,
    status: StatusCode,
    rows: i64,
    method: &Method,
) {
    if s.cfg.no_auth {
        return;
    }
    let org_name = match org {
        Some(o) if s.orgs.resolve(o).is_ok() => o.to_string(),
        _ => PLATFORM_ORG.to_string(),
    };
    // A public link's token is in the path: record the route, not the secret.
    let target = if template.contains("{token}") {
        template.to_string()
    } else {
        path.to_string()
    };
    let outcome = match status.as_u16() {
        401 | 403 => "denied",
        c if c >= 400 => "error",
        _ => "ok",
    };
    s.audit
        .record(
            s,
            &org_name,
            AuditRow {
                id: audit::new_id(),
                username: id.username.clone(),
                action: format!("{method} {template}"),
                target: target.chars().take(500).collect(),
                from_ms: params(query, "from"),
                to_ms: params(query, "to"),
                rows,
                status: status.as_u16() as i64,
                outcome: outcome.into(),
                detail: query.unwrap_or_default().chars().take(1000).collect(),
                ts_ms: s.now_ms(),
            },
        )
        .await;
}
