//! Dashboards (R-B1 – R-B6): the spec of a dashboard, validated into one canonical JSON form so
//! that export, import and save all see the same document. Panels run their saved queries on the
//! server (clients never send SQL), so a public link can only ever show what the dashboard shows.

pub mod panels;
pub mod time;
pub mod vars;

use crate::error::ApiError;
use serde_json::{Map, Value, json};

pub const SPEC_VERSION: i64 = 1;
pub const MAX_TABS: usize = 20;
pub const MAX_PANELS: usize = 50;
pub const MAX_VARS: usize = 20;
pub const MAX_QUERY: usize = 8_000;
pub const MAX_CONTENT: usize = 20_000;
pub const MAX_TITLE: usize = 100;
pub const EXPORT_MARK: &str = "grepui_dashboard";
pub const BUNDLE_MARK: &str = "grepui_dashboards";

fn bad<T>(path: &str, msg: impl AsRef<str>) -> Result<T, ApiError> {
    Err(ApiError::bad(if path.is_empty() {
        msg.as_ref().to_string()
    } else {
        format!("{path}: {}", msg.as_ref())
    }))
}

fn obj<'a>(v: &'a Value, path: &str) -> Result<&'a Map<String, Value>, ApiError> {
    v.as_object()
        .map_or_else(|| bad(path, "must be an object"), Ok)
}

fn only_keys(o: &Map<String, Value>, path: &str, allowed: &[&str]) -> Result<(), ApiError> {
    for k in o.keys() {
        if !allowed.contains(&k.as_str()) {
            return bad(path, format!("unknown key {k:?}"));
        }
    }
    Ok(())
}

fn text(
    o: &Map<String, Value>,
    k: &str,
    path: &str,
    max: usize,
    required: bool,
) -> Result<String, ApiError> {
    match o.get(k) {
        None | Some(Value::Null) if !required => Ok(String::new()),
        None | Some(Value::Null) => bad(&format!("{path}.{k}"), "is required"),
        Some(Value::String(s)) => {
            let t = s.trim();
            if t.chars().count() > max {
                return bad(
                    &format!("{path}.{k}"),
                    format!("is longer than {max} characters"),
                );
            }
            if required && t.is_empty() {
                return bad(&format!("{path}.{k}"), "must not be empty");
            }
            Ok(s.clone())
        }
        Some(_) => bad(&format!("{path}.{k}"), "must be a string"),
    }
}

fn title(o: &Map<String, Value>, path: &str) -> Result<String, ApiError> {
    let t = text(o, "title", path, MAX_TITLE, true)?;
    if t.chars().any(char::is_control) {
        return bad(
            &format!("{path}.title"),
            "must not contain control characters",
        );
    }
    Ok(t.trim().to_string())
}

fn new_id() -> String {
    format!("{:08x}", rand::random::<u32>())
}

fn id(o: &Map<String, Value>, path: &str) -> Result<String, ApiError> {
    match o.get("id") {
        None | Some(Value::Null) => Ok(new_id()),
        Some(Value::String(s))
            if !s.is_empty()
                && s.len() <= 32
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-')) =>
        {
            Ok(s.clone())
        }
        Some(_) => bad(
            &format!("{path}.id"),
            "must be 1-32 letters, digits, - or _",
        ),
    }
}

/// A panel's query: a language and its text.
fn query(o: &Map<String, Value>, path: &str) -> Result<Value, ApiError> {
    let q = obj(
        o.get("query").unwrap_or(&Value::Null),
        &format!("{path}.query"),
    )
    .map_err(|_| ApiError::bad(format!("{path}.query: is required for this panel type")))?;
    let p = format!("{path}.query");
    only_keys(q, &p, &["kind", "text"])?;
    let kind = text(q, "kind", &p, 10, true)?;
    if !["sql", "promql"].contains(&kind.as_str()) {
        return bad(&format!("{p}.kind"), "must be sql or promql");
    }
    let t = text(q, "text", &p, MAX_QUERY, true)?;
    Ok(json!({"kind": kind, "text": t}))
}

fn options(o: &Map<String, Value>, path: &str) -> Result<Value, ApiError> {
    let Some(v) = o.get("options").filter(|v| !v.is_null()) else {
        return Ok(json!({}));
    };
    let p = format!("{path}.options");
    let m = obj(v, &p)?;
    only_keys(
        m,
        &p,
        &["unit", "min", "max", "decimals", "legend", "pivot"],
    )?;
    let mut out = Map::new();
    for (k, val) in m {
        match (k.as_str(), val) {
            ("unit", Value::String(s))
                if s.chars().count() <= 16 && !s.chars().any(char::is_control) => {}
            ("min" | "max", Value::Number(_)) => {}
            ("decimals", Value::Number(n)) if n.as_i64().is_some_and(|d| (0..=6).contains(&d)) => {}
            ("legend", Value::Bool(_)) => {}
            ("pivot", Value::Object(pv))
                if ["row", "column", "value"].iter().all(|k| {
                    pv.get(*k).and_then(Value::as_str).is_some_and(|c| {
                        !c.is_empty() && c.len() <= 64 && !c.chars().any(char::is_control)
                    })
                }) && pv.len() == 3 => {}
            _ => return bad(&format!("{p}.{k}"), "has an invalid value"),
        }
        out.insert(k.clone(), val.clone());
    }
    Ok(Value::Object(out))
}

fn panel(v: &Value, path: &str) -> Result<Value, ApiError> {
    let o = obj(v, path)?;
    only_keys(
        o,
        path,
        &[
            "id", "type", "title", "w", "h", "query", "options", "content",
        ],
    )?;
    let ty = text(o, "type", path, 24, true)?;
    let Some(info) = panels::info(&ty) else {
        return bad(
            &format!("{path}.type"),
            format!("unknown panel type {ty:?}"),
        );
    };
    let dim = |k: &str, default: i64, max: i64| -> Result<i64, ApiError> {
        match o.get(k) {
            None | Some(Value::Null) => Ok(default),
            Some(n) => n.as_i64().filter(|x| (1..=max).contains(x)).map_or_else(
                || {
                    bad(
                        &format!("{path}.{k}"),
                        format!("must be a whole number from 1 to {max}"),
                    )
                },
                Ok,
            ),
        }
    };
    let mut out = json!({
        "id": id(o, path)?, "type": ty, "title": title(o, path)?,
        "w": dim("w", 6, 12)?, "h": dim("h", 3, 8)?, "options": options(o, path)?,
    });
    if info.static_content {
        if o.contains_key("query") && !o["query"].is_null() {
            return bad(
                &format!("{path}.query"),
                "this panel type has content, not a query",
            );
        }
        out["content"] = json!(text(o, "content", path, MAX_CONTENT, false)?);
    } else {
        if o.contains_key("content") && !o["content"].is_null() {
            return bad(
                &format!("{path}.content"),
                "only markdown and html panels have content",
            );
        }
        out["query"] = query(o, path)?;
    }
    Ok(out)
}

fn variable(v: &Value, path: &str) -> Result<Value, ApiError> {
    let o = obj(v, path)?;
    only_keys(
        o,
        path,
        &[
            "name", "label", "kind", "query", "options", "multi", "default",
        ],
    )?;
    let name = text(o, "name", path, 32, true)?;
    if !vars::valid_name(&name) {
        return bad(
            &format!("{path}.name"),
            "must start with a letter and use letters, digits and _ (names starting with __ are reserved)",
        );
    }
    let kind = text(o, "kind", path, 10, true)?;
    let mut out = json!({
        "name": name, "label": text(o, "label", path, MAX_TITLE, false)?, "kind": kind,
        "multi": o.get("multi").and_then(Value::as_bool).unwrap_or(false),
    });
    let strings = |k: &str| -> Result<Vec<String>, ApiError> {
        let Some(a) = o.get(k).filter(|v| !v.is_null()) else {
            return Ok(vec![]);
        };
        let arr = a.as_array().map_or_else(
            || bad(&format!("{path}.{k}"), "must be a list of strings"),
            Ok,
        )?;
        if arr.len() > 200 {
            return bad(&format!("{path}.{k}"), "has more than 200 values");
        }
        arr.iter()
            .enumerate()
            .map(|(i, s)| {
                s.as_str()
                    .filter(|s| vars::valid_value(s))
                    .map(str::to_string)
                    .map_or_else(
                        || {
                            bad(
                                &format!("{path}.{k}[{i}]"),
                                "must be a string of 1-200 characters without control characters",
                            )
                        },
                        Ok,
                    )
            })
            .collect()
    };
    match kind.as_str() {
        "query" => {
            if o.contains_key("options") && !o["options"].is_null() {
                return bad(
                    &format!("{path}.options"),
                    "a query variable gets its values from the query",
                );
            }
            out["query"] = json!(text(o, "query", path, MAX_QUERY, true)?);
        }
        "custom" => {
            if o.contains_key("query") && !o["query"].is_null() {
                return bad(
                    &format!("{path}.query"),
                    "a custom variable lists its values in options",
                );
            }
            let opts = strings("options")?;
            if opts.is_empty() {
                return bad(
                    &format!("{path}.options"),
                    "a custom variable needs at least one value",
                );
            }
            out["options"] = json!(opts);
        }
        _ => return bad(&format!("{path}.kind"), "must be query or custom"),
    }
    let def = strings("default")?;
    if def.len() > 1 && !out["multi"].as_bool().unwrap_or(false) {
        return bad(
            &format!("{path}.default"),
            "has several values but the variable is not multi-select",
        );
    }
    out["default"] = json!(def);
    Ok(out)
}

/// Validate a dashboard spec and return its canonical form: known keys only, defaults filled,
/// ids kept (or assigned when missing). `canonical(canonical(x)) == canonical(x)`.
pub fn canonical(v: &Value) -> Result<Value, ApiError> {
    let o = obj(v, "spec")?;
    only_keys(
        o,
        "spec",
        &["version", "time", "refresh", "variables", "tabs"],
    )?;
    if let Some(ver) = o.get("version")
        && ver.as_i64() != Some(SPEC_VERSION)
    {
        return bad("spec.version", format!("must be {SPEC_VERSION}"));
    }
    let range = match o.get("time") {
        None | Some(Value::Null) => "1h".to_string(),
        Some(t) => {
            let m = obj(t, "spec.time")?;
            only_keys(m, "spec.time", &["range"])?;
            let r = text(m, "range", "spec.time", 8, true)?;
            if !time::RANGES.contains(&r.as_str()) {
                return bad(
                    "spec.time.range",
                    format!("must be one of {}", time::RANGES.join(", ")),
                );
            }
            r
        }
    };
    let refresh = match o.get("refresh") {
        None | Some(Value::Null) => "off".to_string(),
        Some(Value::String(r)) if time::refresh_ms(r).is_some() => r.clone(),
        Some(_) => {
            return bad(
                "spec.refresh",
                format!("must be one of {}", time::REFRESH.join(", ")),
            );
        }
    };
    let mut variables = vec![];
    if let Some(vs) = o.get("variables").filter(|v| !v.is_null()) {
        let arr = vs
            .as_array()
            .map_or_else(|| bad("spec.variables", "must be a list"), Ok)?;
        if arr.len() > MAX_VARS {
            return bad(
                "spec.variables",
                format!("has more than {MAX_VARS} variables"),
            );
        }
        for (i, v) in arr.iter().enumerate() {
            variables.push(variable(v, &format!("spec.variables[{i}]"))?);
        }
        let mut names: Vec<&str> = variables
            .iter()
            .filter_map(|v| v["name"].as_str())
            .collect();
        names.sort_unstable();
        if let Some(w) = names.windows(2).find(|w| w[0] == w[1]) {
            return bad(
                "spec.variables",
                format!("variable {:?} is declared twice", w[0]),
            );
        }
    }
    let tabs_in = o
        .get("tabs")
        .and_then(Value::as_array)
        .map_or_else(|| bad("spec.tabs", "is required and must be a list"), Ok)?;
    if tabs_in.is_empty() || tabs_in.len() > MAX_TABS {
        return bad("spec.tabs", format!("needs 1 to {MAX_TABS} tabs"));
    }
    let mut tabs = vec![];
    let (mut tab_ids, mut panel_ids) = (vec![], vec![]);
    for (i, t) in tabs_in.iter().enumerate() {
        let p = format!("spec.tabs[{i}]");
        let m = obj(t, &p)?;
        only_keys(m, &p, &["id", "title", "panels"])?;
        let tid = id(m, &p)?;
        if tab_ids.contains(&tid) {
            return bad(&format!("{p}.id"), format!("duplicate id {tid:?}"));
        }
        tab_ids.push(tid.clone());
        let ps = match m.get("panels") {
            None | Some(Value::Null) => vec![],
            Some(Value::Array(a)) => a.clone(),
            Some(_) => return bad(&format!("{p}.panels"), "must be a list"),
        };
        if ps.len() > MAX_PANELS {
            return bad(
                &format!("{p}.panels"),
                format!("has more than {MAX_PANELS} panels"),
            );
        }
        let mut panels_out = vec![];
        for (j, pv) in ps.iter().enumerate() {
            let pc = panel(pv, &format!("{p}.panels[{j}]"))?;
            let pid = pc["id"].as_str().unwrap_or_default().to_string();
            if panel_ids.contains(&pid) {
                return bad(
                    &format!("{p}.panels[{j}].id"),
                    format!("duplicate id {pid:?}"),
                );
            }
            panel_ids.push(pid);
            panels_out.push(pc);
        }
        tabs.push(json!({"id": tid, "title": title(m, &p)?, "panels": panels_out}));
    }
    Ok(json!({
        "version": SPEC_VERSION, "time": {"range": range}, "refresh": refresh,
        "variables": variables, "tabs": tabs,
    }))
}

/// Find a panel by id in a canonical spec.
pub fn find_panel<'a>(spec: &'a Value, panel_id: &str) -> Option<&'a Value> {
    spec["tabs"]
        .as_array()?
        .iter()
        .flat_map(|t| t["panels"].as_array().into_iter().flatten())
        .find(|p| p["id"] == panel_id)
}

/// The spec as a public viewer gets it: everything except the queries.
pub fn public_view(spec: &Value) -> Value {
    let mut s = spec.clone();
    if let Some(tabs) = s["tabs"].as_array_mut() {
        for t in tabs {
            for p in t["panels"].as_array_mut().into_iter().flatten() {
                if let Some(o) = p.as_object_mut() {
                    o.remove("query");
                }
            }
        }
    }
    if let Some(vs) = s["variables"].as_array_mut() {
        for v in vs {
            if let Some(o) = v.as_object_mut() {
                o.remove("query");
            }
        }
    }
    s
}

/// A playlist: dashboards shown in turn on a wall display.
pub fn playlist_canonical(v: &Value) -> Result<Value, ApiError> {
    let o = obj(v, "spec")?;
    only_keys(o, "spec", &["dashboards", "interval_s"])?;
    let ids: Vec<String> = o
        .get("dashboards")
        .and_then(Value::as_array)
        .map_or_else(
            || bad("spec.dashboards", "is required and must be a list"),
            Ok,
        )?
        .iter()
        .map(|d| {
            d.as_str()
                .filter(|s| {
                    s.len() == 16 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
                })
                .map(str::to_string)
        })
        .collect::<Option<_>>()
        .map_or_else(
            || bad("spec.dashboards", "must hold dashboard ids (16 hex digits)"),
            Ok,
        )?;
    if ids.is_empty() || ids.len() > 30 {
        return bad("spec.dashboards", "needs 1 to 30 dashboards");
    }
    let interval = match o.get("interval_s") {
        None | Some(Value::Null) => time::PLAYLIST_DEFAULT_S,
        Some(n) => n
            .as_i64()
            .filter(|s| (time::PLAYLIST_MIN_S..=3600).contains(s))
            .map_or_else(
                || {
                    bad(
                        "spec.interval_s",
                        format!("must be {} to 3600 seconds", time::PLAYLIST_MIN_S),
                    )
                },
                Ok,
            )?,
    };
    Ok(json!({"dashboards": ids, "interval_s": interval}))
}
