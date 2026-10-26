//! 2.07: saved views (live), hidden columns, snapshots, and schema changes.

use super::*;
use serde_json::json;
use std::collections::BTreeMap;

fn setup() -> TempWb {
    let mut t = TempWb::new();
    t.wb.create_table_from_query("sales", "SELECT i AS id, ['n','s','e'][1 + i % 3] AS region, i * 10 AS amount, 'note ' || i AS note FROM range(30) r(i)").unwrap();
    t
}

fn big_north() -> ViewSpec {
    ViewSpec {
        table: "sales".into(),
        sort: vec![SortKey { column: "amount".into(), desc: true }],
        filters: vec![Filter { column: "region".into(), op: FilterOp::Eq, value: json!("n") }],
        hidden: vec!["note".into()],
    }
}

#[test]
fn saved_views_persist_and_stay_live() {
    let mut t = setup();
    let widths = BTreeMap::from([("amount".to_string(), 140.0)]);
    let saved = t.wb.save_view(None, "Big north", big_north(), widths.clone()).unwrap();
    t.reopen();
    let list = t.wb.saved_views(Some("sales")).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!((list[0].id.as_str(), list[0].name.as_str()), (saved.id.as_str(), "Big north"));
    assert_eq!(list[0].widths, widths);
    let v = t.wb.open_view(list[0].spec.clone()).unwrap();
    assert_eq!(v.columns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["id", "region", "amount"], "note is hidden");
    let rows = all_rows(&t.wb, v.view_id);
    assert_eq!(rows.len(), 10);
    assert_eq!(rows[0], vec![json!(27), json!("n"), json!(270)]);
    // Live: a later edit shows up when the view is opened again.
    let identity = t.wb.open_view(spec("sales")).unwrap().view_id;
    t.wb.edit(identity, &[edit(0, "amount", "999")]).unwrap();
    let v = t.wb.open_view(list[0].spec.clone()).unwrap();
    assert_eq!(all_rows(&t.wb, v.view_id)[0], vec![json!(0), json!("n"), json!(999)]);
}

#[test]
fn names_are_unique_per_table_and_specs_must_open() {
    let mut t = setup();
    t.wb.save_view(None, "A", big_north(), BTreeMap::new()).unwrap();
    assert!(t.wb.save_view(None, "a", big_north(), BTreeMap::new()).is_err(), "case-insensitive clash");
    assert!(t.wb.save_view(None, " ", big_north(), BTreeMap::new()).is_err());
    let mut bad = big_north();
    bad.filters[0].column = "nope".into();
    assert!(t.wb.save_view(None, "B", bad, BTreeMap::new()).is_err());
    let mut all_hidden = big_north();
    all_hidden.hidden = vec!["id".into(), "region".into(), "amount".into(), "note".into()];
    assert!(t.wb.save_view(None, "C", all_hidden, BTreeMap::new()).is_err(), "at least one column shows");
    let b = t.wb.save_view(None, "B", spec("sales"), BTreeMap::new()).unwrap();
    assert!(t.wb.rename_saved_view(&b.id, "A").is_err());
    t.wb.rename_saved_view(&b.id, "Everything").unwrap();
    t.wb.delete_saved_view(&b.id).unwrap();
    assert!(t.wb.delete_saved_view(&b.id).is_err());
    assert_eq!(t.wb.saved_views(None).unwrap().len(), 1);
}

#[test]
fn hidden_columns_are_left_out_of_fetch_export_and_copies() {
    let mut t = setup();
    let v = t.wb.open_view(big_north()).unwrap().view_id;
    let csv = t.dir.path().join("out.csv");
    t.wb.export_view(v, &csv, ExportFormat::Csv).unwrap();
    let text = std::fs::read_to_string(&csv).unwrap();
    assert_eq!(text.lines().next(), Some("id,region,amount"));
    assert_eq!(text.lines().nth(1), Some("27,n,270"));
    assert_eq!(text.lines().count(), 11);
    let copy = t.wb.sheet_from_view(v, "S", 100).unwrap();
    assert_eq!(copy.sheet.columns, 3);
    // Editing a hidden column through the view is refused: it is not in the view.
    assert!(t.wb.edit(v, &[edit(0, "note", "x")]).is_err());
}

#[test]
fn snapshots_do_not_follow_later_changes() {
    let mut t = setup();
    let v = t.wb.open_view(big_north()).unwrap().view_id;
    let snap = t.wb.table_from_view(v, "north_snapshot").unwrap();
    assert_eq!((snap.row_count, snap.column_count), (10, 3));
    let identity = t.wb.open_view(spec("sales")).unwrap().view_id;
    t.wb.edit(identity, &[edit(27, "amount", "-1")]).unwrap();
    assert_eq!(scalar(&t.wb, "SELECT max(amount) FROM north_snapshot"), json!(270), "a snapshot keeps the old value");
    assert_eq!(scalar(&t.wb, "SELECT string_agg(id::VARCHAR, ',' ORDER BY _rf_id) FROM north_snapshot"), json!("27,24,21,18,15,12,9,6,3,0"), "view order kept");
}

#[test]
fn schema_changes_keep_saved_views_valid() {
    let mut t = setup();
    let saved = t.wb.save_view(None, "Big north", big_north(), BTreeMap::from([("note".to_string(), 90.0), ("amount".to_string(), 120.0)])).unwrap();
    t.wb.rename_column("sales", "amount", "total").unwrap();
    t.wb.rename_column("sales", "note", "comment").unwrap();
    let v = &t.wb.saved_views(None).unwrap()[0];
    assert_eq!(v.spec.sort[0].column, "total");
    assert_eq!(v.spec.hidden, ["comment"]);
    assert_eq!(v.widths, BTreeMap::from([("comment".to_string(), 90.0), ("total".to_string(), 120.0)]));
    let open = t.wb.open_view(v.spec.clone()).unwrap();
    // A column the view sorts or filters by cannot be dropped; a hidden one can.
    let err = t.wb.drop_column("sales", "region").unwrap_err().to_string();
    assert!(err.contains("Big north"), "{err}");
    t.wb.drop_column("sales", "comment").unwrap();
    // The open view drops the hidden column it no longer has, and keeps working.
    let open = t.wb.view_info(open.view_id).unwrap();
    assert!(open.spec.hidden.is_empty());
    assert_eq!(open.spec.sort[0].column, "total");
    let v = &t.wb.saved_views(None).unwrap()[0];
    assert!(v.spec.hidden.is_empty());
    assert!(!v.widths.contains_key("comment"));
    // An open view sorted by a column follows its rename.
    t.wb.rename_column("sales", "total", "grand").unwrap();
    assert_eq!(t.wb.view_info(open.view_id).unwrap().spec.sort[0].column, "grand");
    assert_eq!(t.wb.saved_views(None).unwrap()[0].spec.sort[0].column, "grand");
    // Table rename follows; dropping the table removes its views.
    t.wb.rename_table("sales", "orders").unwrap();
    assert_eq!(t.wb.saved_views(Some("orders")).unwrap()[0].id, saved.id);
    t.wb.open_view(t.wb.saved_views(None).unwrap()[0].spec.clone()).unwrap();
    t.wb.drop_table("orders").unwrap();
    assert!(t.wb.saved_views(None).unwrap().is_empty());
}
