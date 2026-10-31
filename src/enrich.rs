//! Enrichment tables (R-N3): a CSV becomes a table `enrich_<name>` in the org's database, to be
//! joined with logs and traces in SQL mode. Names are validated (never rewritten), types are
//! inferred from every row, sizes are capped. The statements are generated here from fixed
//! templates and re-validated by `admin::check`, which is why [`Spec`] can also parse its own
//! output back: the gate accepts a statement only if regenerating it from what it says gives the
//! same text.

use crate::{
    error::ApiError,
    org::Org,
    query::{Bind, Q, ident, qualified, ts_string},
};

pub const PREFIX: &str = "enrich_";
pub const MAX_BYTES: usize = 1 << 20;
pub const MAX_ROWS: usize = 5_000;
pub const MAX_COLS: usize = 30;
pub const MAX_CELL: usize = 500;
pub const CHUNK_ROWS: usize = 200;
pub const TS: &str = "ts";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ty {
    String,
    Bigint,
    Double,
    Boolean,
}

impl Ty {
    pub fn sql(self) -> &'static str {
        match self {
            Ty::String => "STRING",
            Ty::Bigint => "BIGINT",
            Ty::Double => "DOUBLE",
            Ty::Boolean => "BOOLEAN",
        }
    }
    fn from_sql(s: &str) -> Option<Ty> {
        [Ty::String, Ty::Bigint, Ty::Double, Ty::Boolean]
            .into_iter()
            .find(|t| t.sql() == s)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Spec {
    pub db: String,
    /// The full table name, `enrich_<name>`.
    pub table: String,
    pub columns: Vec<(String, Ty)>,
    pub key: Vec<String>,
}

/// The short name from a path: lowercase letters, digits and `_`, 1-50.
pub fn table_for(name: &str) -> Result<String, ApiError> {
    let ok = !name.is_empty()
        && name.len() <= 50
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
    if !ok {
        return Err(ApiError::bad(
            "the name must be 1-50 lowercase letters, digits or _",
        ));
    }
    Ok(format!("{PREFIX}{name}"))
}

fn column_ok(c: &str) -> bool {
    let b = c.as_bytes();
    !b.is_empty()
        && b.len() <= 64
        && (b[0].is_ascii_alphabetic() || b[0] == b'_')
        && b.iter().all(|x| x.is_ascii_alphanumeric() || *x == b'_')
}

fn infer(cells: &[&str]) -> Ty {
    let vals: Vec<&&str> = cells.iter().filter(|c| !c.is_empty()).collect();
    if vals.is_empty() {
        return Ty::String;
    }
    // Leading zeros (zip codes, ids) stay text.
    let int = |c: &str| {
        c.parse::<i64>().is_ok() && !(c.len() > 1 && c.trim_start_matches('-').starts_with('0'))
    };
    if vals.iter().all(|c| int(c)) {
        Ty::Bigint
    } else if vals.iter().all(|c| {
        let t = c.trim_start_matches(['-', '+']);
        c.parse::<f64>().is_ok_and(f64::is_finite)
            && (!t.starts_with('0') || t == "0" || t.starts_with("0."))
    }) {
        Ty::Double
    } else if vals
        .iter()
        .all(|c| c.eq_ignore_ascii_case("true") || c.eq_ignore_ascii_case("false"))
    {
        Ty::Boolean
    } else {
        Ty::String
    }
}

/// A parsed upload: the table to create and the rows as binds, one `Vec<Bind>` per row, the time
/// index last.
pub struct Upload {
    pub spec: Spec,
    pub rows: Vec<Vec<Bind>>,
}

fn bind_for(cell: &str, ty: Ty) -> Bind {
    if cell.is_empty() && ty != Ty::String {
        return Bind::Null;
    }
    match ty {
        Ty::String => Bind::Str(cell.to_string()),
        Ty::Bigint => Bind::Int(cell.parse().unwrap_or(0)),
        Ty::Double => Bind::Float(cell.parse().unwrap_or(0.0)),
        Ty::Boolean => Bind::Bool(cell.eq_ignore_ascii_case("true")),
    }
}

/// Read a CSV (first row = column names) into an [`Upload`] for `org`. `key` lists the primary
/// key columns; without one every row is kept (append), with one a later row replaces an earlier
/// one with the same key.
pub fn parse_upload(org: &Org, name: &str, text: &str, key: &[String]) -> Result<Upload, ApiError> {
    let table = table_for(name)?;
    if text.len() > MAX_BYTES {
        return Err(ApiError::bad("the CSV is larger than 1 MB"));
    }
    let mut rows = crate::csv::parse(text, MAX_ROWS + 1, MAX_COLS)
        .map_err(|e| ApiError::bad(format!("CSV: {e}")))?;
    if rows.len() < 2 {
        return Err(ApiError::bad(
            "the CSV needs a header row and at least one data row",
        ));
    }
    let head = rows.remove(0);
    let mut seen = std::collections::HashSet::new();
    for h in &head {
        if !column_ok(h) {
            return Err(ApiError::bad(format!(
                "column name {h:?} must be 1-64 letters, digits or _, not starting with a digit"
            )));
        }
        if h.eq_ignore_ascii_case(TS) {
            return Err(ApiError::bad(
                "a column cannot be named ts (it is the time index)",
            ));
        }
        if !seen.insert(h.to_ascii_lowercase()) {
            return Err(ApiError::bad(format!("column {h:?} appears twice")));
        }
    }
    if rows.iter().any(|r| r.len() != head.len()) {
        return Err(ApiError::bad("every row needs as many cells as the header"));
    }
    if rows.iter().flatten().any(|c| {
        c.chars().count() > MAX_CELL
            || c.chars()
                .any(|ch| ch.is_control() && ch != '\n' && ch != '\t')
    }) {
        return Err(ApiError::bad(format!(
            "cells are limited to {MAX_CELL} characters, without control characters"
        )));
    }
    let columns: Vec<(String, Ty)> = head
        .iter()
        .enumerate()
        .map(|(i, h)| {
            (
                h.clone(),
                infer(&rows.iter().map(|r| r[i].as_str()).collect::<Vec<_>>()),
            )
        })
        .collect();
    for k in key {
        let Some(i) = columns.iter().position(|(c, _)| c == k) else {
            return Err(ApiError::bad(format!("key column {k:?} is not in the CSV")));
        };
        if !matches!(columns[i].1, Ty::String | Ty::Bigint) {
            return Err(ApiError::bad(format!(
                "key column {k:?} must hold text or whole numbers"
            )));
        }
        if rows.iter().any(|r| r[i].is_empty()) {
            return Err(ApiError::bad(format!("key column {k:?} has empty cells")));
        }
    }
    if key.len() > 4 || key.iter().collect::<std::collections::HashSet<_>>().len() != key.len() {
        return Err(ApiError::bad("at most 4 distinct key columns"));
    }
    let n = rows.len();
    let out = rows
        .iter()
        .enumerate()
        .map(|(ri, r)| {
            let mut b: Vec<Bind> = r
                .iter()
                .zip(&columns)
                .map(|(c, (_, t))| bind_for(c, *t))
                .collect();
            // Keyed tables upsert at one time index value; unkeyed ones keep every row.
            b.push(Bind::Str(ts_string(if key.is_empty() {
                ri as i64
            } else {
                0
            })));
            b
        })
        .collect();
    let _ = n;
    Ok(Upload {
        spec: Spec {
            db: org.database.clone(),
            table,
            columns,
            key: key.to_vec(),
        },
        rows: out,
    })
}

fn q(db: &str, table: &str) -> String {
    qualified(db, table).unwrap_or_default()
}

impl Spec {
    pub fn create_sql(&self) -> String {
        let mut cols: Vec<String> = self
            .columns
            .iter()
            .map(|(c, t)| format!("`{c}` {}", t.sql()))
            .collect();
        cols.push(format!("`{TS}` TIMESTAMP(3) NOT NULL"));
        cols.push(format!("TIME INDEX (`{TS}`)"));
        if !self.key.is_empty() {
            cols.push(format!(
                "PRIMARY KEY ({})",
                self.key
                    .iter()
                    .map(|k| format!("`{k}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        format!(
            "CREATE TABLE IF NOT EXISTS {} ({})",
            q(&self.db, &self.table),
            cols.join(", ")
        )
    }

    pub fn drop_sql(&self) -> String {
        format!("DROP TABLE IF EXISTS {}", q(&self.db, &self.table))
    }

    pub fn insert(&self, rows: &[Vec<Bind>]) -> Q {
        let cols: Vec<String> = self.columns.iter().map(|(c, _)| c.clone()).collect();
        let binds = rows.iter().flatten().cloned().collect();
        Q::new(
            "enrich_insert",
            insert_sql(&self.db, &self.table, &cols, rows.len()),
            binds,
        )
    }

    /// Parse a statement made by [`Spec::create_sql`] back into a spec (the admin gate's input).
    pub fn from_create(sql: &str) -> Option<Spec> {
        let rest = sql.strip_prefix("CREATE TABLE IF NOT EXISTS ")?;
        let (db, table, rest) = split_name(rest)?;
        let body = rest.strip_prefix(" (")?.strip_suffix(')')?;
        let mut columns = vec![];
        let mut key = vec![];
        let mut parts = body.split(", ");
        for p in parts.by_ref() {
            if p == format!("`{TS}` TIMESTAMP(3) NOT NULL") {
                break;
            }
            let (c, t) = p.strip_prefix('`')?.split_once("` ")?;
            columns.push((c.to_string(), Ty::from_sql(t)?));
        }
        if parts.next()? != format!("TIME INDEX (`{TS}`)") {
            return None;
        }
        let tail: Vec<&str> = parts.collect();
        if !tail.is_empty() {
            let joined = tail.join(", ");
            let inner = joined.strip_prefix("PRIMARY KEY (")?.strip_suffix(')')?;
            for k in inner.split(", ") {
                key.push(k.strip_prefix('`')?.strip_suffix('`')?.to_string());
            }
        }
        Some(Spec {
            db,
            table,
            columns,
            key,
        })
    }
}

/// `INSERT INTO db.table (cols..., ts) VALUES (?, ...), ...`.
pub fn insert_sql(db: &str, table: &str, cols: &[String], rows: usize) -> String {
    let names: Vec<String> = cols
        .iter()
        .map(|c| format!("`{c}`"))
        .chain([format!("`{TS}`")])
        .collect();
    let tuple = format!("({})", vec!["?"; cols.len() + 1].join(", "));
    format!(
        "INSERT INTO {} ({}) VALUES {}",
        q(db, table),
        names.join(", "),
        vec![tuple; rows].join(", ")
    )
}

/// `` `db`.`table` `` and what follows it.
fn split_name(s: &str) -> Option<(String, String, String)> {
    let s = s.strip_prefix('`')?;
    let (db, s) = s.split_once("`.`")?;
    let (table, rest) = s.split_once('`')?;
    Some((db.to_string(), table.to_string(), rest.to_string()))
}

/// What the admin gate checks for enrichment statements: the statement must be exactly what this
/// module generates, for a table `enrich_*` in the org's database.
pub fn gate(sql: &str, binds: usize, org: &Org) -> bool {
    let mine = |db: &str, t: &str| {
        db == org.database
            && t.starts_with(PREFIX)
            && table_for(&t[PREFIX.len()..]).is_ok()
            && ident(t).is_ok()
    };
    if let Some(s) = Spec::from_create(sql) {
        return mine(&s.db, &s.table)
            && s.columns.len() <= MAX_COLS
            && s.columns
                .iter()
                .all(|(c, _)| column_ok(c) && !c.eq_ignore_ascii_case(TS))
            && s.create_sql() == sql;
    }
    if let Some(rest) = sql.strip_prefix("DROP TABLE IF EXISTS ")
        && let Some((db, t, tail)) = split_name(rest)
    {
        return tail.is_empty() && mine(&db, &t);
    }
    if let Some(rest) = sql.strip_prefix("INSERT INTO ")
        && let Some((db, t, tail)) = split_name(rest)
        && let Some(tail) = tail.strip_prefix(" (")
        && let Some((cols, vals)) = tail.split_once(") VALUES ")
    {
        let cols: Option<Vec<String>> = cols
            .split(", ")
            .map(|c| c.strip_prefix('`')?.strip_suffix('`').map(String::from))
            .collect();
        let Some(mut cols) = cols else { return false };
        if cols.pop().as_deref() != Some(TS) || !cols.iter().all(|c| column_ok(c)) {
            return false;
        }
        let rows = vals.matches("(?").count();
        return mine(&db, &t)
            && (1..=CHUNK_ROWS).contains(&rows)
            && binds == rows * (cols.len() + 1)
            && insert_sql(&db, &t, &cols, rows) == sql;
    }
    false
}

// ---- reads and the join helper -----------------------------------------------------------------

pub fn tables(db: &str) -> Q {
    Q::new(
        "enrich_tables",
        "SELECT table_name FROM information_schema.tables WHERE table_schema = ? AND table_name LIKE ? ORDER BY table_name LIMIT 200",
        vec![Bind::Str(db.into()), Bind::Str(format!("{PREFIX}%"))],
    )
}

pub fn count(db: &str, table: &str) -> anyhow::Result<Q> {
    Ok(Q::new(
        "enrich_count",
        format!("SELECT count(*) AS n FROM {}", qualified(db, table)?),
        vec![],
    ))
}

pub fn preview(db: &str, table: &str) -> anyhow::Result<Q> {
    Ok(Q::new(
        "enrich_preview",
        format!("SELECT * FROM {} LIMIT 50", qualified(db, table)?),
        vec![],
    ))
}

/// A starter query for SQL mode that joins a stream with an enrichment table. `left` is
/// `service`, `column:<name>` or `attr:<key>` (logs); `columns` are the enrichment columns shown.
pub fn join_sql(
    org: &Org,
    stream: &str,
    table: &str,
    key: &str,
    left: &str,
    columns: &[String],
) -> Result<String, ApiError> {
    let bad = |m: &str| ApiError::bad(m.to_string());
    let (src, ts) = match stream {
        "logs" => (&org.logs_table, "timestamp"),
        "traces" => (&org.traces_table, "timestamp"),
        _ => return Err(bad("stream must be logs or traces")),
    };
    if !column_ok(key)
        || columns.is_empty()
        || columns.len() > 10
        || !columns.iter().all(|c| column_ok(c))
    {
        return Err(bad(
            "key and columns must be column names (at most 10 columns)",
        ));
    }
    let left_expr = match left.split_once(':') {
        None if left == "service" && stream == "traces" => "l.`service_name`".to_string(),
        None if left == "service" => {
            "json_get_string(l.`resource_attributes`, '[\"service.name\"]')".to_string()
        }
        Some(("column", c)) if column_ok(c) => format!("l.`{c}`"),
        Some(("attr", k))
            if stream == "logs"
                && !k.is_empty()
                && k.len() <= 64
                && k.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-')) =>
        {
            format!("json_get_string(l.`log_attributes`, '[\"{k}\"]')")
        }
        _ => {
            return Err(bad(
                "left must be service, column:<name> or attr:<key> (logs only)",
            ));
        }
    };
    let cols: Vec<String> = columns.iter().map(|c| format!("e.`{c}`")).collect();
    let idx: Vec<String> = (1..=columns.len()).map(|i| i.to_string()).collect();
    Ok(format!(
        "SELECT {}, count(*) AS n FROM {} AS l LEFT JOIN {} AS e ON {left_expr} = e.`{key}` WHERE l.`{ts}` >= $__from AND l.`{ts}` < $__to GROUP BY {} ORDER BY n DESC LIMIT 100",
        cols.join(", "),
        qualified(&org.database, src).map_err(|_| bad("unsupported table name"))?,
        qualified(&org.database, table).map_err(|_| bad("unsupported table name"))?,
        idx.join(", ")
    ))
}
