//! Process-kill recovery: every change the server acknowledged before `SIGKILL` must be
//! present after restart, and nothing half-applied may appear. This tests process
//! termination only; it does not simulate power loss or storage failures.

mod common;

use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use common::*;
use serde_json::{json, Value};

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

struct Proc {
    child: Child,
    addr: std::net::SocketAddr,
}

async fn spawn(data_dir: &std::path::Path) -> Proc {
    let port = free_port();
    let child = Command::new(env!("CARGO_BIN_EXE_rowfinity"))
        .args(["--data-dir", data_dir.to_str().unwrap(), "--port", &port.to_string(), "--threads", "2"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let addr: std::net::SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    while tokio::net::TcpStream::connect(addr).await.is_err() {
        assert!(Instant::now() < deadline, "server did not start");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    Proc { child, addr }
}

impl Proc {
    fn kill(mut self) {
        self.child.kill().unwrap(); // SIGKILL on Unix: no shutdown code runs.
        self.child.wait().unwrap();
    }
}

async fn post_json(addr: std::net::SocketAddr, path: &str, body: Value) -> Value {
    reqwest::Client::new().post(format!("http://{addr}{path}")).json(&body).send().await.unwrap().json().await.unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn acknowledged_changes_survive_sigkill() {
    let dir = tempfile::tempdir().unwrap();
    let p = spawn(dir.path()).await;
    let wb = post_json(p.addr, "/api/workbooks", json!({ "name": "crash" })).await["id"].as_str().unwrap().to_string();
    let mut ws = Ws::connect(p.addr, &wb).await;
    ws.ok("create_table_from_query", json!({ "name": "t", "sql": "SELECT i::INTEGER AS n, 'v' || i AS s FROM range(1000) r(i)" })).await;
    let view = ws.ok("open_view", json!({ "spec": { "table": "t" } })).await["view_id"].as_u64().unwrap();
    // A run of acknowledged edits, appends and deletes.
    let mut expected: Vec<Value> = (0..1000).map(|i| json!([i, format!("v{i}")])).collect();
    for k in 0..40u64 {
        let row = (k * 37) % 1000;
        ws.ok("edit", json!({ "view_id": view, "edits": [{ "row": row, "column": "s", "value": format!("edit {k}") }] })).await;
        expected[row as usize][1] = json!(format!("edit {k}"));
    }
    ws.ok("edit", json!({ "view_id": view, "edits": [{ "row": 1000, "column": "n", "value": "-1" }] })).await;
    expected.push(json!([-1, null]));
    ws.ok("delete_rows", json!({ "view_id": view, "start": 10, "count": 5 })).await;
    expected.drain(10..15);
    ws.ok("add_formula_column", json!({ "table": "t", "name": "n2", "formula": "[n] * 2" })).await;
    for r in expected.iter_mut() {
        let d = r[0].as_i64().map(|n| json!(n * 2)).unwrap_or(Value::Null);
        r.as_array_mut().unwrap().push(d);
    }
    let sheet = ws.ok("sheet_create", json!({ "name": "S" })).await["index"].as_u64().unwrap();
    ws.ok("sheet_set", json!({ "sheet": sheet, "inputs": [{ "row": 0, "column": 0, "value": "21" }, { "row": 0, "column": 1, "value": "=A1*2" }] })).await;
    // Fire one more edit and kill without waiting: it may or may not have committed,
    // but it must be all-or-nothing.
    ws.send_raw(json!({ "id": 9999, "op": "edit", "view_id": view,
        "edits": [{ "row": 0, "column": "s", "value": "maybe" }, { "row": 1, "column": "s", "value": "maybe" }] }))
        .await;
    p.kill();
    drop(ws);

    let p = spawn(dir.path()).await;
    let mut ws = Ws::connect(p.addr, &wb).await;
    let view = ws.ok("open_view", json!({ "spec": { "table": "t" } })).await["view_id"].as_u64().unwrap();
    let rows = ws.all_rows(view).await;
    assert_eq!(rows.len(), expected.len());
    // The unacknowledged two-cell edit is all-or-nothing.
    match (rows[0][1].as_str(), rows[1][1].as_str()) {
        (Some("maybe"), Some("maybe")) => {
            expected[0][1] = json!("maybe");
            expected[1][1] = json!("maybe");
        }
        (a, b) => assert!(a != Some("maybe") && b != Some("maybe"), "torn edit: {a:?} {b:?}"),
    }
    assert_eq!(rows, expected, "every acknowledged change is present after SIGKILL");
    let cells = ws.ok("sheet_fetch", json!({ "sheet": sheet, "row_start": 0, "row_count": 1, "column_count": 2 })).await;
    assert_eq!(cells["rows"][0][1][0], "42");
    drop(ws);
    p.kill();
}

#[tokio::test(flavor = "multi_thread")]
async fn kill_during_a_large_write_leaves_the_old_or_new_state() {
    let dir = tempfile::tempdir().unwrap();
    let p = spawn(dir.path()).await;
    let wb = post_json(p.addr, "/api/workbooks", json!({ "name": "big write" })).await["id"].as_str().unwrap().to_string();
    let mut ws = Ws::connect(p.addr, &wb).await;
    ws.ok("create_table_from_query", json!({ "name": "base", "sql": "SELECT 1 AS x" })).await;
    let addr = p.addr;
    let wb2 = wb.clone();
    let writer = tokio::spawn(async move {
        let mut w = Ws::connect(addr, &wb2).await;
        w.call("create_demo_table", json!({ "rows": 5_000_000 })).await
    });
    // Kill while the demo table is being written (the WAL or database is growing).
    let wbdir = dir.path().join(&wb);
    eventually("large write in progress", || {
        files_under(&wbdir).iter().map(|f| std::fs::metadata(f).map(|m| m.len()).unwrap_or(0)).sum::<u64>() > 2_000_000
    })
    .await;
    p.kill();
    let _ = writer.await;
    drop(ws);

    let p = spawn(dir.path()).await;
    let mut ws = Ws::connect(p.addr, &wb).await;
    let tables = ws.ok("tables", json!({})).await;
    let names: Vec<&str> = tables.as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert!(names.contains(&"base"), "earlier commits survive: {names:?}");
    for t in tables.as_array().unwrap() {
        if t["name"] == "flights" {
            assert_eq!(t["row_count"], 5_000_000, "a table is either complete or absent");
        }
    }
    drop(ws);
    p.kill();
}
