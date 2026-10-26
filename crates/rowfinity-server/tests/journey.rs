//! 1.11 gate: import → edit → filter → export → restart → reopen, through the real HTTP and
//! WebSocket protocol, with exact comparisons against an independently maintained model and
//! failed operations in between.

mod common;

use common::*;
use reqwest::Method;
use serde_json::{json, Value};

const CSV: &str = "id,name,amount,big,note\n\
1,Ada,10.5,9007199254740993,\"comma, inside\"\n\
2,Linus,-3,1,\"say \"\"hi\"\"\"\n\
3,東京,0,9223372036854775807,\n\
4,Grace,1e3,-9007199254740993,emoji 🚀\n";

/// The model: what each row must read back as, in wire form.
fn imported_model() -> Vec<Value> {
    vec![
        json!([1, "Ada", 10.5, "9007199254740993", "comma, inside"]),
        json!([2, "Linus", -3.0, 1, "say \"hi\""]),
        json!([3, "東京", 0.0, "9223372036854775807", null]),
        json!([4, "Grace", 1000.0, "-9007199254740993", "emoji 🚀"]),
    ]
}

async fn open(ws: &mut Ws, spec: Value) -> u64 {
    ws.ok("open_view", json!({ "spec": spec })).await["view_id"].as_u64().unwrap()
}

async fn export_csv(server: &TestServer, wb: &str, spec: Value) -> Vec<Vec<String>> {
    let path = format!("/api/workbooks/{wb}/export?spec={}&format=csv", urlencode(&spec.to_string()));
    let (status, _, bytes) = server.download(&path).await;
    assert_eq!(status, 200);
    parse_csv(&String::from_utf8(bytes).unwrap())
}

#[tokio::test(flavor = "multi_thread")]
async fn table_journey_survives_restart_exactly() {
    let dir = tempfile::tempdir().unwrap();
    let server = TestServer::start(dir.path()).await;
    let wb = server.create_workbook("journey").await;

    // Import.
    let (status, body) = server.upload(&wb, "people.csv", CSV.as_bytes().to_vec()).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["table"]["row_count"], 4);
    let mut ws = server.ws(&wb).await;
    let view = open(&mut ws, json!({ "table": "people" })).await;
    let info = ws.ok("view_info", json!({ "view_id": view })).await;
    let types: Vec<&str> = info["columns"].as_array().unwrap().iter().map(|c| c["sql_type"].as_str().unwrap()).collect();
    assert_eq!(types, ["BIGINT", "VARCHAR", "DOUBLE", "BIGINT", "VARCHAR"]);
    let mut model = imported_model();
    assert_eq!(ws.all_rows(view).await, model);

    // Edit, including typing below the end (append) and an exact large integer.
    let out = ws
        .ok("edit", json!({ "view_id": view, "edits": [
            { "row": 1, "column": "name", "value": "Linus T." },
            { "row": 0, "column": "big", "value": "9007199254740995" },
            { "row": 4, "column": "id", "value": "5" },
            { "row": 4, "column": "name", "value": "Ünïcödé \"quoted\"" },
            { "row": 4, "column": "amount", "value": "0.1" },
        ] }))
        .await;
    assert_eq!((out["cells_changed"].clone(), out["rows_appended"].clone()), (json!(5), json!(1)));
    model[1][1] = json!("Linus T.");
    model[0][3] = json!("9007199254740995");
    model.push(json!([5, "Ünïcödé \"quoted\"", 0.1, null, null]));
    assert_eq!(ws.all_rows(view).await, model);

    // Failed operations change nothing.
    for (edits, why) in [
        (json!([{ "row": 0, "column": "amount", "value": "lots" }]), "not a valid"),
        (json!([{ "row": 2, "column": "name", "value": "x" }, { "row": 9, "column": "id", "value": "1.5" }]), "not a valid"),
        (json!([{ "row": 0, "column": "missing", "value": "1" }]), "unknown column"),
        (json!([{ "row": 0, "column": "big", "value": "9223372036854775808" }]), "not a valid"),
    ] {
        let err = ws.call("edit", json!({ "view_id": view, "edits": edits })).await.unwrap_err();
        assert!(err.contains(why), "{err}");
        assert_eq!(ws.all_rows(view).await, model);
    }
    assert!(ws.call("delete_rows", json!({ "view_id": 999, "start": 0, "count": 1 })).await.is_err());
    assert!(ws.call("drop_column", json!({ "table": "people", "name": "nope" })).await.is_err());
    assert_eq!(ws.all_rows(view).await, model);

    // A formula column, then filter + sort.
    ws.ok("add_formula_column", json!({ "table": "people", "name": "double", "formula": "[amount] * 2" })).await;
    for row in model.iter_mut() {
        let d = row[2].as_f64().map(|a| json!(a * 2.0)).unwrap_or(Value::Null);
        row.as_array_mut().unwrap().push(d);
    }
    let filtered_spec = json!({ "table": "people", "sort": [{ "column": "amount", "desc": true }],
        "filters": [{ "column": "amount", "op": "gte", "value": "0" }] });
    let filtered = open(&mut ws, filtered_spec.clone()).await;
    let mut expected_filtered: Vec<Value> = model.iter().filter(|r| r[2].as_f64().is_some_and(|a| a >= 0.0)).cloned().collect();
    expected_filtered.sort_by(|a, b| b[2].as_f64().partial_cmp(&a[2].as_f64()).unwrap());
    assert_eq!(ws.all_rows(filtered).await, expected_filtered);

    // Export respects the view and keeps exact text.
    let csv = export_csv(&server, &wb, filtered_spec.clone()).await;
    assert_eq!(csv[0], ["id", "name", "amount", "big", "note", "double"]);
    let ids: Vec<&str> = csv[1..].iter().map(|r| r[0].as_str()).collect();
    assert_eq!(ids, ["4", "1", "5", "3"]);
    assert_eq!(csv[2][3], "9007199254740995");
    assert_eq!(csv[1][4], "emoji 🚀");
    assert_eq!(csv[3][1], "Ünïcödé \"quoted\"");
    let full_before = export_csv(&server, &wb, json!({ "table": "people" })).await;
    assert_eq!(full_before.len(), 6);

    // Restart: stop the server (closing every database) and start a new one on the same data.
    ws.close().await;
    server.stop().await;
    let server = TestServer::start(dir.path()).await;
    let (_, list) = server.get("/api/workbooks").await;
    assert_eq!(list[0]["name"], "journey");
    assert_eq!(list[0]["summary"]["rows"], 5);
    let mut ws = server.ws(&wb).await;
    let view = open(&mut ws, json!({ "table": "people" })).await;
    assert_eq!(ws.all_rows(view).await, model, "reopened values are exactly the committed ones");
    let cols = ws.ok("view_info", json!({ "view_id": view })).await["columns"].clone();
    assert_eq!(cols[5]["formula"], "[amount] * 2", "formula metadata survives");
    let filtered = open(&mut ws, filtered_spec.clone()).await;
    assert_eq!(ws.all_rows(filtered).await, expected_filtered);
    assert_eq!(export_csv(&server, &wb, json!({ "table": "people" })).await, full_before);
    ws.close().await;
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn sheet_journey_survives_restart_and_xlsx_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let server = TestServer::start(dir.path()).await;
    let wb = server.create_workbook("sheets").await;
    let mut ws = server.ws(&wb).await;
    let sheet = ws.ok("sheet_create", json!({ "name": "Budget" })).await["index"].as_u64().unwrap();
    ws.ok("sheet_set", json!({ "sheet": sheet, "inputs": [
        { "row": 0, "column": 0, "value": "item" }, { "row": 0, "column": 1, "value": "cost" },
        { "row": 1, "column": 0, "value": "rent" }, { "row": 1, "column": 1, "value": "1200" },
        { "row": 2, "column": 0, "value": "food" }, { "row": 2, "column": 1, "value": "350.25" },
        { "row": 3, "column": 0, "value": "total" }, { "row": 3, "column": 1, "value": "=SUM(B2:B3)" },
    ] }))
    .await;
    // A failed batch (cell outside the grid) leaves the sheet as it was.
    let err = ws
        .call("sheet_set", json!({ "sheet": sheet, "inputs": [{ "row": 1, "column": 1, "value": "0" }, { "row": 2000000, "column": 0, "value": "x" }] }))
        .await
        .unwrap_err();
    assert!(err.contains("outside"), "{err}");
    let total = |rows: &Value| rows[3][1][0].clone();
    assert_eq!(total(&read(&mut ws).await), json!("1550.25"));

    // Undo/redo, then convert to a table.
    ws.ok("sheet_set", json!({ "sheet": sheet, "inputs": [{ "row": 1, "column": 1, "value": "1000" }] })).await;
    assert_eq!(total(&read(&mut ws).await), json!("1350.25"));
    assert_eq!(ws.ok("sheet_undo", json!({})).await, json!(true));
    assert_eq!(total(&read(&mut ws).await), json!("1550.25"));
    let table = ws.ok("sheet_to_table", json!({ "sheet": sheet, "name": "budget" })).await;
    assert_eq!(table["row_count"], 3);
    let q = ws.ok("query", json!({ "sql": "SELECT sum(cost) FROM budget WHERE item <> 'total'" })).await;
    assert_eq!(q["rows"][0][0], json!(1550.25));

    // Restart.
    ws.close().await;
    server.stop().await;
    let server = TestServer::start(dir.path()).await;
    let mut ws = server.ws(&wb).await;
    let rows = read(&mut ws).await;
    assert_eq!(total(&rows), json!("1550.25"));
    assert_eq!(rows[3][1][2], json!("=SUM(B2:B3)"), "the formula is kept, not just its value");
    // Recalculation still works after reopening.
    ws.ok("sheet_set", json!({ "sheet": 0, "inputs": [{ "row": 2, "column": 1, "value": "0.75" }] })).await;
    assert_eq!(total(&read(&mut ws).await), json!("1200.75"));

    // XLSX out and into a fresh workbook.
    let (status, _, bytes) = server.download(&format!("/api/workbooks/{wb}/export.xlsx")).await;
    assert_eq!(status, 200);
    let other = server.create_workbook("copy").await;
    let (status, body) = server.upload(&other, "budget.xlsx", bytes).await;
    assert_eq!(status, 200, "{body}");
    let mut ws2 = server.ws(&other).await;
    let rows2 = ws2.ok("sheet_fetch", json!({ "sheet": 0, "row_start": 0, "row_count": 4, "column_count": 2 })).await["rows"].clone();
    assert_eq!(rows2, read(&mut ws).await, "xlsx round trip keeps values and formulas");
    ws.close().await;
    ws2.close().await;

    // Duplicate copies everything, independently.
    let (status, copy) = server.send(Method::POST, &format!("/api/workbooks/{wb}/duplicate"), None).await;
    assert_eq!(status, 200);
    let mut ws3 = server.ws(copy["id"].as_str().unwrap()).await;
    let t = ws3.ok("tables", json!({})).await;
    assert_eq!(t[0]["name"], "budget");
    ws3.close().await;
    server.stop().await;
}

async fn read(ws: &mut Ws) -> Value {
    ws.ok("sheet_fetch", json!({ "sheet": 0, "row_start": 0, "row_count": 4, "column_count": 2 })).await["rows"].clone()
}
