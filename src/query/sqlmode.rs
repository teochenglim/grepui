//! SQL mode (R-L5): the one place user-written SQL runs. A SQL *parser* (not a regex) accepts
//! exactly one read-only statement, rejects anything that could write, change state or read
//! another tenant's database, and the caller still applies a row cap and a timeout.
//! The real boundary remains a database account without write rights (ARCH 01).

use sqlparser::{
    ast::{Query, SetExpr, Statement, visit_relations},
    dialect::MySqlDialect,
    parser::Parser,
    tokenizer::{Token, Tokenizer},
};
use std::ops::ControlFlow;

pub const MAX_SQL_LEN: usize = 8_000;

/// Words that never belong in a read-only query (GreptimeDB administrative functions).
const DENY_WORDS: &[&str] = &[
    "admin",
    "flush_table",
    "compact_table",
    "flush_region",
    "compact_region",
    "migrate_region",
    "kill",
    "copy",
    "outfile",
    "dumpfile",
];

#[derive(Debug, PartialEq, Eq)]
pub struct Reject(pub String);

fn no<T>(m: impl Into<String>) -> Result<T, Reject> {
    Err(Reject(m.into()))
}

/// Replace the window macros with timestamp literals generated from validated integers.
pub fn expand_macros(sql: &str, from_ms: i64, to_ms: i64) -> String {
    sql.replace("$__from", &format!("'{}'", super::ts_string(from_ms)))
        .replace("$__to", &format!("'{}'", super::ts_string(to_ms)))
}

/// Accept `sql` only when it is one read-only statement that stays inside `database`.
pub fn check(sql: &str, database: &str) -> Result<(), Reject> {
    let sql = sql.trim();
    if sql.is_empty() {
        return no("the SQL is empty");
    }
    if sql.len() > MAX_SQL_LEN {
        return no(format!("the SQL is longer than {MAX_SQL_LEN} characters"));
    }
    let dialect = MySqlDialect {};
    let tokens = Tokenizer::new(&dialect, sql)
        .tokenize()
        .map_err(|e| Reject(format!("cannot read the SQL: {e}")))?;
    for t in &tokens {
        if let Token::Word(w) = t
            && DENY_WORDS.contains(&w.value.to_ascii_lowercase().as_str())
        {
            return no(format!("{} is not allowed in SQL mode", w.value));
        }
    }
    // GreptimeDB's TQL (PromQL over SQL) is not standard SQL; accept only its read forms.
    let lower = sql.trim_end_matches(';').trim().to_ascii_lowercase();
    if let Some(rest) = lower.strip_prefix("tql") {
        let verb = rest.split_whitespace().next().unwrap_or_default();
        if lower.contains(';') {
            return no("only one statement is allowed");
        }
        return if matches!(verb, "eval" | "evaluate" | "explain" | "analyze") {
            Ok(())
        } else {
            no("only TQL EVAL, EXPLAIN and ANALYZE are allowed")
        };
    }
    // GreptimeDB accepts `DESC TABLE t`; the MySQL dialect only knows `DESC t`.
    let normalised = strip_desc_table(sql);
    let stmts = Parser::parse_sql(&dialect, &normalised)
        .map_err(|e| Reject(format!("cannot parse the SQL: {e}")))?;
    let [stmt] = stmts.as_slice() else {
        return no("exactly one statement is allowed");
    };
    check_statement(stmt)?;
    let mut bad = None;
    let _ = visit_relations(stmt, |name| {
        let parts: Vec<String> = name
            .0
            .iter()
            .map(|p| {
                p.to_string()
                    .trim_matches(|c| c == '`' || c == '"')
                    .to_string()
            })
            .collect();
        let (catalog, schema) = match parts.as_slice() {
            [_, s, _] => (Some(parts[0].as_str()), Some(s.as_str())),
            [s, _] => (None, Some(s.as_str())),
            _ => (None, None),
        };
        if catalog.is_some_and(|c| !c.eq_ignore_ascii_case("greptime"))
            || schema.is_some_and(|s| s != database)
        {
            bad = Some(name.to_string());
            return ControlFlow::Break(());
        }
        ControlFlow::<()>::Continue(())
    });
    if let Some(n) = bad {
        return no(format!("{n} is outside this org's database ({database})"));
    }
    Ok(())
}

/// `DESC TABLE x` / `DESCRIBE TABLE x` -> `DESC x`; everything else unchanged.
fn strip_desc_table(sql: &str) -> String {
    let words: Vec<&str> = sql.split_whitespace().collect();
    if words.len() >= 3
        && ["desc", "describe"].contains(&words[0].to_ascii_lowercase().as_str())
        && words[1].eq_ignore_ascii_case("table")
    {
        return format!("DESC {}", words[2..].join(" "));
    }
    sql.to_string()
}

fn check_statement(s: &Statement) -> Result<(), Reject> {
    match s {
        Statement::Query(q) => check_query(q),
        Statement::Explain { statement, .. } => match statement.as_ref() {
            Statement::Query(q) => check_query(q),
            _ => no("EXPLAIN is allowed only for SELECT queries"),
        },
        Statement::ShowTables { .. }
        | Statement::ShowColumns { .. }
        | Statement::ShowCreate { .. }
        | Statement::ExplainTable { .. } => Ok(()),
        _ => no(
            "only SELECT, WITH, SHOW TABLES, SHOW COLUMNS, SHOW CREATE, DESC, EXPLAIN and TQL are allowed",
        ),
    }
}

fn check_query(q: &Query) -> Result<(), Reject> {
    if !q.locks.is_empty() {
        return no("locking clauses are not allowed");
    }
    if let Some(w) = &q.with {
        for cte in &w.cte_tables {
            check_query(&cte.query)?;
        }
    }
    check_set(&q.body)
}

fn check_set(e: &SetExpr) -> Result<(), Reject> {
    match e {
        SetExpr::Select(s) if s.into.is_some() => no("SELECT ... INTO is not allowed"),
        SetExpr::Select(_) | SetExpr::Values(_) | SetExpr::Table(_) => Ok(()),
        SetExpr::Query(q) => check_query(q),
        SetExpr::SetOperation { left, right, .. } => {
            check_set(left)?;
            check_set(right)
        }
        _ => no("data-changing statements are not allowed"),
    }
}
