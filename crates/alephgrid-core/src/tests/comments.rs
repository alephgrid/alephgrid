//! S.11: comments on sheet cells: threads, edit, resolve, delete, persistence across
//! reopening, and anchors that follow row and column inserts and deletes.

use super::*;
use crate::comments::Axis;

#[test]
fn comments_thread_and_follow_their_cells() {
    let mut t = TempWb::new();
    t.wb.with_sheets(|s| s.create("S").map(|_| ())).unwrap();
    let a = t.wb.acting_as("ann@example.com", |wb| wb.add_comment(0, 2, 1, "Is this right?", None)).unwrap();
    assert_eq!((a.row, a.column, a.author.as_str()), (2, 1, "ann@example.com"));
    let r = t.wb.add_comment(0, 2, 1, "Yes", Some(&a.id)).unwrap();
    assert_eq!(r.parent.as_deref(), Some(a.id.as_str()));
    assert!(t.wb.add_comment(0, 0, 0, "  ", None).is_err());
    assert!(t.wb.add_comment(0, 0, 0, "x", Some("nope")).is_err());
    t.wb.edit_comment(&a.id, "Is this total right?").unwrap();
    t.wb.resolve_comment(&a.id, true).unwrap();
    let all = t.wb.comments(0).unwrap();
    assert_eq!(all.len(), 2);
    assert!(all.iter().all(|c| c.resolved));
    assert_eq!(all[0].text, "Is this total right?");
    assert!(all[0].edited_ms.is_some());
    // Rows inserted above move the thread down; columns deleted before it move it left.
    t.wb.with_sheets(|s| s.insert_rows(0, 0, 3)).unwrap();
    t.wb.shift_comments(0, Axis::Rows, 0, 3).unwrap();
    t.wb.shift_comments(0, Axis::Columns, 0, -1).unwrap();
    let all = t.wb.comments(0).unwrap();
    assert!(all.iter().all(|c| (c.row, c.column) == (5, 0)), "{all:?}");
    // Kept across reopening.
    t.reopen();
    assert_eq!(t.wb.comments(0).unwrap().len(), 2);
    // Deleting the row deletes the comments on it; deleting a thread deletes its replies.
    let b = t.wb.add_comment(0, 9, 9, "Later", None).unwrap();
    t.wb.shift_comments(0, Axis::Rows, 5, -1).unwrap();
    assert_eq!(t.wb.comments(0).unwrap().iter().map(|c| c.id.clone()).collect::<Vec<_>>(), vec![b.id.clone()]);
    assert_eq!(t.wb.comments(0).unwrap()[0].row, 8);
    t.wb.delete_comment(&b.id).unwrap();
    assert!(t.wb.comments(0).unwrap().is_empty());
}

/// S.10: validation rules refuse values outside the list or range, before anything is
/// written; rules are kept with the sheet.
#[test]
fn sheet_validation_refuses_bad_inputs() {
    use crate::sheetrules::{SheetColorRule, SheetRules, SheetValidation};
    use crate::sheets::CellArea;
    let mut t = TempWb::new();
    t.wb.with_sheets(|s| s.create("S").map(|_| ())).unwrap();
    let area = CellArea { row: 0, column: 0, rows: 10, columns: 1 };
    let rules = SheetRules {
        colors: vec![SheetColorRule { area, condition: "greater".into(), value: "5".into(), color: "#FFC7CE".into() }],
        validations: vec![
            SheetValidation { area, list: vec!["yes".into(), "no".into()], min: None, max: None },
            SheetValidation { area: CellArea { row: 0, column: 1, rows: 10, columns: 1 }, list: vec![], min: Some(0.0), max: Some(100.0) },
        ],
    };
    t.wb.set_sheet_rules(0, rules).unwrap();
    assert!(t.wb.check_sheet_inputs(0, &[sheet_input(0, 0, "yes")]).is_ok());
    assert!(t.wb.check_sheet_inputs(0, &[sheet_input(0, 0, "maybe")]).unwrap_err().to_string().contains("allowed values"));
    assert!(t.wb.check_sheet_inputs(0, &[sheet_input(1, 1, "101")]).is_err());
    assert!(t.wb.check_sheet_inputs(0, &[sheet_input(1, 1, "abc")]).is_err());
    assert!(t.wb.check_sheet_inputs(0, &[sheet_input(1, 1, "=A1"), sheet_input(20, 0, "anything")]).is_ok());
    t.reopen();
    assert_eq!(t.wb.sheet_rules(0).unwrap().validations.len(), 2);
    let bad = SheetRules { colors: vec![SheetColorRule { area, condition: "huge".into(), value: String::new(), color: "#000000".into() }], validations: vec![] };
    assert!(t.wb.set_sheet_rules(0, bad).is_err());
}

/// S.11: adding a comment is a change in the workbook's history.
#[test]
fn comments_are_in_the_history() {
    let mut t = TempWb::new();
    t.wb.with_sheets(|s| s.create("S").map(|_| ())).unwrap();
    let before = t.wb.revision().unwrap();
    t.wb.add_comment(0, 0, 0, "note", None).unwrap();
    assert!(t.wb.revision().unwrap() > before);
}

/// S.09: charts placed on a sheet are kept with the workbook (by the sheet's stable id, so
/// they follow renames), validated, and listed in the XLSX compatibility notes.
#[test]
fn sheet_charts_are_kept_and_noted_for_xlsx() {
    use crate::sheetcharts::SheetChart;
    use crate::sheets::CellArea;
    let mut t = TempWb::new();
    t.wb.with_sheets(|s| s.create("S").map(|_| ())).unwrap();
    let chart = SheetChart {
        chart: "c1".into(), kind: "line".into(), title: "Sales".into(),
        data: CellArea { row: 0, column: 0, rows: 4, columns: 3 }, row: 6, column: 4, width: 420, height: 260,
    };
    t.wb.set_sheet_charts(0, vec![chart.clone()]).unwrap();
    t.wb.with_sheets(|s| s.rename(0, "Renamed")).unwrap();
    t.reopen();
    assert_eq!(t.wb.sheet_charts(0).unwrap(), vec![chart.clone()]);
    let notes = t.wb.reader().unwrap().xlsx_compatibility().unwrap();
    assert!(notes.iter().any(|n| n.contains("sheet Renamed: 1 chart(s) are not in the file")), "{notes:?}");
    let bad = |f: fn(&mut SheetChart)| { let mut c = chart.clone(); f(&mut c); c };
    assert!(t.wb.set_sheet_charts(0, vec![bad(|c| c.kind = "radar".into())]).is_err());
    assert!(t.wb.set_sheet_charts(0, vec![bad(|c| c.data.columns = 1)]).is_err());
    assert!(t.wb.set_sheet_charts(0, vec![bad(|c| c.width = 10)]).is_err());
    assert!(t.wb.set_sheet_charts(0, vec![chart.clone(), chart.clone()]).is_err(), "ids are unique");
    t.wb.set_sheet_charts(0, vec![]).unwrap();
    assert!(t.wb.sheet_charts(0).unwrap().is_empty());
}
