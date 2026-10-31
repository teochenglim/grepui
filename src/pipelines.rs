//! Pipelines (R-N1): grepui manages GreptimeDB's own pipelines (YAML, applied at ingest) through
//! its HTTP API. Pipelines are server-wide in GreptimeDB, so each org's are stored as
//! `<org>.<name>` and only those are listed or touched. Create and delete need `--admin-writes`
//! and are logged; a dry-run changes nothing.

use crate::{error::ApiError, query::Q};
use serde_json::{Value, json};

pub const MAX_YAML: usize = 32_000;
pub const MAX_DATA: usize = 64_000;
pub const MAX_SAMPLE_ROWS: usize = 20;

/// `[a-z0-9_]{1,64}`.
pub fn short_name(n: &str) -> Result<&str, ApiError> {
    if n.is_empty()
        || n.len() > 64
        || !n
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    {
        return Err(ApiError::bad(
            "the pipeline name must be 1-64 lowercase letters, digits or _",
        ));
    }
    Ok(n)
}

pub fn full_name(org: &str, short: &str) -> String {
    format!("{org}.{short}")
}

/// The part after `<org>.`, when `full` belongs to the org.
pub fn short_of<'a>(org: &str, full: &'a str) -> Option<&'a str> {
    full.strip_prefix(org)?
        .strip_prefix('.')
        .filter(|s| short_name(s).is_ok())
}

/// GreptimeDB names a version by its creation time.
pub fn version_text(ns: i64) -> String {
    let secs = ns.div_euclid(1_000_000_000);
    let nanos = ns.rem_euclid(1_000_000_000);
    chrono::DateTime::from_timestamp(secs, 0)
        .map(|d| format!("{}.{nanos:09}", d.format("%Y-%m-%d %H:%M:%S")))
        .unwrap_or_default()
}

fn pct(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-_.~:".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

pub fn path_for(full: &str) -> String {
    format!("/v1/pipelines/{}", pct(full))
}

pub fn path_with_version(full: &str, version: &str) -> String {
    format!("{}?version={}", path_for(full), pct(version))
}

pub const DRYRUN_PATH: &str = "/v1/pipelines/_dryrun";

/// Every pipeline version on the server (fixed statement; the org filter is applied in code).
pub fn list_q() -> Q {
    Q::new(
        "pipeline_list",
        "SELECT name, CAST(created_at AS BIGINT) AS created_ns FROM greptime_private.pipelines ORDER BY name, created_ns LIMIT 2000",
        vec![],
    )
}

pub fn check_yaml(yaml: &str) -> Result<(), ApiError> {
    if yaml.trim().is_empty() || yaml.len() > MAX_YAML || yaml.contains('\0') {
        return Err(ApiError::bad(
            "the pipeline YAML must be 1-32000 characters",
        ));
    }
    Ok(())
}

/// The body for `_dryrun`: a stored pipeline by name or inline YAML, plus 1-20 sample records.
pub fn dry_body(name: Option<&str>, yaml: Option<&str>, data: &Value) -> Result<String, ApiError> {
    let rows = data
        .as_array()
        .ok_or_else(|| ApiError::bad("data must be an array of objects"))?;
    if rows.is_empty() || rows.len() > MAX_SAMPLE_ROWS || rows.iter().any(|r| !r.is_object()) {
        return Err(ApiError::bad("data must hold 1-20 objects"));
    }
    let data = data.to_string();
    if data.len() > MAX_DATA {
        return Err(ApiError::bad("the sample data is too large"));
    }
    Ok(match (name, yaml) {
        (Some(n), None) => json!({"pipeline_name": n, "data": data}),
        (None, Some(y)) => {
            check_yaml(y)?;
            json!({"pipeline": y, "data": data})
        }
        _ => return Err(ApiError::bad("give either name or yaml")),
    }
    .to_string())
}

/// GreptimeDB's dry-run answer as columns and plain row objects.
pub fn dry_result(raw: &str) -> Result<Value, String> {
    let v: Value = serde_json::from_str(raw)
        .map_err(|_| "GreptimeDB answered with something that is not JSON".to_string())?;
    if let Some(e) = v.get("error").and_then(Value::as_str) {
        return Err(e.chars().take(500).collect());
    }
    let first = v
        .as_array()
        .and_then(|a| a.first())
        .ok_or("the dry-run returned nothing")?;
    let columns: Vec<Value> = first["schema"]
        .as_array()
        .map(|s| s.iter().map(|c| json!({"name": c["name"], "type": c["data_type"], "semantic": c["column_type"]})).collect())
        .unwrap_or_default();
    let rows: Vec<Value> = first["rows"]
        .as_array()
        .map(|rs| {
            rs.iter()
                .map(|r| {
                    let mut o = serde_json::Map::new();
                    for cell in r.as_array().into_iter().flatten() {
                        if let Some(k) = cell["key"].as_str() {
                            o.insert(k.to_string(), cell["value"].clone());
                        }
                    }
                    Value::Object(o)
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(json!({"columns": columns, "rows": rows}))
}

/// The error text of a failed GreptimeDB HTTP call.
pub fn error_text(body: &str) -> String {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v["error"].as_str().map(|s| s.chars().take(500).collect()))
        .unwrap_or_else(|| "GreptimeDB refused the request".into())
}

/// The gate for what grepui sends to GreptimeDB's pipeline API: only `/v1/pipelines/<org>.<name>`
/// (with an optional version) and the dry-run path.
pub fn check_path(org: &str, path: &str) -> bool {
    if path == DRYRUN_PATH {
        return true;
    }
    let Some(rest) = path.strip_prefix("/v1/pipelines/") else {
        return false;
    };
    let (name, query) = rest
        .split_once("?version=")
        .map_or((rest, None), |(n, v)| (n, Some(v)));
    let ok_name = name
        .strip_prefix(&format!("{org}."))
        .is_some_and(|s| short_name(s).is_ok());
    ok_name
        && query.is_none_or(|v| {
            v.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_.~:%".contains(&b))
        })
}
