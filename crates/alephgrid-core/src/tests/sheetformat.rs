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
    assert_eq!(st, CellStyle { b: true, u: true, s: true, color: Some("#1F4E79".into()), fill: Some("#DDEBF7".into()), size: Some(16), h: Some("right".into()), v: Some("top".into()), wrap: true, num_fmt: Some("0.0".into()), i: false });
    assert_eq!(shown(&back, 0, 0), "3.1");
}
