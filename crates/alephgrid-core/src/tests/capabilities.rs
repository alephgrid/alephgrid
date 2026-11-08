//! 4.07–4.08 in the engine: what user SQL and formulas may do, the database-level lockdown,
//! and SQL changes applied with Alephgrid's bookkeeping. Every refusal is checked to leave
//! the workbook exactly as it was (`fingerprint`).

use super::*;
use crate::sqlpolicy::{BadParameter, IsChange, NotAllowed, ParamType};
use proptest::prelude::*;
use serde_json::json;

fn setup() -> TempWb {
    let mut t = TempWb::new();
    t.wb.create_table_from_query("t", "SELECT i AS id, 'r' || (i % 3) AS region, (i * 1.5)::DECIMAL(10,2) AS amount FROM range(10) r(i)").unwrap();
    t.wb.add_formula_column("t", "twice", "[amount] * 2").unwrap();
    t.wb.create_table_from_query("u", "SELECT i AS id FROM range(3) r(i)").unwrap();
    t
}

/// Statements that must be refused by every path that runs user SQL.
const HOSTILE: &[&str] = &[
    "SELECT 1; DROP TABLE t; SELECT 1",
    "SELECT 1) AS _q(_c0) LIMIT 1; DROP TABLE t; SELECT * FROM (SELECT 1",
    "SELECT 1\n) AS _q; DROP TABLE t; --",
    "SELECT * FROM t; INSERT INTO t (id) VALUES (999)",
    "DROP TABLE t",
    "CREATE TABLE evil AS SELECT 1",
    "ALTER TABLE t ADD COLUMN x INT",
    "ATTACH '/tmp/evil.duckdb' AS evil",
    "COPY t TO '/tmp/alephgrid-evil.csv'",
    "COPY (SELECT 1) TO '/tmp/alephgrid-evil.csv'",
    "EXPORT DATABASE '/tmp/alephgrid-evil'",
    "INSTALL httpfs",
    "LOAD httpfs",
    "SET enable_external_access = true",
    "SET memory_limit = '100TB'",
    "RESET threads",
    "PRAGMA enable_profiling",
    "CALL pragma_version()",
    "CHECKPOINT",
    "SELECT * FROM read_csv('/etc/passwd')",
    "SELECT * FROM read_text('/etc/hostname')",
    "select * from READ_PARQUET('x.parquet')",
    "SELECT * FROM '/etc/passwd'",
    "SELECT * FROM 'https://example.com/data.csv'",
    "SELECT * FROM glob('/*')",
    "SELECT * FROM query('SELECT 1')",
    "SELECT * FROM duckdb_settings()",
    "SELECT * FROM duckdb_tables",
    "SELECT * FROM information_schema.tables",
    "SELECT * FROM _rf_history",
    "SELECT * FROM \"_RF_META\"",
    "SELECT * FROM main._rf_views",
    "SELECT * FROM rf_scratch.anything",
    "SELECT current_setting('temp_directory')",
    "SELECT json_execute_serialized_sql(json_serialize_sql('SELECT 1'))",
    "SELECT nextval('seq')",
    "WITH duckdb_settings AS (SELECT 1 AS x) SELECT * FROM (SELECT * FROM duckdb_settings)",
    "/* comment */ SELECT * FROM -- line\n _rf_tables",
    "SHOW TABLES",
    "PIVOT t ON region USING sum(amount)",
];

#[test]
fn every_user_sql_path_refuses_the_same_statements_and_changes_nothing() {
    let mut t = setup();
    let out = t.dir.path().join("out.csv");
    for sql in HOSTILE {
        let before = fingerprint(&t.wb);
        let results = [
            ("run_query", t.wb.run_query(sql, &[], 10).err()),
            ("preview_query", t.wb.preview_query(sql, 10).err()),
            ("export_query", t.wb.export_query(sql, &[], &out, ExportFormat::Csv).err()),
            ("create_table_from_query", t.wb.create_table_from_query_with("x", sql, &[], None).err()),
            ("create_sheet_from_query", t.wb.create_sheet_from_query_with("x", sql, &[], 10, true).err()),
            ("save_query", t.wb.save_query(None, "x", QueryKind::Query, sql, &[]).err()),
            ("sql_change", t.wb.sql_change(sql, &[]).err()),
        ];
        for (path, err) in results {
            let e = err.unwrap_or_else(|| panic!("{path} ran {sql:?}"));
            let refused = e.downcast_ref::<NotAllowed>().is_some()
                || e.downcast_ref::<BadParameter>().is_some()
                || e.downcast_ref::<IsChange>().is_some()
                || format!("{e:#}").starts_with("SQL error:");
            assert!(refused, "{path} {sql:?} failed for another reason: {e:#}");
        }
        assert_eq!(fingerprint(&t.wb), before, "{sql:?} changed the workbook");
        assert!(!out.exists(), "{sql:?} wrote a file");
        assert!(!std::path::Path::new("/tmp/alephgrid-evil.csv").exists());
    }
    assert_eq!(scalar(&t.wb, "SELECT count(*) FROM t"), json!(10));
}

#[test]
fn formulas_get_the_same_function_policy() {
    let mut t = setup();
    let before = fingerprint(&t.wb);
    for f in ["current_setting('temp_directory')", "nextval('s')", "json_execute_serialized_sql('{}')", "upper(getvariable('x'))", "(SELECT 1)", "1; DROP TABLE t"] {
        assert!(t.wb.add_formula_column("t", "bad", f).is_err(), "{f}");
        assert!(t.wb.update_formula("t", "twice", f).is_err(), "{f}");
    }
    assert_eq!(fingerprint(&t.wb), before);
    // Ordinary functions still work.
    t.wb.add_formula_column("t", "label", "upper([region]) || '-' || CAST([id] AS VARCHAR)").unwrap();
    assert_eq!(scalar(&t.wb, "SELECT label FROM t WHERE id = 4"), json!("R1-4"));
}

/// Regression: the `duckdb` crate's `prepare` runs every statement but the last of a
/// multi-statement string. On the phase-3 code, `create_sheet_from_query` described the query
/// outside any transaction, so `SELECT 1; DROP TABLE t; SELECT 1` dropped `t` with no revision
/// or history (verified in a baseline worktree; previews were saved by their snapshot's
/// rollback). Every user-SQL path is covered here.
#[test]
fn earlier_statements_of_a_multi_statement_string_never_run() {
    let mut t = setup();
    for sql in ["SELECT 1; DROP TABLE t; SELECT 1", "SELECT 1; CREATE TABLE made AS SELECT 1; SELECT 2"] {
        assert!(t.wb.preview_query(sql, 10).is_err());
        assert!(t.wb.create_sheet_from_query("S", sql, 10).is_err());
        assert!(t.wb.create_table_from_query("x", sql).is_err());
        assert!(t.wb.export_query(sql, &[], &t.dir.path().join("x.csv"), ExportFormat::Csv).is_err());
    }
    assert_eq!(scalar(&t.wb, "SELECT count(*) FROM t"), json!(10));
    assert!(t.wb.list_tables().unwrap().iter().all(|x| x.name != "made"));
}

#[test]
fn the_database_refuses_files_extensions_and_settings_once_restricted() {
    let mut t = setup();
    let allowed = t.dir.path().join("allowed");
    std::fs::create_dir_all(&allowed).unwrap();
    let outside = t.dir.path().join("outside.csv");
    std::fs::write(&outside, "x\n1\n").unwrap();
    std::fs::write(allowed.join("in.csv"), "x\n7\n").unwrap();
    t.wb.restrict_access(&[&allowed]).unwrap();
    // These go past the SQL policy (raw connection) to test the database layer by itself.
    for sql in [
        format!("SELECT * FROM read_csv('{}')", outside.display()),
        format!("SELECT * FROM '{}'", outside.display()),
        format!("COPY (SELECT 1) TO '{}'", t.dir.path().join("w.csv").display()),
        format!("ATTACH '{}' AS other", t.dir.path().join("other.duckdb").display()),
        "SELECT * FROM read_csv('https://example.com/x.csv')".to_string(),
        "INSTALL httpfs".to_string(),
        "LOAD httpfs".to_string(),
        "SET enable_external_access = true".to_string(),
        "SET allowed_directories = ['/']".to_string(),
        "SET autoload_known_extensions = true".to_string(),
    ] {
        let fp = fingerprint(&t.wb);
        let e = t.wb.raw_query(&sql);
        if sql.starts_with("SET autoload") {
            // Allowed to flip, but loading stays impossible with external access off.
            let _ = e;
            assert!(t.wb.raw_query("LOAD httpfs").is_err());
            t.wb.raw_query("SET autoload_known_extensions = false").unwrap();
            continue;
        }
        assert!(e.is_err(), "{sql} was allowed");
        assert_eq!(fingerprint(&t.wb), fp, "{sql}");
    }
    assert!(!t.dir.path().join("w.csv").exists());
    // The application's own imports and exports inside the allowed directory still work.
    let info = t.wb.import_file(&allowed.join("in.csv"), Some("imported")).unwrap();
    assert_eq!(info.row_count, 1);
    let v = t.wb.open_view(spec("imported")).unwrap();
    t.wb.export_view(v.view_id, &allowed.join("out.csv"), ExportFormat::Csv).unwrap();
    assert_eq!(std::fs::read_to_string(allowed.join("out.csv")).unwrap(), "x\n7\n");
    assert!(t.wb.import_file(&outside, Some("nope")).is_err(), "imports outside the allowed directories fail");
    let (n, _) = t.wb.export_query("SELECT * FROM t", &[], &allowed.join("q.csv"), ExportFormat::Csv).unwrap();
    assert_eq!(n, 10);
    assert!(t.wb.export_query("SELECT * FROM t", &[], &t.dir.path().join("q.csv"), ExportFormat::Csv).is_err());
}

// ------------------------------------------------------------------ 4.08

/// Reference model of table `t` for SQL changes: (record id, id, region, amount).
fn model_rows(t: &mut TempWb) -> Vec<(i64, i64, String, String)> {
    let v = t.wb.open_view(spec("t")).unwrap();
    let block = t.wb.fetch(v.view_id, 0, 5_000).unwrap();
    block
        .rows
        .iter()
        .zip(block.ids.iter())
        .map(|(r, rid)| (*rid, r[0].as_i64().unwrap_or(-1), r[1].as_str().unwrap_or("∅").to_string(), r[2].as_str().unwrap_or("∅").to_string()))
        .collect()
}

#[test]
fn sql_changes_keep_record_ids_order_index_revisions_and_history() {
    let mut t = setup();
    let start = model_rows(&mut t);
    let rev = t.wb.revision().unwrap();

    // UPDATE: values change in place; ids and order do not.
    let out = t.wb.sql_change("UPDATE t SET amount = amount + $d, region = upper(region) WHERE id % 2 = 0", &[QueryParam { name: "d".into(), ty: ParamType::Decimal, value: Some("0.01".into()) }]).unwrap();
    assert_eq!((out.kind.as_str(), out.rows, out.table.as_str()), ("update", 5, "t"));
    let after = model_rows(&mut t);
    for (b, a) in start.iter().zip(after.iter()) {
        assert_eq!(a.0, b.0, "record ids stay");
        assert_eq!(a.1, b.1);
        if b.1 % 2 == 0 {
            let expected = format!("{:.2}", b.3.parse::<f64>().unwrap() + 0.01);
            assert_eq!((a.2.as_str(), a.3.as_str()), (b.2.to_uppercase().as_str(), expected.as_str()));
        } else {
            assert_eq!(a, b);
        }
    }
    // The formula column follows; a WHERE may use formula columns and other tables.
    assert_eq!(scalar(&t.wb, "SELECT twice FROM t WHERE id = 2"), json!("6.02"));
    let out = t.wb.sql_change("DELETE FROM t WHERE twice > 20 OR id IN (SELECT id FROM u WHERE id = 0)", &[]).unwrap();
    assert_eq!((out.kind.as_str(), out.rows), ("delete", 4)); // ids 0, 7, 8, 9 (twice of 7 = 21.00)
    let remaining = model_rows(&mut t);
    assert_eq!(remaining.iter().map(|r| r.1).collect::<Vec<_>>(), [1, 2, 3, 4, 5, 6]);
    assert!(remaining.iter().all(|r| start.iter().any(|s| s.0 == r.0)), "surviving rows keep their ids");
    t.wb.verify_index("t").unwrap(); // consistent (errors otherwise)

    // INSERT: new rows after the last one, with new ids; a column list or all data columns.
    let out = t.wb.sql_change("INSERT INTO t (region, id) VALUES ('new', 100), ($r, 101)", &[QueryParam { name: "r".into(), ty: ParamType::Text, value: Some("x'y".into()) }]).unwrap();
    assert_eq!((out.kind.as_str(), out.rows), ("insert", 2));
    let out = t.wb.sql_change("INSERT INTO t SELECT id + 200, 'copy', amount FROM t WHERE id < 3 ORDER BY id", &[]).unwrap();
    assert_eq!(out.rows, 2);
    let rows = model_rows(&mut t);
    assert_eq!(rows.iter().map(|r| r.1).collect::<Vec<_>>(), [1, 2, 3, 4, 5, 6, 100, 101, 201, 202]);
    assert_eq!(rows[7].2, "x'y");
    assert_eq!(rows[6].3, "∅", "unlisted columns are NULL");
    let max_old = start.iter().map(|r| r.0).max().unwrap();
    assert!(rows[6..].iter().all(|r| r.0 > max_old), "new rows get new ids");
    t.wb.verify_index("t").unwrap(); // consistent (errors otherwise)

    // One revision and one history entry per change.
    assert_eq!(t.wb.revision().unwrap(), rev + 4);
    let history = t.wb.history(rev, 10).unwrap();
    assert_eq!(history.iter().map(|h| h.op.as_str()).collect::<Vec<_>>(), ["sql_change"; 4]);

    // A retried mutation id applies once.
    let apply = |wb: &mut Workbook| {
        wb.mutate(Mutation { id: Some("m-1"), op: "sql_change", actor: None, summary: "insert".into() }, None, |wb| {
            Ok(serde_json::to_value(wb.sql_change("INSERT INTO u VALUES (50)", &[])?)?)
        })
        .unwrap()
    };
    let first = apply(&mut t.wb);
    let second = apply(&mut t.wb);
    assert!(!first.replayed && second.replayed);
    assert_eq!(first.revision, second.revision);
    assert_eq!(scalar(&t.wb, "SELECT count(*) FROM u WHERE id = 50"), json!(1));

    t.reopen();
    assert_eq!(model_rows(&mut t), rows, "changes are durable");
    t.wb.verify_index("t").unwrap(); // consistent (errors otherwise)
}

#[test]
fn sql_changes_that_would_bypass_bookkeeping_are_refused_whole() {
    let mut t = setup();
    for sql in [
        "UPDATE t SET _rf_id = 1",
        "UPDATE t SET _RF_ORD = 0 WHERE id = 1",
        "UPDATE t SET twice = 1",
        "INSERT INTO t (twice) VALUES (1)",
        "INSERT INTO t (_rf_id, id) VALUES (1, 1)",
        "UPDATE _rf_history SET op = 'x'",
        "DELETE FROM _rf_tables",
        "INSERT INTO _rf_meta VALUES ('format', '99')",
        "UPDATE t SET id = 1 FROM u WHERE t.id = u.id",
        "DELETE FROM t USING u WHERE t.id = u.id",
        "UPDATE t SET id = 1 RETURNING *",
        "INSERT OR REPLACE INTO t (id) VALUES (1)",
        "INSERT INTO t (id) VALUES (1) ON CONFLICT DO NOTHING",
        "UPDATE missing SET id = 1",
        "UPDATE t SET id = 'not a number'",
        "UPDATE t SET id = 1.5",
        "UPDATE t SET amount = 1.005",
        "UPDATE t SET amount = 99999999999",
        "INSERT INTO t (id) VALUES (1), ('x')",
        "INSERT INTO t (id, region) VALUES (1)",
        "UPDATE t SET id = 1 ), (2 WHERE id = 3",
        "UPDATE t SET id = 1) AS _rf_v0 FROM t; DROP TABLE t; --",
        "UPDATE t SET id = (SELECT current_setting('threads'))",
        "UPDATE t SET region = (SELECT content FROM read_text('/etc/hostname'))",
        "DELETE FROM t WHERE id IN (SELECT id FROM _rf_history)",
        "DELETE FROM t WHERE id = $missing",
        "INSERT INTO t SELECT * FROM read_csv('/etc/passwd')",
        "TRUNCATE t",
        "UPDATE t SET id = 1; UPDATE t SET id = 2",
    ] {
        let before = fingerprint(&t.wb);
        let e = t.wb.sql_change(sql, &[]).expect_err(sql);
        assert!(!format!("{e:#}").is_empty());
        assert_eq!(fingerprint(&t.wb), before, "{sql:?} changed something");
    }
    // Injected failure after the change, before commit: rolled back.
    let before = fingerprint(&t.wb);
    for sql in ["UPDATE t SET id = id + 1", "DELETE FROM t WHERE id > 5", "INSERT INTO t (id) VALUES (1)"] {
        faults::fail_next("sql_change");
        assert!(t.wb.sql_change(sql, &[]).is_err());
        assert_eq!(fingerprint(&t.wb), before, "{sql}");
    }
    t.wb.verify_index("t").unwrap(); // consistent (errors otherwise)
}

#[test]
fn queries_name_the_change_path_for_dml() {
    let t = setup();
    for sql in ["UPDATE t SET id = 1", "  delete from t", "INSERT INTO t VALUES (1, 'a', 1)"] {
        let e = t.wb.run_query(sql, &[], 10).unwrap_err();
        assert!(e.downcast_ref::<IsChange>().is_some(), "{sql}: {e:#}");
    }
}

// ------------------------------------------------------------------ fuzz

/// Fragments that exercise quoting, comments, nesting, statement separators and forbidden
/// names. Random sequences of them are run as queries and as changes: whatever happens, a
/// query never changes the workbook, and a change either fails without a trace or commits
/// exactly one revision with a consistent table.
const FRAGMENTS: &[&str] = &[
    "SELECT", "*", "1", "FROM", "t", "u", "_rf_history", "duckdb_settings()", "read_csv('/etc/passwd')", "WHERE", "id", "=",
    "$p", "?", "'", "''", "\"", ";", "--", "/*", "*/", "(", ")", "UPDATE", "SET", "DELETE", "INSERT INTO", "VALUES", "DROP TABLE t",
    "WITH", "x AS", ",", "UNION ALL", "current_setting('threads')", "'/tmp/x.csv'", "COPY", "TO", "ATTACH", "\n", "e'\\''", "$$;$$",
    "region", "amount", "+", "twice", "rf_scratch.v1", "main.", "LIMIT 3", "ORDER BY 1",
];

proptest! {
    #![proptest_config(ProptestConfig { cases: std::env::var("PROPTEST_CASES").ok().and_then(|c| c.parse().ok()).unwrap_or(48), ..ProptestConfig::default() })]

    #[test]
    fn random_statements_never_change_state_through_queries(parts in prop::collection::vec(0..FRAGMENTS.len(), 1..14)) {
        let mut t = setup();
        let sql = parts.iter().map(|&i| FRAGMENTS[i]).collect::<Vec<_>>().join(" ");
        let params = [QueryParam { name: "p".into(), ty: ParamType::Integer, value: Some("1".into()) }];
        let before = fingerprint(&t.wb);
        let _ = t.wb.run_query(&sql, &params, 10);
        let _ = t.wb.run_query(&sql, &[], 10);
        prop_assert_eq!(fingerprint(&t.wb), before.clone(), "query {:?}", sql);
        let rev = t.wb.revision().unwrap();
        match t.wb.sql_change(&sql, &params).or_else(|_| t.wb.sql_change(&sql, &[])) {
            Ok(_) => {
                prop_assert_eq!(t.wb.revision().unwrap(), rev + 1, "{:?}", sql);
                prop_assert!(t.wb.verify_index("t").is_ok() && t.wb.verify_index("u").is_ok(), "index consistent after {:?}", sql);
                prop_assert!(t.wb.list_tables().unwrap().len() == 2);
            }
            Err(_) => prop_assert_eq!(fingerprint(&t.wb), before, "change {:?}", sql),
        }
    }
}

/// Regression (found while writing 4.T02): table operations took any table name, so a
/// client could open a view on `_rf_history`, add a column to `_rf_meta` or drop
/// `_rf_views` directly over the API, without any SQL.
#[test]
fn internal_tables_are_not_reachable_through_table_operations() {
    let mut t = setup();
    let before = fingerprint(&t.wb);
    for name in ["_rf_history", "_rf_meta", "_RF_VIEWS", "_rf_tables", "_rf_queries"] {
        assert!(t.wb.open_view(spec(name)).is_err(), "open_view {name}");
        assert!(t.wb.columns(name).is_err(), "columns {name}");
        assert!(t.wb.table_info(name).is_err(), "table_info {name}");
        assert!(t.wb.drop_table(name).is_err(), "drop_table {name}");
        assert!(t.wb.rename_table(name, "x").is_err(), "rename_table {name}");
        assert!(t.wb.add_column(name, "x", "INTEGER").is_err(), "add_column {name}");
        assert!(t.wb.add_formula_column(name, "x", "1").is_err(), "add_formula_column {name}");
        assert!(t.wb.drop_column(name, "op").is_err(), "drop_column {name}");
        assert!(t.wb.rename_column(name, "op", "x").is_err(), "rename_column {name}");
        assert!(t.wb.save_view(None, "v", spec(name), Default::default()).is_err(), "save_view {name}");
        assert!(t.wb.export_table(name, &t.dir.path().join("x.csv"), ExportFormat::Csv).is_err(), "export_table {name}");
    }
    assert_eq!(fingerprint(&t.wb), before);
    assert!(!t.dir.path().join("x.csv").exists());
}

/// Regression (found by the export cancel race in `tests/sqlworkspace.rs`): when a read's
/// COMMIT failed (e.g. interrupted by a cancel that arrived as the read finished), the
/// snapshot transaction was left open and every later read on that connection failed.
#[test]
fn a_failed_snapshot_commit_does_not_poison_the_connection() {
    let t = setup();
    let r = t.wb.reader().unwrap();
    faults::fail_next("snapshot_commit");
    assert!(r.run_query("SELECT count(*) FROM t", &[], 10).is_err());
    assert_eq!(r.run_query("SELECT count(*) FROM t", &[], 10).unwrap().rows, vec![vec![json!(10)]]);
}

/// Regression (nightly pipeline 34): a reader that went back to the server's pool inside an
/// aborted transaction failed every later read with "Current transaction is aborted".
/// `release` takes such a transaction back.
#[test]
fn a_reader_left_in_an_aborted_transaction_is_released() {
    let t = setup();
    let r = t.wb.reader().unwrap();
    assert!(!r.release(), "no transaction was open");
    r.conn.execute_batch("BEGIN TRANSACTION;").unwrap();
    assert!(r.conn.execute_batch("SELECT error('x');").is_err());
    assert!(r.run_query("SELECT count(*) FROM t", &[], 10).is_err(), "the aborted transaction blocks reads");
    assert!(r.release(), "the open transaction is reported");
    assert_eq!(r.run_query("SELECT count(*) FROM t", &[], 10).unwrap().rows, vec![vec![json!(10)]]);
}

/// A panic inside a read's snapshot, after a statement failed in it, does not leave the
/// snapshot's transaction open on the connection.
#[test]
fn a_panic_inside_a_snapshot_takes_its_transaction_back() {
    let t = setup();
    let r = t.wb.reader().unwrap();
    let db = r.db();
    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = db.snapshot(|| -> anyhow::Result<()> {
            assert!(r.conn.execute_batch("SELECT error('x');").is_err());
            panic!("a bug after a failed statement");
        });
    }));
    assert!(panicked.is_err());
    assert!(!r.release(), "no transaction was left open");
    assert_eq!(r.run_query("SELECT count(*) FROM t", &[], 10).unwrap().rows, vec![vec![json!(10)]]);
}
