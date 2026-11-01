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
