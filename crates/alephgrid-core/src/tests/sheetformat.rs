//! S.01 / S.02 / S.T01: cell styles and number formats on sheets: display against strings
//! worked out by hand, one undo step per change, clearing, and an XLSX round trip.

use super::*;
use crate::sheets::{CellArea, CellStyle, Sheets, StylePatch};

fn sheet() -> Sheets {
    let mut s = Sheets::from_bytes(None).unwrap();
    s.create("S").unwrap();
    s.set(0, &[sheet_input(0, 0, "3.14159"), sheet_input(0, 1, "1234567.891"), sheet_input(1, 0, "0.25"), sheet_input(1, 1, "hello")]).unwrap();
    s
}

fn shown(s: &Sheets, r: u32, c: u32) -> String {
    s.fetch(0, r, 1, c + 1).unwrap().rows[0].get(c as usize).cloned().flatten().map(|x| x.0).unwrap_or_default()
}

fn area(row: u32, column: u32, rows: u32, columns: u32) -> CellArea {
    CellArea { row, column, rows, columns }
}

#[test]
fn styles_apply_over_an_area_and_undo_as_one_step() {
    let mut s = sheet();
    let patch = StylePatch { b: Some(true), i: Some(true), color: Some("#c00000".into()), fill: Some("#FFF2CC".into()), h: Some("center".into()), wrap: Some(true), ..Default::default() };
    s.set_style(0, area(0, 0, 2, 2), &patch).unwrap();
    let expect = CellStyle { b: true, i: true, color: Some("#C00000".into()), fill: Some("#FFF2CC".into()), h: Some("center".into()), wrap: true, ..Default::default() };
    for (r, c) in [(0, 0), (0, 1), (1, 0), (1, 1)] {
        assert_eq!(s.cell_style(0, r, c).unwrap(), expect, "{r},{c}");
    }
    // Fetches carry the styles, empty styled cells included.
    s.set_style(0, area(5, 5, 1, 1), &StylePatch { fill: Some("#00FF00".into()), ..Default::default() }).unwrap();
    let block = s.fetch(0, 0, 10, 10).unwrap();
    assert!(block.styles.iter().any(|x| x.row == 5 && x.column == 5 && x.style.fill.as_deref() == Some("#00FF00")));
    assert_eq!(block.styles.iter().filter(|x| x.style.b).count(), 4);
    // Only named properties change: turning bold off keeps the fill.
    s.set_style(0, area(0, 0, 1, 1), &StylePatch { b: Some(false), ..Default::default() }).unwrap();
    let a1 = s.cell_style(0, 0, 0).unwrap();
    assert!(!a1.b && a1.fill.as_deref() == Some("#FFF2CC"));
    // Each change is one undo step.
    assert!(s.undo().unwrap());
    assert!(s.cell_style(0, 0, 0).unwrap().b);
    assert!(s.undo().unwrap());
    assert!(s.undo().unwrap());
    assert!(s.cell_style(0, 1, 1).unwrap().is_default());
    // Clearing formats keeps values.
    s.set_style(0, area(0, 0, 2, 2), &patch).unwrap();
    s.clear_format(0, area(0, 0, 2, 2)).unwrap();
    assert!(s.cell_style(0, 0, 1).unwrap().is_default());
    assert_eq!(shown(&s, 1, 1), "hello");
    // Bad input is refused before anything changes.
    assert!(s.set_style(0, area(0, 0, 1, 1), &StylePatch { color: Some("red".into()), ..Default::default() }).is_err());
    assert!(s.set_style(0, area(0, 0, 1, 1), &StylePatch::default()).is_err());
    assert!(s.set_style(0, area(0, 0, 2000, 2000), &StylePatch { b: Some(true), ..Default::default() }).is_err());
}

#[test]
fn number_formats_change_the_display_not_the_value() {
    let mut s = sheet();
    let cases = [
        ("0.00", (0, 0), "3.14"),
        ("#,##0.00", (0, 1), "1,234,567.89"),
        ("0%", (1, 0), "25%"),
        ("0.000E+00", (0, 0), "3.142E+00"),
        ("$#,##0", (0, 1), "$1,234,568"),
    ];
    for (fmt, (r, c), want) in cases {
        s.set_style(0, area(r, c, 1, 1), &StylePatch { num_fmt: Some(fmt.into()), ..Default::default() }).unwrap();
        assert_eq!(shown(&s, r, c), want, "{fmt}");
        assert_eq!(s.cell_style(0, r, c).unwrap().num_fmt.as_deref(), Some(fmt));
    }
    // The value is exact: a formula reading it sees all digits.
    // (A formula takes the format of what it reads, as in other spreadsheets; give it its own.)
    s.set(0, &[sheet_input(3, 0, "=A1*100000")]).unwrap();
    s.set_style(0, area(3, 0, 1, 1), &StylePatch { num_fmt: Some("0".into()), ..Default::default() }).unwrap();
    assert_eq!(shown(&s, 3, 0), "314159");
    // Back to general.
    s.set_style(0, area(0, 0, 1, 1), &StylePatch { num_fmt: Some(String::new()), ..Default::default() }).unwrap();
    assert_eq!(shown(&s, 0, 0), "3.14159");
}

#[test]
fn styles_and_formats_survive_an_xlsx_round_trip() {
    let mut s = sheet();
    let patch = StylePatch { b: Some(true), u: Some(true), s: Some(true), color: Some("#1F4E79".into()), fill: Some("#DDEBF7".into()), size: Some(16), h: Some("right".into()), v: Some("top".into()), wrap: Some(true), num_fmt: Some("0.0".into()), ..Default::default() };
    s.set_style(0, area(0, 0, 1, 1), &patch).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("styles.xlsx");
    s.export_xlsx(&path).unwrap();
    let back = Sheets::preview_xlsx(&path).map(|_| ()).and_then(|_| {
        let mut t = Sheets::from_bytes(None)?;
        t.import_xlsx(&path)?;
        Ok(t)
    }).unwrap();
    let st = back.cell_style(0, 0, 0).unwrap();
    assert_eq!(st, CellStyle { b: true, u: true, s: true, color: Some("#1F4E79".into()), fill: Some("#DDEBF7".into()), size: Some(16), h: Some("right".into()), v: Some("top".into()), wrap: true, num_fmt: Some("0.0".into()), i: false, borders: None });
    assert_eq!(shown(&back, 0, 0), "3.1");
}

/// S.04: find in values or formulas, case and whole-cell options, and replace as one step.
#[test]
fn sheet_find_and_replace() {
    use crate::sheets::SheetSearch;
    let mut s = Sheets::from_bytes(None).unwrap();
    s.create("S").unwrap();
    s.set(0, &[sheet_input(0, 0, "North"), sheet_input(0, 2, "north pole"), sheet_input(2, 1, "=\"nor\"&\"th\""), sheet_input(3, 0, "south")]).unwrap();
    let q = |text: &str, match_case: bool, whole: bool, formulas: bool| SheetSearch { text: text.into(), match_case, whole, formulas };
    let r = s.find(0, &q("north", false, false, false), 0, false, true).unwrap();
    // Values: A1, C1 and B3's result.
    assert_eq!(r.matches, Some(3));
    assert_eq!(r.hits, vec![crate::FindHit { row: 0, columns: vec![0, 2] }, crate::FindHit { row: 2, columns: vec![1] }]);
    assert_eq!(s.find(0, &q("North", true, false, false), 0, false, true).unwrap().matches, Some(1));
    assert_eq!(s.find(0, &q("north", false, true, false), 0, false, true).unwrap().matches, Some(2));
    // In formulas the formula's text is searched instead.
    assert_eq!(s.find(0, &q("nor\"", false, false, true), 0, false, true).unwrap().matches, Some(1));
    assert_eq!(s.find(0, &q("north", false, false, false), 1, true, false).unwrap().hits[0].row, 0);
    // Replace in values: formulas are left alone; one undo step takes it all back.
    let n = s.replace_all(0, &q("north", false, false, false), "east").unwrap();
    assert_eq!(n, 2);
    assert_eq!(shown(&s, 0, 0), "east");
    assert_eq!(shown(&s, 0, 2), "east pole");
    assert_eq!(shown(&s, 2, 1), "north");
    assert!(s.undo().unwrap());
    assert_eq!(shown(&s, 0, 0), "North");
    assert_eq!(shown(&s, 0, 2), "north pole");
}

/// S.05: frozen columns are kept with the sheet, undo, and survive XLSX.
#[test]
fn frozen_columns_persist_and_undo() {
    let mut s = sheet();
    s.freeze_columns(0, 2).unwrap();
    assert_eq!(s.info(0).unwrap().frozen_columns, 2);
    let back = Sheets::from_bytes(Some(s.to_bytes().unwrap())).unwrap();
    assert_eq!(back.info(0).unwrap().frozen_columns, 2);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("f.xlsx");
    s.export_xlsx(&path).unwrap();
    let mut t = Sheets::from_bytes(None).unwrap();
    t.import_xlsx(&path).unwrap();
    assert_eq!(t.info(0).unwrap().frozen_columns, 2);
    assert!(s.undo().unwrap());
    assert_eq!(s.info(0).unwrap().frozen_columns, 0);
    assert!(s.freeze_columns(0, 51).is_err());
}

/// S.07: duplicating a sheet copies its cells and formats; tab colours are kept and undo.
#[test]
fn sheets_duplicate_and_take_tab_colours() {
    let mut s = sheet();
    s.set_style(0, area(0, 0, 1, 1), &StylePatch { b: Some(true), ..Default::default() }).unwrap();
    let copy = s.duplicate(0).unwrap();
    assert_ne!(copy.name, "S");
    assert_eq!(shown(&s, 0, 0), "3.14159");
    let b = s.fetch(copy.index, 0, 1, 2).unwrap();
    assert_eq!(b.rows[0][0].clone().map(|c| c.0).as_deref(), Some("3.14159"));
    assert!(b.styles.iter().any(|x| x.row == 0 && x.column == 0 && x.style.b));
    s.set_tab_color(0, "#2e75b6").unwrap();
    assert_eq!(s.info(0).unwrap().color.as_deref(), Some("#2E75B6"));
    assert!(s.set_tab_color(0, "blue").is_err());
    assert!(s.undo().unwrap());
    assert_eq!(s.info(0).unwrap().color, None);
}

/// S.06: a range sorts by its columns as one undo step, with a header row kept; numbers
/// before text, empty cells last; ranges with formulas are refused.
#[test]
fn ranges_sort_by_columns() {
    use crate::sheets::SortBy;
    let mut s = Sheets::from_bytes(None).unwrap();
    s.create("S").unwrap();
    let rows = [("name", "n"), ("b", "10"), ("a", "9"), ("c", ""), ("a", "2")];
    let inputs: Vec<SheetInput> = rows.iter().enumerate().flat_map(|(i, (x, y))| [sheet_input(i as u32, 0, x), sheet_input(i as u32, 1, y)]).collect();
    s.set(0, &inputs).unwrap();
    s.sort_range(0, area(0, 0, 5, 2), &[SortBy { column: 0, desc: false }, SortBy { column: 1, desc: true }], true).unwrap();
    let col = |s: &Sheets, c: u32| (0..5).map(|r| shown(s, r, c)).collect::<Vec<_>>();
    assert_eq!(col(&s, 0), vec!["name", "a", "a", "b", "c"]);
    assert_eq!(col(&s, 1), vec!["n", "9", "2", "10", ""]);
    assert!(s.undo().unwrap());
    assert_eq!(col(&s, 0), vec!["name", "b", "a", "c", "a"]);
    s.set(0, &[sheet_input(1, 1, "=1+1")]).unwrap();
    assert!(s.sort_range(0, area(0, 0, 5, 2), &[SortBy { column: 1, desc: false }], true).is_err());
}

/// S.01: borders on an area, one undo step, kept through XLSX.
#[test]
fn borders_apply_and_round_trip() {
    let mut s = sheet();
    s.set_border(0, area(0, 0, 2, 2), "outer", "thin").unwrap();
    let st = |s: &Sheets| s.engine_style(0, 0, 0).unwrap();
    assert!(st(&s).border.top.is_some() && st(&s).border.left.is_some());
    assert!(s.set_border(0, area(0, 0, 1, 1), "diagonal", "thin").is_err());
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("b.xlsx");
    s.export_xlsx(&path).unwrap();
    let mut t = Sheets::from_bytes(None).unwrap();
    t.import_xlsx(&path).unwrap();
    assert!(st(&t).border.top.is_some());
    assert!(s.undo().unwrap());
    assert!(st(&s).border.top.is_none());
}

/// S.07: hiding and showing sheets; the last visible sheet stays; the link sheet is never listed.
#[test]
fn sheets_hide_and_unhide() {
    let mut s = sheet();
    assert!(s.hide_sheet(0).is_err(), "the last visible sheet stays");
    s.create("T").unwrap();
    s.hide_sheet(0).unwrap();
    assert_eq!(s.list().iter().map(|x| x.name.clone()).collect::<Vec<_>>(), vec!["T"]);
    assert_eq!(s.hidden_sheets(), vec!["S".to_string()]);
    assert!(s.unhide_sheet(crate::sheets::LINK_SHEET).is_err());
    s.unhide_sheet("S").unwrap();
    assert_eq!(s.list().len(), 2);
    assert!(s.hidden_sheets().is_empty());
}
