//! Pure SQL builders. Identifiers are validated, values are binds, every statement is tagged.

use super::{Bind, Q, Window, qualified, quote, ts_string};

pub fn health() -> Q {
    Q::new("health", "SELECT 1 AS ok", vec![])
}

/// Tables of one database with GreptimeDB's own row estimate and creation options.
pub fn streams_list(db: &str) -> Q {
    Q::new(
        "streams_list",
        "SELECT table_name, table_type, engine, table_rows, create_options \
         FROM information_schema.tables WHERE table_schema = ? ORDER BY table_name",
        vec![Bind::Str(db.into())],
    )
}

/// Columns of one table with type and role (TAG, FIELD, TIMESTAMP).
pub fn stream_schema(db: &str, table: &str) -> Q {
    Q::new(
        "stream_schema",
        "SELECT column_name, data_type, semantic_type FROM information_schema.columns \
         WHERE table_schema = ? AND table_name = ?",
        vec![Bind::Str(db.into()), Bind::Str(table.into())],
    )
}

/// Exact row count and time range. A full scan: callers run it with the query timeout.
pub fn stream_stats(db: &str, table: &str, ts_col: &str) -> anyhow::Result<Q> {
    let (t, c) = (qualified(db, table)?, quote(ts_col)?);
    Ok(Q::new(
        "stream_stats",
        format!("SELECT count(*) AS total, min({c}) AS oldest, max({c}) AS newest FROM {t}"),
        vec![],
    ))
}

/// Newest rows of one table inside a window.
pub fn stream_sample(db: &str, table: &str, ts_col: &str, w: Window) -> anyhow::Result<Q> {
    let (t, c) = (qualified(db, table)?, quote(ts_col)?);
    Ok(Q::new(
        "stream_sample",
        format!(
            "SELECT * FROM {t} WHERE {c} >= ? AND {c} < ? ORDER BY {c} DESC LIMIT {}",
            w.limit
        ),
        vec![
            Bind::Str(ts_string(w.from_ms)),
            Bind::Str(ts_string(w.to_ms)),
        ],
    ))
}

/// Every builder with representative arguments; the read-only test iterates this (R-F4).
pub fn all_for_tests() -> Vec<Q> {
    let w = Window {
        from_ms: 1_000,
        to_ms: 2_000,
        limit: 10,
    };
    vec![
        health(),
        streams_list("public"),
        stream_schema("public", "t"),
        stream_stats("public", "t", "ts").expect("valid idents"),
        stream_sample("public", "t", "ts", w).expect("valid idents"),
    ]
}
