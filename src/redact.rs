//! Redaction (R-N4): masking rules per field, applied to every data response and export of the
//! API by one layer (`api::redaction_layer`), so no handler can forget it. A rule names a field;
//! it matches an object key equal to the field or ending in `.field` (flat attribute keys such as
//! `http.request.header.authorization`), at any depth, also inside JSON held in a string cell, and
//! a CSV column with that header. Roles: `admin` (when `--admin-writes` is on) and `viewer` (the
//! default, and always on public links). Until access control (v0.11.0) this is the only role
//! model; a rule may exempt `admin`.
//!
//! Limits, stated: a query that renames a column (`SELECT password AS p`) is not recognised by key,
//! and text inside free-form log bodies is not scanned.

use crate::error::ApiError;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

pub const MAX_RULES: usize = 100;
pub const ROLES: &[&str] = &["admin", "viewer"];
pub const MASK_TEXT: &str = "[redacted]";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub field: String,
    /// `full`, `partial` (keep the last 4 characters) or `hash` (8 hex of SHA-256, to correlate).
    #[serde(default = "full")]
    pub mask: String,
    /// Roles that see the real value.
    #[serde(default)]
    pub exempt_roles: Vec<String>,
}

fn full() -> String {
    "full".into()
}

impl Rule {
    pub fn canonical(mut self) -> Result<Rule, ApiError> {
        self.field = self.field.trim().to_string();
        let n = self.field.chars().count();
        if n == 0 || n > 100 || self.field.chars().any(|c| c.is_control() || c == '"') {
            return Err(ApiError::bad("field must be 1-100 characters"));
        }
        if !["full", "partial", "hash"].contains(&self.mask.as_str()) {
            return Err(ApiError::bad("mask must be full, partial or hash"));
        }
        if let Some(r) = self
            .exempt_roles
            .iter()
            .find(|r| !ROLES.contains(&r.as_str()))
        {
            return Err(ApiError::bad(format!("unknown role {r:?}")));
        }
        self.exempt_roles.sort();
        self.exempt_roles.dedup();
        Ok(self)
    }

    fn matches(&self, key: &str) -> bool {
        let (k, f) = (key.to_ascii_lowercase(), self.field.to_ascii_lowercase());
        k == f || k.ends_with(&format!(".{f}"))
    }

    pub fn mask_text(&self, v: &str) -> String {
        match self.mask.as_str() {
            "partial" => {
                let n = v.chars().count();
                if n <= 4 {
                    "*".repeat(n.max(1))
                } else {
                    format!(
                        "{}{}",
                        "*".repeat(n - 4),
                        v.chars().skip(n - 4).collect::<String>()
                    )
                }
            }
            "hash" => Sha256::digest(v.as_bytes())
                .iter()
                .take(4)
                .map(|b| format!("{b:02x}"))
                .collect(),
            _ => MASK_TEXT.into(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Rules(pub Vec<Rule>);

fn scalar_text(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(_) | Value::Bool(_) => Some(v.to_string()),
        _ => None,
    }
}

impl Rules {
    fn active<'a>(&'a self, role: &'a str) -> impl Iterator<Item = &'a Rule> {
        self.0
            .iter()
            .filter(move |r| !r.exempt_roles.iter().any(|x| x == role))
    }

    pub fn is_empty_for(&self, role: &str) -> bool {
        self.active(role).next().is_none()
    }

    fn rule_for<'a>(&'a self, key: &str, role: &'a str) -> Option<&'a Rule> {
        self.active(role).find(|r| r.matches(key))
    }

    /// Mask `v` in place; returns whether anything changed.
    pub fn apply_value(&self, v: &mut Value, role: &str) -> bool {
        if self.is_empty_for(role) {
            return false;
        }
        self.walk(v, role)
    }

    fn walk(&self, v: &mut Value, role: &str) -> bool {
        let mut changed = false;
        match v {
            Value::Object(o) => {
                for (k, x) in o.iter_mut() {
                    if let Some(r) = self.rule_for(k, role) {
                        match scalar_text(x) {
                            Some(t) => *x = Value::String(r.mask_text(&t)),
                            None if x.is_null() => continue,
                            None => *x = Value::String(MASK_TEXT.into()),
                        }
                        changed = true;
                    } else {
                        changed |= self.walk(x, role);
                    }
                }
            }
            Value::Array(a) => {
                for x in a {
                    changed |= self.walk(x, role);
                }
            }
            Value::String(s)
                if s.len() <= 1 << 20 && (s.starts_with('{') || s.starts_with('[')) =>
            {
                if let Ok(mut inner) = serde_json::from_str::<Value>(s)
                    && self.walk(&mut inner, role)
                {
                    *s = inner.to_string();
                    changed = true;
                }
            }
            _ => {}
        }
        changed
    }

    pub fn apply_ndjson(&self, text: &str, role: &str) -> String {
        text.lines()
            .map(|l| match serde_json::from_str::<Value>(l) {
                Ok(mut v) => {
                    self.apply_value(&mut v, role);
                    v.to_string()
                }
                // A line that is not JSON is never passed through unchecked.
                Err(_) => MASK_TEXT.to_string(),
            })
            .collect::<Vec<_>>()
            .join("\n")
            + "\n"
    }

    pub fn apply_csv(&self, text: &str, role: &str) -> String {
        let Ok(mut rows) = crate::csv::parse(text, usize::MAX, 10_000) else {
            return MASK_TEXT.to_string();
        };
        let Some(head) = rows.first().cloned() else {
            return text.to_string();
        };
        for row in rows.iter_mut().skip(1) {
            for (i, cell) in row.iter_mut().enumerate() {
                if let Some(r) = head.get(i).and_then(|h| self.rule_for(h, role)) {
                    if !cell.is_empty() {
                        *cell = r.mask_text(cell);
                    }
                } else if cell.starts_with('{') || cell.starts_with('[') {
                    let mut v = Value::String(std::mem::take(cell));
                    self.apply_value(&mut v, role);
                    *cell = v.as_str().unwrap_or_default().to_string();
                }
            }
        }
        crate::csv::write(&rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(masks: &[(&str, &str)]) -> Rules {
        Rules(
            masks
                .iter()
                .map(|(f, m)| Rule {
                    field: f.to_string(),
                    mask: m.to_string(),
                    exempt_roles: vec!["admin".into()],
                })
                .collect(),
        )
    }

    #[test]
    fn masks() {
        let r = rules(&[("password", "full"), ("card", "partial"), ("email", "hash")]);
        assert_eq!(r.0[1].mask_text("4111111111111111"), "************1111");
        assert_eq!(r.0[2].mask_text("a@b.c").len(), 8);
        let mut v = serde_json::json!({"a": {"user.password": "x", "n": 1}, "list": [{"card": 12345678}], "s": "{\"email\":\"a@b.c\"}"});
        assert!(r.apply_value(&mut v, "viewer"));
        assert_eq!(v["a"]["user.password"], MASK_TEXT);
        assert_eq!(v["a"]["n"], 1);
        assert_eq!(v["list"][0]["card"], "****5678");
        assert!(!v["s"].as_str().unwrap().contains("a@b.c"));
        let mut w = serde_json::json!({"password": "x"});
        assert!(!r.apply_value(&mut w, "admin"));
    }
}
