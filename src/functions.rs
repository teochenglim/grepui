//! Saved SQL functions (R-N2): a named snippet with positional parameters, `$1`, `$2`, ... in its
//! body, called as `@name(1, 'x')` from SQL mode and dashboard SQL panels only. Arguments must be
//! literals and travel as bound parameters, never as text. A body may not call functions, so
//! recursion cannot exist; the expanded statement still goes through `sqlmode::check`.

use crate::{error::ApiError, query::Bind};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub const MAX_BODY: usize = 4_000;
pub const MAX_PARAMS: usize = 8;
pub const MAX_CALLS: usize = 20;
pub const MAX_SQL: usize = 64_000;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Function {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Names of the positional parameters (documentation and arity).
    #[serde(default)]
    pub params: Vec<String>,
    pub body: String,
}

pub fn valid_name(n: &str) -> bool {
    let b = n.as_bytes();
    !b.is_empty()
        && b.len() <= 40
        && b[0].is_ascii_lowercase()
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_')
}

fn bad<T>(m: impl Into<String>) -> Result<T, ApiError> {
    Err(ApiError::bad(m))
}

/// Walk `s` outside quotes; `f` sees each unquoted char with its index. Returns false when a quote
/// is left open.
fn unquoted(s: &str, mut f: impl FnMut(usize, char, &[char]) -> bool) -> bool {
    let cs: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < cs.len() {
        let c = cs[i];
        if matches!(c, '\'' | '"' | '`') {
            i += 1;
            loop {
                match cs.get(i) {
                    None => return false,
                    Some('\\') if c != '`' => i += 2,
                    Some(&q) if q == c => {
                        if cs.get(i + 1) == Some(&c) {
                            i += 2;
                        } else {
                            i += 1;
                            break;
                        }
                    }
                    Some(_) => i += 1,
                }
            }
            continue;
        }
        if !f(i, c, &cs) {
            return true;
        }
        i += 1;
    }
    true
}

impl Function {
    pub fn canonical(mut self) -> Result<Function, ApiError> {
        if !valid_name(&self.name) {
            return bad(
                "name must be lowercase letters, digits and _, starting with a letter (at most 40)",
            );
        }
        if self.description.chars().count() > 300 || self.description.chars().any(char::is_control)
        {
            return bad("description is at most 300 characters");
        }
        if self.params.len() > MAX_PARAMS || self.params.iter().any(|p| !valid_name(p)) {
            return bad(format!(
                "at most {MAX_PARAMS} parameters, named like a function"
            ));
        }
        self.body = self.body.trim().to_string();
        if self.body.is_empty() || self.body.len() > MAX_BODY {
            return bad(format!("the body must be 1-{MAX_BODY} characters"));
        }
        let arity = self.params.len();
        let mut err: Option<String> = None;
        let closed = unquoted(&self.body, |i, c, cs| {
            let next = cs.get(i + 1).copied();
            let problem = match c {
                '?' => Some("a body uses $1, $2, ... for parameters, not ?"),
                ';' => Some("a body is one expression, without ;"),
                '@' => {
                    Some("a function body cannot call functions or use @variables (no recursion)")
                }
                '-' if next == Some('-') => Some("a body has no comments"),
                '/' if next == Some('*') => Some("a body has no comments"),
                '$' => {
                    let digits: String = cs[i + 1..]
                        .iter()
                        .take_while(|d| d.is_ascii_digit())
                        .collect();
                    match digits.parse::<usize>() {
                        Ok(k) if (1..=arity).contains(&k) => None,
                        _ if cs[i + 1..].starts_with(&['_', '_']) => None,
                        _ => Some(
                            "$k must name one of the declared parameters (or be $__from / $__to)",
                        ),
                    }
                }
                _ => None,
            };
            if let Some(p) = problem {
                err = Some(p.into());
                return false;
            }
            true
        });
        if let Some(e) = err {
            return bad(e);
        }
        if !closed {
            return bad("a quote in the body is not closed");
        }
        let mut depth = 0i32;
        unquoted(&self.body, |_, c, _| {
            depth += i32::from(c == '(') - i32::from(c == ')');
            depth >= 0
        });
        if depth != 0 {
            return bad("the parentheses in the body are not balanced");
        }
        Ok(self)
    }

    /// The body with `$k` turned into `?` and the matching binds, in text order.
    fn instantiate(&self, args: &[Bind]) -> (String, Vec<Bind>) {
        let cs: Vec<char> = self.body.chars().collect();
        let (mut out, mut binds, mut i, mut quote) = (String::new(), vec![], 0, None::<char>);
        while i < cs.len() {
            let c = cs[i];
            match quote {
                Some(q) => {
                    out.push(c);
                    if c == '\\' && q != '`' {
                        if let Some(n) = cs.get(i + 1) {
                            out.push(*n);
                            i += 1;
                        }
                    } else if c == q {
                        quote = None;
                    }
                }
                None if matches!(c, '\'' | '"' | '`') => {
                    quote = Some(c);
                    out.push(c);
                }
                None if c == '$' && cs.get(i + 1).is_some_and(char::is_ascii_digit) => {
                    let digits: String = cs[i + 1..]
                        .iter()
                        .take_while(|d| d.is_ascii_digit())
                        .collect();
                    i += digits.len();
                    let k: usize = digits.parse().unwrap_or(0);
                    out.push('?');
                    binds.push(args.get(k.wrapping_sub(1)).cloned().unwrap_or(Bind::Null));
                }
                None => out.push(c),
            }
            i += 1;
        }
        (out, binds)
    }
}

/// One literal argument: a number, `'text'`, `true` or `false`.
fn literal(cs: &[char], i: &mut usize) -> Result<Bind, String> {
    let start = *i;
    if cs.get(*i) == Some(&'\'') {
        *i += 1;
        let mut s = String::new();
        loop {
            match cs.get(*i) {
                None => return Err("a string argument is not closed".into()),
                Some('\\') => return Err("backslashes are not allowed in string arguments".into()),
                Some('\'') if cs.get(*i + 1) == Some(&'\'') => {
                    s.push('\'');
                    *i += 2;
                }
                Some('\'') => {
                    *i += 1;
                    return Ok(Bind::Str(s));
                }
                Some(c) => {
                    s.push(*c);
                    *i += 1;
                }
            }
        }
    }
    while cs
        .get(*i)
        .is_some_and(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    {
        *i += 1;
    }
    let t: String = cs[start..*i].iter().collect();
    match t.to_ascii_lowercase().as_str() {
        "true" => Ok(Bind::Bool(true)),
        "false" => Ok(Bind::Bool(false)),
        _ => t
            .parse::<i64>()
            .map(Bind::Int)
            .or_else(|_| {
                t.parse::<f64>()
                    .ok()
                    .filter(|f| f.is_finite())
                    .map(Bind::Float)
                    .ok_or(())
            })
            .map_err(|()| {
                "function arguments must be numbers, 'strings', true or false".to_string()
            }),
    }
}

/// Replace every `@name(args)` outside quotes and comments by the function's body. `binds` are
/// the binds of the `?` already in `sql`; the result's binds are in the order of the final `?`s.
pub fn expand(
    sql: &str,
    binds: Vec<Bind>,
    fns: &HashMap<String, Function>,
) -> Result<(String, Vec<Bind>), String> {
    let cs: Vec<char> = sql.chars().collect();
    let (mut out, mut ob, mut incoming, mut i, mut calls) =
        (String::new(), vec![], binds.into_iter(), 0, 0);
    while i < cs.len() {
        let c = cs[i];
        if matches!(c, '\'' | '"' | '`') {
            out.push(c);
            i += 1;
            while i < cs.len() {
                out.push(cs[i]);
                if cs[i] == '\\' && c != '`' && i + 1 < cs.len() {
                    out.push(cs[i + 1]);
                    i += 2;
                    continue;
                }
                i += 1;
                if cs[i - 1] == c {
                    if cs.get(i) == Some(&c) {
                        out.push(c);
                        i += 1;
                        continue;
                    }
                    break;
                }
            }
            continue;
        }
        if c == '-' && cs.get(i + 1) == Some(&'-') {
            while i < cs.len() && cs[i] != '\n' {
                out.push(cs[i]);
                i += 1;
            }
            continue;
        }
        if c == '/' && cs.get(i + 1) == Some(&'*') {
            while i < cs.len() && !(cs[i] == '*' && cs.get(i + 1) == Some(&'/')) {
                out.push(cs[i]);
                i += 1;
            }
            out.push_str("*/");
            i += 2;
            continue;
        }
        if c == '?' {
            out.push('?');
            ob.extend(incoming.next());
            i += 1;
            continue;
        }
        let prev_ident = i > 0 && (cs[i - 1].is_ascii_alphanumeric() || cs[i - 1] == '_');
        if c == '@' && !prev_ident && cs.get(i + 1).is_some_and(|n| n.is_ascii_lowercase()) {
            let mut j = i + 1;
            while cs
                .get(j)
                .is_some_and(|n| n.is_ascii_alphanumeric() || *n == '_')
            {
                j += 1;
            }
            let name: String = cs[i + 1..j].iter().collect();
            while cs.get(j).is_some_and(|w| w.is_whitespace()) {
                j += 1;
            }
            if cs.get(j) != Some(&'(') {
                return Err(format!(
                    "@{name} is not a function call; write @{name}(...)"
                ));
            }
            j += 1;
            let f = fns
                .get(&name)
                .ok_or_else(|| format!("no saved function named {name:?}"))?;
            let mut args = vec![];
            loop {
                while cs.get(j).is_some_and(|w| w.is_whitespace()) {
                    j += 1;
                }
                if cs.get(j) == Some(&')') && args.is_empty() {
                    j += 1;
                    break;
                }
                args.push(literal(&cs, &mut j)?);
                while cs.get(j).is_some_and(|w| w.is_whitespace()) {
                    j += 1;
                }
                match cs.get(j) {
                    Some(',') => j += 1,
                    Some(')') => {
                        j += 1;
                        break;
                    }
                    _ => return Err(format!("the call to {name} is not closed")),
                }
            }
            if args.len() != f.params.len() {
                return Err(format!(
                    "{name} takes {} argument(s), got {}",
                    f.params.len(),
                    args.len()
                ));
            }
            calls += 1;
            if calls > MAX_CALLS {
                return Err(format!("at most {MAX_CALLS} function calls per statement"));
            }
            let (body, b) = f.instantiate(&args);
            out.push_str(&body);
            ob.extend(b);
            i = j;
            continue;
        }
        out.push(c);
        i += 1;
    }
    if out.len() > MAX_SQL {
        return Err("the expanded statement is too long".into());
    }
    Ok((out, ob))
}

/// Does the text contain something that looks like a call? (Cheap test before reading the store.)
pub fn has_call(sql: &str) -> bool {
    sql.contains('@')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(name: &str, params: &[&str], body: &str) -> Function {
        Function {
            name: name.into(),
            description: String::new(),
            params: params.iter().map(|s| s.to_string()).collect(),
            body: body.into(),
        }
    }

    #[test]
    fn expands_in_order_with_existing_binds() {
        let fns = HashMap::from([(
            "slow".to_string(),
            f(
                "slow",
                &["ms", "svc"],
                "(duration_nano / 1000000 > $1 AND service_name = $2)",
            )
            .canonical()
            .unwrap(),
        )]);
        let (sql, b) = expand(
            "SELECT 1 FROM t WHERE a = ? AND @slow(250, 'it''s') AND b = ?",
            vec![Bind::Int(1), Bind::Int(2)],
            &fns,
        )
        .unwrap();
        assert_eq!(
            sql,
            "SELECT 1 FROM t WHERE a = ? AND (duration_nano / 1000000 > ? AND service_name = ?) AND b = ?"
        );
        assert_eq!(
            b,
            vec![
                Bind::Int(1),
                Bind::Int(250),
                Bind::Str("it's".into()),
                Bind::Int(2)
            ]
        );
        // Calls inside strings and comments are text.
        let (sql, b) = expand("SELECT '@slow(1,2)' -- @slow(1,2)", vec![], &fns).unwrap();
        assert_eq!(sql, "SELECT '@slow(1,2)' -- @slow(1,2)");
        assert!(b.is_empty());
    }
}
