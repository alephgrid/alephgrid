//! U.16 / U.17 in the engine: a column's distinct values with counts (over the rows the
//! other filters keep), the list filter, and find over a view. Expected values are worked
//! out here from the data, not read back from the engine.

use super::*;
use serde_json::json;

fn table(t: &mut TempWb) {
    // region of row i: north when i % 3 == 0, south when 1, NULL when 2; amount = i.
    t.wb.create_table_from_query("t", "SELECT i AS id, CASE i % 3 WHEN 0 THEN 'north' WHEN 1 THEN 'south' END AS region, i AS amount FROM range(3000) r(i)").unwrap();
}

#[test]
fn column_values_count_each_value_over_the_other_filters() {
    let mut t = TempWb::new();
    table(&mut t);
    let spec = ViewSpec { table: "t".into(), ..Default::default() };
    let v = t.wb.column_values(&spec, "region", "", 100).unwrap();
    assert_eq!(v.rows, 3000);
    assert_eq!(v.distinct, 3);
    assert!(!v.truncated);
    assert_eq!(
        v.values,
        vec![
            ValueCount { value: None, count: 1000 },
            ValueCount { value: Some("north".into()), count: 1000 },
            ValueCount { value: Some("south".into()), count: 1000 },
        ]
    );
    // Other columns' filters apply; a filter on this column does not (so it can be widened).
    let filtered = ViewSpec {
        table: "t".into(),
        filters: vec![
            Filter { column: "amount".into(), op: FilterOp::Lt, value: json!("300") },
            Filter { column: "region".into(), op: FilterOp::Eq, value: json!("north") },
        ],
        ..Default::default()
    };
    let v = t.wb.column_values(&filtered, "region", "", 100).unwrap();
    assert_eq!(v.rows, 300);
    assert_eq!(v.values.iter().map(|x| x.count).sum::<u64>(), 300);
    // Search, and the limit.
    let v = t.wb.column_values(&spec, "region", "NOR", 100).unwrap();
    assert_eq!(v.values, vec![ValueCount { value: Some("north".into()), count: 1000 }]);
    let v = t.wb.column_values(&spec, "id", "", 10).unwrap();
    assert_eq!(v.values.len(), 10);
    assert_eq!(v.distinct, 3000);
    assert!(v.truncated);
    // Numbers in numeric order, not text order.
    assert_eq!(v.values.iter().map(|x| x.value.clone().unwrap()).collect::<Vec<_>>(), (0..10).map(|i| i.to_string()).collect::<Vec<_>>());
}

#[test]
fn a_list_filter_keeps_the_chosen_values_and_empty_cells() {
    let mut t = TempWb::new();
    table(&mut t);
    let spec = ViewSpec {
        table: "t".into(),
        filters: vec![Filter { column: "region".into(), op: FilterOp::In, value: json!(["south", null]) }],
        ..Default::default()
    };
    let v = t.wb.open_view(spec).unwrap();
    assert_eq!(v.row_count, 2000);
    let numbers = ViewSpec { table: "t".into(), filters: vec![Filter { column: "amount".into(), op: FilterOp::In, value: json!(["3", "4", "5000"]) }], ..Default::default() };
    assert_eq!(t.wb.open_view(numbers).unwrap().row_count, 2);
    let bad = ViewSpec { table: "t".into(), filters: vec![Filter { column: "amount".into(), op: FilterOp::In, value: json!("3") }], ..Default::default() };
    assert!(t.wb.open_view(bad).is_err());
}

#[test]
fn find_counts_matching_cells_and_walks_them_in_view_order() {
    let mut t = TempWb::new();
    t.wb.create_table_from_query("t", "SELECT * FROM (VALUES (1, 'North', 'n/a'), (2, 'south', 'north'), (3, 'east', NULL), (4, 'north', 'x')) v(id, region, note)").unwrap();
    // Sorted by id descending: rows 4, 3, 2, 1.
    let spec = ViewSpec { table: "t".into(), sort: vec![SortKey { column: "id".into(), desc: true }], ..Default::default() };
    let v = t.wb.open_view(spec).unwrap().view_id;
    let r = t.wb.find_in_view(v, "north", false, 0, false, true).unwrap();
    // "North", "north" (note of 2), "north" (region of 4): three cells.
    assert_eq!(r.matches, Some(3));
    assert_eq!(r.hits, vec![FindHit { row: 0, columns: vec![1] }, FindHit { row: 2, columns: vec![2] }]);
    let r = t.wb.find_in_view(v, "north", true, 1, false, false).unwrap();
    assert_eq!(r.matches, None);
    assert_eq!(r.hits, vec![FindHit { row: 2, columns: vec![2] }]);
    let r = t.wb.find_in_view(v, "north", false, 2, true, false).unwrap();
    assert_eq!(r.hits, vec![FindHit { row: 2, columns: vec![2] }, FindHit { row: 0, columns: vec![1] }]);
    assert_eq!(t.wb.find_in_view(v, "zzz", false, 0, false, true).unwrap().matches, Some(0));
}
