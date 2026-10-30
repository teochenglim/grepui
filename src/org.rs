//! Orgs (tenants): `{org}` in every route resolves to a database and table names (R-F5, ADR 0003).
//! The resolved database comes from configuration, never from request text.

use crate::{config::Config, error::ApiError};
use std::collections::BTreeMap;

pub const DEFAULT_ORG: &str = "default";

#[derive(Debug, Clone, PartialEq)]
pub struct Org {
    pub name: String,
    pub database: String,
    pub traces_table: String,
    pub logs_table: String,
}

#[derive(Debug, Clone)]
pub struct Orgs(BTreeMap<String, Org>);

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
    /// One `default` org from the flags. Metadata-backed orgs arrive with v0.11.0.
    pub fn from_config(cfg: &Config) -> anyhow::Result<Orgs> {
        let org = Org {
            name: DEFAULT_ORG.into(),
            database: cfg.database()?,
            traces_table: cfg.traces_table.clone(),
            logs_table: cfg.logs_table.clone(),
        };
        Ok(Orgs(BTreeMap::from([(org.name.clone(), org)])))
    }

    pub fn resolve(&self, name: &str) -> Result<&Org, ApiError> {
        if !valid_name(name) {
            return Err(ApiError::bad("invalid org name"));
        }
        self.0
            .get(name)
            .ok_or_else(|| ApiError::not_found("unknown org"))
    }

    pub fn list(&self) -> Vec<&Org> {
        self.0.values().collect()
    }
}
