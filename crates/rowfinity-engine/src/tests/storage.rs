//! 3.06–3.08 in the engine: stable record ids, order keys without tail renumbering,
//! revisions and idempotent mutations, migrations of the committed v1 fixture, snapshot
//! backups, reader connections and scratch cleanup.

use super::*;
use crate::storage;
use serde_json::json;
use std::path::{Path, PathBuf};

fn fixture_copy(dir: &Path) -> PathBuf {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/v1/data.duckdb");
    let dst = dir.join("data.duckdb");
    std::fs::copy(src, &dst).unwrap();
    dst
}

fn scratch_tables(wb: &Workbook) -> i64 {
    // Internal state: read past the SQL policy, which refuses duckdb_tables() to users.
    match wb.raw_query("SELECT count(*) FROM duckdb_tables() WHERE database_name = 'rf_scratch'").unwrap()[0][0] {
        duckdb::types::Value::BigInt(n) => n,
        ref other => panic!("{other:?}"),
    }
}

// ------------------------------------------------------------------- 3.08

#[test]
fn v1_fixture_migrates_with_a_copy_and_keeps_every_value() {
    let dir = tempfile::tempdir().unwrap();
    let path = fixture_copy(dir.path());
    let original = std::fs::read(&path).unwrap();
    let mut wb = Workbook::open(&path).unwrap();
    let report = wb.open_report().clone();
    assert_eq!(report.migrated_from, Some(1));
    let backup = report.backup.unwrap();
    assert_eq!(std::fs::read(&backup).unwrap(), original, "the pre-migration copy is the untouched file");

    // Oracle: the fixture README, written when the fixture was made by the phase-2 server.
    let v = wb.open_view(spec("people")).unwrap();
    assert_eq!(v.columns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["id", "name", "amount", "day", "double"]);
    let block = wb.fetch(v.view_id, 0, 10).unwrap();
    assert_eq!(
        block.rows,
        vec![
            vec![json!(1), json!("Ada"), json!(10.5), json!("2024-02-29"), json!(21.0)],
            vec![json!(3), json!("東京"), json!(0.0), json!("2024-03-02"), json!(0.0)],
            vec![json!(null), json!("Inserted"), json!(1.25), json!(null), json!(2.5)],
            vec![json!(4), json!("Grace"), json!(99.99), json!("2024-03-03"), json!(199.98)],
            vec![json!(5), json!(null), json!(7.1), json!("2024-03-04"), json!(14.2)],
        ]
    );
    // Format-1 ids were positions; they become the stable ids.
    assert_eq!(block.ids, vec![0, 1, 2, 3, 4]);
    assert!(wb.verify_index("people").unwrap(), "migrated tables are dense");
    let saved = wb.saved_views(Some("people")).unwrap();
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].name, "Big spenders");
    assert_eq!(saved[0].spec.hidden, vec!["day".to_string()]);
    assert_eq!(cell_text(&wb, 0, 0, 1).as_deref(), Some("42"));
    assert_eq!(cell_text(&wb, 0, 1, 0).as_deref(), Some("44"));
    assert_eq!(scalar(&wb, "SELECT count(*) FROM flights"), json!(1000));
    assert_eq!(wb.revision().unwrap(), 0);

    // New rows get ids after the old ones, and nothing is migrated twice.
    wb.insert_rows(v.view_id, 1, 1).unwrap();
    assert_eq!(wb.fetch(v.view_id, 0, 10).unwrap().ids, vec![0, 5, 1, 2, 3, 4]);
    drop(wb);
    let wb = Workbook::open(&path).unwrap();
    assert_eq!(wb.open_report().migrated_from, None);
    assert_eq!(wb.revision().unwrap(), 1);
}

#[test]
fn a_failed_migration_leaves_the_file_as_it_was() {
    let dir = tempfile::tempdir().unwrap();
    let path = fixture_copy(dir.path());
    faults::fail_next("migrate");
    let err = Workbook::open(&path).err().expect("migration should fail");
    assert!(format!("{err:#}").contains("migrating workbook from storage format 1"), "{err:#}");
    {
        let conn = duckdb::Connection::open(&path).unwrap();
        assert_eq!(storage::format_of(&conn).unwrap(), 1, "the transaction rolled back");
        let has_ord: i64 = conn
            .query_row("SELECT count(*) FROM duckdb_columns() WHERE table_name = 'people' AND column_name = '_rf_ord'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(has_ord, 0);
    }
    let wb = Workbook::open(&path).unwrap();
    assert_eq!(wb.open_report().migrated_from, Some(1));
    assert_eq!(scalar(&wb, "SELECT count(*) FROM people"), json!(5));
}

#[test]
fn files_from_a_newer_format_are_refused_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("new.duckdb");
    {
        let wb = Workbook::open(&path).unwrap();
        wb.checkpoint().unwrap();
    }
    {
        let conn = duckdb::Connection::open(&path).unwrap();
        conn.execute_batch("UPDATE _rf_meta SET value = '99' WHERE key = 'format'; CHECKPOINT;").unwrap();
    }
    let before = std::fs::read(&path).unwrap();
    let err = Workbook::open(&path).err().expect("must refuse");
    let too_new = err.downcast_ref::<storage::TooNew>().expect("typed error");
    assert_eq!(too_new.found, 99);
    assert!(err.to_string().contains("newer version of Rowfinity"));
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[test]
fn a_database_is_bound_to_one_workbook_id() {
    let t = TempWb::new();
    t.wb.bind_identity("abc").unwrap();
    t.wb.bind_identity("abc").unwrap();
    let err = t.wb.bind_identity("xyz").unwrap_err();
    assert!(err.downcast_ref::<storage::WrongWorkbook>().is_some(), "{err:#}");
}

// ------------------------------------------------------------------- 3.06

/// Rows whose order key differs from a snapshot table taken earlier, and rows new since.
fn moved_and_new(wb: &Workbook, table: &str, snap: &str) -> (i64, Vec<i64>) {
    let moved = scalar(wb, &format!("SELECT count(*) FROM {table} t JOIN {snap} s ON t._rf_id = s.id WHERE t._rf_ord <> s.ord"));
    let new = wb
        .preview_query(&format!("SELECT _rf_id AS id FROM {table} WHERE _rf_id NOT IN (SELECT id FROM {snap}) ORDER BY 1"), 10_000)
        .unwrap()
        .rows
        .iter()
        .map(|r| r[0].as_i64().unwrap())
        .collect();
    (moved.as_i64().unwrap(), new)
}

#[test]
fn inserts_and_deletes_leave_other_rows_untouched() {
    let mut t = TempWb::new();
    t.wb.create_table_from_query("big", "SELECT i AS n FROM range(100000) r(i)").unwrap();
    t.wb.create_table_from_query("snap", "SELECT _rf_id AS id, _rf_ord AS ord FROM big").unwrap();
    let v = t.wb.open_view(spec("big")).unwrap().view_id;
    t.wb.insert_rows(v, 10, 3).unwrap();
    t.wb.delete_rows(v, 50_000, 2).unwrap();
    // No existing row changed its id or order key: 3 rows added, 2 removed, nothing renumbered.
    let (moved, new) = moved_and_new(&t.wb, "big", "snap");
    assert_eq!(moved, 0);
    assert_eq!(new, vec![100_000, 100_001, 100_002]);
    assert_eq!(scalar(&t.wb, "SELECT count(*) FROM big"), json!(100_001));
    // Positions still map exactly, through the index.
    let b = t.wb.fetch(v, 9, 5).unwrap();
    assert_eq!(b.ids, vec![9, 100_000, 100_001, 100_002, 10]);
    assert_eq!(b.rows[0], vec![json!(9)]);
    assert_eq!(b.rows[1], vec![json!(null)]);
    // After the 3-row insert, position p >= 13 held record p - 3, so deleting positions
    // 50,000 and 50,001 removed records 49,997 and 49,998.
    assert_eq!(scalar(&t.wb, "SELECT count(*) FROM big WHERE _rf_id IN (49997, 49998)"), json!(0));
    let b = t.wb.fetch(v, 49_999, 3).unwrap();
    assert_eq!(b.ids, vec![49_996, 49_999, 50_000]);
    assert_eq!(b.rows, vec![vec![json!(49_996)], vec![json!(49_999)], vec![json!(50_000)]]);
    assert!(!t.wb.verify_index("big").unwrap());
    t.reopen();
    let v = t.wb.open_view(spec("big")).unwrap().view_id;
    assert_eq!(t.wb.fetch(v, 9, 5).unwrap().ids, vec![9, 100_000, 100_001, 100_002, 10]);
    assert_eq!(t.wb.view_info(v).unwrap().row_count, 100_001);
}

#[test]
fn exhausted_gaps_are_rebalanced_locally() {
    let mut t = TempWb::new();
    t.wb.create_table_from_query("t", "SELECT i AS n FROM range(5000) r(i)").unwrap();
    t.wb.create_table_from_query("snap", "SELECT _rf_id AS id, _rf_ord AS ord FROM t").unwrap();
    let v = t.wb.open_view(spec("t")).unwrap().view_id;
    // Each insert at position 1 halves the gap after row 0; 2^20 runs out after 20.
    for _ in 0..30 {
        t.wb.insert_rows(v, 1, 1).unwrap();
    }
    let (moved, new) = moved_and_new(&t.wb, "t", "snap");
    assert!(moved > 0, "the gap must have been exhausted at least once");
    assert!(moved < 200, "rebalancing stays near the insert point; moved {moved} of 5000");
    assert_eq!(new.len(), 30);
    let ids = t.wb.fetch(v, 0, 32).unwrap().ids;
    // Newest insert is first after row 0 (each went to position 1).
    let expected: Vec<i64> = std::iter::once(0).chain((5000..5030).rev()).chain(std::iter::once(1)).collect();
    assert_eq!(ids, expected);
    t.wb.verify_index("t").unwrap();
    let b = t.wb.fetch(v, 31, 3).unwrap();
    assert_eq!(b.rows, vec![vec![json!(1)], vec![json!(2)], vec![json!(3)]]);
}

#[test]
fn an_edit_by_record_lands_on_that_record_after_rows_move() {
    let mut t = TempWb::new();
    t.wb.create_table_from_query("p", "SELECT * FROM (VALUES ('a'), ('b'), ('c')) v(name)").unwrap();
    let v = t.wb.open_view(spec("p")).unwrap().view_id;
    let fetched = t.wb.fetch(v, 0, 3).unwrap();
    let c_id = fetched.ids[2];
    // Someone else inserts two rows at the top; "c" is now at position 4.
    t.wb.insert_rows(v, 0, 2).unwrap();
    let edit = CellEdit { row: 2, column: "name".into(), value: Some("C!".into()), record: Some(c_id) };
    t.wb.edit(v, &[edit]).unwrap();
    let rows = t.wb.fetch(v, 0, 5).unwrap().rows;
    assert_eq!(rows, vec![vec![json!(null)], vec![json!(null)], vec![json!("a")], vec![json!("b")], vec![json!("C!")]]);
    // A record deleted meanwhile: the edit fails and changes nothing, not even the revision.
    t.wb.delete_rows(v, 4, 1).unwrap();
    let rev = t.wb.revision().unwrap();
    let edits = [
        CellEdit { row: 0, column: "name".into(), value: Some("x".into()), record: None },
        CellEdit { row: 0, column: "name".into(), value: Some("y".into()), record: Some(c_id) },
    ];
    let err = t.wb.edit(v, &edits).unwrap_err();
    assert!(format!("{err:#}").contains("deleted by someone else"), "{err:#}");
    assert_eq!(t.wb.revision().unwrap(), rev);
    assert_eq!(t.wb.fetch(v, 0, 1).unwrap().rows, vec![vec![json!(null)]]);
}

#[test]
fn sorted_views_break_ties_by_display_order() {
    let mut t = TempWb::new();
    t.wb.create_table_from_query("p", "SELECT * FROM (VALUES (1, 'x'), (1, 'y')) v(k, name)").unwrap();
    let v = t.wb.open_view(spec("p")).unwrap().view_id;
    t.wb.insert_rows(v, 0, 1).unwrap();
    t.wb.edit(v, &[edit(0, "k", "1"), edit(0, "name", "w")]).unwrap();
    let mut s = spec("p");
    s.sort.push(SortKey { column: "k".into(), desc: false });
    let sorted = t.wb.open_view(s).unwrap().view_id;
    let names: Vec<Value> = t.wb.fetch(sorted, 0, 3).unwrap().rows.into_iter().map(|r| r[1].clone()).collect();
    assert_eq!(names, vec![json!("w"), json!("x"), json!("y")], "the inserted row is first in display order");
}

// ------------------------------------------------------------------- 3.07

#[test]
fn every_change_is_one_revision_with_a_history_entry() {
    let mut t = TempWb::new();
    assert_eq!(t.wb.revision().unwrap(), 0);
    t.wb.create_table_from_query("p", "SELECT 1 AS n").unwrap();
    let v = t.wb.open_view(spec("p")).unwrap().view_id;
    t.wb.edit(v, &[edit(0, "n", "2")]).unwrap();
    // Refused before any change: no revision.
    assert!(t.wb.edit(v, &[edit(0, "n", "two")]).is_err());
    t.wb.with_sheets(|s| s.create("S")).unwrap();
    t.wb.insert_rows(v, 0, 1).unwrap();
    assert_eq!(t.wb.revision().unwrap(), 4);
    let ops: Vec<String> = t.wb.history(0, 100).unwrap().into_iter().map(|h| h.op).collect();
    assert_eq!(ops, ["create_table_from_query", "edit", "sheets", "insert_rows"]);
    let revisions: Vec<u64> = t.wb.history(0, 100).unwrap().into_iter().map(|h| h.revision).collect();
    assert_eq!(revisions, vec![1, 2, 3, 4]);
    t.reopen();
    assert_eq!(t.wb.revision().unwrap(), 4);
    assert_eq!(t.wb.history(2, 100).unwrap().len(), 2);
}

fn insert_one(wb: &mut Workbook, view: u64, id: Option<&str>) -> Committed {
    wb.mutate(Mutation { id, op: "insert_rows", actor: Some("test"), summary: "1 row".into() }, Some(view), |wb| {
        wb.insert_rows(view, 0, 1)?;
        Ok(json!({ "inserted": 1 }))
    })
    .unwrap()
}

#[test]
fn a_retried_mutation_id_is_applied_once() {
    let mut t = TempWb::new();
    t.wb.create_table_from_query("p", "SELECT 1 AS n").unwrap();
    let v = t.wb.open_view(spec("p")).unwrap().view_id;
    let first = insert_one(&mut t.wb, v, Some("m-1"));
    assert!(!first.replayed);
    let again = insert_one(&mut t.wb, v, Some("m-1"));
    assert!(again.replayed);
    assert_eq!(again.revision, first.revision);
    assert_eq!(again.result, json!({ "inserted": 1 }));
    assert_eq!(scalar(&t.wb, "SELECT count(*) FROM p"), json!(2), "inserted once, not twice");
    // Without an id, or with a new one, it applies again.
    insert_one(&mut t.wb, v, Some("m-2"));
    insert_one(&mut t.wb, v, None);
    assert_eq!(scalar(&t.wb, "SELECT count(*) FROM p"), json!(4));
    // The history survives a reopen, and so does deduplication.
    t.reopen();
    let v = t.wb.open_view(spec("p")).unwrap().view_id;
    assert!(insert_one(&mut t.wb, v, Some("m-1")).replayed);
    assert_eq!(scalar(&t.wb, "SELECT count(*) FROM p"), json!(4));
}

#[test]
fn a_failure_after_the_change_rolls_back_data_history_and_memory() {
    let mut t = TempWb::new();
    t.wb.create_table_from_query("p", "SELECT 1 AS n").unwrap();
    let v = t.wb.open_view(spec("p")).unwrap().view_id;
    let rev = t.wb.revision().unwrap();
    for point in ["history", "commit"] {
        faults::fail_next(point);
        let err = t.wb.insert_rows(v, 0, 5).unwrap_err();
        assert!(err.to_string().contains(point), "{err:#}");
        assert_eq!(scalar(&t.wb, "SELECT count(*) FROM p"), json!(1), "{point}");
        assert_eq!(t.wb.revision().unwrap(), rev);
        assert_eq!(t.wb.view_info(v).unwrap().row_count, 1);
        t.wb.verify_index("p").unwrap();
    }
    // A sheet change: memory follows the rollback.
    t.wb.with_sheets(|s| s.create("S")).unwrap();
    t.wb.with_sheets(|s| s.set(0, &[sheet_input(0, 0, "1")])).unwrap();
    faults::fail_next("history");
    assert!(t.wb.with_sheets(|s| s.set(0, &[sheet_input(0, 0, "2")])).is_err());
    assert_eq!(cell_text(&t.wb, 0, 0, 0).as_deref(), Some("1"));
    t.reopen();
    assert_eq!(cell_text(&t.wb, 0, 0, 0).as_deref(), Some("1"));
}

// ----------------------------------------------- readers, scratch, backup

#[test]
fn readers_are_not_blocked_by_a_running_change() {
    let mut t = TempWb::new();
    t.wb.create_table_from_query("p", "SELECT i AS n FROM range(1000) r(i)").unwrap();
    let reader = t.wb.reader().unwrap();
    let v = reader.open_view(spec("p")).unwrap().view_id;
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    std::thread::scope(|s| {
        let wb = &mut t.wb;
        let writer = s.spawn(move || {
            wb.mutate(Mutation { op: "slow", ..Default::default() }, None, |wb| {
                wb.insert_rows(v, 0, 10)?;
                started_tx.send(()).unwrap();
                // Hold the transaction open until the reader has read.
                release_rx.recv().unwrap();
                Ok(Value::Null)
            })
            .unwrap()
        });
        started_rx.recv().unwrap();
        let started = std::time::Instant::now();
        let block = reader.fetch(v, 0, 3, 0, None).unwrap();
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
        // The uncommitted insert is invisible to the reader.
        assert_eq!(block.rows, vec![vec![json!(0)], vec![json!(1)], vec![json!(2)]]);
        release_tx.send(()).unwrap();
        writer.join().unwrap();
    });
    assert_eq!(reader.view_info(v).unwrap().row_count, 1010);
    assert_eq!(reader.fetch(v, 0, 1, 0, None).unwrap().rows, vec![vec![json!(null)]]);
}

#[test]
fn closed_and_replaced_permutations_are_dropped() {
    let mut t = TempWb::new();
    t.wb.create_table_from_query("p", "SELECT i AS n FROM range(100) r(i)").unwrap();
    let mut s = spec("p");
    s.sort.push(SortKey { column: "n".into(), desc: true });
    let a = t.wb.open_view(s.clone()).unwrap().view_id;
    let b = t.wb.open_view(s.clone()).unwrap().view_id;
    assert_eq!(scratch_tables(&t.wb), 2);
    // A structural change makes both stale; using them rebuilds and drops the old tables.
    let plain = t.wb.open_view(spec("p")).unwrap().view_id;
    t.wb.insert_rows(plain, 0, 1).unwrap();
    assert_eq!(t.wb.fetch(a, 0, 1).unwrap().rows, vec![vec![json!(99)]]);
    assert_eq!(t.wb.view_info(b).unwrap().row_count, 101);
    t.wb.close_view(plain);
    assert_eq!(scratch_tables(&t.wb), 2);
    t.wb.close_view(a);
    t.wb.close_view(b);
    assert_eq!(scratch_tables(&t.wb), 0);
}

#[test]
fn expired_views_rebuild_transparently() {
    let mut t = TempWb::new();
    t.wb.create_table_from_query("p", "SELECT i AS n FROM range(50) r(i)").unwrap();
    let mut s = spec("p");
    s.filters.push(Filter { column: "n".into(), op: FilterOp::Gte, value: json!(40) });
    let v = t.wb.open_view(s).unwrap().view_id;
    let before = t.wb.fetch(v, 0, 100).unwrap();
    assert_eq!(t.wb.shared().expire_views(0), 1);
    assert_eq!(t.wb.shared().materialized_rows(), 0);
    let after = t.wb.fetch(v, 0, 100).unwrap();
    assert_eq!(after.rows, before.rows);
    assert_eq!(after.ids, before.ids);
    assert_eq!(scratch_tables(&t.wb), 1, "the expired table was dropped and one rebuilt");
}

#[test]
fn a_backup_holds_one_revision_while_changes_continue() {
    let mut t = TempWb::new();
    t.wb.create_table_from_query("log", "SELECT 0 AS rev WHERE false").unwrap();
    let reader = t.wb.reader().unwrap();
    let dir = tempfile::tempdir().unwrap();
    std::thread::scope(|s| {
        let wb = &mut t.wb;
        let writer = s.spawn(move || {
            // Each change logs the revision it will become, in the same transaction.
            for _ in 0..150 {
                wb.mutate(Mutation { op: "log", ..Default::default() }, None, |wb| {
                    let next = wb.revision()? + 1;
                    wb.create_table_from_query("scratch_tmp", "SELECT 1")?;
                    wb.drop_table("scratch_tmp")?;
                    let v = wb.open_view(spec("log"))?.view_id;
                    let n = wb.view_info(v)?.row_count;
                    wb.edit(v, &[edit(n, "rev", &next.to_string())])?;
                    wb.close_view(v);
                    Ok(Value::Null)
                })
                .unwrap();
            }
        });
        let mut checked = 0;
        for i in 0..6 {
            let path = dir.path().join(format!("b{i}.duckdb"));
            let revision = reader.backup_to(&path).unwrap();
            let copy = Workbook::open(&path).unwrap();
            assert_eq!(copy.revision().unwrap(), revision);
            // Oracle: revision r logged exactly the values 2..=r (r=1 created the table).
            let logged: Vec<i64> = copy
                .preview_query("SELECT rev FROM log ORDER BY _rf_ord", 10_000)
                .unwrap()
                .rows
                .iter()
                .map(|r| r[0].as_i64().unwrap())
                .collect();
            let expected: Vec<i64> = (2..=revision as i64).collect();
            assert_eq!(logged, expected, "backup {i} at revision {revision}");
            checked += 1;
        }
        writer.join().unwrap();
        assert_eq!(checked, 6);
    });
}

// ------------------------------------------------------------------- 4.06

/// Format 2 (phase 3) → 3: the committed fixture written by the phase-3 server migrates with
/// a copy of the original, keeps every value, id, order, view and sheet, and gains data
/// revisions and empty saved-query tables.
#[test]
fn v2_fixture_migrates_to_format_3() {
    let dir = tempfile::tempdir().unwrap();
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/v2/data.duckdb");
    let path = dir.path().join("data.duckdb");
    std::fs::copy(&src, &path).unwrap();
    let original = std::fs::read(&path).unwrap();
    let mut wb = Workbook::open(&path).unwrap();
    let report = wb.open_report().clone();
    assert_eq!(report.migrated_from, Some(2));
    assert_eq!(std::fs::read(report.backup.unwrap()).unwrap(), original, "the pre-migration copy is the untouched file");
    assert_eq!(wb.revision().unwrap(), 8, "the README's revision");
    let v = wb.open_view(spec("sales")).unwrap();
    let block = wb.fetch(v.view_id, 0, 10).unwrap();
    assert_eq!(block.ids, vec![0, 4, 1, 3]);
    assert_eq!(
        block.rows,
        vec![
            vec![json!(1), json!("north"), json!("10.25"), json!("20.50")],
            vec![json!("9007199254740993"), json!("inserted"), json!(null), json!(null)],
            vec![json!(2), json!("south"), json!("0.10"), json!("0.20")],
            vec![json!(4), json!("東京"), json!(null), json!(null)],
        ]
    );
    assert!(!wb.verify_index("sales").unwrap(), "the display order stays non-dense");
    assert_eq!(wb.saved_views(Some("sales")).unwrap()[0].name, "North only");
    assert_eq!(cell_text(&wb, 0, 0, 1).as_deref(), Some("42"));
    // New in format 3: data revisions (existing tables count as changed at the current
    // revision), no saved queries and no materialization records for old tables.
    assert_eq!(storage::data_revisions(&wb.conn).unwrap()["sales"], 8);
    assert!(wb.saved_queries().unwrap().is_empty());
    assert!(wb.materialized().unwrap().is_empty());
    // It works as a format-3 workbook and is not migrated again.
    wb.sql_change("UPDATE sales SET region = 'x' WHERE id = 2", &[]).unwrap();
    assert_eq!(storage::data_revisions(&wb.conn).unwrap()["sales"], 9);
    drop(wb);
    let wb = Workbook::open(&path).unwrap();
    assert_eq!(wb.open_report().migrated_from, None);
    assert_eq!(wb.revision().unwrap(), 9);
}
