//! 3.T03: real storage faults.
//!
//! * Disk full: needs a small, disposable filesystem named by `ROWFINITY_SMALL_FS` (a
//!   64 MB tmpfs in CI, a disk image on macOS: `hdiutil create -size 64m -fs HFS+
//!   -volname rfsmall -attach /tmp/rfsmall.dmg`). Without it these tests say so and
//!   return without checking anything; the matrix records whether they ran.
//! * Permissions: a read-only workbook directory (skipped when running as root, which
//!   ignores permissions).
//!
//! Not simulated here: fsync failures, torn writes, power loss, media errors.

mod common;

use std::os::unix::fs::PermissionsExt;

use common::*;
use serde_json::json;

fn small_fs() -> Option<std::path::PathBuf> {
    match std::env::var("ROWFINITY_SMALL_FS") {
        Ok(p) if !p.is_empty() => Some(std::path::PathBuf::from(p)),
        _ => {
            eprintln!("ROWFINITY_SMALL_FS is not set: disk-full test not run");
            None
        }
    }
}

fn is_root() -> bool {
    std::process::Command::new("id").arg("-u").output().map(|o| String::from_utf8_lossy(&o.stdout).trim() == "0").unwrap_or(false)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_full_disk_fails_the_change_and_keeps_everything_acknowledged() {
    let Some(fs) = small_fs() else { return };
    let dir = tempfile::tempdir_in(&fs).unwrap();
    let server = TestServer::start_with(dir.path(), |c| c.temp_budget = Some(8 << 20)).await;
    let wb = server.create_workbook("full").await;
    let mut ws = server.ws(&wb).await;
    ws.ok("create_table_from_query", json!({ "name": "t", "sql": "SELECT i AS n FROM range(1000) r(i)" })).await;
    let view = ws.ok("open_view", json!({ "spec": { "table": "t" } })).await["view_id"].as_u64().unwrap();
    for k in 0..20 {
        ws.ok("edit", json!({ "view_id": view, "edits": [{ "row": k, "column": "n", "value": "-1" }] })).await;
    }
    let revision = ws.ok("history", json!({ "after": 0, "limit": 1000 })).await.as_array().unwrap().last().unwrap()["revision"].as_u64().unwrap();
    // Far more than the filesystem holds (random text does not compress).
    let err = ws
        .call("create_table_from_query", json!({ "name": "huge", "sql": "SELECT md5(i::VARCHAR) || md5((i + 1)::VARCHAR) AS s FROM range(20000000) r(i)" }))
        .await
        .unwrap_err();
    eprintln!("disk full error: {err}");
    let tables: Vec<String> = ws.ok("tables", json!({})).await.as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap().to_string()).collect();
    assert_eq!(tables, ["t"], "the failed change left nothing");
    let history = ws.ok("history", json!({ "after": 0, "limit": 1000 })).await;
    assert_eq!(history.as_array().unwrap().last().unwrap()["revision"], revision, "no revision for the failed change");
    // An upload larger than the free space fails cleanly too.
    let res = server.http.post(server.url(&format!("/api/workbooks/{wb}/uploads?filename=big.csv"))).body(vec![b'x'; 200 << 20]).send().await;
    if let Ok(res) = res {
        assert!(!res.status().is_success(), "{}", res.status());
    }
    assert!(files_under(&dir.path().join(".uploads")).is_empty());
    ws.close().await;
    server.stop().await;
    // After a restart the acknowledged state is intact.
    let server = TestServer::start(dir.path()).await;
    let mut ws = server.ws(&wb).await;
    assert_eq!(ws.hello["revision"], revision);
    let v = ws.ok("open_view", json!({ "spec": { "table": "t" } })).await["view_id"].as_u64().unwrap();
    let rows = ws.all_rows(v).await;
    assert!(rows[..20].iter().all(|r| r == &json!([-1])));
    assert_eq!(rows[20], json!([20]));
    ws.close().await;
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_read_only_workbook_directory_fails_to_open_cleanly_and_recovers() {
    if is_root() {
        eprintln!("running as root: permission test not run");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let server = TestServer::start(dir.path()).await;
    let wb = server.create_workbook("ro").await;
    let mut ws = server.ws(&wb).await;
    ws.ok("create_table_from_query", json!({ "name": "t", "sql": "SELECT 9 AS x" })).await;
    ws.close().await;
    let (code, _) = server.send(reqwest::Method::POST, &format!("/api/workbooks/{wb}/runtime"), Some(json!({ "action": "stop" }))).await;
    assert_eq!(code, 200);
    let wbdir = dir.path().join(&wb);
    let db = wbdir.join("data.duckdb");
    std::fs::set_permissions(&db, std::fs::Permissions::from_mode(0o444)).unwrap();
    std::fs::set_permissions(&wbdir, std::fs::Permissions::from_mode(0o555)).unwrap();
    let before = std::fs::read(&db).unwrap();
    let (code, err) = server.get(&format!("/api/workbooks/{wb}")).await;
    assert_eq!(code, 503, "{err}");
    eprintln!("read-only error: {}", err["error"]);
    assert_eq!(std::fs::read(&db).unwrap(), before, "nothing was written");
    std::fs::set_permissions(&wbdir, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::set_permissions(&db, std::fs::Permissions::from_mode(0o644)).unwrap();
    let mut ws = server.ws(&wb).await;
    assert_eq!(ws.ok("query", json!({ "sql": "SELECT x FROM t" })).await["rows"], json!([[9]]));
    ws.close().await;
    server.stop().await;
    // A data directory the server cannot write is refused at startup with a clear error.
    let locked = tempfile::tempdir().unwrap();
    std::fs::set_permissions(locked.path(), std::fs::Permissions::from_mode(0o555)).unwrap();
    let err = rowfinity_server::state::AppState::new(rowfinity_server::state::Config::new(locked.path().join("data")))
        .err()
        .expect("must fail")
        .to_string();
    assert!(err.contains("creating data dir") || err.contains("ermission"), "{err}");
    std::fs::set_permissions(locked.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
}
