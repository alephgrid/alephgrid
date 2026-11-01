//! F.09: sheet cells that read tables ("table cells"), across-engine cycles and atomic
//! derived updates. Expected values are worked out by hand from the rows below.
use super::*;

/// orders: (1, West, 100), (2, East, 50), (3, West, 25.5), (4, NULL, 10);
/// customers: (1, Ann), (2, Bob); an empty sheet "Report".
fn report() -> (TempWb, u32) {
    let mut t = TempWb::new();
    t.wb.create_table_from_query(
        "orders",
        "SELECT * FROM (VALUES (1, 'West', 100.0, DATE '2026-01-05'), (2, 'East', 50.0, DATE '2026-01-07'), (3, 'West', 25.5, DATE '2026-02-01'), (4, NULL, 10.0, DATE '2026-02-10')) v(id, region, amount, day)",
    )
    .unwrap();
    t.wb.create_table_from_query("customers", "SELECT * FROM (VALUES (1, 'Ann'), (2, 'Bob')) v(id, name)").unwrap();
    let r = t.wb.with_sheets(|s| s.create("Report")).unwrap().index;
    (t, r)
}

fn shown(wb: &Workbook, sheet: u32, row: u32, column: u32) -> (String, Option<String>) {
    let block = wb.sheets().fetch(sheet, row, 1, column + 1).unwrap();
    let cell = block.rows[0].get(column as usize).cloned().flatten().unwrap_or(crate::sheets::SheetCell(String::new(), crate::sheets::Align::Left, None));
    (cell.0, cell.2)
}

fn value(wb: &Workbook, sheet: u32, row: u32, column: u32) -> String {
    shown(wb, sheet, row, column).0
}

#[test]
fn sheet_cells_read_tables_and_follow_their_changes() {
    let (mut t, r) = report();
    t.wb.set_sheet_cells(r, &[
        sheet_input(0, 0, "West"),
        sheet_input(0, 1, "=SUMIFS(orders[amount], orders[region], A1)"),
        sheet_input(1, 1, "=COUNTIFS(orders[region], \"West\")"),
        sheet_input(2, 1, "=B1*2"),
        sheet_input(3, 1, "=XLOOKUP(2, customers[id], customers[name])"),
        sheet_input(4, 1, "=XLOOKUP(9, customers[id], customers[name])"),
        sheet_input(5, 1, "=AVERAGEIFS(orders[amount], orders[region], \"East\")"),
        sheet_input(6, 1, "=1+2"),
    ]).unwrap();
    // 100 + 25.5; two West rows; twice B1; Bob; no customer 9; the one East row; a plain formula.
    assert_eq!(value(&t.wb, r, 0, 1), "125.5");
    assert_eq!(value(&t.wb, r, 1, 1), "2");
    assert_eq!(value(&t.wb, r, 2, 1), "251");
    assert_eq!(value(&t.wb, r, 3, 1), "Bob");
    assert_eq!(value(&t.wb, r, 4, 1), "#N/A");
    assert_eq!(value(&t.wb, r, 5, 1), "50");
    assert_eq!(value(&t.wb, r, 6, 1), "3");
    // The formula bar shows the formula as typed (cells on the same sheet without its name).
    assert_eq!(shown(&t.wb, r, 0, 1).1.as_deref(), Some("=SUMIFS(orders[amount], orders[region], A1)"));
    assert_eq!(t.wb.sheets().cell_detail(r, 3, 1).unwrap().input, "=XLOOKUP(2, customers[id], customers[name])");
    assert_eq!(shown(&t.wb, r, 6, 1).1.as_deref(), Some("=1+2"), "formulas the sheet reads stay sheet formulas");
    // Stored with the sheet's name, so it can follow the cell.
    assert_eq!(crate::storage::table_cells(&t.wb.conn).unwrap()[0].formula, "SUMIFS(orders[amount], orders[region], Report!A1)");

    // Editing the criteria cell recomputes the table cell and what reads it.
    t.wb.set_sheet_cells(r, &[sheet_input(0, 0, "East")]).unwrap();
    assert_eq!(value(&t.wb, r, 0, 1), "50");
    assert_eq!(value(&t.wb, r, 2, 1), "100");

    // Editing the table recomputes it in the same change, and says so.
    let v = t.wb.open_view(spec("orders")).unwrap();
    t.wb.edit(v.view_id, &[edit(1, "amount", "70")]).unwrap();
    assert!(t.wb.changed_table_cells());
    assert_eq!(value(&t.wb, r, 0, 1), "70");
    assert_eq!(value(&t.wb, r, 2, 1), "140");
    assert_eq!(value(&t.wb, r, 5, 1), "70");
    // A change that no table cell reads leaves them alone.
    let c = t.wb.open_view(spec("customers")).unwrap();
    t.wb.edit(c.view_id, &[edit(0, "name", "Ann B")]).unwrap();
    assert!(!t.wb.changed_table_cells());
    t.wb.edit(c.view_id, &[edit(1, "name", "Bobby")]).unwrap();
    assert!(t.wb.changed_table_cells());
    assert_eq!(value(&t.wb, r, 3, 1), "Bobby");

    // Rows inserted above move the criteria cell; the formula follows it.
    t.wb.with_sheets(|s| s.insert_rows(r, 0, 2)).unwrap();
    assert_eq!(shown(&t.wb, r, 2, 1).1.as_deref(), Some("=SUMIFS(orders[amount], orders[region], A3)"));
    assert_eq!(value(&t.wb, r, 2, 1), "70");
    assert!(t.wb.with_sheets(|s| s.undo()).unwrap());
    assert_eq!(shown(&t.wb, r, 0, 1).1.as_deref(), Some("=SUMIFS(orders[amount], orders[region], A1)"));

    // Renames follow; deleting what a table cell reads is refused.
    t.wb.rename_column("orders", "amount", "total").unwrap();
    assert_eq!(shown(&t.wb, r, 0, 1).1.as_deref(), Some("=SUMIFS(orders[total], orders[region], A1)"));
    t.wb.rename_table("customers", "clients").unwrap();
    assert_eq!(shown(&t.wb, r, 3, 1).1.as_deref(), Some("=XLOOKUP(2, clients[id], clients[name])"));
    let e = t.wb.drop_column("orders", "region").unwrap_err().to_string();
    assert!(e.contains("sheet cell Report!B1"), "{e}");
    let e = t.wb.drop_table("clients").unwrap_err().to_string();
    assert!(e.contains("sheet cell Report!B4"), "{e}");
    assert_eq!(value(&t.wb, r, 0, 1), "70");

    // Undo removes a table cell and redo brings it back with its formula.
    t.wb.set_sheet_cells(r, &[sheet_input(8, 1, "=SUMIFS(orders[total], orders[region], \"West\")")]).unwrap();
    assert_eq!(value(&t.wb, r, 8, 1), "125.5");
    assert!(t.wb.with_sheets(|s| s.undo()).unwrap());
    assert_eq!(value(&t.wb, r, 8, 1), "");
    // While it is gone, nothing reads the table through it: the column can be deleted…
    assert!(t.wb.with_sheets(|s| s.redo()).unwrap());
    assert_eq!(value(&t.wb, r, 8, 1), "125.5");
    assert_eq!(shown(&t.wb, r, 8, 1).1.as_deref(), Some("=SUMIFS(orders[total], orders[region], \"West\")"));

    // Reopening keeps formulas and values.
    t.wb.close_view(v.view_id);
    t.wb.close_view(c.view_id);
    t.reopen();
    assert_eq!(value(&t.wb, r, 0, 1), "70");
    assert_eq!(shown(&t.wb, r, 0, 1).1.as_deref(), Some("=SUMIFS(orders[total], orders[region], A1)"));
    let v = t.wb.open_view(spec("orders")).unwrap();
    t.wb.edit(v.view_id, &[edit(1, "total", "80")]).unwrap();
    assert_eq!(value(&t.wb, r, 0, 1), "80");

    // Export writes values, and says which cells it replaced.
    let path = t.dir.path().join("out.xlsx");
    let notes = t.wb.sheets().export_xlsx(&path).unwrap();
    // B1, B2, B4, B5, B6 and B9.
    assert_eq!(notes.len(), 6, "{notes:?}");
    assert!(notes[0].starts_with("Report!B1: the table formula =SUMIFS(orders[total], orders[region], A1) is exported as its value \"80\""), "{notes:?}");
    let (sheets, _) = crate::sheets::Sheets::preview_xlsx(&path).unwrap();
    assert_eq!(sheets.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["Report"]);
}

#[test]
fn table_cell_values_have_the_sheet_types() {
    let (mut t, r) = report();
    t.wb.set_sheet_cells(r, &[
        // A date reaches the sheet as its serial number: 2026-02-10 is day 46063.
        sheet_input(0, 0, "=XLOOKUP(4, orders[id], orders[day])"),
        sheet_input(1, 0, "=XLOOKUP(1, orders[id], orders[region])=\"West\""),
        sheet_input(2, 0, "=XLOOKUP(4, orders[id], orders[region])"),
        sheet_input(3, 0, "=SUMIFS(orders[amount], orders[region], \"North\")"),
        sheet_input(4, 0, "=XLOOKUP(1, customers[id], customers[name])&\"!\""),
        sheet_input(0, 1, "=A1+1"),
        sheet_input(1, 1, "=IF(A2,\"yes\",\"no\")"),
        sheet_input(2, 1, "=ISBLANK(A3)"),
        sheet_input(3, 1, "=A4"),
        sheet_input(4, 1, "=LEN(A5)"),
    ]).unwrap();
    assert_eq!(value(&t.wb, r, 0, 1), "46064");
    assert_eq!(value(&t.wb, r, 1, 0), "TRUE");
    assert_eq!(value(&t.wb, r, 1, 1), "yes");
    // A blank lookup result reads as a reference to an empty cell does: 0, not blank text.
    assert_eq!(value(&t.wb, r, 2, 0), "0");
    assert_eq!(value(&t.wb, r, 3, 1), "0");
    assert_eq!(value(&t.wb, r, 4, 0), "Ann!");
    assert_eq!(value(&t.wb, r, 4, 1), "4");
}

#[test]
fn table_formulas_that_cannot_work_in_a_sheet_are_refused_with_where() {
    let (mut t, r) = report();
    let fails = |wb: &mut Workbook, f: &str| wb.set_sheet_cells(r, &[sheet_input(0, 0, f)]).unwrap_err().to_string();
    let e = fails(&mut t.wb, "=SUMIFS(orders[amount], orders[region], [@region])");
    assert!(e.starts_with("A1: [@region] reads a row of a table, and a sheet cell has no row"), "{e}");
    let e = fails(&mut t.wb, "=SUM(orders[amount]) + SUM(B1:B3)");
    assert!(e.contains("ranges such as Sheet1!A1:B4 are not available") && e.contains("at character 28"), "{e}");
    let e = fails(&mut t.wb, "=SUMIFS(orders[nope], orders[region], \"West\")");
    assert!(e.starts_with("A1: "), "{e}");
    // An unknown table is refused too.
    let e = fails(&mut t.wb, "=SUM(nosuch[x])");
    assert!(e.starts_with("A1: "), "{e}");
    // Nothing may read the link sheet, or claim a slot.
    let e = fails(&mut t.wb, "=_rf_links!$B$1");
    assert!(e.contains("reserved"), "{e}");
    let e = fails(&mut t.wb, "=SUM(_rf_links!A1:A9)");
    assert!(e.contains("reserved"), "{e}");
    assert!(t.wb.with_sheets(|s| s.define_name("x", None, "=_rf_links!$A$1")).unwrap_err().to_string().contains("reserved"));
    // The pre-save check gives the same problem, at the same place.
    let reader = t.wb.reader().unwrap();
    let p = reader.check_sheet_formula(r, 0, 0, "=SUM(orders[amount]) + SUM(B1:B3)").unwrap().unwrap();
    assert_eq!(p.position, 27, "{p:?}");
    assert!(reader.check_sheet_formula(r, 0, 0, "=SUM(orders[amount]) + Report!B1").unwrap().is_none());
    assert!(reader.check_sheet_formula(r, 0, 0, "=SUM(B1:B3)").unwrap().is_none());
    assert!(reader.check_sheet_formula(r, 0, 0, "=SUM(B1:B3").unwrap().is_none(), "the sheet completes one missing )");
}

#[test]
fn circular_references_between_sheets_and_tables_are_refused() {
    let (mut t, r) = report();
    let s = t.wb.with_sheets(|s| s.create("Settings")).unwrap().index;
    t.wb.set_sheet_cells(s, &[sheet_input(0, 1, "0.5")]).unwrap();
    t.wb.add_formula_column_as("orders", "share", "=[@amount]*Settings!$B$1", FormulaLanguage::Sheet).unwrap();
    // Report!B1 reads orders[share], which reads Settings!B1.
    t.wb.set_sheet_cells(r, &[sheet_input(0, 1, "=SUM(orders[share])")]).unwrap();
    assert_eq!(value(&t.wb, r, 0, 1), "92.75");

    // Settings!B1 reading Report!B1 closes the loop, also through other sheet cells.
    let before = fingerprint(&t.wb);
    let e = t.wb.set_sheet_cells(s, &[sheet_input(0, 1, "=Report!B1/1000")]).unwrap_err().to_string();
    assert_eq!(e, "circular reference: Report!B1 depends on its own value through the tables and sheet cells its formula reads");
    assert_eq!(fingerprint(&t.wb), before, "a refused change leaves nothing behind");
    assert_eq!(value(&t.wb, s, 0, 1), "0.5");
    t.wb.set_sheet_cells(s, &[sheet_input(5, 5, "=Report!B1")]).unwrap();
    let e = t.wb.set_sheet_cells(s, &[sheet_input(0, 1, "=SUM(F6:F7)/1000")]).unwrap_err().to_string();
    assert!(e.starts_with("circular reference: Report!B1"), "{e}");
    // A name in between, too.
    t.wb.with_sheets(|sh| sh.define_name("feed", None, "=Report!$B$1")).unwrap();
    let e = t.wb.set_sheet_cells(s, &[sheet_input(0, 1, "=feed/1000")]).unwrap_err().to_string();
    assert!(e.starts_with("circular reference"), "{e}");
    assert_eq!(value(&t.wb, r, 0, 1), "92.75");

    // Two table cells reading each other through sheet cells.
    let e = t.wb.set_sheet_cells(r, &[
        sheet_input(1, 1, "=COUNTIFS(orders[amount], \">\"&C2)"),
        sheet_input(1, 2, "=B3"),
        sheet_input(2, 1, "=SUMIFS(orders[amount], orders[id], C3)"),
        sheet_input(2, 2, "=B2"),
    ]).unwrap_err().to_string();
    assert!(e == "circular reference: Report!B2 → Report!B3 → Report!B2 (through the tables and sheet cells their formulas read)", "{e}");

    // A table formula that would close a loop is refused when it is added.
    let e = t.wb.add_formula_column_as("orders", "back", "=Report!$B$1", FormulaLanguage::Sheet).unwrap_err().to_string();
    assert!(e.contains("circular reference"), "{e}");
}

#[test]
fn a_failed_recalculation_leaves_tables_and_sheets_as_they_were() {
    let (mut t, r) = report();
    t.wb.set_sheet_cells(r, &[sheet_input(0, 0, "=SUM(orders[amount])"), sheet_input(0, 1, "=A1*2")]).unwrap();
    assert_eq!(value(&t.wb, r, 0, 1), "371");
    let before = fingerprint(&t.wb);
    let v = t.wb.open_view(spec("orders")).unwrap();
    crate::faults::fail_next("table_cell");
    assert!(t.wb.edit(v.view_id, &[edit(0, "amount", "900")]).is_err());
    crate::faults::reset();
    assert_eq!(fingerprint(&t.wb), before);
    assert_eq!(value(&t.wb, r, 0, 0), "185.5");
    assert_eq!(value(&t.wb, r, 0, 1), "371");
    // The next change goes through, with sources and derived values in one revision.
    t.wb.edit(v.view_id, &[edit(0, "amount", "900")]).unwrap();
    // 185.5 - 100 + 900, and twice that.
    assert_eq!(value(&t.wb, r, 0, 0), "985.5");
    assert_eq!(value(&t.wb, r, 0, 1), "1971");
}

/// F.10: table results spill into the sheet, follow their table, collide like any spill, are
/// bounded, and export as values. By hand from the `report()` rows.
#[test]
fn table_results_spill_into_sheets() {
    let (mut t, r) = report();
    t.wb.set_sheet_cells(r, &[
        sheet_input(0, 0, "=SORT(UNIQUE(orders[region]))"),
        sheet_input(0, 2, "=FILTER(orders[amount], orders[region]=\"West\")"),
        sheet_input(0, 4, "=TAKE(SORT(orders, 3, -1), 2)"),
        sheet_input(0, 8, "=FILTER(orders[id], orders[amount]>1000, \"none\")"),
        sheet_input(0, 10, "=SUM(C1:C2)"),
    ]).unwrap();
    // East, West, then the blank region last.
    assert_eq!((0..4).map(|k| value(&t.wb, r, k, 0)).collect::<Vec<_>>(), ["East", "West", "", ""]);
    assert_eq!([value(&t.wb, r, 0, 2), value(&t.wb, r, 1, 2), value(&t.wb, r, 2, 2)], ["100", "25.5", ""]);
    // The two largest amounts, whole rows: (1, West, 100, …), (2, East, 50, …).
    assert_eq!([value(&t.wb, r, 0, 4), value(&t.wb, r, 0, 5), value(&t.wb, r, 0, 6), value(&t.wb, r, 1, 5), value(&t.wb, r, 1, 6)], ["1", "West", "100", "East", "50"]);
    assert_eq!(value(&t.wb, r, 0, 8), "none");
    assert_eq!(value(&t.wb, r, 0, 10), "125.5");
    assert_eq!(shown(&t.wb, r, 0, 2).1.as_deref(), Some("=FILTER(orders[amount], orders[region]=\"West\")"));
    // The table changes; so does the result, and what reads it.
    let v = t.wb.open_view(spec("orders")).unwrap();
    t.wb.edit(v.view_id, &[edit(2, "amount", "30")]).unwrap();
    assert_eq!(value(&t.wb, r, 1, 2), "30");
    assert_eq!(value(&t.wb, r, 0, 10), "130");
    // Something in the way: #SPILL!, as for any array.
    t.wb.set_sheet_cells(r, &[sheet_input(1, 2, "x")]).unwrap();
    assert_eq!(value(&t.wb, r, 0, 2), "#SPILL!");
    t.wb.set_sheet_cells(r, &[sheet_input(1, 2, "")]).unwrap();
    assert_eq!(value(&t.wb, r, 1, 2), "30");
    // Too large for a sheet: refused when entered.
    t.wb.create_table_from_query("big", "SELECT i FROM range(10001) t(i)").unwrap();
    let e = t.wb.set_sheet_cells(r, &[sheet_input(20, 0, "=FILTER(big[i], big[i]>=0)")]).unwrap_err().to_string();
    assert!(e.contains("more than 10000 cells") && e.contains("create a table from that view"), "{e}");
    // Reopen, export.
    t.wb.close_view(v.view_id);
    t.reopen();
    assert_eq!(value(&t.wb, r, 1, 0), "West");
    let path = t.dir.path().join("out.xlsx");
    let notes = t.wb.sheets().export_xlsx(&path).unwrap();
    assert!(notes.iter().any(|n| n.starts_with("Report!C1:C2: the table formula =FILTER(orders[amount], orders[region]=\"West\") is exported as its 2×1 values")), "{notes:?}");
    let (sheets, _) = crate::sheets::Sheets::preview_xlsx(&path).unwrap();
    assert_eq!(sheets.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["Report"]);
}

/// F.10: named ranges feed dynamic arrays; renaming the name keeps them working. By hand
/// from A1:A4 = 1, 2, 2, 4.
#[test]
fn named_ranges_feed_dynamic_arrays() {
    let mut t = TempWb::new();
    let s = t.wb.with_sheets(|sh| sh.create("Data")).unwrap().index;
    t.wb.set_sheet_cells(s, &[sheet_input(0, 0, "1"), sheet_input(1, 0, "2"), sheet_input(2, 0, "2"), sheet_input(3, 0, "4")]).unwrap();
    t.wb.with_sheets(|sh| sh.define_name("Sales", None, "=Data!$A$1:$A$4")).unwrap();
    t.wb.set_sheet_cells(s, &[
        sheet_input(0, 2, "=SUM(FILTER(Sales,Sales>1))"),
        sheet_input(1, 2, "=ROWS(UNIQUE(Sales))"),
        sheet_input(0, 4, "=SORT(Sales,1,-1)"),
        sheet_input(0, 6, "=LET(x,MAX(Sales),x*2)"),
    ]).unwrap();
    let col = |wb: &Workbook, c: u32| (0..5).map(|r| cell_text(wb, s, r, c).unwrap_or_default()).collect::<Vec<_>>();
    assert_eq!(cell_text(&t.wb, s, 0, 2).as_deref(), Some("8"));
    assert_eq!(cell_text(&t.wb, s, 1, 2).as_deref(), Some("3"));
    assert_eq!(col(&t.wb, 4), ["4", "2", "2", "1", ""]);
    assert_eq!(cell_text(&t.wb, s, 0, 6).as_deref(), Some("8"));
    t.wb.with_sheets(|sh| sh.update_name("Sales", None, "Units", None, "=Data!$A$1:$A$4")).unwrap();
    t.wb.set_sheet_cells(s, &[sheet_input(3, 0, "10")]).unwrap();
    assert_eq!(col(&t.wb, 4), ["10", "2", "2", "1", ""]);
    assert_eq!(cell_text(&t.wb, s, 0, 2).as_deref(), Some("14"));
    t.reopen();
    assert_eq!(cell_text(&t.wb, s, 0, 6).as_deref(), Some("20"));
}

/// Fill, drag fill and paste copy a table cell as a new table cell whose sheet references
/// moved with it (`$` parts stay), not as a second pointer to the original's result.
#[test]
fn copied_table_cells_read_their_own_rows() {
    use crate::sheets::{CellArea, FillDirection, PasteMode};
    let (mut t, r) = report();
    // Customer ids 1, 2, 1 in A1:A3; B1 looks the first one up, C1 totals West with $A$1.
    t.wb.set_sheet_cells(r, &[
        sheet_input(0, 0, "1"), sheet_input(1, 0, "2"), sheet_input(2, 0, "1"),
        sheet_input(0, 1, "=XLOOKUP(A1, customers[id], customers[name])"),
        sheet_input(0, 3, "West"),
        sheet_input(0, 2, "=SUMIFS(orders[amount], orders[region], $D$1)"),
    ]).unwrap();
    assert_eq!(value(&t.wb, r, 0, 1), "Ann");
    // Fill down B1:B3 (Ctrl+D): Ann, Bob, Ann, each reading its own row.
    t.wb.with_sheets(|s| s.fill(r, CellArea { row: 0, column: 1, rows: 3, columns: 1 }, FillDirection::Down)).unwrap();
    assert_eq!((value(&t.wb, r, 1, 1), value(&t.wb, r, 2, 1)), ("Bob".into(), "Ann".into()));
    assert_eq!(t.wb.sheets().cell_detail(r, 2, 1).unwrap().input, "=XLOOKUP(A3, customers[id], customers[name])");
    // One undo step takes the copies back.
    t.wb.with_sheets(|s| s.undo()).unwrap();
    assert_eq!(value(&t.wb, r, 1, 1), "");
    assert_eq!(value(&t.wb, r, 0, 1), "Ann");
    // Drag fill C1 down to C3: $D$1 stays, so each is 100 + 25.5.
    t.wb.with_sheets(|s| s.drag_fill(r, CellArea { row: 0, column: 2, rows: 1, columns: 1 }, CellArea { row: 0, column: 2, rows: 3, columns: 1 })).unwrap();
    assert_eq!(value(&t.wb, r, 2, 2), "125.5");
    assert_eq!(t.wb.sheets().cell_detail(r, 2, 2).unwrap().input, "=SUMIFS(orders[amount], orders[region], $D$1)");
    // Paste B1 at F2: A1 becomes E2 (empty): no match.
    t.wb.with_sheets(|s| s.paste(r, CellArea { row: 0, column: 1, rows: 1, columns: 1 }, r, (1, 5), PasteMode::All, false)).unwrap();
    assert_eq!(t.wb.sheets().cell_detail(r, 1, 5).unwrap().input, "=XLOOKUP(E2, customers[id], customers[name])");
    assert_eq!(value(&t.wb, r, 1, 5), "#N/A");
    // The copies survive a reopen.
    t.reopen();
    assert_eq!(value(&t.wb, r, 2, 2), "125.5");
    assert_eq!(value(&t.wb, r, 1, 5), "#N/A");
}
