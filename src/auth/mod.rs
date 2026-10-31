//! Access control (R-C1 – R-C3, R-C6; ADR 0012): who is calling and what they may do.
//!
//! Deny by default. Every route is classified by [`access_for`] (an exhaustive table, proven by a
//! test against `api::ROUTES`); a route that is not classified is refused for everyone. A caller
//! is a person with a session cookie, a service account with a bearer token, or (with `--no-auth`)
//! the implicit local admin. Roles are per org: `viewer` reads, `editor` also changes saved
//! state, `admin` also manages the org (members, tokens, rules, DDL templates). A platform admin is
//! an admin everywhere and manages orgs and users.

pub mod jwt;
pub mod oidc;
pub mod password;
pub mod store;

use crate::api::Shared;
use axum::http::Method;
use rand::{RngCore, rngs::OsRng};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap},
    sync::Mutex,
    time::{Duration, Instant},
};

pub const COOKIE: &str = "grepui_session";
pub const TOKEN_PREFIX: &str = "gu_";
pub const CACHE_TTL: Duration = Duration::from_secs(5);
pub const MIN_PASSWORD: usize = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Viewer,
    Editor,
    Admin,
}

impl Role {
    pub fn parse(s: &str) -> Option<Role> {
        match s {
            "viewer" => Some(Role::Viewer),
            "editor" => Some(Role::Editor),
            "admin" => Some(Role::Admin),
            _ => None,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Role::Viewer => "viewer",
            Role::Editor => "editor",
            Role::Admin => "admin",
        }
    }
}

/// Who is calling.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Identity {
    /// `user`, `token`, `local` (with `--no-auth`) or `anonymous`.
    pub kind: &'static str,
    pub username: String,
    pub platform_admin: bool,
    pub orgs: BTreeMap<String, Role>,
    /// Set for token callers: the key the rate limit counts against.
    #[serde(skip)]
    pub token_key: Option<String>,
    /// Set for cookie callers: they get the CSRF check.
    #[serde(skip)]
    pub cookie: bool,
}

impl Identity {
    pub fn anonymous() -> Identity {
        Identity {
            kind: "anonymous",
            username: "anonymous".into(),
            platform_admin: false,
            orgs: BTreeMap::new(),
            token_key: None,
            cookie: false,
        }
    }

    /// `--no-auth`: one implicit admin of everything.
    pub fn local() -> Identity {
        Identity {
            kind: "local",
            username: crate::store::LOCAL_USER.into(),
            platform_admin: true,
            orgs: BTreeMap::new(),
            token_key: None,
            cookie: false,
        }
    }

    pub fn is_anonymous(&self) -> bool {
        self.kind == "anonymous"
    }

    pub fn role_in(&self, org: &str) -> Option<Role> {
        if self.platform_admin {
            return Some(Role::Admin);
        }
        self.orgs.get(org).copied()
    }
}

/// What a route demands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// Open: health, login, public links (which carry their own token).
    Anonymous,
    /// Any signed-in caller (the answer is filtered by what they may see).
    Authenticated,
    /// At least this role in the org of the path.
    Org(Role),
    Platform,
    /// Not classified: nobody.
    Deny,
}

const ORG_PREFIX: &str = "/api/v1/orgs/{org}";
/// Paths (after the org prefix) whose mutations need an org admin.
const ADMIN_MUTATING: &[&str] = &[
    "/rollups",
    "/retention",
    "/redaction",
    "/destinations",
    "/members",
    "/tokens",
    "/branding",
];
/// Paths whose reads need an org admin (they show people, secrets or rules).
const ADMIN_READING: &[&str] = &["/audit", "/members", "/tokens", "/redaction"];

/// The exhaustive route classification. Anything not named here is `Deny`.
pub fn access_for(method: &Method, template: &str) -> Access {
    let get = method == Method::GET || method == Method::HEAD;
    let post = method == Method::POST;
    match template {
        "/healthz" | "/api/v1/health" | "/api/v1/info" if get => return Access::Anonymous,
        "/api/v1/orgs" if get => return Access::Authenticated,
        "/api/v1/auth/me" if get => return Access::Anonymous,
        "/api/v1/auth/setup" | "/api/v1/auth/login" if post => return Access::Anonymous,
        "/api/v1/auth/oidc/login" | "/api/v1/auth/oidc/callback" if get => {
            return Access::Anonymous;
        }
        "/api/v1/auth/logout" | "/api/v1/auth/password" if post => return Access::Authenticated,
        _ => {}
    }
    // Only routes that exist are classified; a template nobody registered is nobody's.
    if !crate::api::ROUTES.contains(&template) {
        return Access::Deny;
    }
    let writes = crate::api::WRITE_ROUTES
        .iter()
        .any(|(m, p)| *m == method.as_str() && *p == template);
    if template.starts_with("/api/v1/platform/") {
        return if get || writes {
            Access::Platform
        } else {
            Access::Deny
        };
    }
    let Some(rest) = template.strip_prefix(ORG_PREFIX) else {
        return Access::Deny;
    };
    // Public dashboard links carry a token of their own; they never need a login.
    if rest.starts_with("/public/") {
        return if get { Access::Anonymous } else { Access::Deny };
    }
    let under = |list: &[&str]| {
        list.iter()
            .any(|p| rest == *p || rest.starts_with(&format!("{p}/")))
    };
    if get {
        return Access::Org(if under(ADMIN_READING) {
            Role::Admin
        } else if rest == "/destinations" {
            // Editors pick destinations in the alert editor; viewers never see their URLs.
            Role::Editor
        } else {
            Role::Viewer
        });
    }
    if !writes {
        return Access::Deny;
    }
    let admin = under(ADMIN_MUTATING)
        || ((rest.starts_with("/pipelines/") && rest != "/pipelines/dry-run")
            || rest.starts_with("/enrichment/"));
    Access::Org(if admin { Role::Admin } else { Role::Editor })
}

/// Does this identity satisfy `access` for `org` (the org of the path, when there is one)?
pub fn allows(id: &Identity, access: Access, org: Option<&str>) -> bool {
    match access {
        Access::Anonymous => true,
        Access::Authenticated => !id.is_anonymous(),
        Access::Platform => id.platform_admin,
        Access::Deny => false,
        Access::Org(need) => org.and_then(|o| id.role_in(o)).is_some_and(|r| r >= need),
    }
}

// ---- secrets -----------------------------------------------------------------------------------

/// `n` random bytes from the OS as URL-safe text.
pub fn random_secret(n: usize) -> String {
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    let mut b = vec![0u8; n];
    OsRng.fill_bytes(&mut b);
    URL_SAFE_NO_PAD.encode(b)
}

pub fn sha_hex(s: &str) -> String {
    Sha256::digest(s.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A new API token for `org`: `gu_<org>.<43 random characters>`. Only its hash is stored.
pub fn new_token(org: &str) -> String {
    format!("{TOKEN_PREFIX}{org}.{}", random_secret(32))
}

/// The org a token claims to belong to (checked against the store by its hash).
pub fn token_org(token: &str) -> Option<&str> {
    let rest = token.strip_prefix(TOKEN_PREFIX)?;
    let (org, secret) = rest.split_once('.')?;
    (crate::org::valid_name(org) && secret.len() >= 40).then_some(org)
}

pub fn valid_username(u: &str) -> bool {
    let b = u.as_bytes();
    !b.is_empty()
        && b.len() <= 64
        && b[0].is_ascii_alphanumeric()
        && b.iter().all(|c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'.' | b'_' | b'-' | b'@')
        })
}

pub fn check_password(p: &str) -> Result<(), crate::error::ApiError> {
    if p.chars().count() < MIN_PASSWORD || p.len() > 200 {
        return Err(crate::error::ApiError::bad(format!(
            "the password must be {MIN_PASSWORD}-200 characters"
        )));
    }
    Ok(())
}

// ---- stored users --------------------------------------------------------------------------------

/// A person. Memberships live here (org -> role) so one read answers "what may they do".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct User {
    pub username: String,
    #[serde(default)]
    pub display: String,
    /// argon2id hash; empty for single-sign-on users.
    #[serde(default)]
    pub password_hash: String,
    #[serde(default)]
    pub platform_admin: bool,
    #[serde(default)]
    pub disabled: bool,
    /// `local` or `oidc`.
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub orgs: BTreeMap<String, Role>,
}

impl User {
    pub fn identity(&self, cookie: bool) -> Identity {
        Identity {
            kind: "user",
            username: self.username.clone(),
            platform_admin: self.platform_admin,
            orgs: self.orgs.clone(),
            token_key: None,
            cookie,
        }
    }
}

// ---- caches and limits ---------------------------------------------------------------------------

/// Short-lived memory in front of the store, and the rate limiters.
#[derive(Default)]
pub struct AuthState {
    pub cache: Mutex<HashMap<String, (Instant, Identity)>>,
    pub windows: Mutex<HashMap<String, (Instant, u32)>>,
}

impl AuthState {
    pub fn cached(&self, key: &str) -> Option<Identity> {
        let c = self.cache.lock().ok()?;
        c.get(key)
            .filter(|(t, _)| t.elapsed() < CACHE_TTL)
            .map(|(_, i)| i.clone())
    }
    pub fn remember(&self, key: &str, id: &Identity) {
        if let Ok(mut c) = self.cache.lock() {
            if c.len() > 10_000 {
                c.clear();
            }
            c.insert(key.to_string(), (Instant::now(), id.clone()));
        }
    }
    pub fn forget(&self, key: &str) {
        if let Ok(mut c) = self.cache.lock() {
            c.remove(key);
        }
    }
    pub fn forget_all(&self) {
        if let Ok(mut c) = self.cache.lock() {
            c.clear();
        }
    }

    /// Count one use of `key` in a one-minute window; `Err(seconds)` when over `limit`.
    pub fn hit(&self, key: &str, limit: u32) -> Result<(), u64> {
        let Ok(mut w) = self.windows.lock() else {
            return Ok(());
        };
        if w.len() > 50_000 {
            w.retain(|_, (t, _)| t.elapsed() < Duration::from_secs(60));
        }
        let e = w.entry(key.to_string()).or_insert((Instant::now(), 0));
        if e.0.elapsed() >= Duration::from_secs(60) {
            *e = (Instant::now(), 0);
        }
        e.1 += 1;
        if e.1 > limit {
            Err(60 - e.0.elapsed().as_secs().min(59))
        } else {
            Ok(())
        }
    }
}

// ---- users, sessions, tokens (store-backed) ---------------------------------------------------------

use crate::{error::ApiError, store::build::PLATFORM_ORG};
use axum::http::StatusCode;

pub async fn user_get(s: &Shared, username: &str) -> Result<Option<(User, i64)>, ApiError> {
    if !valid_username(username) {
        return Ok(None);
    }
    Ok(store::get(s, PLATFORM_ORG, "user", username)
        .await?
        .and_then(|r| {
            serde_json::from_str::<User>(&r.spec)
                .ok()
                .map(|u| (u, r.created_ms))
        }))
}

pub async fn user_put(s: &Shared, u: &User, created_ms: i64) -> Result<(), ApiError> {
    let r = store::row(
        &u.username,
        &u.username,
        u,
        "platform",
        created_ms,
        s.now_ms(),
    );
    store::put(s, PLATFORM_ORG, "user", &r).await?;
    s.auth.forget_all();
    Ok(())
}

pub async fn users(s: &Shared) -> Result<Vec<User>, ApiError> {
    Ok(store::list(s, PLATFORM_ORG, "user")
        .await?
        .iter()
        .filter_map(|r| serde_json::from_str(&r.spec).ok())
        .collect())
}

#[derive(Serialize, Deserialize)]
struct SessionRec {
    user: String,
    expires_ms: i64,
}

/// Start a session for `username`; returns the cookie value (only its hash is stored).
pub async fn create_session(s: &Shared, username: &str) -> Result<String, ApiError> {
    let cookie = random_secret(32);
    let now = s.now_ms();
    let rec = SessionRec {
        user: username.into(),
        expires_ms: now + (s.cfg.session_hours as i64) * 3_600_000,
    };
    store::put(
        s,
        PLATFORM_ORG,
        "session",
        &store::row(&sha_hex(&cookie), username, &rec, username, now, now),
    )
    .await?;
    Ok(cookie)
}

pub async fn end_session(s: &Shared, cookie: &str) -> Result<(), ApiError> {
    let key = sha_hex(cookie);
    s.auth.forget(&key);
    if let Some(r) = store::get(s, PLATFORM_ORG, "session", &key).await? {
        store::remove(s, PLATFORM_ORG, "session", r, s.now_ms()).await?;
    }
    Ok(())
}

/// The user behind a session cookie, if it exists, has not expired and the user is not disabled.
pub async fn from_cookie(s: &Shared, cookie: &str) -> Result<Option<Identity>, ApiError> {
    let key = sha_hex(cookie);
    if let Some(id) = s.auth.cached(&key) {
        return Ok(Some(id));
    }
    let Some(r) = store::get(s, PLATFORM_ORG, "session", &key).await? else {
        return Ok(None);
    };
    let Ok(rec) = serde_json::from_str::<SessionRec>(&r.spec) else {
        return Ok(None);
    };
    if rec.expires_ms <= s.now_ms() {
        return Ok(None);
    }
    let Some((u, _)) = user_get(s, &rec.user).await? else {
        return Ok(None);
    };
    if u.disabled {
        return Ok(None);
    }
    let id = u.identity(true);
    s.auth.remember(&key, &id);
    Ok(Some(id))
}

#[derive(Serialize, Deserialize)]
pub struct TokenRec {
    pub name: String,
    pub role: Role,
    pub created_by: String,
    #[serde(default)]
    pub expires_ms: Option<i64>,
}

/// The service account behind a bearer token. `Ok(None)`: unknown, revoked or expired.
pub async fn from_bearer(s: &Shared, token: &str) -> Result<Option<Identity>, ApiError> {
    let Some(org) = token_org(token) else {
        return Ok(None);
    };
    let key = sha_hex(token);
    if let Some(id) = s.auth.cached(&key) {
        return Ok(Some(id));
    }
    let Some(r) = store::get(s, org, "token", &key).await? else {
        return Ok(None);
    };
    let Ok(rec) = serde_json::from_str::<TokenRec>(&r.spec) else {
        return Ok(None);
    };
    if rec.expires_ms.is_some_and(|e| e <= s.now_ms()) {
        return Ok(None);
    }
    let id = Identity {
        kind: "token",
        username: format!("token:{}", rec.name),
        platform_admin: false,
        orgs: BTreeMap::from([(org.to_string(), rec.role)]),
        token_key: Some(key.clone()),
        cookie: false,
    };
    s.auth.remember(&key, &id);
    Ok(Some(id))
}

pub fn too_many(retry_s: u64) -> ApiError {
    let _ = retry_s;
    ApiError(
        StatusCode::TOO_MANY_REQUESTS,
        "too many requests; slow down".into(),
    )
}
