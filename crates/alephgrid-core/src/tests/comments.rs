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
