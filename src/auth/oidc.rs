//! OpenID Connect sign-in (R-C4): authorization code flow with PKCE. grepui sends the browser to
//! the provider with a `state`, a `nonce` and an S256 code challenge, then on the callback
//! exchanges the code (server to server), verifies the ID token (signature against the provider's
//! keys, issuer, audience, expiry, nonce) and turns the configured claim into org roles. The
//! pending login is a one-time row in the state store, so any replica can finish it.

use super::{Role, User, jwt, random_secret, sha_hex, store};
use crate::{
    api::Shared,
    config::Config,
    error::ApiError,
    store::{LOCAL_USER, build::PLATFORM_ORG},
};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const PENDING_MS: i64 = 10 * 60_000;

#[derive(Debug, Clone, PartialEq)]
pub struct Oidc {
    pub issuer: String,
    pub client_id: String,
    pub client_secret: Option<String>,
    pub auth_url: String,
    pub token_url: String,
    pub jwks_url: String,
    pub redirect_url: String,
    pub scopes: String,
    pub claim: String,
    pub roles: Vec<RoleRule>,
}

/// `claim value = role @ org` or `claim value = platform_admin`.
#[derive(Debug, Clone, PartialEq)]
pub struct RoleRule {
    pub value: String,
    pub grant: Grant,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Grant {
    Platform,
    Org(String, Role),
}

pub fn parse_rule(s: &str) -> Result<RoleRule, String> {
    let (value, grant) = s
        .split_once('=')
        .ok_or("a role mapping looks like value=role@org")?;
    let value = value.trim().to_string();
    if value.is_empty() {
        return Err("a role mapping needs a claim value".into());
    }
    let grant = grant.trim();
    if grant == "platform_admin" {
        return Ok(RoleRule {
            value,
            grant: Grant::Platform,
        });
    }
    let (role, org) = grant
        .split_once('@')
        .ok_or("a role mapping looks like value=role@org")?;
    let role = Role::parse(role.trim()).ok_or("the role must be viewer, editor or admin")?;
    if !crate::org::valid_name(org.trim()) {
        return Err("the org name is not valid".into());
    }
    Ok(RoleRule {
        value,
        grant: Grant::Org(org.trim().to_string(), role),
    })
}

impl Oidc {
    /// The provider settings, when all the required flags are set.
    pub fn from_config(c: &Config) -> Option<Oidc> {
        Some(Oidc {
            issuer: c.oidc_issuer.clone()?,
            client_id: c.oidc_client_id.clone()?,
            client_secret: c.oidc_client_secret.clone(),
            auth_url: c.oidc_auth_url.clone()?,
            token_url: c.oidc_token_url.clone()?,
            jwks_url: c.oidc_jwks_url.clone()?,
            redirect_url: c.oidc_redirect_url.clone()?,
            scopes: c.oidc_scopes.clone(),
            claim: c.oidc_role_claim.clone(),
            roles: c
                .oidc_role_map
                .iter()
                .filter_map(|r| parse_rule(r).ok())
                .collect(),
        })
    }

    pub fn authorize_url(
        &self,
        state: &str,
        nonce: &str,
        challenge: &str,
    ) -> Result<String, ApiError> {
        let mut u = url::Url::parse(&self.auth_url)
            .map_err(|_| ApiError(StatusCode::INTERNAL_SERVER_ERROR, "internal error".into()))?;
        u.query_pairs_mut()
            .append_pair("response_type", "code")
            .append_pair("client_id", &self.client_id)
            .append_pair("redirect_uri", &self.redirect_url)
            .append_pair("scope", &self.scopes)
            .append_pair("state", state)
            .append_pair("nonce", nonce)
            .append_pair("code_challenge", challenge)
            .append_pair("code_challenge_method", "S256");
        Ok(u.to_string())
    }

    /// What the claims grant: platform admin and a role per org (the highest wins).
    pub fn map_roles(&self, claims: &serde_json::Value) -> (bool, BTreeMap<String, Role>) {
        let values: Vec<String> = match &claims[self.claim.as_str()] {
            serde_json::Value::String(s) => vec![s.clone()],
            serde_json::Value::Array(a) => a
                .iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect(),
            _ => vec![],
        };
        let (mut platform, mut orgs) = (false, BTreeMap::new());
        for r in self.roles.iter().filter(|r| values.contains(&r.value)) {
            match &r.grant {
                Grant::Platform => platform = true,
                Grant::Org(o, role) => {
                    let e = orgs.entry(o.clone()).or_insert(*role);
                    *e = (*e).max(*role);
                }
            }
        }
        (platform, orgs)
    }
}

#[derive(Serialize, Deserialize)]
struct Pending {
    nonce: String,
    verifier: String,
    expires_ms: i64,
}

fn fail(msg: &str) -> ApiError {
    ApiError(StatusCode::UNAUTHORIZED, msg.into())
}

/// Start a login: remember state, nonce and verifier, and return the provider URL.
pub async fn begin(s: &Shared, o: &Oidc) -> Result<String, ApiError> {
    let (state, nonce, verifier) = (random_secret(32), random_secret(24), random_secret(48));
    let now = s.now_ms();
    let p = Pending {
        nonce: nonce.clone(),
        verifier: verifier.clone(),
        expires_ms: now + PENDING_MS,
    };
    store::put(
        s,
        PLATFORM_ORG,
        "oidc",
        &store::row(&sha_hex(&state), "login", &p, "oidc", now, now),
    )
    .await?;
    o.authorize_url(&state, &nonce, &jwt::pkce_challenge(&verifier))
}

/// The provider's answer: a code and the state we sent. Returns the signed-in user.
pub async fn finish(s: &Shared, o: &Oidc, code: &str, state: &str) -> Result<User, ApiError> {
    let now = s.now_ms();
    // The pending login is used once, whatever happens next.
    let row = store::get(s, PLATFORM_ORG, "oidc", &sha_hex(state))
        .await?
        .ok_or_else(|| fail("this sign-in was not started here, or was already used"))?;
    store::remove(s, PLATFORM_ORG, "oidc", row.clone(), now).await?;
    let p: Pending =
        serde_json::from_str(&row.spec).map_err(|_| fail("this sign-in is not valid"))?;
    if p.expires_ms <= now {
        return Err(fail("this sign-in took too long; start again"));
    }
    // Built in a block: the serializer is not `Send` and must not live across an await.
    let form_body = {
        let mut form = url::form_urlencoded::Serializer::new(String::new());
        form.append_pair("grant_type", "authorization_code")
            .append_pair("code", code)
            .append_pair("redirect_uri", &o.redirect_url)
            .append_pair("client_id", &o.client_id)
            .append_pair("code_verifier", &p.verifier);
        if let Some(sec) = &o.client_secret {
            form.append_pair("client_secret", sec);
        }
        form.finish()
    };
    let tu = url::Url::parse(&o.token_url).map_err(|_| fail("the token endpoint is not valid"))?;
    let path = format!(
        "{}{}",
        tu.path(),
        tu.query().map(|q| format!("?{q}")).unwrap_or_default()
    );
    let base = tu.origin().ascii_serialization();
    let (code_s, body) = crate::http::request(
        "POST",
        &base,
        &path,
        &[
            ("content-type", "application/x-www-form-urlencoded"),
            ("accept", "application/json"),
        ],
        form_body.as_bytes(),
    )
    .await
    .map_err(|e| {
        tracing::warn!(error = %e, "OIDC token request failed");
        ApiError(
            StatusCode::BAD_GATEWAY,
            "the identity provider could not be reached".into(),
        )
    })?;
    let tok: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
    let Some(id_token) = tok["id_token"].as_str().filter(|_| code_s == 200) else {
        tracing::warn!(status = code_s, "OIDC token endpoint refused the code");
        return Err(fail("the identity provider refused the sign-in"));
    };
    let ju = url::Url::parse(&o.jwks_url).map_err(|_| fail("the key endpoint is not valid"))?;
    let jpath = format!(
        "{}{}",
        ju.path(),
        ju.query().map(|q| format!("?{q}")).unwrap_or_default()
    );
    let (jc, jbody) = crate::http::request(
        "GET",
        &ju.origin().ascii_serialization(),
        &jpath,
        &[("accept", "application/json")],
        &[],
    )
    .await
    .map_err(|_| {
        ApiError(
            StatusCode::BAD_GATEWAY,
            "the identity provider's keys could not be read".into(),
        )
    })?;
    let keys: Vec<jwt::Jwk> = serde_json::from_str::<serde_json::Value>(&jbody)
        .ok()
        .filter(|_| jc == 200)
        .and_then(|v| serde_json::from_value(v["keys"].clone()).ok())
        .unwrap_or_default();
    let want = jwt::Expect {
        issuer: &o.issuer,
        audience: &o.client_id,
        nonce: Some(&p.nonce),
        now_secs: now / 1000,
        leeway_secs: 60,
    };
    let claims = jwt::verify(id_token, &keys, &want).map_err(|e| {
        tracing::warn!(error = %e, "OIDC token rejected");
        fail("the identity provider's token was rejected")
    })?;
    let (platform, orgs) = o.map_roles(&claims);
    if !platform && orgs.is_empty() {
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            "your account has no role in this grepui".into(),
        ));
    }
    // One stable local user per (issuer, subject); roles are re-derived from the claims at every login.
    let sub = claims["sub"].as_str().unwrap_or_default();
    let username = format!("sso-{}", &sha_hex(&format!("{}|{sub}", o.issuer))[..16]);
    let display = ["email", "preferred_username", "name"]
        .iter()
        .find_map(|k| claims[*k].as_str())
        .unwrap_or(sub)
        .chars()
        .take(100)
        .collect();
    let created = super::user_get(s, &username).await?.map_or(now, |(_, c)| c);
    let user = User {
        username,
        display,
        password_hash: String::new(),
        platform_admin: platform,
        disabled: false,
        source: "oidc".into(),
        orgs,
    };
    super::user_put(s, &user, created).await?;
    let _ = LOCAL_USER;
    Ok(user)
}
