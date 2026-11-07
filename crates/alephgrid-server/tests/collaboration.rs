//! Phase 7 (7.08–7.09; 7.T02): concurrent editors. Stale edits are refused instead of
//! overwriting someone else's change, position-addressed changes are refused once rows or
//! cells moved, every revision is announced in order, retries apply once, and table undo
//! takes back only what nobody changed since. The conflict policy is docs/COLLABORATION.md.
//!
//! Expected values are written out by hand from the tables each test creates; they are not
//! read back from the server's own bookkeeping.

mod common;

use std::collections::BTreeSet;

use common::*;
use serde_json::{json, Value};

const TABLE_SQL: &str = "SELECT i AS n, 'x' || i AS name, (i * 1.5)::DECIMAL(6,2) AS amount FROM range(6) r(i)";

/// The table every test starts from, as fetched: `[n, name, amount]` per row.
fn start_rows() -> Vec<Value> {
    vec![
        json!([0, "x0", "0.00"]),
        json!([1, "x1", "1.50"]),
        json!([2, "x2", "3.00"]),
        json!([3, "x3", "4.50"]),
        json!([4, "x4", "6.00"]),
        json!([5, "x5", "7.50"]),
    ]
}

fn spec(table: &str) -> Value {
    json!({ "table": table, "sort": [], "filters": [], "hidden": [] })
}

/// A workbook with table `t` and two editors, each with a view of it.
async fn two_editors(server: &TestServer) -> (String, Ws, u64, Ws, u64) {
    let wb = server.create_workbook("together").await;
    let mut a = server.ws(&wb).await;
    a.ok("create_table_from_query", json!({ "name": "t", "sql": TABLE_SQL })).await;
    let va = a.ok("open_view", json!({ "spec": spec("t") })).await["view_id"].as_u64().unwrap();
    let mut b = server.ws(&wb).await;
    let vb = b.ok("open_view", json!({ "spec": spec("t") })).await["view_id"].as_u64().unwrap();
    (wb, a, va, b, vb)
}

/// Record ids of a view's rows, in view order.
async fn ids(ws: &mut Ws, view: u64) -> Vec<i64> {
    let block = ws.ok("fetch", json!({ "view_id": view, "row_start": 0, "row_count": 1000 })).await;
    block["ids"].as_array().unwrap().iter().map(|v| v.as_i64().unwrap()).collect()
}

async fn generation(ws: &mut Ws, view: u64) -> i64 {
    ws.ok("view_info", json!({ "view_id": view })).await["generation"].as_i64().unwrap()
}

/// A refused request's whole reply frame.
async fn refused(ws: &mut Ws, op: &str, params: Value) -> Value {
    let id = ws.send(op, params).await;
    let reply = ws.reply(id).await.expect("a reply");
    assert_eq!(reply["ok"], false, "{op} should have been refused: {reply}");
    reply
}

fn edit(record: i64, column: &str, value: &str, base: Value) -> Value {
    json!({ "row": 0, "record": record, "column": column, "value": value, "base": base })
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_edits_of_the_same_cell_conflict_and_other_cells_merge() {
    let dir = tempfile::tempdir().unwrap();
    let server = TestServer::start(dir.path()).await;
    let (_wb, mut a, va, mut b, vb) = two_editors(&server).await;
    let id = ids(&mut a, va).await;
    let mut model = start_rows();
    assert_eq!(a.all_rows(va).await, model);

    // Both saw "x0" in row 0. A saves first; B's edit of the same cell is refused, not applied.
    a.ok("edit", json!({ "view_id": va, "edits": [edit(id[0], "name", "from A", json!("x0"))] })).await;
    model[0][1] = json!("from A");
    let r = refused(&mut b, "edit", json!({ "view_id": vb, "edits": [edit(id[0], "name", "from B", json!("x0"))] })).await;
    assert_eq!(r["code"], "conflict", "{r}");
    assert_eq!(r["moved"], Value::Null, "a value conflict can be overwritten: {r}");
    assert!(r["error"].as_str().unwrap().contains("someone else changed"), "{r}");
    assert_eq!(b.all_rows(vb).await, model);

    // Other cells, even in the same row, merge.
    b.ok("edit", json!({ "view_id": vb, "edits": [edit(id[0], "amount", "9.25", json!("0.00")), edit(id[1], "name", "B1", json!("x1"))] })).await;
    model[0][2] = json!("9.25");
    model[1][1] = json!("B1");
    assert_eq!(a.all_rows(va).await, model);

    // A base in another spelling of the same value matches (1.5 for DECIMAL 1.50).
    a.ok("edit", json!({ "view_id": va, "edits": [edit(id[1], "amount", "2", json!(1.5))] })).await;
    model[1][2] = json!("2.00");

    // One stale cell refuses the whole batch: nothing of it is written.
    let r = refused(&mut b, "edit", json!({ "view_id": vb, "edits": [
        edit(id[2], "name", "kept?", json!("x2")),
        edit(id[0], "name", "stale", json!("x0")),
    ] }))
    .await;
    assert_eq!(r["code"], "conflict");
    assert!(r["error"].as_str().unwrap().contains(&format!("\"name\" of record {}", id[0])), "{r}");
    assert_eq!(b.all_rows(vb).await, model);

    // Choosing to overwrite: no base.
    b.ok("edit", json!({ "view_id": vb, "edits": [{ "row": 0, "record": id[0], "column": "name", "value": "B wins" }] })).await;
    model[0][1] = json!("B wins");
    // An empty cell's base is null.
    a.ok("edit", json!({ "view_id": va, "edits": [{ "row": 0, "record": id[3], "column": "name", "value": null, "base": "x3" }] })).await;
    model[3][1] = Value::Null;
    a.ok("edit", json!({ "view_id": va, "edits": [edit(id[3], "name", "refilled", Value::Null)] })).await;
    model[3][1] = json!("refilled");
    let r = refused(&mut b, "edit", json!({ "view_id": vb, "edits": [edit(id[3], "name", "late", Value::Null)] })).await;
    assert_eq!(r["code"], "conflict");
    assert_eq!(a.all_rows(va).await, model);
    assert_eq!(b.all_rows(vb).await, model);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn structural_races_are_refused_instead_of_hitting_other_rows() {
    let dir = tempfile::tempdir().unwrap();
    let server = TestServer::start(dir.path()).await;
    let (_wb, mut a, va, mut b, vb) = two_editors(&server).await;
    let id = ids(&mut a, va).await;
    let g = generation(&mut a, va).await;
    let mut model = start_rows();

    // B deletes row 0. A, still looking at the old arrangement, deletes "row 0" too: refused,
    // since that position now holds another row.
    b.ok("delete_rows", json!({ "view_id": vb, "start": 0, "count": 1, "generation": g })).await;
    model.remove(0);
    let r = refused(&mut a, "delete_rows", json!({ "view_id": va, "start": 0, "count": 1, "generation": g })).await;
    assert_eq!((r["code"].clone(), r["moved"].clone()), (json!("conflict"), json!(true)), "{r}");
    let r = refused(&mut a, "insert_rows", json!({ "view_id": va, "at": 2, "count": 1, "generation": g })).await;
    assert_eq!(r["moved"], true);
    assert_eq!(a.all_rows(va).await, model);

    // An edit of the deleted record is refused; one of another record lands on it although
    // its position changed.
    let r = refused(&mut a, "edit", json!({ "view_id": va, "edits": [edit(id[0], "name", "gone", json!("x0"))] })).await;
    assert_eq!((r["code"].clone(), r["moved"].clone()), (json!("conflict"), json!(true)));
    a.ok("edit", json!({ "view_id": va, "edits": [{ "row": 3, "record": id[2], "column": "name", "value": "by record", "base": "x2" }] })).await;
    model[1][1] = json!("by record");
    assert_eq!(b.all_rows(vb).await, model);

    // Two appends from the same arrangement: the second is refused, then lands after the first.
    let g = generation(&mut a, va).await;
    assert_eq!(generation(&mut b, vb).await, g);
    a.ok("edit", json!({ "view_id": va, "generation": g, "edits": [{ "row": 5, "column": "name", "value": "new A" }] })).await;
    let r = refused(&mut b, "edit", json!({ "view_id": vb, "generation": g, "edits": [{ "row": 5, "column": "name", "value": "new B" }] })).await;
    assert_eq!(r["moved"], true);
    let g2 = generation(&mut b, vb).await;
    assert_ne!(g2, g);
    b.ok("edit", json!({ "view_id": vb, "generation": g2, "edits": [{ "row": 6, "column": "name", "value": "new B" }] })).await;
    model.push(json!([null, "new A", null]));
    model.push(json!([null, "new B", null]));
    assert_eq!(a.all_rows(va).await, model);

    // A renamed column: edits naming the old one are refused as moved.
    b.ok("rename_column", json!({ "table": "t", "name": "name", "new_name": "label" })).await;
    let r = refused(&mut a, "edit", json!({ "view_id": va, "edits": [edit(id[1], "name", "old name", json!("x1"))] })).await;
    assert_eq!((r["code"].clone(), r["moved"].clone()), (json!("conflict"), json!(true)), "{r}");

    // A sorted view keeps the arrangement it showed until rows are inserted or deleted, so a
    // delete by position removes the row its user saw even after values were edited.
    let sorted = a.ok("open_view", json!({ "spec": { "table": "t", "sort": [{ "column": "amount", "desc": true }], "filters": [], "hidden": [] } })).await;
    let sv = sorted["view_id"].as_u64().unwrap();
    let top = ids(&mut a, sv).await[0];
    let gs = sorted["generation"].as_i64().unwrap();
    // id[5] has the largest amount (7.50); B lowers it, which would reorder a rebuilt view.
    assert_eq!(top, id[5]);
    b.ok("edit", json!({ "view_id": vb, "edits": [{ "row": 0, "record": id[5], "column": "amount", "value": "0.10", "base": "7.50" }] })).await;
    a.ok("delete_rows", json!({ "view_id": sv, "start": 0, "count": 1, "generation": gs })).await;
    let left: BTreeSet<i64> = ids(&mut b, vb).await.into_iter().collect();
    assert!(!left.contains(&id[5]), "the row A saw on top was deleted");
    assert_eq!(left.len(), model.len() - 1);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn sheet_edits_check_values_and_layouts() {
    let dir = tempfile::tempdir().unwrap();
    let server = TestServer::start(dir.path()).await;
    let wb = server.create_workbook("sheets").await;
    let mut a = server.ws(&wb).await;
    let mut b = server.ws(&wb).await;
    let s = a.ok("sheet_create", json!({ "name": "Plan" })).await;
    let layout = s["layout"].as_u64().unwrap();
    let set = |row: u32, column: u32, value: &str, base: Option<&str>| {
        let mut i = json!({ "row": row, "column": column, "value": value });
        if let Some(b) = base {
            i["base"] = json!(b);
        }
        i
    };
    let cell = |block: &Value, r: usize, c: usize| block["rows"][r].get(c).cloned().unwrap_or(Value::Null);

    // Same cell: the second writer, who saw it empty, is refused.
    a.ok("sheet_set", json!({ "sheet": 0, "inputs": [set(0, 0, "1.50", Some(""))], "layout": layout })).await;
    let r = refused(&mut b, "sheet_set", json!({ "sheet": 0, "inputs": [set(0, 0, "2", Some(""))], "layout": layout })).await;
    assert_eq!(r["code"], "conflict");
    assert!(r["error"].as_str().unwrap().contains("A1"), "{r}");
    // Another cell merges; a base spelled as typed ("1.50") matches the stored 1.5.
    b.ok("sheet_set", json!({ "sheet": 0, "inputs": [set(0, 1, "=A1*2", Some("")), set(0, 0, "3", Some("1.50"))], "layout": layout })).await;
    let block = a.ok("sheet_fetch", json!({ "sheet": 0, "row_start": 0, "row_count": 10, "column_count": 5 })).await;
    assert_eq!(cell(&block, 0, 0)[0], "3");
    assert_eq!(cell(&block, 0, 1)[0], "6");
    assert_eq!(block["layout"].as_u64().unwrap(), layout, "editing values does not move cells");

    // B inserts a row above. A's edit addressed by the old layout is refused: A2 now holds
    // what A saw as A1.
    b.ok("sheet_insert_rows", json!({ "sheet": 0, "at": 0, "count": 1, "layout": layout })).await;
    let r = refused(&mut a, "sheet_set", json!({ "sheet": 0, "inputs": [set(1, 0, "lost?", None)], "layout": layout })).await;
    assert_eq!((r["code"].clone(), r["moved"].clone()), (json!("conflict"), json!(true)), "{r}");
    for (op, params) in [
        ("sheet_delete_rows", json!({ "sheet": 0, "at": 0, "count": 1 })),
        ("sheet_insert_columns", json!({ "sheet": 0, "at": 0, "count": 1 })),
        ("sheet_fill", json!({ "sheet": 0, "area": { "row": 0, "column": 0, "rows": 2, "columns": 1 }, "direction": "down" })),
        ("sheet_drag_fill", json!({ "sheet": 0, "source": { "row": 0, "column": 0, "rows": 1, "columns": 1 }, "target": { "row": 0, "column": 0, "rows": 3, "columns": 1 } })),
        ("sheet_move", json!({ "sheet": 0, "source": { "row": 1, "column": 0, "rows": 1, "columns": 1 }, "row": 5, "column": 5 })),
        ("sheet_rename", json!({ "sheet": 0, "name": "Renamed" })),
    ] {
        let mut p = params.clone();
        p["layout"] = json!(layout);
        let r = refused(&mut a, op, p).await;
        assert_eq!(r["moved"], true, "{op}: {r}");
    }
    let block = a.ok("sheet_fetch", json!({ "sheet": 0, "row_start": 0, "row_count": 10, "column_count": 5 })).await;
    let now = block["layout"].as_u64().unwrap();
    assert!(now > layout);
    assert_eq!(cell(&block, 0, 0), Value::Null);
    assert_eq!(cell(&block, 1, 0)[0], "3");
    a.ok("sheet_set", json!({ "sheet": 0, "inputs": [set(2, 0, "below", Some(""))], "layout": now })).await;

    // Deleting a sheet moves the later ones down an index: their old layouts no longer fit.
    a.ok("sheet_create", json!({ "name": "Second" })).await;
    let third = a.ok("sheet_create", json!({ "name": "Third" })).await;
    let third_layout = third["layout"].as_u64().unwrap();
    let second_layout = b.ok("sheets", json!({})).await[1]["layout"].as_u64().unwrap();
    b.ok("sheet_delete", json!({ "sheet": 1, "layout": second_layout })).await;
    let r = refused(&mut a, "sheet_set", json!({ "sheet": 2, "inputs": [set(0, 0, "x", None)], "layout": third_layout })).await;
    assert!(r["error"].as_str().unwrap().contains("unknown sheet 2"), "{r}");
    let r = refused(&mut a, "sheet_set", json!({ "sheet": 1, "inputs": [set(0, 0, "x", None)], "layout": third_layout })).await;
    assert_eq!(r["moved"], true, "index 1 is now Third, which moved: {r}");
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn every_revision_is_announced_once_in_order() {
    let dir = tempfile::tempdir().unwrap();
    let server = TestServer::start(dir.path()).await;
    let (wb, mut a, va, mut b, vb) = two_editors(&server).await;
    let id = ids(&mut a, va).await;
    let mut watcher = server.ws(&wb).await;
    let start = watcher.hello["revision"].as_u64().unwrap();
    // Two editors send changes at once, without waiting for replies.
    let mut sent = Vec::new();
    for k in 0..20 {
        sent.push((true, a.send("edit", json!({ "view_id": va, "edits": [{ "row": 0, "record": id[k % 6], "column": "n", "value": k.to_string() }] })).await));
        sent.push((false, b.send("edit", json!({ "view_id": vb, "edits": [{ "row": 0, "record": id[(k + 3) % 6], "column": "amount", "value": format!("{k}.5") }] })).await));
    }
    // A change that changes nothing visible is announced too.
    sent.push((true, a.send("sheet_undo", json!({})).await));
    let mut last = start;
    for (from_a, request) in sent {
        let r = if from_a { a.reply(request).await } else { b.reply(request).await }.unwrap();
        assert_eq!(r["ok"], true, "{r}");
        last = last.max(r["revision"].as_u64().unwrap());
    }
    assert_eq!(last, start + 41);
    // The watcher sees revisions in order, with none skipped.
    let mut seen = Vec::new();
    while seen.last().copied() != Some(last) {
        let e = watcher.event_any().await.expect("events");
        if let Some(r) = e["revision"].as_u64() {
            if e["event"] == "committed" {
                assert_eq!(r, last, "the no-op undo is the last change");
            }
            seen.push(r);
        }
    }
    assert!(seen.windows(2).all(|w| w[0] <= w[1]), "out of order: {seen:?}");
    let distinct: Vec<u64> = seen.iter().copied().collect::<BTreeSet<_>>().into_iter().collect();
    assert_eq!(distinct, (start + 1..=last).collect::<Vec<_>>());
    // Reconnecting says how far the workbook got, so a client knows what it missed.
    let again = server.ws(&wb).await;
    assert_eq!(again.hello["revision"].as_u64().unwrap(), last);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn retried_changes_apply_once_and_stale_resends_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let server = TestServer::start(dir.path()).await;
    let (wb, mut a, va, mut b, vb) = two_editors(&server).await;
    let id = ids(&mut a, va).await;
    let mut model = start_rows();

    // A's change went through but A never saw the reply (the connection dropped).
    let frame = json!({ "view_id": va, "mutation_id": "a-1", "edits": [edit(id[0], "name", "A once", json!("x0"))] });
    let first = a.send("edit", frame.clone()).await;
    let committed = a.reply(first).await.unwrap();
    a.close().await;
    model[0][1] = json!("A once");
    // Meanwhile B edits the same cell on top of A's value.
    b.ok("edit", json!({ "view_id": vb, "edits": [edit(id[0], "name", "B after", json!("A once"))] })).await;
    model[0][1] = json!("B after");
    // A reconnects, reopens its view and resends the kept change with its mutation id: it
    // replays the recorded result instead of applying again over B's edit.
    let mut a = server.ws(&wb).await;
    assert!(a.hello["revision"].as_u64().unwrap() > committed["revision"].as_u64().unwrap(), "A learns it missed changes");
    let va = a.ok("open_view", json!({ "spec": spec("t") })).await["view_id"].as_u64().unwrap();
    let mut frame = frame;
    frame["view_id"] = json!(va);
    let id2 = a.send("edit", frame).await;
    let replay = a.reply(id2).await.unwrap();
    assert_eq!((replay["ok"].clone(), replay["replayed"].clone(), replay["revision"].clone()), (json!(true), json!(true), committed["revision"].clone()));
    assert_eq!(a.all_rows(va).await, model);

    // A change that never reached the server, resent after B changed its cell, is refused
    // and reported, not applied over B's value.
    b.ok("edit", json!({ "view_id": vb, "edits": [edit(id[1], "name", "B1", json!("x1"))] })).await;
    model[1][1] = json!("B1");
    let r = refused(&mut a, "edit", json!({ "view_id": va, "mutation_id": "a-2", "edits": [edit(id[1], "name", "late", json!("x1"))] })).await;
    assert_eq!(r["code"], "conflict");
    assert_eq!(a.all_rows(va).await, model);
    // Refused changes are not recorded: the same id can be sent again once rebased.
    a.ok("edit", json!({ "view_id": va, "mutation_id": "a-2", "edits": [edit(id[1], "name", "rebased", json!("B1"))] })).await;
    model[1][1] = json!("rebased");
    assert_eq!(b.all_rows(vb).await, model);
    server.stop().await;
}

const PASSWORD: &str = "a long enough password";

/// Edit one cell by record and return the revision it committed as.
async fn edit_ok(ws: &mut Ws, view: u64, record: i64, column: &str, value: &str, base: Value) -> u64 {
    let id = ws.send("edit", json!({ "view_id": view, "edits": [edit(record, column, value, base)] })).await;
    let r = ws.reply(id).await.unwrap();
    assert_eq!(r["ok"], true, "{r}");
    r["revision"].as_u64().unwrap()
}

/// Accounts mode: the admin (A) and an editor (B) of one space, each with a view of `t`.
async fn two_people(dir: &std::path::Path) -> (TestServer, Ws, u64, Ws, u64) {
    use reqwest::Method;
    let server = TestServer::start_accounts(dir, |_| {}).await;
    let admin = server.admin().await;
    let org = admin.me["orgs"][0]["org"]["id"].as_str().unwrap().to_string();
    let space = admin.ok(Method::POST, "/api/spaces", Some(json!({ "name": "Team" }))).await["id"].as_str().unwrap().to_string();
    let wb = admin.ok(Method::POST, "/api/workbooks", Some(json!({ "name": "shared", "space": space }))).await["id"].as_str().unwrap().to_string();
    let code = admin.invite(&org, "bea@example.com", Some(&space), Some("editor")).await;
    let bea = server.accept(&code, "Bea", PASSWORD).await;
    let mut a = admin.ws(&wb).await.unwrap();
    a.ok("create_table_from_query", json!({ "name": "t", "sql": TABLE_SQL })).await;
    let va = a.ok("open_view", json!({ "spec": spec("t") })).await["view_id"].as_u64().unwrap();
    let mut b = bea.ws(&wb).await.unwrap();
    let vb = b.ok("open_view", json!({ "spec": spec("t") })).await["view_id"].as_u64().unwrap();
    (server, a, va, b, vb)
}

#[tokio::test(flavor = "multi_thread")]
async fn undo_takes_back_own_changes_unless_someone_changed_them_since() {
    let dir = tempfile::tempdir().unwrap();
    let (server, mut a, va, mut b, vb) = two_people(dir.path()).await;
    let id = ids(&mut a, va).await;
    let mut model = start_rows();
    let undo = |revision: Option<u64>, redo: bool| {
        let mut p = json!({ "redo": redo });
        if let Some(r) = revision {
            p["revision"] = json!(r);
        }
        p
    };

    // Each takes back their own change, not the other's latest.
    let r1 = edit_ok(&mut a, va, id[0], "name", "A0", json!("x0")).await;
    edit_ok(&mut b, vb, id[1], "name", "B1", json!("x1")).await;
    model[1][1] = json!("B1");
    let out = a.ok("undo", undo(None, false)).await;
    assert_eq!(out, json!({ "table": "t", "undone": r1, "kind": "undo", "rows": 1 }));
    assert_eq!(b.all_rows(vb).await, model);
    // Redo, once.
    assert_eq!(a.ok("undo", undo(None, true)).await["kind"], "redo");
    model[0][1] = json!("A0");
    assert_eq!(a.ok("undo", undo(None, true)).await, Value::Null);
    assert_eq!(b.all_rows(vb).await, model);

    // B edited the cell A set: A's undo is refused and changes nothing.
    edit_ok(&mut a, va, id[2], "name", "A2", json!("x2")).await;
    edit_ok(&mut b, vb, id[2], "name", "B2", json!("A2")).await;
    model[2][1] = json!("B2");
    let r = refused(&mut a, "undo", undo(None, false)).await;
    assert_eq!(r["code"], "conflict", "{r}");
    assert!(r["error"].as_str().unwrap().contains("cell(s) of \"name\""), "{r}");
    assert_eq!(a.all_rows(va).await, model);
    // Once B takes back theirs, A's applies.
    b.ok("undo", undo(None, false)).await;
    a.ok("undo", undo(None, false)).await;
    model[2][1] = json!("x2");
    assert_eq!(b.all_rows(vb).await, model);

    // Deleted rows come back with their ids, values and places.
    let before = ids(&mut b, vb).await;
    let g = generation(&mut b, vb).await;
    b.ok("delete_rows", json!({ "view_id": vb, "start": 3, "count": 2, "generation": g })).await;
    assert_eq!(b.all_rows(vb).await.len(), 4);
    assert_eq!(b.ok("undo", undo(None, false)).await["rows"], 2);
    assert_eq!(ids(&mut a, va).await, before);
    assert_eq!(a.all_rows(va).await, model);
    // Inserted and appended rows go away again.
    let g = generation(&mut a, va).await;
    a.ok("insert_rows", json!({ "view_id": va, "at": 1, "count": 2, "generation": g })).await;
    a.ok("undo", undo(None, false)).await;
    let g = generation(&mut a, va).await;
    a.ok("edit", json!({ "view_id": va, "generation": g, "edits": [{ "row": 6, "column": "name", "value": "appended" }, { "row": 0, "record": id[0], "column": "n", "value": "100", "base": 0 }] })).await;
    a.ok("undo", undo(None, false)).await;
    assert_eq!(ids(&mut b, vb).await, before);
    assert_eq!(b.all_rows(vb).await, model);

    // SQL changes too.
    a.ok("sql_change", json!({ "sql": "UPDATE t SET amount = amount * 2 WHERE n >= 4" })).await;
    let changed = a.all_rows(va).await;
    assert_eq!((changed[4][2].clone(), changed[5][2].clone()), (json!("12.00"), json!("15.00")));
    a.ok("undo", undo(None, false)).await;
    a.ok("sql_change", json!({ "sql": "DELETE FROM t WHERE n = 0" })).await;
    a.ok("undo", undo(None, false)).await;
    a.ok("sql_change", json!({ "sql": "INSERT INTO t (n, name) VALUES (99, 'ins')" })).await;
    a.ok("undo", undo(None, false)).await;
    assert_eq!(ids(&mut b, vb).await, before);
    assert_eq!(b.all_rows(vb).await, model);

    // Anyone's change can be taken back by revision, once.
    let rb = edit_ok(&mut b, vb, id[5], "name", "B5", json!("x5")).await;
    let ua = a.ok("undo", undo(Some(rb), false)).await;
    assert_eq!(ua["undone"], rb);
    let r = refused(&mut a, "undo", undo(Some(rb), false)).await;
    assert!(r["error"].as_str().unwrap().contains("already taken back"), "{r}");
    let history = a.ok("history", json!({ "after": rb - 1 })).await;
    assert_eq!(history[0]["undo"], json!({ "table": "t", "kind": "change", "undone_by": rb + 1 }));
    assert_eq!(history[1]["undo"], json!({ "table": "t", "kind": "undo", "undoes": rb }));
    assert_eq!(history[1]["op"], "undo");
    assert_eq!(a.all_rows(va).await, model);

    // Changed columns block it.
    edit_ok(&mut a, va, id[1], "amount", "8", json!("1.50")).await;
    b.ok("add_column", json!({ "table": "t", "name": "extra", "sql_type": "VARCHAR" })).await;
    let r = refused(&mut a, "undo", undo(None, false)).await;
    assert_eq!((r["code"].clone(), r["moved"].clone()), (json!("conflict"), json!(true)), "{r}");

    // Sheet undo, likewise, takes back only the caller's own step.
    a.ok("sheet_create", json!({ "name": "Plan" })).await;
    a.ok("sheet_set", json!({ "sheet": 0, "inputs": [{ "row": 5, "column": 0, "value": "A's" }] })).await;
    let r = refused(&mut b, "sheet_undo", json!({})).await;
    assert_eq!(r["code"], "conflict");
    assert!(r["error"].as_str().unwrap().contains("someone else"), "{r}");
    assert_eq!(a.ok("sheet_undo", json!({})).await, true);
    let block = b.ok("sheet_fetch", json!({ "sheet": 0, "row_start": 5, "row_count": 1, "column_count": 2 })).await;
    assert_eq!(block["rows"][0].get(0).cloned().unwrap_or(Value::Null), Value::Null);
    assert_eq!(refused(&mut b, "sheet_redo", json!({})).await["code"], "conflict");
    assert_eq!(a.ok("sheet_redo", json!({})).await, true);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn undo_is_kept_across_restarts_for_recent_changes_of_bounded_size() {
    let dir = tempfile::tempdir().unwrap();
    let server = TestServer::start(dir.path()).await;
    let (wb, mut a, va, _b, _vb) = two_editors(&server).await;
    let id = ids(&mut a, va).await;
    let mut revisions = Vec::new();
    for k in 0..55 {
        let r = a.send("edit", json!({ "view_id": va, "edits": [{ "row": 0, "record": id[0], "column": "n", "value": k.to_string() }] })).await;
        revisions.push(a.reply(r).await.unwrap()["revision"].as_u64().unwrap());
    }
    // The last 50 can be taken back; older ones are no longer kept.
    let history = a.ok("history", json!({ "after": revisions[0] - 1, "limit": 55 })).await;
    let kept: Vec<bool> = history.as_array().unwrap().iter().map(|h| h.get("undo").is_some()).collect();
    assert_eq!(kept, [vec![false; 5], vec![true; 50]].concat());
    let r = refused(&mut a, "undo", json!({ "revision": revisions[0] })).await;
    assert!(r["error"].as_str().unwrap().contains("no longer kept"), "{r}");
    a.close().await;
    _b.close().await;
    server.stop().await;

    // After a restart the latest change is still undoable.
    let server = TestServer::start(dir.path()).await;
    let mut a = server.ws(&wb).await;
    let va = a.ok("open_view", json!({ "spec": spec("t") })).await["view_id"].as_u64().unwrap();
    assert_eq!(a.ok("undo", json!({})).await["undone"], revisions[54]);
    assert_eq!(a.all_rows(va).await[0][0], 53);

    // A change of more than a million rows is made but not undoable.
    a.ok("create_table_from_query", json!({ "name": "big", "sql": "SELECT i FROM range(1000001) r(i)" })).await;
    let vbig = a.ok("open_view", json!({ "spec": spec("big") })).await["view_id"].as_u64().unwrap();
    let g = generation(&mut a, vbig).await;
    let gone = a.send("delete_rows", json!({ "view_id": vbig, "start": 0, "count": 1000001, "generation": g })).await;
    let r = a.reply(gone).await.unwrap();
    assert_eq!(r["ok"], true, "{r}");
    let last = a.ok("history", json!({ "after": r["revision"].as_u64().unwrap() - 1 })).await;
    assert!(last[0].get("undo").is_none(), "{last}");
    assert_eq!(a.ok("view_info", json!({ "view_id": vbig })).await["row_count"], 0);
    server.stop().await;
}

/// A sheet change another connection has already seen is committed: it stays when the
/// connection that made it leaves at once (a page reload while "Saving…"). Readers used to
/// see the shared sheet model while the change was still being written; the session end
/// then interrupted that write and took the change back (CI, WebKit: a freeze and a redo
/// confirmed by the test's own connection were gone after a reload).
#[tokio::test(flavor = "multi_thread")]
async fn a_sheet_change_another_connection_saw_survives_its_author_leaving() {
    let dir = tempfile::tempdir().unwrap();
    let server = TestServer::start(dir.path()).await;
    let wb = server.create_workbook("seen").await;
    let mut b = server.ws(&wb).await;
    b.ok("sheet_create", json!({ "name": "S" })).await;
    // A large sheet makes saving the model (every sheet change does) take a while, so the
    // author can leave while the change is being written.
    let inputs: Vec<Value> = (0..200_000u32).map(|i| json!({ "row": i / 20, "column": i % 20, "value": format!("text {i}") })).collect();
    b.ok("sheet_set", json!({ "sheet": 0, "inputs": inputs })).await;
    let frozen = |sheets: &Value| sheets[0]["frozen_columns"].as_u64().unwrap_or(0);
    // The author leaves at a different moment of the write each time.
    for attempt in 0..30u64 {
        let columns = attempt % 2 + 1;
        let mut a = server.ws(&wb).await;
        a.send("sheet_freeze", json!({ "sheet": 0, "columns": columns })).await;
        while frozen(&b.ok("sheets", json!({})).await) != columns {}
        tokio::time::sleep(std::time::Duration::from_millis(attempt * 3)).await;
        drop(a);
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert_eq!(frozen(&b.ok("sheets", json!({})).await), columns, "attempt {attempt}: a change another connection saw was taken back");
    }
    b.close().await;
    server.stop().await;
}
