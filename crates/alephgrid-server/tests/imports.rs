//! 2.05 over HTTP: staged uploads, preview with type overrides, actionable errors with
//! retry, cancellation of a running import, progress and upload expiry.

mod common;

use std::time::{Duration, Instant};

use common::*;
use reqwest::Method;
use serde_json::{json, Value};

async fn stage(server: &TestServer, wb: &str, filename: &str, bytes: Vec<u8>) -> (u16, Value) {
    let res = server
        .http
        .post(server.url(&format!("/api/workbooks/{wb}/uploads?filename={}", urlencode(filename))))
        .body(bytes)
        .send()
        .await
        .unwrap();
    let status = res.status().as_u16();
    (status, res.json().await.unwrap())
}

async fn tables(server: &TestServer, wb: &str) -> Vec<String> {
    let (_, info) = server.get(&format!("/api/workbooks/{wb}")).await;
    info["tables"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap().to_string()).collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn stage_preview_override_and_import() {
    let dir = tempfile::tempdir().unwrap();
    let server = TestServer::start(dir.path()).await;
    let wb = server.create_workbook("staged").await;
    let (status, body) = stage(&server, &wb, "codes.csv", b"code,amount,when\n007,1.50,2024-02-29\n042,2.25,2024-03-01\n".to_vec()).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["filename"], "codes.csv");
    assert_eq!(body["preview"]["kind"], "table");
    let preview = &body["preview"]["preview"];
    assert_eq!(preview["suggested_name"], "codes");
    let types: Vec<&str> = preview["columns"].as_array().unwrap().iter().map(|c| c["sql_type"].as_str().unwrap()).collect();
    assert_eq!(types, ["VARCHAR", "DOUBLE", "DATE"]);
    assert_eq!(preview["rows"][0], json!(["007", 1.5, "2024-02-29"]));
    assert!(tables(&server, &wb).await.is_empty(), "staging imports nothing");

    let upload = body["upload_id"].as_str().unwrap().to_string();
    let base = format!("/api/workbooks/{wb}/uploads/{upload}");
    let (status, p) = server.send(Method::POST, &format!("{base}/preview"), Some(json!({ "types": [{ "name": "amount", "sql_type": "DECIMAL(10,2)" }] }))).await;
    assert_eq!(status, 200, "{p}");
    assert_eq!(p["preview"]["rows"][0][1], "1.50");

    let (status, imported) = server
        .send(Method::POST, &format!("{base}/import"), Some(json!({ "table": "Price list", "types": [{ "name": "amount", "sql_type": "DECIMAL(10,2)" }] })))
        .await;
    assert_eq!(status, 200, "{imported}");
    assert_eq!(imported["table"]["name"], "Price_list");
    let mut ws = server.ws(&wb).await;
    let q = ws.ok("query", json!({ "sql": "SELECT code, amount, typeof(amount) FROM Price_list ORDER BY code" })).await;
    assert_eq!(q["rows"], json!([["007", "1.50", "DECIMAL(10,2)"], ["042", "2.25", "DECIMAL(10,2)"]]));
    ws.close().await;

    // The upload is consumed.
    let (status, err) = server.send(Method::POST, &format!("{base}/import"), Some(json!({}))).await;
    assert_eq!(status, 400);
    assert!(err["error"].as_str().unwrap().contains("expired"), "{err}");
    assert!(files_under(&dir.path().join(".uploads")).is_empty());
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn failed_import_explains_and_retries_without_reupload() {
    let dir = tempfile::tempdir().unwrap();
    let server = TestServer::start(dir.path()).await;
    let wb = server.create_workbook("retry").await;
    let mut csv = String::from("n,label\n");
    for i in 0..120_000 {
        csv.push_str(&format!("{i},x\n"));
    }
    csv.push_str("oops,y\n");
    let (_, body) = stage(&server, &wb, "late.csv", csv.into_bytes()).await;
    let base = format!("/api/workbooks/{wb}/uploads/{}", body["upload_id"].as_str().unwrap());
    let (status, err) = server.send(Method::POST, &format!("{base}/import"), Some(json!({}))).await;
    assert_eq!(status, 400);
    assert_eq!(err["problem"]["line"], 120_002);
    assert_eq!(err["problem"]["column"], "n");
    assert_eq!(err["problem"]["suggested_type"], "VARCHAR");
    assert!(err["error"].as_str().unwrap().starts_with("Line 120002:"), "{err}");
    assert!(tables(&server, &wb).await.is_empty());
    // Same upload, suggested type.
    let (status, imported) =
        server.send(Method::POST, &format!("{base}/import"), Some(json!({ "types": [{ "name": "n", "sql_type": "VARCHAR" }] }))).await;
    assert_eq!(status, 200, "{imported}");
    assert_eq!(imported["table"]["row_count"], 120_001);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn uploads_are_private_to_their_workbook_and_can_be_cancelled() {
    let dir = tempfile::tempdir().unwrap();
    let server = TestServer::start(dir.path()).await;
    let a = server.create_workbook("a").await;
    let b = server.create_workbook("b").await;
    let (_, body) = stage(&server, &a, "t.csv", b"x\n1\n".to_vec()).await;
    let upload = body["upload_id"].as_str().unwrap();
    let (status, _) = server.send(Method::POST, &format!("/api/workbooks/{b}/uploads/{upload}/import"), Some(json!({}))).await;
    assert_eq!(status, 400, "another workbook cannot import it");
    assert!(tables(&server, &b).await.is_empty());
    let (status, _) = server.send(Method::DELETE, &format!("/api/workbooks/{a}/uploads/{upload}"), None).await;
    assert_eq!(status, 204);
    assert!(files_under(&dir.path().join(".uploads")).is_empty(), "cancel removes the file");
    let (status, _) = server.send(Method::POST, &format!("/api/workbooks/{a}/uploads/{upload}/import"), Some(json!({}))).await;
    assert_eq!(status, 400);
    // A file that cannot be read is rejected at staging, with nothing left behind.
    let (status, err) = stage(&server, &a, "bad.parquet", b"nope".to_vec()).await;
    assert_eq!(status, 400, "{err}");
    assert!(files_under(&dir.path().join(".uploads")).is_empty());
    // xlsx staging previews sheets.
    let mut src = alephgrid_core::Workbook::open_in_memory().unwrap();
    src.with_sheets(|s| s.create("Only")).unwrap();
    let x = dir.path().join("x.xlsx");
    src.sheets().export_xlsx(&x).unwrap();
    let (status, body) = stage(&server, &a, "x.xlsx", std::fs::read(&x).unwrap()).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["preview"], json!({ "kind": "sheets", "sheets": [{ "index": 0, "name": "Only", "rows": 0, "columns": 0, "layout": 0, "frozen_columns": 0, "frozen_rows": 0 }], "warnings": [] }));
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_running_import_reports_progress_and_can_be_cancelled() {
    let dir = tempfile::tempdir().unwrap();
    let server = TestServer::start(dir.path()).await;
    let wb = server.create_workbook("cancel").await;
    let mut csv = String::with_capacity(120_000_000);
    csv.push_str("a,b,c,d\n");
    for i in 0..6_000_000u64 {
        csv.push_str(&format!("{i},{},{},text {}\n", i * 7 % 1000, i % 13, i % 101));
    }
    let (_, body) = stage(&server, &wb, "big.csv", csv.into_bytes()).await;
    let upload = body["upload_id"].as_str().unwrap().to_string();

    let (status, _) = server.send(Method::DELETE, &format!("/api/workbooks/{wb}/jobs/j1"), None).await;
    assert_eq!(status, 409, "nothing to cancel yet");
    let http = server.http.clone();
    let url = server.url(&format!("/api/workbooks/{wb}/uploads/{upload}/import"));
    let import = tokio::spawn(async move {
        let res = http.post(url).json(&json!({ "job": "j1" })).send().await.unwrap();
        (res.status().as_u16(), res.json::<Value>().await.unwrap())
    });
    // Wait until the job runs and reports progress.
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut saw_progress = false;
    loop {
        let (_, st) = server.get(&format!("/api/workbooks/{wb}/jobs/j1")).await;
        if st["running"] == true && st["percentage"].is_number() {
            saw_progress = true;
            let pct = st["percentage"].as_f64().unwrap();
            assert!((0.0..=100.0).contains(&pct), "{st}");
            break;
        }
        if import.is_finished() || Instant::now() > deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(saw_progress, "the import finished before progress could be observed; make the file larger");
    let (status, _) = server.send(Method::DELETE, &format!("/api/workbooks/{wb}/jobs/j1"), None).await;
    assert_eq!(status, 202);
    let (status, body) = import.await.unwrap();
    assert_eq!(status, 400, "{body}");
    assert_eq!(body["error"], "Cancelled.");
    assert!(tables(&server, &wb).await.is_empty(), "a cancelled import leaves no table");
    let (_, st) = server.get(&format!("/api/workbooks/{wb}/jobs/j1")).await;
    assert_eq!(st["running"], false);
    // The workbook keeps working, and the upload can still be imported.
    let (status, imported) = server.send(Method::POST, &format!("/api/workbooks/{wb}/uploads/{upload}/import"), Some(json!({}))).await;
    assert_eq!(status, 200, "{imported}");
    assert_eq!(imported["table"]["row_count"], 6_000_000);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn unclaimed_uploads_expire() {
    let dir = tempfile::tempdir().unwrap();
    let server = TestServer::start(dir.path()).await;
    let wb = server.create_workbook("ttl").await;
    let (_, body) = stage(&server, &wb, "old.csv", b"x\n1\n".to_vec()).await;
    let old = body["upload_id"].as_str().unwrap().to_string();
    // Age the upload past the TTL, then stage another: the sweep removes the old one.
    {
        let mut uploads = server.state.uploads.lock().unwrap();
        let u = uploads.get_mut(&old).unwrap();
        u.touched = Instant::now() - alephgrid_server::state::UPLOAD_TTL - Duration::from_secs(1);
    }
    let (_, fresh) = stage(&server, &wb, "new.csv", b"x\n2\n".to_vec()).await;
    let (status, _) = server.send(Method::POST, &format!("/api/workbooks/{wb}/uploads/{old}/import"), Some(json!({}))).await;
    assert_eq!(status, 400);
    assert_eq!(files_under(&dir.path().join(".uploads")).len(), 1, "only the fresh upload remains");
    let (status, _) = server
        .send(Method::POST, &format!("/api/workbooks/{wb}/uploads/{}/import", fresh["upload_id"].as_str().unwrap()), Some(json!({})))
        .await;
    assert_eq!(status, 200);
    server.stop().await;
}
