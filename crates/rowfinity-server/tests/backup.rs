//! 3.08: online backups during a stream of acknowledged edits hold exactly one revision
//! per workbook; restore verifies checksums, refuses non-empty targets and damaged
//! backups without writing, and yields identical data; offline backup fails closed while
//! a server holds the data directory.

mod common;

use common::*;
use reqwest::Method;
use serde_json::{json, Value};

async fn backup(server: &TestServer) -> (std::path::PathBuf, Value) {
    let (code, body) = server.send(Method::POST, "/api/admin/backups", None).await;
    assert_eq!(code, 200, "{body}");
    (std::path::PathBuf::from(body["path"].as_str().unwrap()), body["manifest"].clone())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_backup_taken_during_edits_restores_exactly_one_revision() {
    let dir = tempfile::tempdir().unwrap();
    let server = TestServer::start(dir.path()).await;
    let wb = server.create_workbook("busy").await;
    let quiet = server.create_workbook("quiet").await;
    let mut q = server.ws(&quiet).await;
    q.ok("create_table_from_query", json!({ "name": "t", "sql": "SELECT 'unchanged' AS s" })).await;
    q.ok("sheet_create", json!({ "name": "S" })).await;
    q.ok("sheet_set", json!({ "sheet": 0, "inputs": [{ "row": 0, "column": 0, "value": "6" }, { "row": 0, "column": 1, "value": "=A1*7" }] })).await;
    q.close().await;
    let mut ws = server.ws(&wb).await;
    ws.ok("create_table_from_query", json!({ "name": "log", "sql": "SELECT 0 AS k WHERE false" })).await;
    let view = ws.ok("open_view", json!({ "spec": { "table": "log" } })).await["view_id"].as_u64().unwrap();
    // Edits keep coming while backups are taken; each acknowledged edit's revision is logged.
    let writer = tokio::spawn(async move {
        let mut acked: Vec<(u64, u64)> = Vec::new(); // (revision, k)
        for k in 0..300u64 {
            let id = ws.send("edit", json!({ "view_id": view, "edits": [{ "row": k, "column": "k", "value": k.to_string() }] })).await;
            let r = ws.reply(id).await.unwrap();
            assert_eq!(r["ok"], true, "{r}");
            acked.push((r["revision"].as_u64().unwrap(), k));
        }
        ws.close().await;
        acked
    });
    let mut backups = Vec::new();
    for _ in 0..3 {
        backups.push(backup(&server).await);
    }
    let acked = writer.await.unwrap();
    let revs: Vec<u64> = backups
        .iter()
        .map(|(_, m)| m["workbooks"].as_array().unwrap().iter().find(|w| w["id"] == wb).unwrap()["revision"].as_u64().unwrap())
        .collect();
    let (first, last) = (acked.first().unwrap().0, acked.last().unwrap().0);
    eprintln!("backup revisions {revs:?}, edits at {first}..={last}");
    assert!(revs.iter().any(|&r| r > first && r < last), "at least one backup must land mid-stream: {revs:?} vs {first}..{last}");
    for (path, manifest) in backups {
        let entry = manifest["workbooks"].as_array().unwrap().iter().find(|w| w["id"] == wb).unwrap().clone();
        let revision = entry["revision"].as_u64().unwrap();
        let target = tempfile::tempdir().unwrap();
        let data = target.path().join("data");
        rowfinity_server::backup::restore(&path, &data).unwrap();
        let restored = TestServer::start(&data).await;
        let mut r = restored.ws(&wb).await;
        assert_eq!(r.hello["revision"], revision, "the restored workbook is at the backed-up revision");
        // Oracle: exactly the edits acknowledged at or before that revision, in order.
        let expected: Vec<Value> = acked.iter().filter(|(rev, _)| *rev <= revision).map(|(_, k)| json!([k])).collect();
        let v = r.ok("open_view", json!({ "spec": { "table": "log" } })).await["view_id"].as_u64().unwrap();
        assert_eq!(r.all_rows(v).await, expected, "backup at revision {revision}");
        let history = r.ok("history", json!({ "after": 0, "limit": 1000 })).await;
        assert_eq!(history.as_array().unwrap().last().unwrap()["revision"], revision);
        r.close().await;
        // The quiet workbook, sheets included.
        let mut r = restored.ws(&quiet).await;
        assert_eq!(r.ok("query", json!({ "sql": "SELECT s FROM t" })).await["rows"], json!([["unchanged"]]));
        let cells = r.ok("sheet_fetch", json!({ "sheet": 0, "row_start": 0, "row_count": 1, "column_count": 2 })).await;
        assert_eq!(cells["rows"][0][1][0], "42");
        r.close().await;
        restored.stop().await;
    }
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn restore_refuses_damaged_backups_and_non_empty_targets_without_writing() {
    let dir = tempfile::tempdir().unwrap();
    let server = TestServer::start(dir.path()).await;
    let wb = server.create_workbook("w").await;
    let mut ws = server.ws(&wb).await;
    ws.ok("create_table_from_query", json!({ "name": "t", "sql": "SELECT i FROM range(1000) r(i)" })).await;
    ws.close().await;
    let (path, _) = backup(&server).await;
    server.stop().await;

    // Non-empty target.
    let busy = tempfile::tempdir().unwrap();
    std::fs::write(busy.path().join("keep.txt"), b"mine").unwrap();
    let err = rowfinity_server::backup::restore(&path, busy.path()).unwrap_err().to_string();
    assert!(err.contains("not empty"), "{err}");
    assert_eq!(files_under(busy.path()).len(), 1);
    // A damaged file: refused before anything is written.
    let db = path.join(&wb).join("data.duckdb");
    let mut bytes = std::fs::read(&db).unwrap();
    let mid = bytes.len() / 2;
    bytes[mid] ^= 0xff;
    std::fs::write(&db, &bytes).unwrap();
    let target = tempfile::tempdir().unwrap();
    let data = target.path().join("data");
    let err = rowfinity_server::backup::restore(&path, &data).unwrap_err().to_string();
    assert!(err.contains("damaged"), "{err}");
    assert!(!data.exists());
    assert!(std::fs::read_dir(target.path()).unwrap().next().is_none(), "no staging left behind");
    // Without a manifest it is not a backup.
    std::fs::remove_file(path.join("manifest.json")).unwrap();
    assert!(rowfinity_server::backup::restore(&path, &data).unwrap_err().to_string().contains("not a complete backup"));
}

#[tokio::test(flavor = "multi_thread")]
async fn offline_backup_fails_closed_while_a_server_runs_and_round_trips_when_stopped() {
    let dir = tempfile::tempdir().unwrap();
    let server = TestServer::start(dir.path()).await;
    let wb = server.create_workbook("off").await;
    let mut ws = server.ws(&wb).await;
    ws.ok("create_table_from_query", json!({ "name": "t", "sql": "SELECT i, 'v' || i AS s FROM range(500) r(i)" })).await;
    ws.close().await;
    let out = tempfile::tempdir().unwrap();
    let err = rowfinity_server::backup::backup_offline(dir.path(), &out.path().join("b")).unwrap_err().to_string();
    assert!(err.contains("is using"), "{err}");
    assert!(!out.path().join("b").exists());
    let before = server.download(&format!("/api/workbooks/{wb}/export?spec={}&format=csv", urlencode(r#"{"table":"t"}"#))).await.2;
    server.stop().await;
    let m = rowfinity_server::backup::backup_offline(dir.path(), &out.path().join("b")).unwrap();
    assert_eq!(m.workbooks.len(), 1);
    let data = out.path().join("restored");
    rowfinity_server::backup::restore(&out.path().join("b"), &data).unwrap();
    let restored = TestServer::start(&data).await;
    let after = restored.download(&format!("/api/workbooks/{wb}/export?spec={}&format=csv", urlencode(r#"{"table":"t"}"#))).await.2;
    assert_eq!(after, before, "byte-identical export after backup and restore");
    restored.stop().await;
}
