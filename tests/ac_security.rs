//! AC0.3.7: SQL mode keeps every statement inside the org's database. These cases came from a
//! security review of v0.2.0 and were reproduced against a live server before the fix:
//! `SHOW TABLES FROM <other>` and `SHOW CREATE TABLE <other>.t` returned another database's
//! objects, and TQL's `__schema__` / `__database__` matchers select a database invisibly to SQL.
mod common;
use axum::http::StatusCode;
use common::*;
use grepui::query::sqlmode;

const DB: &str = "public";
const BASE: &str = "/api/v1/orgs/default";

#[test]
fn ac_0_3_7_sql_mode_other_database_statements_rejected() {
    for bad in [
        // The two reproduced leaks.
        "SHOW TABLES FROM demo",
        "SHOW CREATE TABLE demo.opentelemetry_logs",
        // Every sibling variant.
        "SHOW TABLES IN demo",
        "SHOW FULL TABLES FROM demo",
        "SHOW TABLES FROM greptime.demo",
        "SHOW COLUMNS FROM t FROM demo",
        "SHOW COLUMNS FROM demo.t",
        "SHOW COLUMNS IN t IN demo",
        "SHOW CREATE TABLE greptime.demo.t",
        "SHOW CREATE TABLE other.t",
        "SHOW CREATE VIEW demo.v",
        "SHOW CREATE TABLE information_schema.tables",
        "DESC demo.t",
        "DESC TABLE demo.t",
        "DESCRIBE demo.t",
        "DESCRIBE TABLE greptime.demo.t",
        "EXPLAIN SELECT * FROM demo.t",
        "SELECT * FROM demo.t",
        "SELECT * FROM greptime.demo.t",
        "WITH x AS (SELECT * FROM demo.t) SELECT * FROM x",
        "SELECT * FROM t WHERE a IN (SELECT a FROM demo.u)",
        "SELECT * FROM t JOIN demo.u ON t.a = u.a",
        "SELECT * FROM (SELECT * FROM demo.t) s",
        "SELECT * FROM t UNION ALL SELECT * FROM demo.t",
        "SHOW DATABASES",
        "SHOW SCHEMAS",
        "USE demo",
    ] {
        assert!(sqlmode::check(bad, DB).is_err(), "must reject: {bad}");
    }
    // Quoted identifiers and case are not a way around it.
    for bad in [
        "SHOW CREATE TABLE `demo`.`t`",
        "show create table \"demo\".\"t\"",
        "SHOW TABLES FROM `demo`",
        "show tables from DEMO",
        "SHOW CREATE TABLE DEMO.t",
    ] {
        assert!(sqlmode::check(bad, DB).is_err(), "must reject: {bad}");
    }
    // The org's own database, qualified or not, still works.
    for ok in [
        "SHOW TABLES",
        "SHOW TABLES FROM public",
        "SHOW TABLES IN `public`",
        "SHOW FULL TABLES FROM greptime.public",
        "SHOW COLUMNS FROM t",
        "SHOW COLUMNS FROM public.t",
        "SHOW CREATE TABLE t",
        "SHOW CREATE TABLE public.t",
        "SHOW CREATE TABLE greptime.public.t",
        "DESC t",
        "DESC TABLE public.t",
        "SELECT * FROM public.t",
    ] {
        assert_eq!(sqlmode::check(ok, DB), Ok(()), "{ok}");
    }
    // The check follows the org: the same statement is fine in its own database.
    assert_eq!(sqlmode::check("SHOW TABLES FROM demo", "demo"), Ok(()));
    assert!(sqlmode::check("SHOW TABLES FROM public", "demo").is_err());
}

#[tokio::test]
async fn ac_0_3_7_sql_mode_refuses_tql() {
    for tql in [
        "TQL EVAL (0, 10, '1s') up",
        "tql eval (0, 10, '1s') up",
        "  TQL   EXPLAIN (0,10,'1s') rate(x[5m])",
        "TQL ANALYZE (0,10,'1s') x",
        "TQL EVAL (0, 10, '1s') m{__schema__=\"demo\"}",
        "TQL EVAL (0, 10, '1s') m{__database__=\"demo\"}",
        "tql eval (0,1,'1s') {\"__sch\\u0065ma__\"=\"demo\"}",
        "TQL EVAL (0,1,'1s') {__SCHEMA__=\"demo\"}",
        "\n\tTQL EVAL (0,1,'1s') up",
        "TQL/**/EVAL (0,1,'1s') up",
        "TQL EVAL (0,1,'1s') up; SELECT 1",
    ] {
        let e = sqlmode::check(tql, DB).unwrap_err().0;
        assert!(e.contains("TQL is not available"), "{tql}: {e}");
    }
    // Over HTTP: refused with 400 before anything reaches the database.
    let f = Fake::new(sample_db);
    let a = app(f.clone());
    let q = "TQL%20EVAL%20(0%2C10%2C%271s%27)%20m%7B__schema__%3D%22demo%22%7D";
    let r = get(&a, &format!("{BASE}/sql?sql={q}&from=1000&to=2000")).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert!(
        r.json()["error"]
            .as_str()
            .unwrap()
            .contains("TQL is not available")
    );
    for stmt in [
        "SHOW%20TABLES%20FROM%20demo",
        "SHOW%20CREATE%20TABLE%20demo.t",
    ] {
        let r = get(&a, &format!("{BASE}/sql?sql={stmt}&from=1000&to=2000")).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{stmt}");
        assert!(
            r.json()["error"]
                .as_str()
                .unwrap()
                .contains("outside this org's database"),
            "{stmt}"
        );
    }
    assert!(
        f.seen_names().is_empty(),
        "rejected statements never reach the database"
    );
}
