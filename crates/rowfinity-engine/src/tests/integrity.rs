//! 1.03: table edits are atomic, empty batches are no-ops, last write wins.

use super::*;
use serde_json::json;

fn people(t: &mut TempWb) -> u64 {
    t.wb
        .create_table_from_query("people", "SELECT * FROM (VALUES (1, 'Ada'), (2, 'Linus'), (3, 'Grace')) v(age, name)")
        .unwrap();
    t.wb.open_view(spec("people")).unwrap().view_id
}

fn snapshot(t: &TempWb, view: u64) -> Vec<Vec<Value>> {
    all_rows(&t.wb, view)
}

#[test]
fn empty_batch_is_a_noop_on_empty_and_full_tables() {
    let mut t = TempWb::new();
    t.wb.create_table_from_query("empty", "SELECT 1::INTEGER AS a WHERE false").unwrap();
    let v = t.wb.open_view(spec("empty")).unwrap().view_id;
    let out = t.wb.edit(v, &[]).unwrap();
    assert_eq!((out.cells_changed, out.rows_appended), (0, 0));
    assert_eq!(t.wb.view_info(v).unwrap().row_count, 0, "an empty batch must not append a row");

    let v = people(&mut t);
    let before = snapshot(&t, v);
    t.wb.edit(v, &[]).unwrap();
    assert_eq!(snapshot(&t, v), before);
}

#[test]
fn failed_append_leaves_no_rows_and_no_cells() {
    let mut t = TempWb::new();
    let v = people(&mut t);
    let before = snapshot(&t, v);
    // A valid edit, a valid append, then an invalid value in the appended row.
    let err = t
        .wb
        .edit(v, &[edit(0, "name", "Changed"), edit(5, "name", "New"), edit(6, "age", "not a number")])
        .unwrap_err();
    assert!(err.to_string().contains("not a valid"), "{err}");
    assert_eq!(t.wb.view_info(v).unwrap().row_count, 3);
    assert_eq!(snapshot(&t, v), before);

    // The failure must not have reached the file either.
    t.reopen();
    let v = t.wb.open_view(spec("people")).unwrap().view_id;
    assert_eq!(snapshot(&t, v), before);
}

#[test]
fn unknown_and_formula_columns_fail_before_mutation() {
    let mut t = TempWb::new();
    let v = people(&mut t);
    t.wb.add_formula_column("people", "older", "[age] + 1").unwrap();
    let v2 = t.wb.open_view(spec("people")).unwrap().view_id;
    let before = snapshot(&t, v2);
    assert!(t.wb.edit(v2, &[edit(9, "name", "x"), edit(0, "nope", "1")]).is_err());
    assert!(t.wb.edit(v2, &[edit(9, "name", "x"), edit(0, "older", "1")]).is_err());
    assert_eq!(snapshot(&t, v2), before);
    t.wb.close_view(v);
}

#[test]
fn append_on_sorted_view_is_rejected_without_change() {
    let mut t = TempWb::new();
    people(&mut t);
    let mut s = spec("people");
    s.sort.push(SortKey { column: "name".into(), desc: false });
    let v = t.wb.open_view(s).unwrap().view_id;
    let before = snapshot(&t, v);
    assert!(t.wb.edit(v, &[edit(0, "name", "A"), edit(3, "name", "B")]).is_err());
    assert_eq!(snapshot(&t, v), before);
}

#[test]
fn last_write_wins_within_a_batch() {
    let mut t = TempWb::new();
    let v = people(&mut t);
    let out = t.wb.edit(v, &[edit(1, "name", "first"), edit(1, "name", "second"), edit(1, "age", "40")]).unwrap();
    assert_eq!(out.cells_changed, 2, "duplicate cells count once");
    let rows = snapshot(&t, v);
    assert_eq!(rows[1], vec![json!(40), json!("second")]);
}

#[test]
fn sorted_view_edits_hit_the_right_record() {
    let mut t = TempWb::new();
    people(&mut t);
    let mut s = spec("people");
    s.sort.push(SortKey { column: "name".into(), desc: true }); // Linus, Grace, Ada
    let v = t.wb.open_view(s).unwrap().view_id;
    t.wb.edit(v, &[edit(2, "age", "99")]).unwrap();
    let identity = t.wb.open_view(spec("people")).unwrap().view_id;
    let rows = snapshot(&t, identity);
    assert_eq!(rows[0], vec![json!(99), json!("Ada")]);
    assert_eq!(rows[1][0], json!(2));
}

#[test]
fn clearing_cells_writes_null() {
    let mut t = TempWb::new();
    let v = people(&mut t);
    t.wb.edit(v, &[CellEdit { row: 0, column: "name".into(), value: None }, edit(1, "age", "")]).unwrap();
    let rows = snapshot(&t, v);
    assert_eq!(rows[0][1], Value::Null);
    assert_eq!(rows[1][0], Value::Null);
}

#[test]
fn unicode_and_quotes_roundtrip_through_edits() {
    let mut t = TempWb::new();
    let v = people(&mut t);
    let tricky = "O'Brien \"quoted\" — 東京 🚀 \\n";
    t.wb.edit(v, &[edit(0, "name", tricky)]).unwrap();
    t.reopen();
    let v = t.wb.open_view(spec("people")).unwrap().view_id;
    assert_eq!(snapshot(&t, v)[0][1], json!(tricky));
}
