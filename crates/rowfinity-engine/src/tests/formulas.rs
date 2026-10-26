//! 1.06: column rename/delete keep formula columns valid, or fail before any change.

use super::*;
use serde_json::json;

fn sales(t: &mut TempWb) {
    t.wb
        .create_table_from_query("sales", "SELECT * FROM (VALUES (100, 60, 'a'), (50, 50, 'b'), (0, 10, 'c')) v(revenue, cost, \"Prix €\")")
        .unwrap();
    t.wb.add_formula_column("sales", "profit", "=[revenue] - cost").unwrap();
    t.wb.add_formula_column("sales", "margin", "=CASE WHEN revenue = 0 THEN 0 ELSE [profit] / revenue END").unwrap();
    t.wb.add_formula_column("sales", "label", "='[revenue]' || ' ' || [Prix €]").unwrap();
}

fn column_values(t: &mut TempWb, name: &str) -> Vec<Value> {
    let v = t.wb.open_view(spec("sales")).unwrap();
    let i = v.columns.iter().position(|c| c.name == name).unwrap_or_else(|| panic!("no column {name}"));
    let rows = all_rows(&t.wb, v.view_id);
    rows.into_iter().map(|r| r[i].clone()).collect()
}

fn formula(t: &TempWb, name: &str) -> String {
    t.wb.columns("sales").unwrap().into_iter().find(|c| c.name == name).unwrap().formula.unwrap()
}

#[test]
fn renaming_a_data_column_rewrites_dependent_formulas() {
    let mut t = TempWb::new();
    sales(&mut t);
    let margins = column_values(&mut t, "margin");
    t.wb.rename_column("sales", "revenue", "Turnover (€)").unwrap();
    assert_eq!(formula(&t, "profit"), "=[Turnover (€)] - cost");
    assert_eq!(formula(&t, "margin"), "=CASE WHEN \"Turnover (€)\" = 0 THEN 0 ELSE [profit] / \"Turnover (€)\" END");
    assert_eq!(formula(&t, "label"), "='[revenue]' || ' ' || [Prix €]", "string literals are not references");
    assert_eq!(column_values(&mut t, "margin"), margins);
    assert_eq!(column_values(&mut t, "profit"), vec![json!(40), json!(0), json!(-10)]);
    t.reopen();
    assert_eq!(column_values(&mut t, "margin"), margins, "rewritten formulas persist");
}

#[test]
fn renaming_a_formula_column_rewrites_its_dependents() {
    let mut t = TempWb::new();
    sales(&mut t);
    t.wb.rename_column("sales", "profit", "gain").unwrap();
    assert_eq!(formula(&t, "margin"), "=CASE WHEN revenue = 0 THEN 0 ELSE [gain] / revenue END");
    assert_eq!(column_values(&mut t, "margin")[0], json!(0.4));
}

#[test]
fn unicode_names_are_rewritten_at_the_right_offsets() {
    let mut t = TempWb::new();
    sales(&mut t);
    t.wb.rename_column("sales", "Prix €", "東京 price").unwrap();
    assert_eq!(formula(&t, "label"), "='[revenue]' || ' ' || [東京 price]");
    assert_eq!(column_values(&mut t, "label")[1], json!("[revenue] b"));
}

#[test]
fn deleting_a_referenced_column_is_rejected_before_mutation() {
    let mut t = TempWb::new();
    sales(&mut t);
    let before = t.wb.columns("sales").unwrap().len();
    let err = t.wb.drop_column("sales", "cost").unwrap_err().to_string();
    assert!(err.contains("profit"), "{err}");
    let err = t.wb.drop_column("sales", "profit").unwrap_err().to_string();
    assert!(err.contains("margin"), "{err}");
    assert_eq!(t.wb.columns("sales").unwrap().len(), before);
    assert_eq!(column_values(&mut t, "profit"), vec![json!(40), json!(0), json!(-10)]);
    // Dependents first, then the column itself.
    t.wb.drop_column("sales", "margin").unwrap();
    t.wb.drop_column("sales", "profit").unwrap();
    t.wb.drop_column("sales", "cost").unwrap();
    assert_eq!(t.wb.columns("sales").unwrap().len(), before - 3);
}

#[test]
fn rename_conflicts_and_case_only_renames() {
    let mut t = TempWb::new();
    sales(&mut t);
    assert!(t.wb.rename_column("sales", "cost", "REVENUE").is_err());
    assert!(t.wb.rename_column("sales", "cost", "_rf_id").is_err());
    assert!(t.wb.rename_column("sales", "nope", "x").is_err());
    t.wb.rename_column("sales", "cost", "Cost").unwrap();
    assert_eq!(column_values(&mut t, "profit"), vec![json!(40), json!(0), json!(-10)]);
}

#[test]
fn formulas_cannot_contain_subqueries_or_extra_statements() {
    let mut t = TempWb::new();
    sales(&mut t);
    for bad in [
        "(SELECT 1)",
        "revenue) AS x FROM sales; DROP TABLE sales; SELECT (1",
        "1; DROP TABLE sales",
        "revenue IN (SELECT cost FROM sales)",
        "EXISTS (SELECT 1)",
        "",
    ] {
        assert!(t.wb.add_formula_column("sales", "bad", bad).is_err(), "{bad:?} should be rejected");
        assert!(t.wb.update_formula("sales", "profit", bad).is_err(), "{bad:?} should be rejected");
    }
    assert_eq!(t.wb.list_tables().unwrap().len(), 1);
    assert_eq!(formula(&t, "profit"), "=[revenue] - cost");
}

#[test]
fn updating_a_formula_into_a_cycle_or_missing_column_fails() {
    let mut t = TempWb::new();
    sales(&mut t);
    assert!(t.wb.update_formula("sales", "profit", "[margin] * 2").is_err(), "forward reference / cycle");
    assert!(t.wb.update_formula("sales", "profit", "[missing] * 2").is_err());
    assert_eq!(formula(&t, "profit"), "=[revenue] - cost");
    t.wb.update_formula("sales", "profit", "revenue - cost * 2").unwrap();
    assert_eq!(column_values(&mut t, "profit"), vec![json!(-20), json!(-50), json!(-20)]);
}

#[test]
fn renamed_table_keeps_formulas() {
    let mut t = TempWb::new();
    sales(&mut t);
    let name = t.wb.rename_table("sales", "orders").unwrap();
    assert_eq!(name, "orders");
    assert_eq!(scalar(&t.wb, "SELECT sum(profit) FROM orders"), json!(30));
}
