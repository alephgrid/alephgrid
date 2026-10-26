//! 1.02 full sheet export, 1.04 atomic sheet mutations and grouped undo.

use super::*;
use serde_json::json;

/// A header plus `n` data rows: A = i, B = "s<i>", C = A*2 (formula).
fn numbered_sheet(wb: &mut Workbook, n: u32) -> u32 {
    let sheet = wb.with_sheets(|s| s.create("Numbers")).unwrap().index;
    let mut inputs = vec![sheet_input(0, 0, "i"), sheet_input(0, 1, "label"), sheet_input(0, 2, "double")];
    for i in 0..n {
        let r = i + 1;
        inputs.push(sheet_input(r, 0, &i.to_string()));
        inputs.push(sheet_input(r, 1, &format!("s{i}")));
        inputs.push(sheet_input(r, 2, &format!("=A{}*2", r + 1)));
    }
    wb.with_sheets(|s| s.set(sheet, &inputs)).unwrap();
    sheet
}

#[test]
fn sheet_to_table_keeps_every_row_around_the_fetch_cap() {
    for n in [1u32, 4_999, 5_000, 5_001, 20_000] {
        let mut t = TempWb::new();
        let sheet = numbered_sheet(&mut t.wb, n);
        assert_eq!(t.wb.sheets().info(sheet).unwrap().rows, n + 1);
        let table = t.wb.sheet_to_table(sheet, "numbers").unwrap();
        assert_eq!(table.row_count, n as u64, "n = {n}");
        assert_eq!(table.column_count, 3);
        // Independent oracle: closed-form sums over 0..n.
        let n64 = n as i64;
        let expected_sum = n64 * (n64 - 1) / 2;
        assert_eq!(scalar(&t.wb, "SELECT sum(i) FROM numbers"), json!(expected_sum), "n = {n}");
        assert_eq!(scalar(&t.wb, "SELECT sum(double) FROM numbers"), json!(2 * expected_sum));
        assert_eq!(scalar(&t.wb, "SELECT count(DISTINCT label) FROM numbers"), json!(n64));
        let last = format!("SELECT label FROM numbers WHERE i = {}", n - 1);
        assert_eq!(scalar(&t.wb, &last), json!(format!("s{}", n - 1)));
        // Row order follows the sheet.
        let v = t.wb.open_view(spec("numbers")).unwrap().view_id;
        let rows = all_rows(&t.wb, v);
        assert!(rows.iter().enumerate().all(|(k, r)| r[0] == json!(k)), "n = {n}");
    }
}

#[test]
fn sheet_to_table_infers_types_from_cell_values() {
    let mut t = TempWb::new();
    let s = t.wb.with_sheets(|s| s.create("Mixed")).unwrap().index;
    let rows = [
        ["int", "float", "flag", "text", "mixed", "zip", "", "int"],
        ["1", "1.5", "TRUE", "a", "1", "'00123", "x", "7"],
        ["2", "0.1", "FALSE", "b", "two", "'04500", "y", "8"],
        ["", "", "", "", "", "", "", ""],
        ["4", "1e20", "TRUE", "", "4", "'1", "z", "9"],
    ];
    let mut inputs = Vec::new();
    for (r, row) in rows.iter().enumerate() {
        for (c, v) in row.iter().enumerate() {
            if !v.is_empty() {
                inputs.push(sheet_input(r as u32, c as u32, v));
            }
        }
    }
    t.wb.with_sheets(|sh| sh.set(s, &inputs)).unwrap();
    t.wb.sheet_to_table(s, "mixed").unwrap();
    let cols = t.wb.columns("mixed").unwrap();
    let names: Vec<_> = cols.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["int", "float", "flag", "text", "mixed", "zip", "column_7", "int_2"]);
    let types: Vec<_> = cols.iter().map(|c| c.sql_type.as_str()).collect();
    assert_eq!(types, ["BIGINT", "DOUBLE", "BOOLEAN", "VARCHAR", "VARCHAR", "VARCHAR", "VARCHAR", "BIGINT"]);
    assert_eq!(scalar(&t.wb, "SELECT count(*) FROM mixed"), json!(4), "blank rows inside the range are kept");
    // Leading zeros of text cells survive (a CSV round trip would sniff them as integers).
    assert_eq!(scalar(&t.wb, "SELECT string_agg(zip, ',' ORDER BY _rf_id) FROM mixed"), json!("00123,04500,1"));
    assert_eq!(scalar(&t.wb, "SELECT sum(float) FROM mixed"), json!(1e20 + 1.6));
    assert_eq!(scalar(&t.wb, "SELECT count(*) FROM mixed WHERE int IS NULL"), json!(1));
}

#[test]
fn sheet_to_table_keeps_dates_as_dates() {
    let mut t = TempWb::new();
    let s = t.wb.with_sheets(|s| s.create("Dates")).unwrap().index;
    t.wb.with_sheets(|sh| sh.set(s, &[sheet_input(0, 0, "day"), sheet_input(1, 0, "2024-02-29"), sheet_input(2, 0, "1999-12-31")]))
        .unwrap();
    t.wb.sheet_to_table(s, "days").unwrap();
    assert_eq!(t.wb.columns("days").unwrap()[0].sql_type, "DATE");
    assert_eq!(scalar(&t.wb, "SELECT min(day)::VARCHAR FROM days"), json!("1999-12-31"));
    assert_eq!(scalar(&t.wb, "SELECT max(day)::VARCHAR FROM days"), json!("2024-02-29"));
}

#[test]
fn sheet_to_table_requires_header_and_data() {
    let mut t = TempWb::new();
    let s = t.wb.with_sheets(|s| s.create("Empty")).unwrap().index;
    assert!(t.wb.sheet_to_table(s, "x").is_err());
    t.wb.with_sheets(|sh| sh.set(s, &[sheet_input(0, 0, "only header")])).unwrap();
    assert!(t.wb.sheet_to_table(s, "x").is_err());
    assert!(t.wb.list_tables().unwrap().is_empty());
}

#[test]
fn invalid_cell_in_a_batch_changes_nothing() {
    let mut t = TempWb::new();
    let s = t.wb.with_sheets(|s| s.create("S")).unwrap().index;
    t.wb.with_sheets(|sh| sh.set(s, &[sheet_input(0, 0, "keep")])).unwrap();
    let err = t
        .wb
        .with_sheets(|sh| sh.set(s, &[sheet_input(0, 0, "overwritten"), sheet_input(1, 0, "x"), sheet_input(2_000_000, 0, "bad")]))
        .unwrap_err();
    assert!(err.to_string().contains("row"), "{err}");
    assert_eq!(cell_text(&t.wb, s, 0, 0).as_deref(), Some("keep"));
    assert_eq!(cell_text(&t.wb, s, 1, 0), None);
    assert!(t.wb.with_sheets(|sh| sh.set(s, &[sheet_input(0, 20_000, "bad column")])).is_err());
    assert!(t.wb.with_sheets(|sh| sh.set(9, &[sheet_input(0, 0, "bad sheet")])).is_err());
    // A batch rejected up front keeps the undo history of earlier edits.
    assert!(t.wb.with_sheets(|sh| sh.undo()).unwrap());
    assert_eq!(cell_text(&t.wb, s, 0, 0), None);
    t.wb.with_sheets(|sh| sh.redo()).unwrap();
    t.reopen();
    assert_eq!(cell_text(&t.wb, s, 0, 0).as_deref(), Some("keep"));
}

#[test]
fn failed_save_rolls_memory_back_to_disk() {
    let mut t = TempWb::new();
    let s = t.wb.with_sheets(|s| s.create("S")).unwrap().index;
    t.wb.with_sheets(|sh| sh.set(s, &[sheet_input(0, 0, "1")])).unwrap();
    crate::faults::fail_next("save_sheets");
    assert!(t.wb.with_sheets(|sh| sh.set(s, &[sheet_input(0, 0, "2")])).is_err());
    assert_eq!(cell_text(&t.wb, s, 0, 0).as_deref(), Some("1"), "memory must match the last durable state");
    t.reopen();
    assert_eq!(cell_text(&t.wb, s, 0, 0).as_deref(), Some("1"));
    // The workbook keeps working after the failure.
    t.wb.with_sheets(|sh| sh.set(s, &[sheet_input(0, 0, "3")])).unwrap();
    assert_eq!(cell_text(&t.wb, s, 0, 0).as_deref(), Some("3"));
}

#[test]
fn undo_reverts_a_whole_batch_and_redo_reapplies_it() {
    let mut t = TempWb::new();
    let s = t.wb.with_sheets(|s| s.create("S")).unwrap().index;
    t.wb.with_sheets(|sh| sh.set(s, &[sheet_input(0, 0, "1"), sheet_input(1, 0, "2")])).unwrap();
    t.wb.with_sheets(|sh| sh.set(s, &[sheet_input(2, 0, "=A1+A2"), sheet_input(0, 0, "10"), sheet_input(0, 1, "x")])).unwrap();
    assert_eq!(cell_text(&t.wb, s, 2, 0).as_deref(), Some("12"));
    assert!(t.wb.with_sheets(|sh| sh.undo()).unwrap());
    assert_eq!(cell_text(&t.wb, s, 0, 0).as_deref(), Some("1"));
    assert_eq!(cell_text(&t.wb, s, 0, 1), None);
    assert_eq!(cell_text(&t.wb, s, 2, 0), None, "the whole paste is one undo step");
    assert!(t.wb.with_sheets(|sh| sh.redo()).unwrap());
    assert_eq!(cell_text(&t.wb, s, 2, 0).as_deref(), Some("12"));
    // Two undos go back past both batches; creating the sheet is a separate step.
    assert!(t.wb.with_sheets(|sh| sh.undo()).unwrap());
    assert!(t.wb.with_sheets(|sh| sh.undo()).unwrap());
    assert_eq!(cell_text(&t.wb, s, 0, 0), None);
    // Undone state is persisted: reopening shows the same thing.
    t.reopen();
    assert_eq!(cell_text(&t.wb, s, 0, 0), None);
    assert!(!t.wb.with_sheets(|sh| sh.undo()).unwrap(), "history does not survive a restart");
}

#[test]
fn recalculation_after_undo_redo_and_reopen() {
    let mut t = TempWb::new();
    let s = t.wb.with_sheets(|s| s.create("S")).unwrap().index;
    t.wb.with_sheets(|sh| sh.set(s, &[sheet_input(0, 0, "2"), sheet_input(0, 1, "=A1*A1")])).unwrap();
    t.wb.with_sheets(|sh| sh.set(s, &[sheet_input(0, 0, "5")])).unwrap();
    assert_eq!(cell_text(&t.wb, s, 0, 1).as_deref(), Some("25"));
    t.wb.with_sheets(|sh| sh.undo()).unwrap();
    assert_eq!(cell_text(&t.wb, s, 0, 1).as_deref(), Some("4"));
    t.wb.with_sheets(|sh| sh.redo()).unwrap();
    t.reopen();
    assert_eq!(cell_text(&t.wb, s, 0, 1).as_deref(), Some("25"));
    t.wb.with_sheets(|sh| sh.set(s, &[sheet_input(0, 0, "-3")])).unwrap();
    assert_eq!(cell_text(&t.wb, s, 0, 1).as_deref(), Some("9"));
}

#[test]
fn structural_edits_are_single_undo_steps() {
    let mut t = TempWb::new();
    let s = t.wb.with_sheets(|s| s.create("S")).unwrap().index;
    t.wb.with_sheets(|sh| sh.set(s, &[sheet_input(0, 0, "1"), sheet_input(1, 0, "2"), sheet_input(2, 0, "=SUM(A1:A2)")])).unwrap();
    t.wb.with_sheets(|sh| sh.insert_rows(s, 1, 2)).unwrap();
    assert_eq!(cell_text(&t.wb, s, 4, 0).as_deref(), Some("3"));
    assert_eq!(t.wb.sheets().fetch(s, 4, 1, 1).unwrap().rows[0][0].as_ref().unwrap().2.as_deref(), Some("=SUM(A1:A4)"));
    t.wb.with_sheets(|sh| sh.undo()).unwrap();
    assert_eq!(cell_text(&t.wb, s, 2, 0).as_deref(), Some("3"));
    t.wb.with_sheets(|sh| sh.delete_columns(s, 0, 1)).unwrap();
    assert_eq!(cell_text(&t.wb, s, 0, 0), None);
    t.wb.with_sheets(|sh| sh.undo()).unwrap();
    assert_eq!(cell_text(&t.wb, s, 0, 0).as_deref(), Some("1"));
}

#[test]
fn query_to_sheet_is_bounded_and_keeps_text_exact() {
    let mut t = TempWb::new();
    t.wb.create_table_from_query("t", "SELECT i, '=1+1' AS f, 9007199254740993::BIGINT AS big FROM range(30) r(i)").unwrap();
    let info = t.wb.create_sheet_from_query("Q", "SELECT * FROM t ORDER BY i", 10).unwrap().sheet;
    assert_eq!((info.rows, info.columns), (11, 3), "header + 10 rows");
    assert_eq!(cell_text(&t.wb, info.index, 1, 1).as_deref(), Some("=1+1"), "formula-looking text stays text");
    assert_eq!(cell_text(&t.wb, info.index, 1, 2).as_deref(), Some("9007199254740993"), "beyond 2^53 stays exact text");
}

#[test]
fn copies_into_sheets_are_bounded_and_say_so() {
    let mut t = TempWb::new();
    t.wb.create_table_from_query("t", "SELECT i, i * 2 AS d, 'r' || i AS s FROM range(250) r(i)").unwrap();
    // Query → sheet within the limit, then over it.
    let all = t.wb.create_sheet_from_query("All", "SELECT * FROM t ORDER BY i", 1000).unwrap();
    assert_eq!((all.rows_copied, all.truncated, all.total_rows), (250, false, None));
    let some = t.wb.create_sheet_from_query("Some", "SELECT * FROM t ORDER BY i", 100).unwrap();
    assert_eq!((some.rows_copied, some.truncated, some.row_limit), (100, true, 100));
    assert_eq!((some.sheet.rows, some.sheet.columns), (101, 3), "header + 100 rows");
    // The cell budget caps wide results below the row limit.
    assert_eq!(Workbook::sheet_copy_limit(3, 100_000), 100_000);
    assert_eq!(Workbook::sheet_copy_limit(100, 100_000), 9_999);
    assert_eq!(Workbook::sheet_copy_limit(2_000_000, 100_000), 1);
    // View → sheet follows the view's sort and filter, and reports the view's size.
    let mut s = spec("t");
    s.sort.push(SortKey { column: "i".into(), desc: true });
    s.filters.push(Filter { column: "i".into(), op: FilterOp::Gte, value: json!("200") });
    let v = t.wb.open_view(s).unwrap().view_id;
    let copy = t.wb.sheet_from_view(v, "Top", 10).unwrap();
    assert_eq!((copy.rows_copied, copy.total_rows, copy.truncated), (10, Some(50), true));
    assert_eq!(cell_text(&t.wb, copy.sheet.index, 1, 0).as_deref(), Some("249"));
    assert_eq!(cell_text(&t.wb, copy.sheet.index, 10, 2).as_deref(), Some("r240"));
    let id = t.wb.open_view(spec("t")).unwrap().view_id;
    let whole = t.wb.sheet_from_view(id, "Whole", 1000).unwrap();
    assert_eq!((whole.rows_copied, whole.total_rows, whole.truncated), (250, Some(250), false));
    assert_eq!(cell_text(&t.wb, whole.sheet.index, 250, 0).as_deref(), Some("249"));
}
