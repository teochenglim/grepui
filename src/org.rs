//! Orgs (tenants): `{org}` in every route resolves to a database and table names (R-F5, ADR 0003).
//! The resolved database comes from configuration, never from request text.

use crate::{config::Config, error::ApiError};
use std::{collections::BTreeMap, sync::RwLock};

pub const DEFAULT_ORG: &str = "default";

#[derive(Debug, Clone, PartialEq)]
pub struct Org {
    pub name: String,
    pub database: String,
    pub traces_table: String,
    pub logs_table: String,
}

/// The orgs this process serves. `default` comes from the flags; others come from the state
/// store (v0.11.0) and are reloaded as they change. An `Org` is leaked once per distinct
/// definition so handlers can keep borrowing `&Org` across awaits; orgs are few and a definition
/// only changes when a platform admin edits it.
#[derive(Debug)]
pub struct Orgs(RwLock<BTreeMap<String, &'static Org>>);

/// Lowercase letters, digits, `-` and `_`; 1..=32 chars; starts alphanumeric.
pub fn valid_name(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty()
        && b.len() <= 32
        && b[0].is_ascii_alphanumeric()
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-' || *c == b'_')
}

impl Orgs {
    /// One `default` org from the flags.
    pub fn from_config(cfg: &Config) -> anyhow::Result<Orgs> {
        let org = Org {
            name: DEFAULT_ORG.into(),
            database: cfg.database()?,
            traces_table: cfg.traces_table.clone(),
            logs_table: cfg.logs_table.clone(),
        };
        let o = Orgs(RwLock::new(BTreeMap::new()));
        o.set(org);
        Ok(o)
    }

    pub fn resolve(&self, name: &str) -> Result<&'static Org, ApiError> {
        if !valid_name(name) {
            return Err(ApiError::bad("invalid org name"));
        }
        self.0
            .read()
            .ok()
            .and_then(|m| m.get(name).copied())
            .ok_or_else(|| ApiError::not_found("unknown org"))
    }

    pub fn list(&self) -> Vec<&'static Org> {
        self.0
            .read()
            .map(|m| m.values().copied().collect())
            .unwrap_or_default()
    }

    /// Add or replace an org; an identical definition keeps the existing one.
    pub fn set(&self, org: Org) {
        let Ok(mut m) = self.0.write() else { return };
        if m.get(&org.name).is_some_and(|o| **o == org) {
            return;
        }
        m.insert(org.name.clone(), Box::leak(Box::new(org)));
    }

    /// Forget an org (never `default`).
    pub fn remove(&self, name: &str) {
        if name == DEFAULT_ORG {
            return;
        }
        if let Ok(mut m) = self.0.write() {
            m.remove(name);
        }
    }
}
