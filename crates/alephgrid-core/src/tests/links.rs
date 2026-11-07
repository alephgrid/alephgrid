//! F.09: table formulas that read sheet cells, and changes that reach tables indirectly.
use super::*;

/// Orders, and a Settings sheet with B1 = 0.1 and B2 = "EUR" (B3 empty).
fn settings() -> (TempWb, u32) {
    let mut t = TempWb::new();
    t.wb.create_table_from_query("orders", "SELECT * FROM (VALUES (1, 100.0), (2, 50.0), (3, NULL)) v(id, amount)").unwrap();
    let s = t.wb.with_sheets(|s| s.create("Settings")).unwrap().index;
    t.wb.with_sheets(|sh| sh.set(s, &[sheet_input(0, 1, "0.1"), sheet_input(1, 1, "EUR")])).unwrap();
    for (name, f) in [
        ("gross", "=[@amount]*(1+Settings!$B$1)"),
        ("cur", "=Settings!B2&\" \"&[@id]"),
        ("note", "=IF(Settings!B3=\"\",\"none\",\"set\")"),
    ] {
        t.wb.add_formula_column_as("orders", name, f, FormulaLanguage::Sheet).unwrap();
    }
    (t, s)
}

fn rows(wb: &mut Workbook, table: &str) -> Vec<String> {
    let v = wb.open_view(spec(table)).unwrap();
    let b = wb.fetch(v.view_id, 0, 100).unwrap();
    wb.close_view(v.view_id);
    b.rows.iter().enumerate().map(|(r, row)| {
        row.iter().enumerate().map(|(c, v)| {
            match b.errors.iter().find(|e| e.row as usize == r && e.column as usize == c) {
                Some(e) => e.code.clone(),
                None => v.to_string(),
            }
        }).collect::<Vec<_>>().join("|")
    }).collect()
}

fn formula(wb: &Workbook, table: &str, column: &str) -> String {
    wb.columns(table).unwrap().into_iter().find(|c| c.name == column).and_then(|c| c.formula).unwrap()
}

fn formulas(wb: &Workbook) -> [String; 3] {
    ["gross", "cur", "note"].map(|c| formula(wb, "orders", c))
}

#[test]
fn table_formulas_read_sheet_cells_and_follow_them() {
    let (mut t, s) = settings();
    assert_eq!(rows(&mut t.wb, "orders"), [
        "1|\"100.0\"|110.00000000000001|\"EUR 1\"|\"none\"",
        "2|\"50.0\"|55.00000000000001|\"EUR 2\"|\"none\"",
        "3|null|0.0|\"EUR 3\"|\"none\"",
    ]);
    // The link sheet is internal: not listed, not addressable.
    assert_eq!(t.wb.sheets().list().iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["Settings"]);
    assert!(t.wb.sheets().fetch(s + 1, 0, 1, 1).is_err());
    assert!(t.wb.with_sheets(|sh| sh.set(s + 1, &[sheet_input(0, 0, "x")])).is_err());

    // Editing a cell changes the table's values and its data revision.
    t.wb.with_sheets(|sh| sh.set(s, &[sheet_input(0, 1, "0.2"), sheet_input(2, 1, "x")])).unwrap();
    let rev = t.wb.revision().unwrap();
    // B3 went from blank to text: a new value type, so the columns changed too.
    assert_eq!(t.wb.changed_tables(rev).unwrap(), [("orders".to_string(), true)]);
    assert_eq!(rows(&mut t.wb, "orders")[0], "1|\"100.0\"|120.0|\"EUR 1\"|\"set\"");
    t.wb.with_sheets(|sh| sh.set(s, &[sheet_input(0, 1, "0.25"), sheet_input(2, 1, "y")])).unwrap();
    assert_eq!(t.wb.changed_tables(t.wb.revision().unwrap()).unwrap(), [("orders".to_string(), false)], "same types: values only");
    t.wb.with_sheets(|sh| sh.set(s, &[sheet_input(0, 1, "0.2")])).unwrap();
    // A sheet change that leaves the cells read alone changes no table.
    t.wb.with_sheets(|sh| sh.set(s, &[sheet_input(5, 5, "unrelated")])).unwrap();
    assert!(t.wb.changed_tables(t.wb.revision().unwrap()).unwrap().is_empty());

    // References follow their cells: a row inserted above, undo, redo.
    let at_b1 = ["=[@amount]*(1+Settings!$B$1)", "=Settings!B2&\" \"&[@id]", "=IF(Settings!B3=\"\",\"none\",\"set\")"].map(String::from);
    let at_b2 = ["=[@amount]*(1+Settings!$B$2)", "=Settings!B3&\" \"&[@id]", "=IF(Settings!B4=\"\",\"none\",\"set\")"].map(String::from);
    t.wb.with_sheets(|sh| sh.insert_rows(s, 0, 1)).unwrap();
    assert_eq!(formulas(&t.wb), at_b2);
    assert_eq!(rows(&mut t.wb, "orders")[0], "1|\"100.0\"|120.0|\"EUR 1\"|\"set\"");
    assert!(t.wb.with_sheets(|sh| sh.undo()).unwrap());
    assert_eq!(formulas(&t.wb), at_b1);
    assert!(t.wb.with_sheets(|sh| sh.redo()).unwrap());
    assert_eq!(formulas(&t.wb), at_b2);
    assert!(t.wb.with_sheets(|sh| sh.undo()).unwrap());
    assert_eq!(formulas(&t.wb), at_b1);
    // Undo still undoes the user's own changes, not the link bookkeeping.
    assert!(t.wb.with_sheets(|sh| sh.undo()).unwrap());
    assert_eq!(cell_text(&t.wb, s, 5, 5), None);
    assert!(t.wb.with_sheets(|sh| sh.redo()).unwrap());
    assert_eq!(cell_text(&t.wb, s, 5, 5).as_deref(), Some("unrelated"));

    // A renamed sheet is renamed in the formulas; a sheet they read cannot be deleted.
    t.wb.with_sheets(|sh| sh.rename(s, "Plan 2026")).unwrap();
    assert_eq!(formula(&t.wb, "orders", "gross"), "=[@amount]*(1+'Plan 2026'!$B$1)");
    let before = fingerprint(&t.wb);
    let err = t.wb.delete_sheet(s).unwrap_err().to_string();
    assert!(err.contains("orders.\"gross\"") && err.contains("orders.\"note\""), "{err}");
    assert_eq!(fingerprint(&t.wb), before);

    // Deleting the row of a cell read makes the reference #REF!, as in a sheet.
    t.wb.with_sheets(|sh| sh.delete_rows(s, 0, 1)).unwrap();
    assert_eq!(formulas(&t.wb), ["=[@amount]*(1+#REF!)", "='Plan 2026'!B1&\" \"&[@id]", "=IF('Plan 2026'!B2=\"\",\"none\",\"set\")"].map(String::from));
    assert_eq!(rows(&mut t.wb, "orders")[0], "1|\"100.0\"|#REF!|\"EUR 1\"|\"set\"");

    // Everything survives a reopen, and edits keep flowing.
    t.reopen();
    assert_eq!(formula(&t.wb, "orders", "cur"), "='Plan 2026'!B1&\" \"&[@id]");
    assert_eq!(rows(&mut t.wb, "orders")[1], "2|\"50.0\"|#REF!|\"EUR 2\"|\"set\"");
    t.wb.with_sheets(|sh| sh.set(s, &[sheet_input(0, 1, "USD")])).unwrap();
    assert_eq!(rows(&mut t.wb, "orders")[1], "2|\"50.0\"|#REF!|\"USD 2\"|\"set\"");
    assert_eq!(t.wb.sheets().list().len(), 1);
}

#[test]
fn unknown_and_internal_sheets_are_refused() {
    let (mut t, _) = settings();
    for f in ["=Nope!A1", "=_rf_links!A1", "='_rf_links'!A1"] {
        let err = t.wb.add_formula_column_as("orders", "bad", f, FormulaLanguage::Sheet).unwrap_err().to_string();
        assert!(err.contains("no sheet named"), "{f}: {err}");
    }
    let err = t.wb.add_formula_column_as("orders", "bad", "=SUM(Settings!B1:B2)", FormulaLanguage::Sheet).unwrap_err().to_string();
    assert!(err.contains("single sheet cells"), "{err}");
    // Without any sheet at all, too.
    let mut empty = TempWb::new();
    empty.wb.create_table_from_query("t", "SELECT 1 AS a").unwrap();
    let err = empty.wb.add_formula_column_as("t", "x", "=Sheet1!A1", FormulaLanguage::Sheet).unwrap_err().to_string();
    assert!(err.contains("no sheet named"), "{err}");
}

#[test]
fn value_types_and_formula_changes_keep_links_exact() {
    let (mut t, s) = settings();
    let col = |wb: &Workbook, c: &str| wb.columns("orders").unwrap().into_iter().find(|x| x.name == c).unwrap().sql_type;
    t.wb.add_formula_column_as("orders", "raw", "=Settings!B1", FormulaLanguage::Sheet).unwrap();
    assert_eq!(col(&t.wb, "raw"), "DOUBLE");
    // A value of another type changes the column's type, and the table counts as restructured.
    t.wb.with_sheets(|sh| sh.set(s, &[sheet_input(0, 1, "abc")])).unwrap();
    assert_eq!(col(&t.wb, "raw"), "VARCHAR");
    assert_eq!(t.wb.changed_tables(t.wb.revision().unwrap()).unwrap(), [("orders".to_string(), true)]);
    assert_eq!(rows(&mut t.wb, "orders")[0], "1|\"100.0\"|#VALUE!|\"EUR 1\"|\"none\"|\"abc\"");
    t.wb.with_sheets(|sh| sh.set(s, &[sheet_input(0, 1, "=1/0")])).unwrap();
    assert_eq!(rows(&mut t.wb, "orders")[0], "1|\"100.0\"|#DIV/0!|\"EUR 1\"|\"none\"|#DIV/0!");
    t.wb.with_sheets(|sh| sh.set(s, &[sheet_input(0, 1, "")])).unwrap();
    assert_eq!(rows(&mut t.wb, "orders")[0], "1|\"100.0\"|100.0|\"EUR 1\"|\"none\"|null");
    // Reader connections see committed cell values (they read them from the snapshot).
    let reader = t.wb.reader().unwrap();
    let v = reader.open_view(spec("orders")).unwrap();
    t.wb.with_sheets(|sh| sh.set(s, &[sheet_input(0, 1, "0.5")])).unwrap();
    assert_eq!(reader.fetch(v.view_id, 0, 1, 0, None).unwrap().rows[0][2].to_string(), "150.0");

    // Links follow formula changes: one per cell read, none once nothing reads cells.
    let links = |wb: &Workbook| scalar_raw(wb, "SELECT count(*) FROM _rf_links");
    assert_eq!(links(&t.wb), "3");
    t.wb.update_formula("orders", "raw", "=Settings!B2&Settings!$C$9").unwrap();
    assert_eq!(links(&t.wb), "4");
    for c in ["raw", "gross", "cur", "note"] {
        t.wb.drop_column("orders", c).unwrap();
    }
    assert_eq!(links(&t.wb), "0");
    t.reopen();
    assert_eq!(links(&t.wb), "0");
    // The anchors are gone too: exporting shows only the user's sheet.
    let path = t.dir.path().join("out.xlsx");
    t.wb.add_formula_column_as("orders", "again", "=Settings!B1*2", FormulaLanguage::Sheet).unwrap();
    t.wb.sheets().export_xlsx(&path).unwrap();
    let (sheets, _) = crate::sheets::Sheets::preview_xlsx(&path).unwrap();
    assert_eq!(sheets.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["Settings"]);
}

fn scalar_raw(wb: &Workbook, sql: &str) -> String {
    match &wb.raw_query(sql).unwrap()[0][0] {
        duckdb::types::Value::BigInt(n) => n.to_string(),
        other => format!("{other:?}"),
    }
}

/// Lookups and saved query results depend on the tables formulas read, not only the ones named.
#[test]
fn changes_reach_tables_that_read_them_indirectly() {
    let mut t = TempWb::new();
    let s = t.wb.with_sheets(|s| s.create("Settings")).unwrap().index;
    t.wb.with_sheets(|sh| sh.set(s, &[sheet_input(0, 0, " (vip)")])).unwrap();
    t.wb.create_table_from_query("customers", "SELECT * FROM (VALUES (1, 'Ann'), (2, 'Bo')) v(id, name)").unwrap();
    t.wb.create_table_from_query("orders", "SELECT * FROM (VALUES (10, 1), (11, 2)) v(id, cid)").unwrap();
    t.wb.add_formula_column_as("customers", "label", "=[@name]&Settings!A1", FormulaLanguage::Sheet).unwrap();
    t.wb.add_formula_column_as("orders", "who", "=XLOOKUP([@cid],customers[id],customers[label])", FormulaLanguage::Sheet).unwrap();
    t.wb.create_table_from_query("snapshot", "SELECT id, who FROM orders").unwrap();
    let m = t.wb.materialized().unwrap().into_iter().find(|m| m.table == "snapshot").unwrap();
    assert_eq!(m.sources.iter().map(|s| s.table.as_str()).collect::<Vec<_>>(), ["customers", "orders"]);
    assert!(!m.stale);

    // A sheet edit reaches customers (reads the cell) and orders (looks customers up).
    t.wb.with_sheets(|sh| sh.set(s, &[sheet_input(0, 0, " (gold)")])).unwrap();
    let changed = |wb: &Workbook| wb.changed_tables(wb.revision().unwrap()).unwrap().into_iter().map(|(t, _)| t).collect::<Vec<_>>();
    assert_eq!(changed(&t.wb), ["customers", "orders"]);
    assert_eq!(rows(&mut t.wb, "orders")[0], "10|1|\"Ann (gold)\"");
    assert!(t.wb.materialized().unwrap().into_iter().find(|m| m.table == "snapshot").unwrap().stale);

    // So does an edit of customers; the snapshot table reads nothing live.
    let v = t.wb.open_view(spec("customers")).unwrap();
    t.wb.edit(v.view_id, &[edit(1, "name", "Bob")]).unwrap();
    assert_eq!(changed(&t.wb), ["orders"], "the edit itself is about customers");
    assert_eq!(rows(&mut t.wb, "orders")[1], "11|2|\"Bob (gold)\"");
    let run = t.wb.run_query("SELECT who FROM orders ORDER BY id", &[], 10).unwrap();
    assert_eq!(run.sources.iter().map(|s| s.table.as_str()).collect::<Vec<_>>(), ["customers", "orders"]);
}

#[test]
fn moved_cells_take_their_readers_along() {
    let (mut t, s) = settings();
    let b1 = CellArea { row: 0, column: 1, rows: 1, columns: 1 };
    t.wb.with_sheets(|sh| sh.move_cells(s, b1, s, (4, 3))).unwrap();
    assert_eq!(formula(&t.wb, "orders", "gross"), "=[@amount]*(1+Settings!$D$5)");
    assert_eq!(rows(&mut t.wb, "orders")[0], "1|\"100.0\"|110.00000000000001|\"EUR 1\"|\"none\"");
    assert!(t.wb.with_sheets(|sh| sh.undo()).unwrap());
    assert_eq!(formula(&t.wb, "orders", "gross"), "=[@amount]*(1+Settings!$B$1)");
}
