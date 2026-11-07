//! S.11 / S.T02: comments stay with their cells through cut and paste, and through undo and
//! redo of the row and column changes that moved them. Expected anchors are written out by
//! hand from the cells each test creates.

mod common;

use common::*;
use serde_json::{json, Value};

async fn anchors(ws: &mut Ws) -> Vec<(String, u64, u64)> {
    let list = ws.ok("comments", json!({ "sheet": 0 })).await;
    let mut out: Vec<(String, u64, u64)> = list.as_array().unwrap().iter()
        .map(|c| (c["text"].as_str().unwrap().to_string(), c["row"].as_u64().unwrap(), c["column"].as_u64().unwrap()))
        .collect();
    out.sort();
    out
}

fn a(text: &str, row: u64, column: u64) -> (String, u64, u64) {
    (text.to_string(), row, column)
}

#[tokio::test(flavor = "multi_thread")]
async fn comments_follow_moves_and_undo_of_structural_changes() {
    let dir = tempfile::tempdir().unwrap();
    let server = TestServer::start(dir.path()).await;
    let wb = server.create_workbook("comments").await;
    let mut ws = server.ws(&wb).await;
    ws.ok("sheet_create", json!({ "name": "S" })).await;
    ws.ok("sheet_set", json!({ "sheet": 0, "inputs": [{ "row": 1, "column": 1, "value": "b2" }, { "row": 4, "column": 0, "value": "a5" }] })).await;
    ws.ok("add_comment", json!({ "sheet": 0, "row": 1, "column": 1, "text": "on B2" })).await;
    ws.ok("add_comment", json!({ "sheet": 0, "row": 4, "column": 0, "text": "on A5" })).await;
    let layout = |v: &Value| v.as_array().unwrap()[0]["layout"].as_u64().unwrap();

    // Deleting row 2 removes B2's comment and lifts A5's to A4; undo brings both back.
    let l = layout(&ws.ok("sheets", json!({})).await);
    ws.ok("sheet_delete_rows", json!({ "sheet": 0, "at": 1, "count": 1, "layout": l })).await;
    assert_eq!(anchors(&mut ws).await, vec![a("on A5", 3, 0)]);
    ws.ok("sheet_undo", json!({})).await;
    assert_eq!(anchors(&mut ws).await, vec![a("on A5", 4, 0), a("on B2", 1, 1)]);
    ws.ok("sheet_redo", json!({})).await;
    assert_eq!(anchors(&mut ws).await, vec![a("on A5", 3, 0)]);
    ws.ok("sheet_undo", json!({})).await;

    // Inserting two columns before B moves B2's comment to D2; undo moves it back.
    let l = layout(&ws.ok("sheets", json!({})).await);
    ws.ok("sheet_insert_columns", json!({ "sheet": 0, "at": 1, "count": 2, "layout": l })).await;
    assert_eq!(anchors(&mut ws).await, vec![a("on A5", 4, 0), a("on B2", 1, 3)]);
    ws.ok("sheet_undo", json!({})).await;
    assert_eq!(anchors(&mut ws).await, vec![a("on A5", 4, 0), a("on B2", 1, 1)]);

    // Cutting B2 and pasting it at E10 takes its comment along; undo takes it back.
    let l = layout(&ws.ok("sheets", json!({})).await);
    ws.ok("sheet_move", json!({ "sheet": 0, "source": { "row": 1, "column": 1, "rows": 1, "columns": 1 }, "row": 9, "column": 4, "layout": l })).await;
    assert_eq!(anchors(&mut ws).await, vec![a("on A5", 4, 0), a("on B2", 9, 4)]);
    ws.ok("sheet_undo", json!({})).await;
    assert_eq!(anchors(&mut ws).await, vec![a("on A5", 4, 0), a("on B2", 1, 1)]);

    // Moving onto a commented cell removes the comment of the cell it covers.
    let l = layout(&ws.ok("sheets", json!({})).await);
    ws.ok("sheet_move", json!({ "sheet": 0, "source": { "row": 1, "column": 1, "rows": 1, "columns": 1 }, "row": 4, "column": 0, "layout": l })).await;
    assert_eq!(anchors(&mut ws).await, vec![a("on B2", 4, 0)]);
    ws.ok("sheet_undo", json!({})).await;
    assert_eq!(anchors(&mut ws).await, vec![a("on A5", 4, 0), a("on B2", 1, 1)]);
    server.stop().await;
}

/// F.04b/d, U.11a: a cut pasted on another sheet is one operation and one undo step: the
/// cells, a formula elsewhere that reads them and their comment move to the other sheet,
/// and the comment of the cell they cover goes with its value. Refused cuts (a stale
/// layout of either sheet, hidden rows, too many cells, a read-only viewer is covered in
/// teams.rs) leave both sheets as they were. Expected cells and anchors written by hand.
#[tokio::test(flavor = "multi_thread")]
async fn cut_to_another_sheet_moves_cells_comments_and_references() {
    let dir = tempfile::tempdir().unwrap();
    let server = TestServer::start(dir.path()).await;
    let wb = server.create_workbook("cross").await;
    let mut ws = server.ws(&wb).await;
    ws.ok("sheet_create", json!({ "name": "S" })).await;
    ws.ok("sheet_create", json!({ "name": "T" })).await;
    ws.ok("sheet_set", json!({ "sheet": 0, "inputs": [{ "row": 1, "column": 1, "value": "7" }, { "row": 0, "column": 2, "value": "=B2*3" }] })).await;
    ws.ok("sheet_set", json!({ "sheet": 1, "inputs": [{ "row": 0, "column": 0, "value": "old" }] })).await;
    ws.ok("add_comment", json!({ "sheet": 0, "row": 1, "column": 1, "text": "on S!B2" })).await;
    ws.ok("add_comment", json!({ "sheet": 1, "row": 0, "column": 0, "text": "on T!A1" })).await;
    async fn on(ws: &mut Ws, sheet: u32) -> Vec<(String, u64, u64)> {
        let list = ws.ok("comments", json!({ "sheet": sheet })).await;
        list.as_array().unwrap().iter().map(|c| (c["text"].as_str().unwrap().to_string(), c["row"].as_u64().unwrap(), c["column"].as_u64().unwrap())).collect()
    }
    async fn cell(ws: &mut Ws, sheet: u32, row: u32, column: u32) -> String {
        let b = ws.ok("sheet_fetch", json!({ "sheet": sheet, "row_start": row, "row_count": 1, "column_count": column + 1 })).await;
        let c = &b["rows"][0][column as usize];
        if c.is_null() { String::new() } else { c[2].as_str().or(c[0].as_str()).unwrap().to_string() }
    }
    let layouts = |v: &Value| (v[0]["layout"].as_u64().unwrap(), v[1]["layout"].as_u64().unwrap());
    let source = json!({ "row": 1, "column": 1, "rows": 1, "columns": 1 });
    async fn unchanged(ws: &mut Ws) {
        assert_eq!((cell(ws, 0, 1, 1).await, cell(ws, 0, 0, 2).await, cell(ws, 1, 0, 0).await), ("7".into(), "=B2*3".into(), "old".into()));
        assert_eq!(on(ws, 0).await, vec![a("on S!B2", 1, 1)]);
        assert_eq!(on(ws, 1).await, vec![a("on T!A1", 0, 0)]);
    }
    unchanged(&mut ws).await;

    // Refused: a stale layout of the source sheet, or of the target.
    let (ls, lt) = layouts(&ws.ok("sheets", json!({})).await);
    let e = ws.call("sheet_move", json!({ "sheet": 1, "from_sheet": 0, "source": source, "row": 0, "column": 0, "layout": lt, "source_layout": ls + 1 })).await.unwrap_err();
    assert!(e.contains("since you read them"), "{e}");
    let e = ws.call("sheet_move", json!({ "sheet": 1, "from_sheet": 0, "source": source, "row": 0, "column": 0, "layout": lt + 1, "source_layout": ls })).await.unwrap_err();
    assert!(e.contains("since you read them"), "{e}");
    // Too many cells, and hidden rows.
    let e = ws.call("sheet_move", json!({ "sheet": 1, "from_sheet": 0, "source": { "row": 0, "column": 0, "rows": 2000, "columns": 1000 }, "row": 0, "column": 0 })).await.unwrap_err();
    assert!(e.contains("too many cells"), "{e}");
    ws.ok("sheet_set_hidden", json!({ "sheet": 0, "axis": "rows", "start": 1, "count": 1, "hidden": true })).await;
    let e = ws.call("sheet_move", json!({ "sheet": 1, "from_sheet": 0, "source": source, "row": 0, "column": 0 })).await.unwrap_err();
    assert!(e.contains("hidden"), "{e}");
    ws.ok("sheet_undo", json!({})).await;
    unchanged(&mut ws).await;

    // S!B2 → T!A1.
    let (ls, lt) = layouts(&ws.ok("sheets", json!({})).await);
    let area = ws.ok("sheet_move", json!({ "sheet": 1, "from_sheet": 0, "source": source, "row": 0, "column": 0, "layout": lt, "source_layout": ls })).await;
    assert_eq!(area, json!({ "row": 0, "column": 0, "rows": 1, "columns": 1 }));
    async fn moved(ws: &mut Ws) {
        assert_eq!((cell(ws, 0, 1, 1).await, cell(ws, 1, 0, 0).await), ("".into(), "7".into()));
        assert_eq!(cell(ws, 0, 0, 2).await, "=T!A1*3");
        assert!(on(ws, 0).await.is_empty(), "the comment left S");
        assert_eq!(on(ws, 1).await, vec![a("on S!B2", 0, 0)], "and replaced the covered cell's");
    }
    moved(&mut ws).await;
    ws.ok("sheet_undo", json!({})).await;
    unchanged(&mut ws).await;
    ws.ok("sheet_redo", json!({})).await;
    moved(&mut ws).await;
    ws.ok("sheet_undo", json!({})).await;
    unchanged(&mut ws).await;

    // U.11a: a paste with one value the sheet's allowed values refuse, or that runs off the
    // sheet, pastes nothing at all.
    ws.ok("set_sheet_rules", json!({ "sheet": 1, "rules": { "colors": [], "validations": [{ "area": { "row": 0, "column": 1, "rows": 5, "columns": 1 }, "list": ["yes", "no"] }] } })).await;
    let e = ws.call("sheet_paste_cells", json!({ "sheet": 1, "row": 0, "column": 0, "cells": [[{ "value": "new" }, { "value": "yes" }], [{ "value": "new" }, { "value": "maybe" }]] })).await.unwrap_err();
    assert!(e.contains("B2"), "{e}");
    let e = ws.call("sheet_paste_cells", json!({ "sheet": 1, "row": 0, "column": 16383, "cells": [[{ "value": "new" }, { "value": "new" }]] })).await.unwrap_err();
    assert!(e.contains("outside the sheet"), "{e}");
    unchanged(&mut ws).await;
    assert_eq!(cell(&mut ws, 1, 0, 1).await, "");
    server.stop().await;
}

/// S.T01: two editors format and lay out the same sheet at once; each change is announced
/// to the other and both land.
#[tokio::test(flavor = "multi_thread")]
async fn two_editors_format_and_lay_out_one_sheet() {
    let dir = tempfile::tempdir().unwrap();
    let server = TestServer::start(dir.path()).await;
    let wb = server.create_workbook("together").await;
    let mut a = server.ws(&wb).await;
    let mut b = server.ws(&wb).await;
    a.ok("sheet_create", json!({ "name": "S" })).await;
    a.ok("sheet_set", json!({ "sheet": 0, "inputs": [{ "row": 0, "column": 0, "value": "1.5" }, { "row": 0, "column": 1, "value": "x" }] })).await;
    a.ok("sheet_style", json!({ "sheet": 0, "area": { "row": 0, "column": 0, "rows": 1, "columns": 1 }, "style": { "b": true, "num_fmt": "0.00" } })).await;
    b.ok("sheet_style", json!({ "sheet": 0, "area": { "row": 0, "column": 1, "rows": 1, "columns": 1 }, "style": { "fill": "#FFEB9C" } })).await;
    a.ok("sheet_resize", json!({ "sheet": 0, "axis": "columns", "start": 0, "count": 1, "size": 150 })).await;
    b.ok("sheet_set_hidden", json!({ "sheet": 0, "axis": "rows", "start": 5, "count": 2, "hidden": true })).await;
    // Each sees the other's changes as sheet events.
    let ev = b.event("sheet_changed").await.expect("B hears of A's change");
    assert_eq!(ev["sheet"], 0);
    let block = b.ok("sheet_fetch", json!({ "sheet": 0, "row_start": 0, "row_count": 1, "column_count": 2 })).await;
    assert_eq!(block["rows"][0][0][0], "1.50", "A's number format is shown to B");
    let styles = block["styles"].as_array().unwrap();
    assert!(styles.iter().any(|s| s["column"] == 0 && s["style"]["b"] == true));
    assert!(styles.iter().any(|s| s["column"] == 1 && s["style"]["fill"] == "#FFEB9C"));
    let layout = a.ok("sheet_layout", json!({ "sheet": 0 })).await;
    assert_eq!(layout["columns"], json!([{ "index": 0, "size": 150, "hidden": false }]));
    assert_eq!(layout["rows"].as_array().unwrap().iter().filter(|r| r["hidden"] == true).count(), 2);
    server.stop().await;
}
