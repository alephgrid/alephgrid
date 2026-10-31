//! HTTP + WebSocket integration tests against a real in-process server (real DuckDB and
//! IronCalc, temporary data directories). Covers 1.01, 1.08, 1.09 and 1.10.

mod common;

use std::collections::BTreeSet;

use common::*;
use reqwest::Method;
use alephgrid_core::{EditOutcome, Imported, QueryPreview, RowBlock, SheetBlock, SheetInfo, Stats, TableInfo, ViewInfo};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{json, Value};
use tokio::io::AsyncWriteExt;

fn xlsx_bytes() -> Vec<u8> {
    let dir = tempfile::tempdir().unwrap();
    let mut wb = alephgrid_core::Workbook::open_in_memory().unwrap();
    wb.with_sheets(|s| {
        let sheet = s.create("Budget")?;
        s.set(sheet.index, &[
            alephgrid_core::SheetInput { row: 0, column: 0, value: "2".into(), base: None },
            alephgrid_core::SheetInput { row: 1, column: 0, value: "=A1*21".into(), base: None },
        ])
    })
    .unwrap();
    let path = dir.path().join("budget.xlsx");
    wb.sheets().export_xlsx(&path).unwrap();
    std::fs::read(path).unwrap()
}

// ------------------------------------------------------------------ 1.01

#[tokio::test(flavor = "multi_thread")]
async fn import_returns_tagged_table_or_sheets() {
    let dir = tempfile::tempdir().unwrap();
    let server = TestServer::start(dir.path()).await;
    let wb = server.create_workbook("imports").await;

    let (status, body) = server.upload(&wb, "people.csv", b"name,age\nAda,36\nLinus,54\n".to_vec()).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body, json!({ "kind": "table", "table": { "name": "people", "row_count": 2, "column_count": 2 } }));
    assert!(matches!(serde_json::from_value::<Imported>(body).unwrap(), Imported::Table { .. }));

    let (status, body) = server.upload(&wb, "budget.xlsx", xlsx_bytes()).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["kind"], "sheets");
    assert_eq!(body["sheets"][0]["name"], "Budget");
    assert!(matches!(serde_json::from_value::<Imported>(body).unwrap(), Imported::Sheets { .. }));

    // Both landed in the workbook and the formula was evaluated.
    let (_, info) = server.get(&format!("/api/workbooks/{wb}")).await;
    assert_eq!(info["tables"][0]["name"], "people");
    let mut ws = server.ws(&wb).await;
    let block = ws.ok("sheet_fetch", json!({ "sheet": 0, "row_start": 0, "row_count": 2, "column_count": 1 })).await;
    assert_eq!(block["rows"][1][0][0], "42");

    // Bad input fails with a message and leaves nothing behind.
    let (status, body) = server.upload(&wb, "broken.parquet", b"not parquet".to_vec()).await;
    assert_eq!(status, 400);
    assert!(body["error"].as_str().unwrap().len() > 3);
    // A CSV that turns bad far past the type-sniffing sample fails as a whole.
    let mut late = String::from("n\n");
    for i in 0..300_000 {
        late.push_str(&format!("{i}\n"));
    }
    late.push_str("not a number\n");
    let (status, body) = server.upload(&wb, "late.csv", late.into_bytes()).await;
    assert_eq!(status, 400, "{body}");
    let (_, info) = server.get(&format!("/api/workbooks/{wb}")).await;
    let names: Vec<&str> = info["tables"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["people"], "failed imports leave no partial table");
    let (status, _) = server.upload(&wb, "../escape.csv", b"a\n1\n".to_vec()).await;
    assert_eq!(status, 200, "path components are stripped from the filename");
    let (status, _) = server.upload(&wb, ".hidden", b"a\n1\n".to_vec()).await;
    assert_eq!(status, 400);
    assert!(files_under(&dir.path().join(".uploads")).is_empty());
    ws.close().await;
    server.stop().await;
}

// ------------------------------------------------------------------ 1.08

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_same_name_uploads_do_not_collide() {
    let dir = tempfile::tempdir().unwrap();
    let server = TestServer::start(dir.path()).await;
    let a = server.create_workbook("a").await;
    let b = server.create_workbook("b").await;
    let body = |tag: &str, n: usize| {
        let mut s = String::from("tag,i\n");
        for i in 0..n {
            s.push_str(&format!("{tag},{i}\n"));
        }
        s.into_bytes()
    };
    // Four uploads of the same file name at once: two into one workbook, two into another.
    let (r1, r2, r3, r4) = tokio::join!(
        server.upload(&a, "data.csv", body("first", 20_000)),
        server.upload(&a, "data.csv", body("second", 30_000)),
        server.upload(&b, "data.csv", body("third", 40_000)),
        server.upload(&b, "data.csv", body("fourth", 50_000)),
    );
    for (status, body) in [&r1, &r2, &r3, &r4] {
        assert_eq!(*status, 200, "{body}");
    }
    for (wb, expected) in [(&a, [("first", 20_000), ("second", 30_000)]), (&b, [("third", 40_000), ("fourth", 50_000)])] {
        let mut ws = server.ws(wb).await;
        let p = ws.ok("query", json!({ "sql": "SELECT tag, count(*) AS n, count(DISTINCT i) AS d FROM (SELECT * FROM data UNION ALL SELECT * FROM data_2) GROUP BY 1 ORDER BY 1" })).await;
        let mut got: Vec<(String, i64, i64)> = p["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| (r[0].as_str().unwrap().to_string(), r[1].as_i64().unwrap(), r[2].as_i64().unwrap()))
            .collect();
        got.sort();
        let mut want: Vec<(String, i64, i64)> = expected.iter().map(|(t, n)| (t.to_string(), *n, *n)).collect();
        want.sort();
        assert_eq!(got, want, "each table holds exactly one upload's rows");
        ws.close().await;
    }
    assert!(files_under(&dir.path().join(".uploads")).is_empty(), "uploads are removed after import");
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn interrupted_upload_leaves_no_file_and_no_table() {
    let dir = tempfile::tempdir().unwrap();
    let server = TestServer::start(dir.path()).await;
    let wb = server.create_workbook("interrupted").await;
    // Promise 1 MB, send 100 bytes, then hang up.
    let mut tcp = tokio::net::TcpStream::connect(server.addr).await.unwrap();
    let head = format!(
        "POST /api/workbooks/{wb}/import?filename=big.csv HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 1000000\r\n\r\n"
    );
    tcp.write_all(head.as_bytes()).await.unwrap();
    tcp.write_all(&b"a,b\n1,2\n".repeat(13)[..100]).await.unwrap();
    tcp.flush().await.unwrap();
    // The server has created its scratch file once some bytes arrived.
    let uploads = dir.path().join(".uploads");
    eventually("upload file created", || !files_under(&uploads).is_empty()).await;
    drop(tcp);
    eventually("interrupted upload removed", || files_under(&uploads).is_empty()).await;
    let (_, info) = server.get(&format!("/api/workbooks/{wb}")).await;
    assert_eq!(info["tables"], json!([]), "no partial table");
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn startup_removes_stale_scratch_files_but_not_data() {
    let dir = tempfile::tempdir().unwrap();
    let server = TestServer::start(dir.path()).await;
    let wb = server.create_workbook("keep me").await;
    server.upload(&wb, "t.csv", b"x\n1\n".to_vec()).await;
    server.stop().await;
    for scratch in [".uploads/abc/half.csv", ".exports/def/export"] {
        let p = dir.path().join(scratch);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, b"partial").unwrap();
    }
    let server = TestServer::start(dir.path()).await;
    assert!(!dir.path().join(".uploads").exists());
    assert!(!dir.path().join(".exports").exists());
    let (_, list) = server.get("/api/workbooks").await;
    assert_eq!(list.as_array().unwrap().len(), 1, "scratch dirs are never listed as workbooks");
    let (_, info) = server.get(&format!("/api/workbooks/{wb}")).await;
    assert_eq!(info["tables"][0]["row_count"], 1);
    server.stop().await;
}

// ------------------------------------------------------------------ 1.09

#[tokio::test(flavor = "multi_thread")]
async fn delete_disconnects_clients_moves_to_trash_and_restores() {
    let dir = tempfile::tempdir().unwrap();
    let server = TestServer::start(dir.path()).await;
    let wb = server.create_workbook("to delete").await;
    server.upload(&wb, "t.csv", b"x\n1\n2\n3\n".to_vec()).await;
    let mut ws = server.ws(&wb).await;
    let view = ws.ok("open_view", json!({ "spec": { "table": "t" } })).await["view_id"].as_u64().unwrap();

    let (status, _) = server.send(Method::DELETE, &format!("/api/workbooks/{wb}"), None).await;
    assert_eq!(status, 204);
    let ev = ws.event("workbook_deleted").await.expect("clients are told");
    assert_eq!(ev["event"], "workbook_deleted");
    assert!(ws.closed().await, "the server closes the socket");

    // Gone from the library, present in the trash, not reachable.
    let (_, list) = server.get("/api/workbooks").await;
    assert!(list.as_array().unwrap().iter().all(|w| w["id"] != wb.as_str()));
    let (_, trash) = server.get("/api/trash").await;
    assert_eq!(trash[0]["id"], wb.as_str());
    assert!(trash[0]["deleted_at"].as_u64().unwrap() > 0);
    let (status, _) = server.get(&format!("/api/workbooks/{wb}")).await;
    assert_eq!(status, 404, "a trashed workbook is not found");
    assert!(Ws::try_connect(server.addr, &wb).await.is_err());
    let (status, _) = server.send(Method::DELETE, &format!("/api/workbooks/{wb}"), None).await;
    assert_eq!(status, 404, "deleting twice fails cleanly: it is no longer there");

    // Restore brings back the data unchanged.
    let (status, meta) = server.send(Method::POST, &format!("/api/trash/{wb}/restore"), None).await;
    assert_eq!(status, 200, "{meta}");
    assert!(meta.get("deleted_at").is_none());
    let mut ws = server.ws(&wb).await;
    let view2 = ws.ok("open_view", json!({ "spec": { "table": "t" } })).await["view_id"].as_u64().unwrap();
    assert_eq!(ws.all_rows(view2).await, vec![json!([1]), json!([2]), json!([3])]);
    let _ = view;
    ws.close().await;

    // Delete forever.
    let (status, body) = server.send(Method::DELETE, &format!("/api/workbooks/{wb}"), None).await;
    assert_eq!(status, 204, "{body}");
    let (status, _) = server.send(Method::DELETE, &format!("/api/trash/{wb}"), None).await;
    assert_eq!(status, 204);
    assert!(!dir.path().join(".trash").join(&wb).exists());
    assert!(!dir.path().join(&wb).exists());
    let (status, _) = server.send(Method::POST, &format!("/api/trash/{wb}/restore"), None).await;
    assert_eq!(status, 404, "a purged workbook is not in the trash");
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn delete_waits_for_the_running_operation() {
    let dir = tempfile::tempdir().unwrap();
    let server = TestServer::start(dir.path()).await;
    let wb = server.create_workbook("busy").await;
    let mut ws = server.ws(&wb).await;
    ws.ok("tables", json!({})).await;
    // A slow write runs while the delete arrives.
    let addr = server.addr;
    let wb2 = wb.clone();
    let slow = tokio::spawn(async move {
        let mut w = Ws::connect(addr, &wb2).await;
        w.call("create_demo_table", json!({ "rows": 3_000_000 })).await
    });
    eventually("slow op holds the workbook", || {
        // The demo table is being written once the database grows past its empty size.
        files_under(&dir.path().join(&wb)).iter().map(|p| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0)).sum::<u64>() > 1_000_000
    })
    .await;
    let (status, _) = server.send(Method::DELETE, &format!("/api/workbooks/{wb}"), None).await;
    assert_eq!(status, 204);
    let slow_result = slow.await.unwrap();
    // The write either finished before the database was closed or was refused; never torn.
    server.send(Method::POST, &format!("/api/trash/{wb}/restore"), None).await;
    let mut ws2 = server.ws(&wb).await;
    let tables = ws2.ok("tables", json!({})).await;
    match slow_result {
        Ok(info) => {
            assert_eq!(info["row_count"], 3_000_000);
            assert_eq!(tables[0]["row_count"], 3_000_000);
        }
        Err(e) => {
            assert!(e.contains("deleted") || e.contains("closed"), "{e}");
            assert!(tables.as_array().unwrap().iter().all(|t| t["row_count"] == 3_000_000));
        }
    }
    let _ = ws.closed().await;
    ws2.close().await;
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn empty_trash_removes_everything_and_restore_survives_restart() {
    let dir = tempfile::tempdir().unwrap();
    let server = TestServer::start(dir.path()).await;
    let a = server.create_workbook("a").await;
    let b = server.create_workbook("b").await;
    server.send(Method::DELETE, &format!("/api/workbooks/{a}"), None).await;
    server.send(Method::DELETE, &format!("/api/workbooks/{b}"), None).await;
    server.stop().await;
    let server = TestServer::start(dir.path()).await;
    let (_, trash) = server.get("/api/trash").await;
    assert_eq!(trash.as_array().unwrap().len(), 2, "the trash survives a restart");
    let (status, _) = server.send(Method::DELETE, "/api/trash", None).await;
    assert_eq!(status, 204);
    let (_, trash) = server.get("/api/trash").await;
    assert_eq!(trash, json!([]));
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn unicode_names_download_safely() {
    let dir = tempfile::tempdir().unwrap();
    let server = TestServer::start(dir.path()).await;
    let wb = server.create_workbook("東京 \"report\"\r\n").await;
    server.upload(&wb, "données.csv", b"x\n1\n".to_vec()).await;
    let mut ws = server.ws(&wb).await;
    ws.ok("sheet_create", json!({ "name": "S" })).await;
    ws.close().await;
    let (status, headers, bytes) = server.download(&format!("/api/workbooks/{wb}/export.xlsx")).await;
    assert_eq!(status, 200);
    assert!(bytes.starts_with(b"PK"), "an xlsx zip");
    let cd = headers["content-disposition"].to_str().unwrap();
    assert!(cd.contains("filename*=UTF-8''%E6%9D%B1%E4%BA%AC"), "{cd}");
    let spec = urlencode(r#"{"table":"données"}"#);
    let (status, headers, bytes) = server.download(&format!("/api/workbooks/{wb}/export?spec={spec}&format=csv")).await;
    assert_eq!(status, 200);
    assert_eq!(String::from_utf8(bytes).unwrap(), "x\n1\n");
    assert!(headers["content-disposition"].to_str().unwrap().contains("donn%C3%A9es.csv"));
    server.stop().await;
}

// ------------------------------------------------------------------ 1.10

/// Deserialize into the declared result type and serialize back: equal JSON means the reply
/// has exactly the declared shape (no missing, extra or mistyped fields).
fn conforms<T: DeserializeOwned + Serialize>(op: &str, v: &Value) {
    let typed: T = serde_json::from_value(v.clone()).unwrap_or_else(|e| panic!("{op}: {e}\n{v}"));
    assert_eq!(&serde_json::to_value(typed).unwrap(), v, "{op} reply does not match its declared type");
}

fn check_result(op: &str, ty: &str, v: &Value) {
    match ty {
        "AnalysisResult" => conforms::<alephgrid_core::AnalysisResult>(op,v),
        "SavedAnalysis" => conforms::<alephgrid_core::SavedAnalysis>(op,v),
        "SavedAnalysis[]" => conforms::<Vec<alephgrid_core::SavedAnalysis>>(op,v),
        "ReplacePreview" => conforms::<alephgrid_core::ReplacePreview>(op,v),
        "ReplaceOutcome" => conforms::<alephgrid_core::ReplaceOutcome>(op,v),
        "Presentation" => conforms::<alephgrid_core::Presentation>(op,v),
        "WorkbookFunction" => conforms::<alephgrid_core::WorkbookFunction>(op, v),
        "WorkbookFunction[]" => conforms::<Vec<alephgrid_core::WorkbookFunction>>(op, v),
        "FunctionTestResult[]" => conforms::<Vec<alephgrid_core::FunctionTestResult>>(op, v),
        "TableInfo[]" => conforms::<Vec<TableInfo>>(op, v),
        "TableInfo" => conforms::<TableInfo>(op, v),
        "ViewInfo" => conforms::<ViewInfo>(op, v),
        "RowBlock" => conforms::<RowBlock>(op, v),
        "EditOutcome" => conforms::<EditOutcome>(op, v),
        "Stats" => conforms::<Stats>(op, v),
        "QueryPreview" => conforms::<QueryPreview>(op, v),
        "SheetInfo[]" => conforms::<Vec<SheetInfo>>(op, v),
        "SheetInfo" => conforms::<SheetInfo>(op, v),
        "SheetBlock" => conforms::<SheetBlock>(op, v),
        "SheetCopy" => conforms::<alephgrid_core::SheetCopy>(op, v),
        "NamedRange[]" => conforms::<Vec<alephgrid_core::NamedRange>>(op, v),
        "CellDetail" => conforms::<alephgrid_core::CellDetail>(op, v),
        "FormulaProblem | null" => conforms::<Option<alephgrid_core::FormulaProblem>>(op, v),
        "CellArea" => conforms::<alephgrid_core::CellArea>(op, v),
        "SavedView" => conforms::<alephgrid_core::SavedView>(op, v),
        "SavedView[]" => conforms::<Vec<alephgrid_core::SavedView>>(op, v),
        "HistoryEntry[]" => conforms::<Vec<alephgrid_core::HistoryEntry>>(op, v),
        "UndoOutcome | null" => conforms::<Option<alephgrid_core::UndoOutcome>>(op, v),
        "QueryResult" => conforms::<alephgrid_core::QueryResult>(op, v),
        "TableSchema[]" => conforms::<Vec<alephgrid_core::TableSchema>>(op, v),
        "QueryExport" => conforms::<alephgrid_server::ws::QueryExport>(op, v),
        "SqlChangeOutcome" => conforms::<alephgrid_core::SqlChangeOutcome>(op, v),
        "SavedQuery" => conforms::<alephgrid_core::SavedQuery>(op, v),
        "SavedQuery[]" => conforms::<Vec<alephgrid_core::SavedQuery>>(op, v),
        "QueryVersion[]" => conforms::<Vec<alephgrid_core::QueryVersion>>(op, v),
        "MaterializedTable" => conforms::<alephgrid_core::MaterializedTable>(op, v),
        "MaterializedTable[]" => conforms::<Vec<alephgrid_core::MaterializedTable>>(op, v),
        "Dashboard" => conforms::<alephgrid_core::reports::Dashboard>(op, v),
        "Dashboard[]" => conforms::<Vec<alephgrid_core::reports::Dashboard>>(op, v),
        "DashboardResult" => conforms::<alephgrid_core::reports::DashboardResult>(op, v),
        "ViewSpec" => conforms::<alephgrid_core::ViewSpec>(op, v),
        "string[]" => conforms::<Vec<String>>(op, v),
        "Validation[]" => conforms::<Vec<alephgrid_core::tools::Validation>>(op, v),
        "ValidationReport" => conforms::<alephgrid_core::tools::ValidationReport>(op, v),
        "TransformPreview" => conforms::<alephgrid_core::tools::TransformPreview>(op, v),
        "FormulaInspection" => conforms::<alephgrid_core::tools::FormulaInspection>(op, v),
        "boolean" => assert!(v.is_boolean(), "{op}: {v}"),
        "string" => assert!(v.is_string(), "{op}: {v}"),
        "null" => assert!(v.is_null(), "{op}: {v}"),
        other => panic!("no checker for result type {other}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn every_operation_replies_with_its_declared_type() {
    let dir = tempfile::tempdir().unwrap();
    let server = TestServer::start(dir.path()).await;
    let wb = server.create_workbook("contract").await;
    server.upload(&wb, "t.csv", b"n,s\n1,a\n2,b\n3,c\n".to_vec()).await;
    let mut ws = server.ws(&wb).await;
    let mut seen = BTreeSet::new();
    let declared: std::collections::HashMap<&str, &str> = alephgrid_server::protocol::RESULTS.iter().copied().collect();
    let script = common::operation_script();
    for (op, _) in &script {
        seen.insert(*op);
    }
    let mut saved_id = String::new();
    let mut query_id = String::new();
    let mut analysis_id = String::new();
    let mut dashboard_id = String::new();
    let mut preview_revision = Value::Null;
    let mut undo_revision = Value::Null;
    for (op, mut params) in script {
        if params["analysis"]=="ANALYSIS" {params["analysis"]=json!(analysis_id);}
        if params["tiles"][0]["analysis"]=="ANALYSIS" {params["tiles"][0]["analysis"]=json!(analysis_id);}
        if params["dashboard"]=="DASHBOARD" {params["dashboard"]=json!(dashboard_id);}
        if params["revision"]=="PREVIEW_REV" {params["revision"]=preview_revision.clone();}
        if params["revision"]=="UNDO_REV" {params["revision"]=undo_revision.clone();}
        if params["saved"] == "PLACEHOLDER" {
            params["saved"] = json!(saved_id);
        }
        if params["saved"] == "QUERY" {
            params["saved"] = json!(query_id);
        }
        let v = ws.call(op, params.clone()).await.unwrap_or_else(|e| panic!("{op} {params}: {e}"));
        check_result(op, declared[op], &v);
        if op=="save_analysis" {analysis_id=v["id"].as_str().unwrap().to_string();}
        if op=="save_dashboard" {dashboard_id=v["id"].as_str().unwrap().to_string();}
        if op=="replace_preview" {preview_revision=v["sample"]["revision"].clone();}
        if op=="replace_apply" {undo_revision=v["undo_revision"].clone();}
        if op == "save_view" {
            saved_id = v["id"].as_str().unwrap().to_string();
        }
        if op == "save_query" {
            query_id = v["id"].as_str().unwrap().to_string();
        }
    }
    let all: BTreeSet<&str> = declared.keys().copied().collect();
    assert_eq!(seen, all, "every declared operation is exercised");

    // Malformed frames get an error reply carrying the request id.
    let err = ws.call("no_such_op", json!({})).await.unwrap_err();
    assert!(err.contains("bad request"), "{err}");
    let err = ws.call("fetch", json!({ "view_id": "x" })).await.unwrap_err();
    assert!(err.contains("bad request"), "{err}");
    ws.close().await;
    server.stop().await;
}

/// F.09: a sheet edit reaches the tables whose formulas read the cell, and the tables that
/// look those up; every session hears about it, the one that edited the sheet included.
#[tokio::test(flavor = "multi_thread")]
async fn sheet_cells_read_by_tables_notify_their_readers() {
    let dir = tempfile::tempdir().unwrap();
    let server = TestServer::start(dir.path()).await;
    let wb = server.create_workbook("links").await;
    let mut a = server.ws(&wb).await;
    let mut b = server.ws(&wb).await;
    a.ok("create_table_from_query", json!({ "name": "customers", "sql": "SELECT * FROM (VALUES (1, 'Ann')) v(id, name)" })).await;
    a.ok("create_table_from_query", json!({ "name": "orders", "sql": "SELECT * FROM (VALUES (10, 1)) v(id, cid)" })).await;
    a.ok("sheet_create", json!({ "name": "Settings" })).await;
    a.ok("sheet_set", json!({ "sheet": 0, "inputs": [{ "row": 0, "column": 0, "value": " (vip)" }] })).await;
    a.ok("add_formula_column", json!({ "table": "customers", "name": "label", "formula": "=[@name]&Settings!A1", "language": "sheet" })).await;
    a.ok("add_formula_column", json!({ "table": "orders", "name": "who", "formula": "=XLOOKUP([@cid],customers[id],customers[label])", "language": "sheet" })).await;
    let id = a.send("sheet_set", json!({ "sheet": 0, "inputs": [{ "row": 0, "column": 0, "value": " (gold)" }] })).await;
    let rev = a.reply(id).await.unwrap()["revision"].clone();
    for ws in [&mut a, &mut b] {
        let mut tables = Vec::new();
        for _ in 0..2 {
            let e = ws.event("cells_changed").await.unwrap();
            assert_eq!((e["origin"].clone(), e["revision"].clone()), (json!(0), rev.clone()), "{e}");
            tables.push(e["table"].as_str().unwrap().to_string());
        }
        tables.sort();
        assert_eq!(tables, ["customers", "orders"]);
    }
    let v = b.ok("open_view", json!({ "spec": { "table": "orders" } })).await["view_id"].as_u64().unwrap();
    assert_eq!(b.ok("fetch", json!({ "view_id": v, "row_start": 0, "row_count": 1 })).await["rows"], json!([[10, 1, "Ann (gold)"]]));
    let err = a.call("sheet_delete", json!({ "sheet": 0 })).await.unwrap_err();
    assert!(err.contains("read by formula column"), "{err}");
    a.close().await;
    b.close().await;
    server.stop().await;
}

/// F.09: a sheet cell that reads a table is recomputed in the same change as the table, and
/// every session is told to refetch the sheets.
#[tokio::test(flavor = "multi_thread")]
async fn sheet_cells_that_read_tables_follow_table_edits() {
    let dir = tempfile::tempdir().unwrap();
    let server = TestServer::start(dir.path()).await;
    let wb = server.create_workbook("tablecells").await;
    let mut a = server.ws(&wb).await;
    let mut b = server.ws(&wb).await;
    a.ok("create_table_from_query", json!({ "name": "orders", "sql": "SELECT * FROM (VALUES (1, 2.0), (2, 1.0)) v(id, amount)" })).await;
    a.ok("sheet_create", json!({ "name": "Report" })).await;
    let problem = a.ok("sheet_check_formula", json!({ "sheet": 0, "row": 0, "column": 1, "formula": "=SUM(orders[nope])" })).await;
    assert_eq!(problem["position"], json!(5), "{problem}");
    a.ok("sheet_set", json!({ "sheet": 0, "inputs": [{ "row": 0, "column": 1, "value": "=SUM(orders[amount])" }] })).await;
    let cell = |block: &Value| block["rows"][0][1].clone();
    assert_eq!(cell(&a.ok("sheet_fetch", json!({ "sheet": 0, "row_start": 0, "row_count": 1, "column_count": 2 })).await), json!(["3", "right", "=SUM(orders[amount])"]));
    let v = b.ok("open_view", json!({ "spec": { "table": "orders" } })).await["view_id"].as_u64().unwrap();
    let id = b.send("edit", json!({ "view_id": v, "edits": [{ "row": 0, "column": "amount", "value": "5" }] })).await;
    let rev = b.reply(id).await.unwrap()["revision"].clone();
    for ws in [&mut a, &mut b] {
        loop {
            // Entering the table cell itself was announced at an earlier revision.
            let e = ws.event("sheet_changed").await.unwrap();
            if e["revision"] == rev {
                assert_eq!((e["sheet"].clone(), e["origin"].clone()), (Value::Null, json!(0)), "{e}");
                break;
            }
        }
    }
    assert_eq!(cell(&a.ok("sheet_fetch", json!({ "sheet": 0, "row_start": 0, "row_count": 1, "column_count": 2 })).await), json!(["6", "right", "=SUM(orders[amount])"]));
    a.close().await;
    b.close().await;
    server.stop().await;
}

/// F.09: after a change, a large table's stored formula results are recalculated in the
/// background; sorted views keep the last results, flagged, until then, and every session
/// hears when they are replaced. A cancelled recalculation says so.
#[tokio::test(flavor = "multi_thread")]
async fn stored_results_are_recalculated_in_the_background() {
    let dir = tempfile::tempdir().unwrap();
    let server = TestServer::start(dir.path()).await;
    let wb = server.create_workbook("results").await;
    let mut a = server.ws(&wb).await;
    let mut b = server.ws(&wb).await;
    // 200,000 orders (the threshold for stored results) over 1,000 customers.
    a.ok("create_table_from_query", json!({ "name": "customers", "sql": "SELECT i AS id, 'c' || lpad(CAST(i AS VARCHAR), 4, '0') AS name FROM range(1000) t(i)" })).await;
    a.ok("create_table_from_query", json!({ "name": "orders", "sql": "SELECT i AS id, i % 1000 AS cid FROM range(200000) t(i)" })).await;
    a.ok("add_formula_column", json!({ "table": "orders", "name": "who", "formula": "=XLOOKUP([@cid],customers[id],customers[name])", "language": "sheet" })).await;
    let sorted = json!({ "spec": { "table": "orders", "sort": [{ "column": "who", "desc": true }] } });
    let v = b.ok("open_view", sorted.clone()).await;
    assert!(v.get("stale").is_none(), "{v}");
    let first = b.ok("fetch", json!({ "view_id": v["view_id"], "row_start": 0, "row_count": 1 })).await;
    assert_eq!(first["rows"][0], json!([999, 999, "c0999"]));
    // Renaming customer 999 makes orders' stored results stale; they are rebuilt in the
    // background and every session is told.
    let c = a.ok("open_view", json!({ "spec": { "table": "customers" } })).await["view_id"].as_u64().unwrap();
    let id = a.send("edit", json!({ "view_id": c, "edits": [{ "row": 999, "column": "name", "value": "a999" }] })).await;
    let rev = a.reply(id).await.unwrap()["revision"].clone();
    for ws in [&mut a, &mut b] {
        loop {
            let e = ws.event("table_changed").await.unwrap();
            if e["table"] == "orders" && e["revision"] == rev {
                assert_eq!(e["origin"], json!(0), "{e}");
                break;
            }
        }
    }
    let v = b.ok("open_view", sorted.clone()).await;
    assert!(v.get("stale").is_none(), "{v}");
    let first = b.ok("fetch", json!({ "view_id": v["view_id"], "row_start": 0, "row_count": 1 })).await;
    assert_eq!(first["rows"][0], json!([998, 998, "c0998"]), "the renamed customer sorts last now");
    // Nothing to cancel now; a retry request is accepted.
    assert_eq!(a.ok("cancel_recalculation", json!({ "table": "orders" })).await, json!(false));
    assert_eq!(a.ok("recalculate", json!({ "table": "orders" })).await, json!(true));
    a.close().await;
    b.close().await;
    server.stop().await;
}
