//! 1.07: calculated columns are visible to every supported query form.

use super::*;
use serde_json::json;

fn setup() -> TempWb {
    let mut t = TempWb::new();
    t.wb.create_table_from_query("orders", "SELECT i AS id, i % 3 AS region, (i * 10)::INTEGER AS amount FROM range(10) r(i)").unwrap();
    t.wb.add_formula_column("orders", "double_amount", "[amount] * 2").unwrap();
    t.wb.create_table_from_query("regions", "SELECT * FROM (VALUES (0, 'north'), (1, 'south'), (2, 'west')) v(region, name)").unwrap();
    t.wb.add_formula_column("regions", "shout", "upper(name)").unwrap();
    t
}

/// Expected total of double_amount, computed without the formula column.
const TOTAL: i64 = 2 * 10 * 45;

#[test]
fn plain_select_sees_formula_columns() {
    let t = setup();
    assert_eq!(scalar(&t.wb, "SELECT sum(double_amount) FROM orders"), json!(TOTAL));
    assert_eq!(scalar(&t.wb, "select sum(double_amount) from orders;"), json!(TOTAL));
    assert_eq!(scalar(&t.wb, "FROM orders SELECT sum(double_amount)"), json!(TOTAL));
}

#[test]
fn user_ctes_see_formula_columns() {
    let t = setup();
    for sql in [
        "WITH x AS (SELECT double_amount FROM orders) SELECT sum(double_amount) FROM x",
        "with x as (select * from orders) select sum(double_amount) from x",
        "  -- a comment first\n WITH x AS (SELECT double_amount FROM orders) SELECT sum(double_amount) FROM x",
        "/* block */ WITH x AS (SELECT double_amount FROM orders), y AS (SELECT * FROM x) SELECT sum(double_amount) FROM y",
        "WITH RECURSIVE x AS (SELECT double_amount FROM orders) SELECT sum(double_amount) FROM x",
        "WITH MATERIALIZED_NOT AS (SELECT 1) SELECT sum(double_amount) FROM orders",
    ] {
        assert_eq!(scalar(&t.wb, sql), json!(TOTAL), "{sql}");
    }
}

#[test]
fn user_cte_with_a_table_name_wins() {
    let t = setup();
    assert_eq!(scalar(&t.wb, "WITH orders AS (SELECT 7 AS n) SELECT n FROM orders"), json!(7));
    // The other table's formula columns stay visible next to a shadowing CTE.
    assert_eq!(
        scalar(&t.wb, "WITH orders AS (SELECT 0 AS region) SELECT shout FROM orders JOIN regions USING (region)"),
        json!("NORTH")
    );
}

#[test]
fn joins_aggregates_and_nested_queries() {
    let t = setup();
    let p = t
        .wb
        .preview_query(
            "SELECT r.shout, sum(o.double_amount) AS total
             FROM orders o JOIN regions r ON o.region = r.region
             WHERE o.id IN (SELECT id FROM orders WHERE double_amount > 0)
             GROUP BY ALL ORDER BY 1",
            10,
        )
        .unwrap();
    // Oracle computed by hand: ids by region 0:{0,3,6,9} 1:{1,4,7} 2:{2,5,8}; amount = 10*id, doubled.
    assert_eq!(p.rows, vec![vec![json!("NORTH"), json!(360)], vec![json!("SOUTH"), json!(240)], vec![json!("WEST"), json!(300)]]);
    assert_eq!(scalar(&t.wb, "SELECT max(x) FROM (SELECT double_amount + 1 AS x FROM orders)"), json!(181));
}

#[test]
fn materialized_results_keep_formula_values() {
    let mut t = setup();
    let info = t.wb.create_table_from_query("snap", "WITH s AS (SELECT id, double_amount FROM orders) SELECT * FROM s").unwrap();
    assert_eq!(info.row_count, 10);
    assert_eq!(scalar(&t.wb, "SELECT sum(double_amount) FROM snap"), json!(TOTAL));
    let sheet = t.wb.create_sheet_from_query("S", "WITH s AS (SELECT sum(double_amount) AS total FROM orders) SELECT total FROM s", 10).unwrap();
    assert_eq!(cell_text(&t.wb, sheet.index, 1, 0).as_deref(), Some("900"));
}

#[test]
fn malformed_and_non_select_sql_fails_cleanly() {
    let t = setup();
    for bad in ["SELEC 1", "WITH x AS (SELECT 1", "", "SELECT * FROM missing_table", "WITH x AS (SELECT 1) SELECT * FROM nope"] {
        assert!(t.wb.preview_query(bad, 10).is_err(), "{bad:?}");
    }
    // Internal metadata is not a user table.
    assert!(t.wb.list_tables().unwrap().iter().all(|t| !t.name.starts_with("_rf")));
}

#[test]
fn truncation_is_reported() {
    let t = setup();
    let p = t.wb.preview_query("SELECT * FROM orders", 3).unwrap();
    assert_eq!((p.rows.len(), p.truncated), (3, true));
    let p = t.wb.preview_query("SELECT * FROM orders", 10).unwrap();
    assert_eq!((p.rows.len(), p.truncated), (10, false));
    assert!(p.columns.iter().all(|c| c.name != "_rf_id"), "the internal row id is hidden");
}
