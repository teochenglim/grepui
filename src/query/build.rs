//! Pure SQL builders. Identifiers are validated, values are binds, every statement is tagged.

use super::{
    Bind, Q, Window,
    filter::{FtMode, LogFilter},
    logs, qualified, quote, ts_string,
};

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

/// Newest row per stream within a bounded look-back, in one statement (R-I2).
pub fn ingest_freshness(
    db: &str,
    traces_table: &str,
    logs_table: &str,
    since_ms: i64,
) -> anyhow::Result<Q> {
    let (l, t) = (qualified(db, logs_table)?, qualified(db, traces_table)?);
    Ok(Q::new(
        "ingest_freshness",
        format!(
            "SELECT 'logs' AS stream, max(`timestamp`) AS newest FROM {l} WHERE `timestamp` >= ? UNION ALL SELECT 'traces' AS stream, max(`timestamp`) AS newest FROM {t} WHERE `timestamp` >= ?"
        ),
        vec![
            Bind::Str(ts_string(since_ms)),
            Bind::Str(ts_string(since_ms)),
        ],
    ))
}

/// Rows per bucket for one stream; the bucket size comes from the fixed set in `logs::BUCKETS`.
pub fn ingest_rate(db: &str, table: &str, w: Window) -> anyhow::Result<Q> {
    let t = qualified(db, table)?;
    let (ms, _) = logs::bucket_for(w.to_ms - w.from_ms);
    Ok(Q::new(
        "ingest_rate",
        format!(
            "SELECT (CAST(`timestamp` AS BIGINT) / {}) * {ms} AS bucket_ms, count(*) AS n FROM {t} WHERE `timestamp` >= ? AND `timestamp` < ? GROUP BY 1 ORDER BY 1",
            ms * 1_000_000
        ),
        vec![
            Bind::Str(ts_string(w.from_ms)),
            Bind::Str(ts_string(w.to_ms)),
        ],
    ))
}

/// Last span per service within the window (the traces table's `service_name` tag).
pub fn ingest_services(db: &str, traces_table: &str, w: Window) -> anyhow::Result<Q> {
    let t = qualified(db, traces_table)?;
    Ok(Q::new(
        "ingest_services",
        format!(
            "SELECT `service_name`, max(`timestamp`) AS last_seen, count(*) AS spans FROM {t} WHERE `timestamp` >= ? AND `timestamp` < ? GROUP BY 1 ORDER BY 2 DESC LIMIT 200"
        ),
        vec![
            Bind::Str(ts_string(w.from_ms)),
            Bind::Str(ts_string(w.to_ms)),
        ],
    ))
}

/// Table-level facts from `information_schema`: size, row estimate, engine, creation options.
pub fn stream_info(db: &str, table: &str) -> Q {
    Q::new(
        "stream_info",
        "SELECT table_name, engine, table_rows, data_length, index_length, create_options \
         FROM information_schema.tables WHERE table_schema = ? AND table_name = ?",
        vec![Bind::Str(db.into()), Bind::Str(table.into())],
    )
}

/// `SHOW CREATE TABLE`: indexes, partitions, TTL and options live in the DDL.
pub fn show_create(db: &str, table: &str) -> anyhow::Result<Q> {
    Ok(Q::new(
        "stream_ddl",
        format!("SHOW CREATE TABLE {}", qualified(db, table)?),
        vec![],
    ))
}

/// Every builder with representative arguments; the read-only test iterates this (R-F4).
pub fn all_for_tests() -> Vec<Q> {
    let w = Window {
        from_ms: 1_000,
        to_ms: 2_000,
        limit: 10,
    };
    let f = LogFilter::new(
        Some("level=error http.method~GET timeout"),
        None,
        FtMode::Term,
    )
    .expect("valid filter");
    vec![
        health(),
        streams_list("public"),
        stream_schema("public", "t"),
        stream_stats("public", "t", "ts").expect("valid idents"),
        stream_sample("public", "t", "ts", w).expect("valid idents"),
        logs::search("public", "l", &f, w, 0).expect("valid idents"),
        logs::histogram("public", "l", &f, w).expect("valid idents"),
        logs::top_values("public", "l", &logs::Col::Plain("severity_text"), &f, w, 10)
            .expect("valid idents"),
        logs::record("public", "l", 1_760_000_000_000_000_000).expect("valid idents"),
        logs::around("public", "l", 1_760_000_000_000_000_000, 5, 5).expect("valid idents"),
        logs::ddl("public", "l").expect("valid idents"),
        ingest_freshness("public", "t", "l", 0).expect("valid idents"),
        ingest_rate("public", "t", w).expect("valid idents"),
        ingest_services("public", "t", w).expect("valid idents"),
        stream_info("public", "t"),
        show_create("public", "t").expect("valid idents"),
        super::services::services("public", "t", Some("cart"), w).expect("valid idents"),
        super::services::series("public", "t", "cart", w).expect("valid idents"),
        super::services::operations("public", "t", "cart", w).expect("valid idents"),
        super::services::dependencies("public", "t", false, w).expect("valid idents"),
    ]
}
