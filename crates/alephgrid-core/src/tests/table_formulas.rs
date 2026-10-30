//! F.05: spreadsheet-syntax calculated columns stored in a workbook and read through views.
use super::*;

fn setup() -> TempWb {
    let mut t = TempWb::new();
    t.wb.create_table_from_query("orders", "SELECT * FROM (VALUES (1, 100.0, 4, 'Ann'), (2, 50.0, 0, NULL), (3, NULL, 2, 'Bo')) v(id, amount, units, buyer)").unwrap();
    t
}

fn block(wb: &mut Workbook) -> RowBlock {
    let v = wb.open_view(spec("orders")).unwrap();
    wb.fetch(v.view_id, 0, 10).unwrap()
}

#[test]
fn sheet_formula_columns_compute_errors_and_survive_reopen() {
    let mut t = setup();
    t.wb.add_formula_column_as("orders", "unit_price", "=[@amount]/[@units]", FormulaLanguage::Sheet).unwrap();
    t.wb.add_formula_column_as("orders", "label", "=IF([@unit_price]>20,\"high\",\"low\")&\" \"&[@buyer]", FormulaLanguage::Sheet).unwrap();
    t.wb.add_formula_column_as("orders", "safe", "=IFERROR([@unit_price],\"\")", FormulaLanguage::Sheet).unwrap();
    let cols = t.wb.columns("orders").unwrap();
    let names: Vec<&str> = cols.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["id", "amount", "units", "buyer", "unit_price", "label", "safe"], "error columns stay hidden");
    assert_eq!(cols[4].formula_language, Some(FormulaLanguage::Sheet));
    assert_eq!(cols[4].sql_type, "DOUBLE");
    assert_eq!(cols[5].sql_type, "VARCHAR");
    for reopen in [false, true] {
        if reopen { t.reopen(); }
        let b = block(&mut t.wb);
        let row = |r: usize| b.rows[r].iter().map(|v| v.to_string()).collect::<Vec<_>>().join("|");
        assert_eq!(row(0), "1|\"100.0\"|4|\"Ann\"|25.0|\"high Ann\"|25.0");
        // Row 2: 50/0 is #DIV/0!; the label depends on it and shows the same error.
        assert_eq!(row(1), "2|\"50.0\"|0|null|null|null|null");
        // Row 3: blank amount is 0, so 0/2 = 0.
        assert_eq!(row(2), "3|null|2|\"Bo\"|0.0|\"low Bo\"|0.0");
        let marks: Vec<(u32, u32, &str)> = b.errors.iter().map(|e| (e.row, e.column, e.code.as_str())).collect();
        assert_eq!(marks, [(1, 4, "#DIV/0!"), (1, 5, "#DIV/0!")], "IFERROR(…, \"\") leaves a blank, not an error");
    }
}

#[test]
fn dependencies_follow_renames_and_block_deletes() {
    let mut t = setup();
    t.wb.add_formula_column_as("orders", "total", "=[@amount]*[units]+[@[amount]]", FormulaLanguage::Sheet).unwrap();
    t.wb.rename_column("orders", "amount", "net amount").unwrap();
    let total = t.wb.columns("orders").unwrap().into_iter().find(|c| c.name == "total").unwrap();
    assert_eq!(total.formula.as_deref(), Some("=[@[net amount]]*[units]+[@[net amount]]"));
    let err = t.wb.drop_column("orders", "units").unwrap_err().to_string();
    assert!(err.contains("total"), "{err}");
    // Rejected formulas change nothing.
    let err = t.wb.add_formula_column_as("orders", "bad", "=[@missing]+1", FormulaLanguage::Sheet).unwrap_err().to_string();
    assert!(err.contains("no column named") && err.contains("character"), "{err}");
    assert!(t.wb.update_formula("orders", "total", "=[@total]+1").unwrap_err().to_string().contains("no column named"), "a formula cannot read itself");
    t.wb.update_formula("orders", "total", "=ROUND([@[net amount]]/3, 1)").unwrap();
    let b = block(&mut t.wb);
    assert_eq!(b.rows[0][4].to_string(), "33.3");
    // Reserved names cannot be created.
    assert!(t.wb.add_column("orders", "_rf_e_x", "INTEGER").is_err());
}
