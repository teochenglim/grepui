//! Sign-in (R-C1, R-C4): first-run setup, password login, logout, password change, the session
//! description and the OpenID Connect redirect and callback.

use super::{Shared, ok};
use crate::{
    audit,
    auth::{self, Identity, Role, User, oidc::Oidc, password},
    error::ApiError,
    query::Meta,
    store::build::PLATFORM_ORG,
};
use axum::{
    Extension, Json,
    extract::{Query, State},
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Redirect, Response},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub fn meta(name: &str) -> Meta {
    Meta {
        query: name.into(),
        sql: String::new(),
        binds: vec![],
        ms: 0,
        rows: 0,
        source: "raw",
        note: None,
    }
}

fn cookie_header(value: &str, hours: u64) -> HeaderValue {
    // HttpOnly: scripts never see it; SameSite=Strict: other sites cannot send it. `Secure` is the
    // reverse proxy's business (grepui itself speaks plain HTTP).
    let max = hours * 3600;
    HeaderValue::from_str(&format!(
        "{}={value}; HttpOnly; SameSite=Strict; Path=/; Max-Age={max}",
        auth::COOKIE
    ))
    .unwrap_or(HeaderValue::from_static(""))
}

fn with_cookie(mut r: Response, value: &str, hours: u64) -> Response {
    r.headers_mut()
        .append(header::SET_COOKIE, cookie_header(value, hours));
    r
}

fn user_json(u: &User) -> Value {
    json!({"username": u.username, "display": u.display, "platform_admin": u.platform_admin, "source": u.source,
           "orgs": u.orgs.iter().map(|(o, r)| (o.clone(), json!(r.name()))).collect::<serde_json::Map<_, _>>()})
}

pub async fn me(
    State(s): State<Shared>,
    Extension(id): Extension<Identity>,
) -> Result<Response, ApiError> {
    let setup_required = !s.cfg.no_auth && id.is_anonymous() && auth::users(&s).await?.is_empty();
    let user = if id.kind == "user" {
        auth::user_get(&s, &id.username)
            .await?
            .map(|(u, _)| user_json(&u))
    } else {
        None
    };
    Ok(ok(
        json!({"authenticated": !id.is_anonymous(), "no_auth": s.cfg.no_auth, "kind": id.kind, "username": id.username,
               "platform_admin": id.platform_admin, "orgs": id.orgs.iter().map(|(o, r)| (o.clone(), json!(r.name()))).collect::<serde_json::Map<_, _>>(),
               "user": user, "setup_required": setup_required, "oidc": Oidc::from_config(&s.cfg).is_some()}),
        meta("auth_me"),
    ))
}

#[derive(Deserialize)]
pub struct Credentials {
    username: String,
    password: String,
}

/// The first administrator. Only while there are no users at all.
pub async fn setup(
    State(s): State<Shared>,
    Json(b): Json<Credentials>,
) -> Result<Response, ApiError> {
    if s.cfg.no_auth {
        return Err(ApiError::bad("sign-in is switched off (--no-auth)"));
    }
    let username = b.username.trim().to_ascii_lowercase();
    if !auth::valid_username(&username) {
        return Err(ApiError::bad(
            "the username must be 1-64 characters: lowercase letters, digits, . _ - @",
        ));
    }
    auth::check_password(&b.password)?;
    if !auth::users(&s).await?.is_empty() {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "the first administrator already exists".into(),
        ));
    }
    // GreptimeDB has no compare-and-set: write a marker, read it back, and only the writer it
    // names goes on (a second setup that raced it fails here).
    let now = s.now_ms();
    let marker = auth::store::row(
        "admin-created",
        "setup",
        &json!({"username": username}),
        &username,
        now,
        now,
    );
    auth::store::put(&s, PLATFORM_ORG, "setup", &marker).await?;
    let back = auth::store::get(&s, PLATFORM_ORG, "setup", "admin-created").await?;
    if back
        .and_then(|r| serde_json::from_str::<Value>(&r.spec).ok())
        .is_some_and(|v| v["username"] != json!(username))
    {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "the first administrator already exists".into(),
        ));
    }
    let hash =
        password::hash(&b.password).map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let u = User {
        username: username.clone(),
        display: username.clone(),
        password_hash: hash,
        platform_admin: true,
        disabled: false,
        source: "local".into(),
        orgs: BTreeMap::new(),
    };
    auth::user_put(&s, &u, now).await?;
    audit::event(
        &s,
        PLATFORM_ORG,
        &username,
        "auth.setup",
        "ok",
        "first administrator created",
    )
    .await;
    let cookie = auth::create_session(&s, &username).await?;
    Ok(with_cookie(
        ok(user_json(&u), meta("auth_setup")),
        &cookie,
        s.cfg.session_hours,
    ))
}

pub async fn login(
    State(s): State<Shared>,
    Json(b): Json<Credentials>,
) -> Result<Response, ApiError> {
    if s.cfg.no_auth {
        return Err(ApiError::bad("sign-in is switched off (--no-auth)"));
    }
    let username = b.username.trim().to_ascii_lowercase();
    // Brute force: ten tries a minute per username, and a ceiling for everyone together.
    if s.auth.hit(&format!("login:{username}"), 10).is_err() || s.auth.hit("login:*", 600).is_err()
    {
        audit::event(
            &s,
            PLATFORM_ORG,
            &username,
            "auth.login",
            "denied",
            "rate limited",
        )
        .await;
        return Err(auth::too_many(60));
    }
    let found = auth::user_get(&s, &username).await?;
    let good = match &found {
        Some((u, _)) if !u.disabled && !u.password_hash.is_empty() => {
            password::verify(&b.password, &u.password_hash)
        }
        _ => {
            password::burn(&b.password);
            false
        }
    };
    let Some((u, _)) = found.filter(|_| good) else {
        audit::event(
            &s,
            PLATFORM_ORG,
            &username,
            "auth.login",
            "denied",
            "bad credentials",
        )
        .await;
        return Err(ApiError(
            StatusCode::UNAUTHORIZED,
            "the username or password is not right".into(),
        ));
    };
    let cookie = auth::create_session(&s, &u.username).await?;
    audit::event(
        &s,
        PLATFORM_ORG,
        &u.username,
        "auth.login",
        "ok",
        "password",
    )
    .await;
    Ok(with_cookie(
        ok(user_json(&u), meta("auth_login")),
        &cookie,
        s.cfg.session_hours,
    ))
}

pub async fn logout(
    State(s): State<Shared>,
    Extension(id): Extension<Identity>,
    headers: axum::http::HeaderMap,
) -> Result<Response, ApiError> {
    for v in headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
    {
        if let Some(c) = v.trim().strip_prefix(&format!("{}=", auth::COOKIE)) {
            auth::end_session(&s, c).await?;
        }
    }
    audit::event(&s, PLATFORM_ORG, &id.username, "auth.logout", "ok", "").await;
    let mut r = ok(json!({"signed_out": true}), meta("auth_logout"));
    r.headers_mut().append(
        header::SET_COOKIE,
        HeaderValue::from_static("grepui_session=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0"),
    );
    Ok(r)
}

#[derive(Deserialize)]
pub struct PasswordChange {
    current: String,
    new: String,
}

pub async fn change_password(
    State(s): State<Shared>,
    Extension(id): Extension<Identity>,
    Json(b): Json<PasswordChange>,
) -> Result<Response, ApiError> {
    let Some((mut u, created)) = auth::user_get(&s, &id.username)
        .await?
        .filter(|_| id.kind == "user")
    else {
        return Err(ApiError::bad(
            "only people with a local password can change it here",
        ));
    };
    if u.password_hash.is_empty() || !password::verify(&b.current, &u.password_hash) {
        audit::event(
            &s,
            PLATFORM_ORG,
            &u.username,
            "auth.password",
            "denied",
            "current password wrong",
        )
        .await;
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            "the current password is not right".into(),
        ));
    }
    auth::check_password(&b.new)?;
    u.password_hash =
        password::hash(&b.new).map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    auth::user_put(&s, &u, created).await?;
    auth::end_user_sessions(&s, &u.username).await?;
    audit::event(&s, PLATFORM_ORG, &u.username, "auth.password", "ok", "").await;
    // every session is gone, including this one: hand back a fresh cookie
    let cookie = auth::create_session(&s, &u.username).await?;
    Ok(with_cookie(
        ok(json!({"changed": true}), meta("auth_password")),
        &cookie,
        s.cfg.session_hours,
    ))
}

// ---- OpenID Connect ------------------------------------------------------------------------------

pub async fn oidc_login(State(s): State<Shared>) -> Result<Response, ApiError> {
    let o = Oidc::from_config(&s.cfg)
        .ok_or_else(|| ApiError::not_found("single sign-on is not configured"))?;
    let url = auth::oidc::begin(&s, &o).await?;
    Ok(Redirect::to(&url).into_response())
}

#[derive(Deserialize)]
pub struct Callback {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

pub async fn oidc_callback(
    State(s): State<Shared>,
    Query(q): Query<Callback>,
) -> Result<Response, ApiError> {
    let o = Oidc::from_config(&s.cfg)
        .ok_or_else(|| ApiError::not_found("single sign-on is not configured"))?;
    if q.error.is_some() {
        audit::event(
            &s,
            PLATFORM_ORG,
            "sso",
            "auth.oidc",
            "denied",
            "the provider reported an error",
        )
        .await;
        return Err(ApiError(
            StatusCode::UNAUTHORIZED,
            "the identity provider refused the sign-in".into(),
        ));
    }
    let (Some(code), Some(state)) = (q.code.as_deref(), q.state.as_deref()) else {
        return Err(ApiError::bad("code and state are required"));
    };
    match auth::oidc::finish(&s, &o, code, state).await {
        Ok(u) => {
            let cookie = auth::create_session(&s, &u.username).await?;
            audit::event(
                &s,
                PLATFORM_ORG,
                &u.username,
                "auth.oidc",
                "ok",
                &format!("roles from claim {}", o.claim),
            )
            .await;
            let mut r = Redirect::to("/").into_response();
            r.headers_mut().append(
                header::SET_COOKIE,
                cookie_header(&cookie, s.cfg.session_hours),
            );
            Ok(r)
        }
        Err(e) => {
            audit::event(&s, PLATFORM_ORG, "sso", "auth.oidc", "denied", &e.1).await;
            Err(e)
        }
    }
}

pub fn _role_name(r: Role) -> &'static str {
    r.name()
}
