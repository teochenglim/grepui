//! Dashboard variables (R-B3). A value reaches SQL only as a bound parameter, never as text: each
//! `$name` outside a string, comment or quoted name becomes `?` (several for a multi-select), and
//! the values travel in the bind list. PromQL has no bind parameters, so a value is substituted
//! as text only if it is made of letters, digits and `_ . : / -`, and the whole expression is then
//! checked like any other PromQL.

use crate::{error::ApiError, query::Bind};
use serde_json::Value;
use std::collections::HashMap;

/// Selected values by variable name, from `var-<name>=<value>` query parameters (repeatable).
pub type Selected = HashMap<String, Vec<String>>;

pub fn valid_name(n: &str) -> bool {
    let b = n.as_bytes();
    !b.is_empty()
        && b.len() <= 32
        && b[0].is_ascii_alphabetic()
        && b.iter().all(|c| c.is_ascii_alphanumeric() || *c == b'_')
        && !n.starts_with("__")
}

pub fn valid_value(v: &str) -> bool {
    !v.is_empty() && v.chars().count() <= 200 && !v.chars().any(char::is_control)
}

/// `var-name=value` pairs from a raw query string.
pub fn selected_from_query(raw: &str) -> Result<Selected, ApiError> {
    let mut m: Selected = HashMap::new();
    for (k, v) in url::form_urlencoded::parse(raw.as_bytes()) {
        if let Some(name) = k.strip_prefix("var-") {
            if !valid_name(name) {
                return Err(ApiError::bad("invalid variable name"));
            }
            if !valid_value(&v) {
                return Err(ApiError::bad(format!(
                    "the value of ${name} must be 1-200 characters without control characters"
                )));
            }
            let e = m.entry(name.to_string()).or_default();
            if e.len() >= 200 {
                return Err(ApiError::bad(format!("too many values for ${name}")));
            }
            e.push(v.into_owned());
        }
    }
    Ok(m)
}

fn decl<'a>(decls: &'a [Value], name: &str) -> Result<&'a Value, ApiError> {
    decls
        .iter()
        .find(|d| d["name"] == name)
        .ok_or_else(|| ApiError::bad(format!("${name} is not a variable of this dashboard")))
}

/// The values to use for a variable: the selection, else its default; checked against what the
/// variable allows.
pub fn values_for(d: &Value, selected: &Selected) -> Result<Vec<String>, ApiError> {
    let name = d["name"].as_str().unwrap_or_default();
    let mut v: Vec<String> = selected.get(name).cloned().unwrap_or_default();
    if v.is_empty() {
        v = d["default"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|s| s.as_str().map(str::to_string))
            .collect();
    }
    if !d["multi"].as_bool().unwrap_or(false) && v.len() > 1 {
        return Err(ApiError::bad(format!(
            "${name} is not multi-select but got {} values",
            v.len()
        )));
    }
    // A custom variable allows only its own values.
    if d["kind"] == "custom" {
        let allowed: Vec<&str> = d["options"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        if let Some(x) = v.iter().find(|x| !allowed.contains(&x.as_str())) {
            return Err(ApiError::bad(format!(
                "{x:?} is not one of the values of ${name}"
            )));
        }
    }
    Ok(v)
}

fn ident_at(c: &[char], i: usize) -> Option<(String, usize)> {
    let s = i + 1;
    if !c
        .get(s)
        .is_some_and(|ch| ch.is_ascii_alphabetic() || *ch == '_')
    {
        return None;
    }
    let mut e = s;
    while c
        .get(e)
        .is_some_and(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
    {
        e += 1;
    }
    Some((c[s..e].iter().collect(), e))
}

/// Replace `$name` in SQL by `?` and collect the values as binds, in order of appearance.
/// Window macros (`$__from`, `$__to`) and anything inside quotes or comments are left alone.
pub fn bind_sql(
    sql: &str,
    decls: &[Value],
    selected: &Selected,
) -> Result<(String, Vec<Bind>), ApiError> {
    let c: Vec<char> = sql.chars().collect();
    let (mut out, mut binds) = (String::new(), vec![]);
    let mut i = 0;
    while i < c.len() {
        let ch = c[i];
        match ch {
            '\'' | '"' | '`' => {
                // A quoted run, with doubled quotes and backslash escapes, copied as is.
                out.push(ch);
                i += 1;
                while i < c.len() {
                    out.push(c[i]);
                    if c[i] == '\\' && ch != '`' && i + 1 < c.len() {
                        out.push(c[i + 1]);
                        i += 2;
                        continue;
                    }
                    if c[i] == ch {
                        i += 1;
                        break;
                    }
                    i += 1;
                }
            }
            '-' if c.get(i + 1) == Some(&'-') => {
                while i < c.len() && c[i] != '\n' {
                    out.push(c[i]);
                    i += 1;
                }
            }
            '/' if c.get(i + 1) == Some(&'*') => {
                while i < c.len() && !(c[i] == '*' && c.get(i + 1) == Some(&'/')) {
                    out.push(c[i]);
                    i += 1;
                }
                if i < c.len() {
                    out.push_str("*/");
                    i += 2;
                }
            }
            '$' => match ident_at(&c, i) {
                Some((name, end)) if !name.starts_with("__") => {
                    let d = decl(decls, &name)?;
                    let vals = values_for(d, selected)?;
                    if vals.is_empty() {
                        // An empty multi-select matches nothing instead of failing the panel.
                        if d["multi"].as_bool().unwrap_or(false) {
                            out.push_str("NULL");
                        } else {
                            return Err(ApiError::bad(format!("${name} has no value; pick one")));
                        }
                    } else {
                        out.push_str(&vec!["?"; vals.len()].join(", "));
                        binds.extend(vals.into_iter().map(Bind::Str));
                    }
                    i = end;
                }
                Some((_, end)) => {
                    out.extend(&c[i..end]);
                    i = end;
                }
                None => {
                    out.push('$');
                    i += 1;
                }
            },
            _ => {
                out.push(ch);
                i += 1;
            }
        }
    }
    Ok((out, binds))
}

fn promql_safe(v: &str) -> bool {
    !v.is_empty()
        && v.len() <= 100
        && v.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b':' | b'/' | b'-'))
}

/// Replace `$name` in PromQL by its value text (several joined with `|` for a regex matcher).
pub fn substitute_promql(
    expr: &str,
    decls: &[Value],
    selected: &Selected,
) -> Result<String, ApiError> {
    let c: Vec<char> = expr.chars().collect();
    let (mut out, mut i) = (String::new(), 0);
    while i < c.len() {
        if c[i] == '$'
            && let Some((name, end)) = ident_at(&c, i)
            && !name.starts_with("__")
        {
            let d = decl(decls, &name)?;
            let vals = values_for(d, selected)?;
            if vals.is_empty() {
                return Err(ApiError::bad(format!("${name} has no value; pick one")));
            }
            if let Some(bad) = vals.iter().find(|v| !promql_safe(v)) {
                return Err(ApiError::bad(format!(
                    "{bad:?} cannot be used in PromQL: values may contain only letters, digits and _ . : / -"
                )));
            }
            out.push_str(&vals.join("|"));
            i = end;
        } else {
            out.push(c[i]);
            i += 1;
        }
    }
    Ok(out)
}
