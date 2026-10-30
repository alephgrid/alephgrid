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

// ---------------------------------------------------------------- F.06 lookups across tables

/// Customers has a duplicate id (1: "Ann" then "Ann B") and a blank id; orders look them up.
fn lookups() -> TempWb {
    let mut t = TempWb::new();
    t.wb.create_table_from_query("customers", "SELECT * FROM (VALUES (1, 'Ann', 'gold'), (2, 'Bo', 'silver'), (1, 'Ann B', 'bronze'), (NULL, 'Nobody', 'none')) v(id, name, tier)").unwrap();
    t.wb.create_table_from_query("orders", "SELECT * FROM (VALUES (10, 1, 'bronze'), (11, 2, 'gold'), (12, 3, 'gold'), (13, NULL, 'none')) v(id, cid, want)").unwrap();
    for (name, f) in [
        ("customer", "=XLOOKUP([@cid],customers[id],customers[name],\"unknown\")"),
        ("last", "=XLOOKUP([@cid],customers[id],customers[name],\"unknown\",0,-1)"),
        ("tier", "=VLOOKUP([@cid],customers,3,FALSE)"),
        ("both", "=XLOOKUP(1,(customers[id]=[@cid])*(customers[tier]=[@want]),customers[name],\"none\")"),
    ] {
        t.wb.add_formula_column_as("orders", name, f, FormulaLanguage::Sheet).unwrap();
    }
    t
}

fn table_block(wb: &mut Workbook, table: &str) -> RowBlock {
    let v = wb.open_view(spec(table)).unwrap();
    wb.fetch(v.view_id, 0, 100).unwrap()
}

/// Rendered rows: values as JSON text, error cells as their code.
fn rendered(b: &RowBlock) -> Vec<String> {
    b.rows.iter().enumerate().map(|(r, row)| {
        row.iter().enumerate().map(|(c, v)| {
            b.errors.iter().find(|e| e.row as usize == r && e.column as usize == c).map(|e| e.code.clone()).unwrap_or_else(|| v.to_string())
        }).collect::<Vec<_>>().join("|")
    }).collect()
}

#[test]
fn lookups_give_one_result_per_row_and_follow_source_edits() {
    let mut t = lookups();
    let expect = |first: &str| vec![
        format!("10|1|\"bronze\"|\"{first}\"|\"Ann B\"|\"gold\"|\"Ann B\""),
        "11|2|\"gold\"|\"Bo\"|\"Bo\"|\"silver\"|\"none\"".to_string(),
        "12|3|\"gold\"|\"unknown\"|\"unknown\"|#N/A|\"none\"".to_string(),
        // A blank key finds the blank id; the pair (blank, "none") matches too.
        "13|null|\"none\"|\"Nobody\"|\"Nobody\"|\"none\"|\"Nobody\"".to_string(),
    ];
    assert_eq!(rendered(&table_block(&mut t.wb, "orders")), expect("Ann"), "duplicates give the first or last match, one row per order");
    // Editing the source changes the lookups that read it, and nothing else.
    let v = t.wb.open_view(spec("customers")).unwrap();
    t.wb.edit(v.view_id, &[CellEdit { row: 0, column: "name".into(), value: Some("Anna".into()), record: None }]).unwrap();
    assert_eq!(rendered(&table_block(&mut t.wb, "orders")), expect("Anna"));
    t.reopen();
    assert_eq!(rendered(&table_block(&mut t.wb, "orders")), expect("Anna"));
}

#[test]
fn renames_rewrite_lookups_in_other_tables_and_deletes_are_refused() {
    let mut t = lookups();
    let before = rendered(&table_block(&mut t.wb, "orders"));
    let formula = |wb: &Workbook, name: &str| wb.columns("orders").unwrap().into_iter().find(|c| c.name == name).unwrap().formula.unwrap();
    t.wb.rename_column("customers", "name", "full name").unwrap();
    assert_eq!(formula(&t.wb, "customer"), "=XLOOKUP([@cid],customers[id],customers[full name],\"unknown\")");
    assert_eq!(rendered(&table_block(&mut t.wb, "orders")), before);
    assert_eq!(t.wb.rename_table("customers", "clients").unwrap(), "clients");
    assert_eq!(formula(&t.wb, "tier"), "=VLOOKUP([@cid],clients,3,FALSE)");
    assert_eq!(formula(&t.wb, "both"), "=XLOOKUP(1,(clients[id]=[@cid])*(clients[tier]=[@want]),clients[full name],\"none\")");
    assert_eq!(rendered(&table_block(&mut t.wb, "orders")), before);
    // A name that formulas cannot spell is refused while formulas refer to the table.
    assert!(t.wb.rename_table("clients", "2024").unwrap_err().to_string().contains("letters, digits"));
    let err = t.wb.drop_column("clients", "full name").unwrap_err().to_string();
    assert!(err.contains("orders.\"customer\"") && err.contains("orders.\"last\""), "{err}");
    // VLOOKUP reads by column number: deleting a column at or before the one it returns would
    // shift its result; deleting one after it is fine.
    let err = t.wb.drop_column("clients", "tier").unwrap_err().to_string();
    assert!(err.contains("orders.\"tier\""), "{err}");
    t.wb.add_column("clients", "extra", "INTEGER").unwrap();
    t.wb.drop_column("clients", "extra").unwrap();
    assert!(t.wb.drop_table("clients").unwrap_err().to_string().contains("orders.\"customer\""));
    t.reopen();
    assert_eq!(rendered(&table_block(&mut t.wb, "orders")), before);
}

#[test]
fn tables_may_look_each_other_up_but_circular_references_are_refused() {
    let mut t = lookups();
    // Orders read customers and customers read orders, through different columns:
    // customers.first_label → orders.label → customers.label is a chain, not a cycle.
    t.wb.add_formula_column_as("customers", "label", "=[@name]&\"!\"", FormulaLanguage::Sheet).unwrap();
    t.wb.add_formula_column_as("orders", "label", "=XLOOKUP([@cid],customers[id],customers[label],\"-\")", FormulaLanguage::Sheet).unwrap();
    t.wb.add_formula_column_as("customers", "first_order", "=XLOOKUP([@id],orders[cid],orders[id],0)", FormulaLanguage::Sheet).unwrap();
    t.wb.add_formula_column_as("customers", "first_label", "=XLOOKUP([@id],orders[cid],orders[label],\"-\")", FormulaLanguage::Sheet).unwrap();
    for reopen in [false, true] {
        if reopen { t.reopen(); }
        let orders: Vec<String> = rendered(&table_block(&mut t.wb, "orders")).iter().map(|r| r.rsplit('|').next().unwrap().to_string()).collect();
        assert_eq!(orders, ["\"Ann!\"", "\"Bo!\"", "\"-\"", "\"Nobody!\""]);
        let customers: Vec<String> = rendered(&table_block(&mut t.wb, "customers")).iter().map(|r| r.split('|').skip(3).collect::<Vec<_>>().join("|")).collect();
        assert_eq!(customers, ["\"Ann!\"|10.0|\"Ann!\"", "\"Bo!\"|11.0|\"Bo!\"", "\"Ann B!\"|10.0|\"Ann!\"", "\"Nobody!\"|13.0|\"Nobody!\""]);
    }
    // Making customers.label read orders.label closes the loop.
    let err = t.wb.update_formula("customers", "label", "=XLOOKUP([@id],orders[cid],orders[label],\"-\")").unwrap_err().to_string();
    assert!(err.contains("circular reference: customers.label → orders.label → customers.label"), "{err}");
    let err = t.wb.add_formula_column_as("orders", "loop", "=XLOOKUP([@cid],customers[id],customers[first_label],\"-\")", FormulaLanguage::Sheet);
    assert!(err.is_ok(), "reading a column that reads this table is not circular by itself: {err:?}");
}
