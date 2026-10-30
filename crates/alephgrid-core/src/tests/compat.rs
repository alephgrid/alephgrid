//! F.13: XLSX compatibility notes, and the published sample workbook.
use super::*;

#[test]
fn xlsx_downloads_say_what_they_leave_out_or_replace() {
    let mut t = TempWb::new();
    t.wb.create_table_from_query("orders", "SELECT * FROM (VALUES (1, 100.0), (2, 50.0)) v(id, amount)").unwrap();
    let s = t.wb.with_sheets(|sh| sh.create("Report")).unwrap().index;
    let catalog: serde_json::Value = serde_json::from_str(include_str!("../../data/function_catalog.json")).unwrap();
    // A function the sheet engine knows that Excel does not (by the catalog's sources).
    let foreign = catalog["functions"].as_array().unwrap().iter()
        .find(|f| f["sheet_status"] == "registered" && !f["sources"].as_array().unwrap().iter().any(|x| x == "excel"))
        .map(|f| f["name"].as_str().unwrap().to_string());
    let mut inputs = vec![sheet_input(0, 1, "=SUM(orders[amount])"), sheet_input(1, 0, "=SORT(A10:A12)"), sheet_input(9, 0, "3"), sheet_input(10, 0, "1"), sheet_input(11, 0, "2")];
    if let Some(f) = &foreign {
        inputs.push(sheet_input(20, 0, &format!("={f}(1)")));
    }
    t.wb.set_sheet_cells(s, &inputs).unwrap();
    let notes = t.wb.reader().unwrap().xlsx_compatibility().unwrap();
    let has = |needle: &str| notes.iter().any(|n| n.contains(needle));
    assert!(has("Report!B1: the table formula =SUM(orders[amount]) is exported as its value \"150\""), "{notes:?}");
    assert!(has("Report!A2: =SORT(A10:A12) is a dynamic array formula"), "{notes:?}");
    assert!(has("table orders (2 rows) is not in the file"), "{notes:?}");
    if let Some(f) = foreign {
        assert!(has(&format!("Report!A21: {f} is not an Excel function")), "{notes:?}");
    }
    // Nothing is written where the user would see it.
    assert!(t.wb.sheets().list().iter().all(|i| i.name == "Report"));
}

/// The sample workbook (docs/samples/analyst-sample.xlsx): sheets only, so it opens anywhere.
/// Regenerate with `cargo test -p alephgrid-core --lib write_sample_workbook -- --ignored`.
fn sample() -> Vec<SheetInput> {
    let mut v = vec![sheet_input(0, 0, "order"), sheet_input(0, 1, "customer"), sheet_input(0, 2, "amount"), sheet_input(0, 4, "id"), sheet_input(0, 5, "region")];
    for (r, (o, c, a)) in [(1, 10, 120.0), (2, 11, 80.0), (3, 10, 40.5), (4, 12, 15.0)].iter().enumerate() {
        v.extend([sheet_input(r as u32 + 1, 0, &o.to_string()), sheet_input(r as u32 + 1, 1, &c.to_string()), sheet_input(r as u32 + 1, 2, &a.to_string())]);
    }
    for (r, (id, region)) in [(10, "West"), (11, "East"), (12, "West")].iter().enumerate() {
        v.extend([sheet_input(r as u32 + 1, 4, &id.to_string()), sheet_input(r as u32 + 1, 5, region)]);
    }
    v.extend([
        sheet_input(0, 3, "region"),
        sheet_input(1, 3, "=XLOOKUP(B2,$E$2:$E$4,$F$2:$F$4,\"none\")"),
        sheet_input(2, 3, "=XLOOKUP(B3,$E$2:$E$4,$F$2:$F$4,\"none\")"),
        sheet_input(3, 3, "=XLOOKUP(B4,$E$2:$E$4,$F$2:$F$4,\"none\")"),
        sheet_input(4, 3, "=XLOOKUP(B5,$E$2:$E$4,$F$2:$F$4,\"none\")"),
        sheet_input(6, 0, "West total"), sheet_input(6, 1, "=SUMIFS(C2:C5,D2:D5,\"West\")"),
        sheet_input(7, 0, "Average order"), sheet_input(7, 1, "=LET(t,SUM(C2:C5),t/COUNT(C2:C5))"),
    ]);
    v
}

#[test]
#[ignore]
fn write_sample_workbook() {
    let mut t = TempWb::new();
    t.wb.with_sheets(|s| { s.create("Orders")?; s.set(0, &sample()) }).unwrap();
    let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/samples/analyst-sample.xlsx");
    t.wb.sheets().export_xlsx(&out).unwrap();
}

/// The committed sample opens with these values, worked out by hand: customers 10, 11, 10, 12
/// are West, East, West, West; West orders 120 + 40.5 + 15 = 175.5; the average of 255.5 over
/// four orders is 63.875.
#[test]
fn the_published_sample_workbook_opens_with_its_documented_values() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/samples/analyst-sample.xlsx");
    let mut t = TempWb::new();
    let Imported::Sheets { .. } = t.wb.import_any(&path, None).unwrap() else { panic!("sheets expected") };
    let cell = |r, c| cell_text(&t.wb, 0, r, c).unwrap_or_default();
    assert_eq!([cell(1, 3), cell(2, 3), cell(3, 3), cell(4, 3)], ["West", "East", "West", "West"]);
    assert_eq!(cell(6, 1), "175.5");
    assert_eq!(cell(7, 1), "63.875");
    // It is reproducible: the generator's inputs are what the file holds.
    for i in sample() {
        let detail = t.wb.sheets().cell_detail(0, i.row, i.column).unwrap();
        assert_eq!(detail.input, i.value, "{}{}", crate::sheets::column_name(i.column), i.row + 1);
    }
}
