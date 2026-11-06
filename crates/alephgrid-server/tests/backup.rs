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
        alephgrid_server::backup::restore(&path, &data).unwrap();
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
    let err = alephgrid_server::backup::restore(&path, busy.path()).unwrap_err().to_string();
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
    let err = alephgrid_server::backup::restore(&path, &data).unwrap_err().to_string();
    assert!(err.contains("damaged"), "{err}");
    assert!(!data.exists());
    assert!(std::fs::read_dir(target.path()).unwrap().next().is_none(), "no staging left behind");
    // Without a manifest it is not a backup.
    std::fs::remove_file(path.join("manifest.json")).unwrap();
    assert!(alephgrid_server::backup::restore(&path, &data).unwrap_err().to_string().contains("not a complete backup"));
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
    let err = alephgrid_server::backup::backup_offline(dir.path(), &out.path().join("b")).unwrap_err().to_string();
    assert!(err.contains("is using"), "{err}");
    assert!(!out.path().join("b").exists());
    let before = server.download(&format!("/api/workbooks/{wb}/export?spec={}&format=csv", urlencode(r#"{"table":"t"}"#))).await.2;
    server.stop().await;
    let m = alephgrid_server::backup::backup_offline(dir.path(), &out.path().join("b")).unwrap();
    assert_eq!(m.workbooks.len(), 1);
    let data = out.path().join("restored");
    alephgrid_server::backup::restore(&out.path().join("b"), &data).unwrap();
    let restored = TestServer::start(&data).await;
    let after = restored.download(&format!("/api/workbooks/{wb}/export?spec={}&format=csv", urlencode(r#"{"table":"t"}"#))).await.2;
    assert_eq!(after, before, "byte-identical export after backup and restore");
    restored.stop().await;
}

/// 9.04: the documented procedure, with the binary's own `--backup-to` and `--restore-from`:
/// a data directory with accounts, a space, a workbook (table and sheet), a member and a
/// service token is backed up to another location, the original is lost, the backup is
/// restored into an empty directory and a server started on it serves the same data to the
/// same people.
#[tokio::test(flavor = "multi_thread")]
async fn the_cli_backs_up_accounts_and_workbooks_and_restores_them_on_a_clean_data_directory() {
    const PASSWORD: &str = "a long enough password";
    let original = tempfile::tempdir().unwrap();
    let server = TestServer::start_accounts(original.path(), |_| {}).await;
    let admin = server.admin().await;
    let org = admin.me["orgs"][0]["org"]["id"].as_str().unwrap().to_string();
    let space = admin.ok(Method::POST, "/api/spaces", Some(json!({ "name": "Ops" }))).await["id"].as_str().unwrap().to_string();
    let wb = admin.ok(Method::POST, "/api/workbooks", Some(json!({ "name": "cities", "space": space }))).await["id"].as_str().unwrap().to_string();
    let (s, v) = admin.send_bytes(Method::POST, &format!("/api/workbooks/{wb}/import?filename=cities.csv"), b"city,people\nPune,7\nOslo,3\n".to_vec()).await;
    assert_eq!(s, 200, "{v}");
    let mut ws = admin.ws(&wb).await.unwrap();
    ws.ok("sheet_create", json!({ "name": "S" })).await;
    ws.ok("sheet_set", json!({ "sheet": 0, "inputs": [{ "row": 0, "column": 0, "value": "6" }, { "row": 0, "column": 1, "value": "=A1*7" }] })).await;
    ws.close().await;
    let code = admin.invite(&org, "editor@example.com", Some(&space), Some("editor")).await;
    let editor = server.accept(&code, "editor", PASSWORD).await;
    let token = admin.ok(Method::POST, &format!("/api/spaces/{space}/tokens"), Some(json!({ "name": "reports", "role": "viewer", "days": 30 }))).await["secret"]
        .as_str()
        .unwrap()
        .to_string();
    let export = format!("/api/workbooks/{wb}/export?spec={}&format=csv", urlencode(r#"{"table":"cities"}"#));
    let (s, before) = editor.get(&export).await;
    assert_eq!(s, 200, "{before}");
    server.stop().await;

    // Offline backup to another location, with the CLI.
    let elsewhere = tempfile::tempdir().unwrap();
    let backup = elsewhere.path().join("backup-1");
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_alephgrid"))
        .args(["--data-dir", original.path().to_str().unwrap(), "--backup-to", backup.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success(), "--backup-to: {}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stdout).contains("backed up 1 workbook(s)"));
    // The original host is gone.
    drop(original);

    // Restore into an empty directory on a "clean host", with the CLI.
    let clean = tempfile::tempdir().unwrap();
    let data = clean.path().join("data");
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_alephgrid"))
        .args(["--data-dir", data.to_str().unwrap(), "--restore-from", backup.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success(), "--restore-from: {}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    // Restoring again over it is refused: restore never writes into a used directory.
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_alephgrid"))
        .args(["--data-dir", data.to_str().unwrap(), "--restore-from", backup.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(!out.status.success(), "a second restore into the same directory must fail");

    let restored = TestServer::start_accounts(&data, |_| {}).await;
    assert_eq!(restored.get("/readyz").await.0, 200);
    // The member signs in with the same password and keeps the role; the data is the same.
    let editor_again = restored.login("editor@example.com", PASSWORD).await.expect("the restored user signs in");
    let (s, v) = editor_again.get(&format!("/api/workbooks/{wb}")).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["role"], "editor");
    assert_eq!(v["workbook"]["space"], space.as_str());
    assert_eq!(editor_again.get(&export).await, (200, before));
    let mut ws = editor_again.ws(&wb).await.unwrap();
    let cells = ws.ok("sheet_fetch", json!({ "sheet": 0, "row_start": 0, "row_count": 1, "column_count": 2 })).await;
    assert_eq!(cells["rows"][0][1][0], "42");
    ws.close().await;
    // The service token carries over, still read-only.
    let reports = restored.bearer(&token);
    assert_eq!(reports.get(&format!("/api/workbooks/{wb}")).await.0, 200);
    assert_eq!(reports.send(Method::PATCH, &format!("/api/workbooks/{wb}"), Some(json!({ "name": "renamed" }))).await.0, 403);
    // Sign-in sessions are in the control database too: one made before the backup still
    // works until it expires (or the user is disabled, or signs out).
    let old = Session { addr: restored.addr, ..editor.clone() };
    assert_eq!(old.get("/api/me").await.0, 200);
    restored.stop().await;
}
