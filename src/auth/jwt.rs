//! JWT verification for OIDC sign-in (R-C4): compact JWS with RS256 or ES256 only, checked against
//! the identity provider's published keys. `none` and the HMAC algorithms are refused (an attacker
//! must never pick the algorithm). Claims checked: issuer, audience (and `azp`), expiry, not-before,
//! issued-at and the nonce of this login.

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD as B64};
use ring::{rsa::PublicKeyComponents, signature};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct Jwk {
    pub kty: String,
    #[serde(default)]
    pub kid: Option<String>,
    #[serde(default)]
    pub alg: Option<String>,
    #[serde(default, rename = "use")]
    pub use_: Option<String>,
    #[serde(default)]
    pub n: Option<String>,
    #[serde(default)]
    pub e: Option<String>,
    #[serde(default)]
    pub crv: Option<String>,
    #[serde(default)]
    pub x: Option<String>,
    #[serde(default)]
    pub y: Option<String>,
}

pub struct Expect<'a> {
    pub issuer: &'a str,
    pub audience: &'a str,
    pub nonce: Option<&'a str>,
    pub now_secs: i64,
    pub leeway_secs: i64,
}

fn part(s: &str) -> Result<Vec<u8>, String> {
    B64.decode(s)
        .map_err(|_| "the token is not valid base64url".to_string())
}

fn verify_signature(alg: &str, key: &Jwk, msg: &[u8], sig: &[u8]) -> Result<(), String> {
    let bad = || "the signature is not valid".to_string();
    match (alg, key.kty.as_str()) {
        ("RS256", "RSA") => {
            let n = part(key.n.as_deref().ok_or("the key has no modulus")?)?;
            let e = part(key.e.as_deref().ok_or("the key has no exponent")?)?;
            PublicKeyComponents {
                n: n.as_slice(),
                e: e.as_slice(),
            }
            .verify(&signature::RSA_PKCS1_2048_8192_SHA256, msg, sig)
            .map_err(|_| bad())
        }
        ("ES256", "EC") if key.crv.as_deref() == Some("P-256") => {
            let x = part(key.x.as_deref().ok_or("the key has no x")?)?;
            let y = part(key.y.as_deref().ok_or("the key has no y")?)?;
            if x.len() != 32 || y.len() != 32 {
                return Err("the key is not a P-256 point".into());
            }
            let mut point = vec![4u8];
            point.extend(&x);
            point.extend(&y);
            signature::UnparsedPublicKey::new(&signature::ECDSA_P256_SHA256_FIXED, point)
                .verify(msg, sig)
                .map_err(|_| bad())
        }
        _ => Err("the key does not match the token's algorithm".into()),
    }
}

/// Verify `token` and return its claims.
pub fn verify(token: &str, keys: &[Jwk], want: &Expect) -> Result<Value, String> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 || token.len() > 16_384 {
        return Err("the token is not a signed JWT".into());
    }
    let header: Value =
        serde_json::from_slice(&part(parts[0])?).map_err(|_| "the token header is not JSON")?;
    let alg = header["alg"].as_str().unwrap_or_default();
    if alg != "RS256" && alg != "ES256" {
        return Err(format!("the algorithm {alg:?} is not accepted"));
    }
    let kid = header["kid"].as_str();
    let usable: Vec<&Jwk> = keys
        .iter()
        .filter(|k| {
            k.use_.as_deref().is_none_or(|u| u == "sig")
                && k.alg.as_deref().is_none_or(|a| a == alg)
        })
        .filter(|k| match (kid, k.kid.as_deref()) {
            (Some(a), Some(b)) => a == b,
            (None, _) => true,
            (Some(_), None) => keys.len() == 1,
        })
        .collect();
    if usable.is_empty() {
        return Err("no key of the identity provider matches the token".into());
    }
    let msg = format!("{}.{}", parts[0], parts[1]);
    let sig = part(parts[2])?;
    if !usable
        .iter()
        .any(|k| verify_signature(alg, k, msg.as_bytes(), &sig).is_ok())
    {
        return Err("the signature is not valid".into());
    }
    let claims: Value =
        serde_json::from_slice(&part(parts[1])?).map_err(|_| "the token claims are not JSON")?;
    if claims["iss"].as_str() != Some(want.issuer) {
        return Err("the issuer is not the configured one".into());
    }
    let aud_ok = match &claims["aud"] {
        Value::String(a) => a == want.audience,
        Value::Array(a) => a.iter().any(|x| x.as_str() == Some(want.audience)),
        _ => false,
    };
    if !aud_ok {
        return Err("the token is for another audience".into());
    }
    if claims["aud"].as_array().is_some_and(|a| a.len() > 1)
        && claims["azp"].as_str() != Some(want.audience)
    {
        return Err("the authorized party is not this client".into());
    }
    let exp = claims["exp"].as_i64().ok_or("the token has no expiry")?;
    if exp + want.leeway_secs <= want.now_secs {
        return Err("the token has expired".into());
    }
    if claims["nbf"]
        .as_i64()
        .is_some_and(|n| n - want.leeway_secs > want.now_secs)
    {
        return Err("the token is not valid yet".into());
    }
    if claims["iat"]
        .as_i64()
        .is_some_and(|i| i - want.leeway_secs > want.now_secs)
    {
        return Err("the token was issued in the future".into());
    }
    if let Some(n) = want.nonce
        && claims["nonce"].as_str() != Some(n)
    {
        return Err("the nonce does not match this login".into());
    }
    if claims["sub"].as_str().is_none_or(str::is_empty) {
        return Err("the token has no subject".into());
    }
    Ok(claims)
}

/// PKCE: the S256 challenge of a verifier.
pub fn pkce_challenge(verifier: &str) -> String {
    use sha2::{Digest, Sha256};
    B64.encode(Sha256::digest(verifier.as_bytes()))
}
