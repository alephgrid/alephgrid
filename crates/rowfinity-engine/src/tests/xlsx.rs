//! 2.03: XLSX round trips and compatibility warnings.

use super::*;
use std::io::{Read, Write};

/// Every non-empty cell of every sheet: (sheet, row, col) -> (display, align, input).
fn snapshot(wb: &Workbook) -> Vec<(String, u32, u32, String, String, Option<String>)> {
    let mut out = Vec::new();
    for info in wb.sheets().list() {
        let block = wb.sheets().fetch(info.index, 0, info.rows.max(1), info.columns.max(1)).unwrap();
        for (r, row) in block.rows.iter().enumerate() {
            for (c, cell) in row.iter().enumerate() {
                if let Some(cell) = cell {
                    out.push((info.name.clone(), r as u32, c as u32, cell.0.clone(), format!("{:?}", cell.1), cell.2.clone()));
                }
            }
        }
    }
    out
}

fn rich_workbook() -> Workbook {
    let mut wb = Workbook::open_in_memory().unwrap();
    wb.with_sheets(|s| {
        let a = s.create("Inputs")?.index;
        s.set(a, &[
            sheet_input(0, 0, "label"), sheet_input(0, 1, "value"),
            sheet_input(1, 0, "integer"), sheet_input(1, 1, "42"),
            sheet_input(2, 0, "decimal"), sheet_input(2, 1, "3.14159"),
            sheet_input(3, 0, "percent"), sheet_input(3, 1, "12.5%"),
            sheet_input(4, 0, "date"), sheet_input(4, 1, "2024-02-29"),
            sheet_input(5, 0, "bool"), sheet_input(5, 1, "TRUE"),
            sheet_input(6, 0, "text number"), sheet_input(6, 1, "'00123"),
            sheet_input(7, 0, "unicode"), sheet_input(7, 1, "東京 🚀 \"q\""),
            sheet_input(8, 0, "error"), sheet_input(8, 1, "=1/0"),
            sheet_input(9, 0, "sum"), sheet_input(9, 1, "=SUM(B2:B3)"),
        ])?;
        let b = s.create("Report")?.index;
        s.set(b, &[
            sheet_input(0, 0, "=Inputs!B2*2"),
            sheet_input(1, 0, "=IF(Inputs!B6,\"yes\",\"no\")"),
            sheet_input(2, 0, "=XLOOKUP(\"decimal\",Inputs!A2:A11,Inputs!B2:B11)"),
            sheet_input(3, 0, "=ROUND(Inputs!B3,2)&\" \"&Inputs!A2"),
        ])
    })
    .unwrap();
    wb
}

#[test]
fn xlsx_round_trip_keeps_values_formulas_and_types() {
    let dir = tempfile::tempdir().unwrap();
    let wb = rich_workbook();
    let before = snapshot(&wb);
    assert!(before.len() >= 24, "fixture should be non-trivial: {}", before.len());
    let path = dir.path().join("rich.xlsx");
    wb.sheets().export_xlsx(&path).unwrap();

    // Into an empty workbook: the model is adopted, every cell matches exactly.
    let mut fresh = Workbook::open_in_memory().unwrap();
    let Imported::Sheets { sheets, warnings } = fresh.import_any(&path, None).unwrap() else { panic!("sheets expected") };
    assert_eq!(sheets.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["Inputs", "Report"]);
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(snapshot(&fresh), before);
    // Independent spot checks of computed values.
    assert_eq!(cell_text(&fresh, 1, 0, 0).as_deref(), Some("84"));
    assert_eq!(cell_text(&fresh, 1, 1, 0).as_deref(), Some("yes"));
    assert_eq!(cell_text(&fresh, 1, 3, 0).as_deref(), Some("3.14 integer"));
    assert_eq!(cell_text(&fresh, 0, 8, 1).as_deref(), Some("#DIV/0!"));
    // Second generation: export the import again; nothing drifts.
    let again = dir.path().join("again.xlsx");
    fresh.sheets().export_xlsx(&again).unwrap();
    let mut third = Workbook::open_in_memory().unwrap();
    third.import_any(&again, None).unwrap();
    assert_eq!(snapshot(&third), before);
}

#[test]
fn importing_next_to_existing_sheets_copies_inputs_and_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rich.xlsx");
    rich_workbook().sheets().export_xlsx(&path).unwrap();
    let mut wb = Workbook::open_in_memory().unwrap();
    wb.with_sheets(|s| s.create("Inputs")).unwrap();
    let Imported::Sheets { sheets, warnings } = wb.import_any(&path, None).unwrap() else { panic!() };
    assert_eq!(sheets.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["Inputs 2", "Report"]);
    assert!(warnings.iter().any(|w| w.contains("formatting")), "{warnings:?}");
    // Formulas follow the renamed copy, not the unrelated sheet that had the name.
    let report = wb.sheets().list().into_iter().find(|s| s.name == "Report").unwrap().index;
    let block = wb.sheets().fetch(report, 0, 4, 1).unwrap();
    assert_eq!(block.rows[0][0].as_ref().unwrap().0, "84");
    assert_eq!(block.rows[0][0].as_ref().unwrap().2.as_deref(), Some("='Inputs 2'!B2*2"));
    assert_eq!(block.rows[1][0].as_ref().unwrap().0, "yes");
    assert_eq!(block.rows[2][0].as_ref().unwrap().0, "3.14159");
    assert_eq!(wb.sheets().list().len(), 3);
    // The whole import is one undo step.
    assert!(wb.with_sheets(|s| s.undo()).unwrap());
    assert_eq!(wb.sheets().list().len(), 1);
}

/// Re-pack an .xlsx with extra package parts, as Excel writes for charts, macros and so on.
fn with_parts(xlsx: &[u8], extra: &[&str]) -> Vec<u8> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(xlsx)).unwrap();
    let mut out = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for i in 0..archive.len() {
        let mut f = archive.by_index(i).unwrap();
        let name = f.name().unwrap().into_owned();
        let mut data = Vec::new();
        f.read_to_end(&mut data).unwrap();
        out.start_file(name, zip::write::SimpleFileOptions::default()).unwrap();
        out.write_all(&data).unwrap();
    }
    for part in extra {
        out.start_file(*part, zip::write::SimpleFileOptions::default()).unwrap();
        out.write_all(b"<x/>").unwrap();
    }
    out.finish().unwrap().into_inner()
}

#[test]
fn unsupported_parts_are_reported() {
    let dir = tempfile::tempdir().unwrap();
    let plain = dir.path().join("plain.xlsx");
    rich_workbook().sheets().export_xlsx(&plain).unwrap();
    let bytes = with_parts(
        &std::fs::read(&plain).unwrap(),
        &["xl/charts/chart1.xml", "xl/vbaProject.bin", "xl/drawings/drawing1.xml", "xl/comments1.xml"],
    );
    let path = dir.path().join("extras.xlsx");
    std::fs::write(&path, bytes).unwrap();
    let mut wb = Workbook::open_in_memory().unwrap();
    let Imported::Sheets { warnings, .. } = wb.import_any(&path, None).unwrap() else { panic!() };
    let joined = warnings.join("\n");
    for expected in ["Charts", "Macros", "Pictures", "Comments"] {
        assert!(joined.contains(expected), "missing {expected}: {joined}");
    }
    assert!(!joined.contains("Pivot"), "only parts that exist are reported: {joined}");
    // The cells still came across.
    assert_eq!(cell_text(&wb, 0, 1, 1).as_deref(), Some("42"));
}

#[test]
fn unknown_functions_are_counted_and_named() {
    let dir = tempfile::tempdir().unwrap();
    let mut wb = Workbook::open_in_memory().unwrap();
    wb.with_sheets(|s| {
        let i = s.create("S")?.index;
        s.set(i, &[
            sheet_input(0, 0, "=FOOBAR(1)+SUM(1)"),
            sheet_input(1, 0, "=NOTAFUNC(\"BAZ(\")"),
            sheet_input(2, 0, "=SUM(1)"),
        ])
    })
    .unwrap();
    assert_eq!(cell_text(&wb, 0, 0, 0).as_deref(), Some("#NAME?"));
    let path = dir.path().join("names.xlsx");
    wb.sheets().export_xlsx(&path).unwrap();
    let mut other = Workbook::open_in_memory().unwrap();
    let Imported::Sheets { warnings, .. } = other.import_any(&path, None).unwrap() else { panic!() };
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].starts_with("2 cells show #NAME?"), "{}", warnings[0]);
    assert!(warnings[0].contains("FOOBAR") && warnings[0].contains("NOTAFUNC"), "{}", warnings[0]);
    assert!(!warnings[0].contains("BAZ"), "names inside strings are ignored: {}", warnings[0]);
}

#[test]
fn corrupt_xlsx_fails_cleanly() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bad.xlsx");
    std::fs::write(&path, b"PK\x03\x04 not really a zip").unwrap();
    let mut wb = Workbook::open_in_memory().unwrap();
    wb.with_sheets(|s| s.create("Keep")).unwrap();
    assert!(wb.import_any(&path, None).is_err());
    assert_eq!(wb.sheets().list().len(), 1, "a failed import adds nothing");
}
