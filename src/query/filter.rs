//! The filter-bar grammar (R-L1): `key=value`, `key!=value`, `key~substring`, quoted values and
//! free-text terms. Parsing is pure and deterministic; nothing here touches SQL.

use crate::error::ApiError;

pub const MAX_TOKENS: usize = 20;
pub const MAX_TOKEN_LEN: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Eq,
    Ne,
    Like,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cond {
    pub key: String,
    pub op: Op,
    pub value: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Parsed {
    pub conds: Vec<Cond>,
    pub terms: Vec<String>,
}

fn key_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-')
}

/// Read a quoted string starting at `chars[i]` (a `"` or `'`); `\` escapes the next character.
fn quoted(chars: &[char], mut i: usize) -> Result<(String, usize), ApiError> {
    let q = chars[i];
    i += 1;
    let mut out = String::new();
    while i < chars.len() {
        match chars[i] {
            '\\' if i + 1 < chars.len() => {
                out.push(chars[i + 1]);
                i += 2;
            }
            c if c == q => return Ok((out, i + 1)),
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    Err(ApiError::bad("the filter has an unterminated quote"))
}

pub fn parse(text: &str) -> Result<Parsed, ApiError> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Parsed::default();
    let (mut i, mut tokens) = (0, 0);
    while i < chars.len() {
        if chars[i].is_whitespace() {
            i += 1;
            continue;
        }
        tokens += 1;
        if tokens > MAX_TOKENS {
            return Err(ApiError::bad(format!(
                "the filter has more than {MAX_TOKENS} parts"
            )));
        }
        if matches!(chars[i], '"' | '\'') {
            let (t, next) = quoted(&chars, i)?;
            i = next;
            check_len(&t)?;
            out.terms.push(t);
            continue;
        }
        let start = i;
        while i < chars.len() && key_char(chars[i]) {
            i += 1;
        }
        let key: String = chars[start..i].iter().collect();
        let op = match (chars.get(i), chars.get(i + 1)) {
            (Some('!'), Some('=')) => Some((Op::Ne, 2)),
            (Some('='), _) => Some((Op::Eq, 1)),
            (Some('~'), _) => Some((Op::Like, 1)),
            _ => None,
        };
        match op {
            Some((op, n)) if !key.is_empty() => {
                i += n;
                let value = if matches!(chars.get(i), Some('"' | '\'')) {
                    let (v, next) = quoted(&chars, i)?;
                    i = next;
                    v
                } else {
                    let s = i;
                    while i < chars.len() && !chars[i].is_whitespace() {
                        i += 1;
                    }
                    chars[s..i].iter().collect()
                };
                check_len(&value)?;
                if value.is_empty() {
                    return Err(ApiError::bad(format!("the filter {key} has no value")));
                }
                out.conds.push(Cond { key, op, value });
            }
            // No recognised operator: the whole token (e.g. `latency>5`) is a free-text term.
            _ => {
                while i < chars.len() && !chars[i].is_whitespace() {
                    i += 1;
                }
                let t: String = chars[start..i].iter().collect();
                check_len(&t)?;
                out.terms.push(t);
            }
        }
    }
    Ok(out)
}

fn check_len(s: &str) -> Result<(), ApiError> {
    if s.chars().count() > MAX_TOKEN_LEN {
        return Err(ApiError::bad(format!(
            "a filter value is longer than {MAX_TOKEN_LEN} characters"
        )));
    }
    Ok(())
}

/// How free text is matched (R-L6): term matching when `body` has a fulltext index, else `LIKE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FtMode {
    Term,
    Like,
}

/// Everything a log query filters on besides the window.
#[derive(Debug, Clone)]
pub struct LogFilter {
    pub parsed: Parsed,
    pub level: Option<String>,
    pub ft: FtMode,
}

impl LogFilter {
    pub fn new(q: Option<&str>, level: Option<&str>, ft: FtMode) -> Result<LogFilter, ApiError> {
        let level = level
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_ascii_uppercase);
        if let Some(l) = &level
            && (l.len() > 16 || !l.bytes().all(|b| b.is_ascii_alphabetic()))
        {
            return Err(ApiError::bad("invalid level"));
        }
        Ok(LogFilter {
            parsed: parse(q.unwrap_or_default())?,
            level,
            ft,
        })
    }
}

/// Escape `\`, `%` and `_` for a `LIKE` pattern (GreptimeDB supports only the backslash escape).
pub fn like_escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        if matches!(c, '\\' | '%' | '_') {
            o.push('\\');
        }
        o.push(c);
    }
    o
}
