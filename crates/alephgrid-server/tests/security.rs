//! SEC.T01 (server side): regression tests from security audit run-1's local validation
//! plans: hostile workbook ids in restored archives, browser-boundary headers, path ids,
//! `--disable-sql` completeness and resource budgets.

mod common;

use common::*;
use reqwest::Method;
use serde_json::{json, Value};

fn set_meta_id(path: &std::path::Path, id: &str) {
    let mut meta: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    meta["id"] = json!(id);
    std::fs::write(path, serde_json::to_vec(&meta).unwrap()).unwrap();
}

/// SEC.02: `workbook.json` is bound to its directory; a hostile id never becomes a path.
#[tokio::test(flavor = "multi_thread")]
async fn a_hostile_workbook_id_in_an_archive_or_data_dir_deletes_nothing() {
    let root = tempfile::tempdir().unwrap();
    let canary = root.path().join("canary");
    std::fs::create_dir_all(&canary).unwrap();
    std::fs::write(canary.join("sentinel.txt"), "keep").unwrap();
    let data = root.path().join("data");
    let server = TestServer::start(&data).await;
    let wb = server.create_workbook("victim").await;
    server.stop().await;
    let backup = root.path().join("backup");
    alephgrid_server::backup::backup_offline(&data, &backup).unwrap();

    for hostile in [canary.to_string_lossy().to_string(), "../../canary".to_string()] {
        // A crafted archive: the database still matches its checksum, only workbook.json lies.
        let crafted = root.path().join("crafted");
        let _ = std::fs::remove_dir_all(&crafted);
        copy_dir(&backup, &crafted);
        set_meta_id(&crafted.join(&wb).join("workbook.json"), &hostile);
        let target = root.path().join("restored");
        let _ = std::fs::remove_dir_all(&target);
        let err = alephgrid_server::backup::restore(&crafted, &target).unwrap_err().to_string();
        assert!(err.contains("different workbook"), "{err}");
        assert!(!target.exists() || std::fs::read_dir(&target).unwrap().next().is_none(), "restore wrote nothing");

        // Someone with write access to the data directory: the workbook refuses to open,
        // and stopping, deleting and backing up touch nothing outside the data directory.
        set_meta_id(&data.join(&wb).join("workbook.json"), &hostile);
        let server = TestServer::start(&data).await;
        let (code, _) = server.get(&format!("/api/workbooks/{wb}")).await;
        assert_ne!(code, 200);
        assert!(Ws::try_connect(server.addr, &wb).await.is_err());
        let _ = server.send(Method::DELETE, &format!("/api/workbooks/{wb}"), None).await;
        server.stop().await;
        let b2 = root.path().join("backup2");
        let _ = std::fs::remove_dir_all(&b2);
        let _ = alephgrid_server::backup::backup_offline(&data, &b2);
        assert!(canary.join("sentinel.txt").exists(), "{hostile}: the canary survives");
        assert!(!canary.join("data.duckdb").exists(), "{hostile}: nothing was written outside the backup");
        set_meta_id(&data.join(&wb).join("workbook.json"), &wb);
    }
    // Restored with its real id it opens again.
    let server = TestServer::start(&data).await;
    let (code, body) = server.get(&format!("/api/workbooks/{wb}")).await;
    assert_eq!(code, 200, "{body}");
    server.stop().await;
}

fn copy_dir(from: &std::path::Path, to: &std::path::Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let p = e.path();
        if p.is_dir() {
            copy_dir(&p, &to.join(e.file_name()));
        } else {
            std::fs::copy(&p, to.join(e.file_name())).unwrap();
        }
    }
}

/// SEC.10: with SQL disabled, every operation that runs user-written SQL is refused,
/// including SQL formula columns and SQL functions; spreadsheet formulas still work.
#[tokio::test(flavor = "multi_thread")]
async fn disable_sql_refuses_sql_formulas_columns_and_functions() {
    let dir = tempfile::tempdir().unwrap();
    // Make a table with a SQL formula column while SQL is allowed.
    let server = TestServer::start(dir.path()).await;
    let wb = server.create_workbook("no sql").await;
    let (code, _) = server.upload(&wb, "t.csv", b"x\n1\n2\n".to_vec()).await;
    assert_eq!(code, 200);
    let mut ws = server.ws(&wb).await;
    ws.ok("add_formula_column", json!({ "table": "t", "name": "y", "formula": "x * 2" })).await;
    ws.close().await;
    server.stop().await;

    let server = TestServer::start_with(dir.path(), |c| c.allow_sql = false).await;
    let mut ws = server.ws(&wb).await;
    let refused = |r: Result<Value, String>| {
        let e = r.expect_err("refused");
        assert!(e.contains("SQL is disabled"), "{e}");
    };
    refused(ws.call("add_formula_column", json!({ "table": "t", "name": "z", "formula": "x + 1" })).await);
    refused(ws.call("add_formula_column", json!({ "table": "t", "name": "z", "formula": "x + 1", "language": "sql" })).await);
    refused(ws.call("update_formula", json!({ "table": "t", "name": "y", "formula": "x * 3" })).await);
    refused(ws.call("query", json!({ "sql": "SELECT 1" })).await);
    let sql_fn = json!({ "name": "TWICE", "kind": "sql", "arguments": ["v"], "source": "v * 2", "documentation": "", "tests": [] });
    refused(ws.call("test_function", json!({ "definition": sql_fn })).await);
    refused(ws.call("publish_function", json!({ "definition": sql_fn })).await);
    // A column type is a type name, never an expression.
    let e = ws.call("add_column", json!({ "table": "t", "name": "c", "sql_type": "INTEGER DEFAULT (42)" })).await.unwrap_err();
    assert!(e.contains("invalid column type"), "{e}");
    ws.ok("add_column", json!({ "table": "t", "name": "c", "sql_type": "BIGINT" })).await;
    // Spreadsheet formulas stay.
    ws.ok("add_formula_column", json!({ "table": "t", "name": "s", "formula": "=[@x]+1", "language": "sheet" })).await;
    ws.close().await;
    server.stop().await;
}

/// Audit run-3: `--disable-sql` also refuses taking a saved query or a SQL function in from
/// the space library (views stay available).
#[tokio::test(flavor = "multi_thread")]
async fn disable_sql_refuses_sql_library_entries() {
    let dir = tempfile::tempdir().unwrap();
    let server = TestServer::start(dir.path()).await;
    let (wb, other) = (server.create_workbook("library").await, server.create_workbook("other").await);
    for w in [&wb, &other] {
        assert_eq!(server.upload(w, "t.csv", b"x\n1\n2\n".to_vec()).await.0, 200);
    }
    let mut ws = server.ws(&wb).await;
    let q = ws.ok("save_query", json!({ "name": "Q", "sql": "SELECT x FROM t" })).await;
    let v = ws.ok("save_view", json!({ "name": "V", "spec": { "table": "t", "sort": [], "filters": [] } })).await;
    let f = json!({ "name": "TWICE", "kind": "sql", "arguments": ["v"], "source": "v * 2", "documentation": "", "tests": [] });
    ws.ok("publish_function", json!({ "definition": f })).await;
    ws.close().await;
    let space = server.get(&format!("/api/workbooks/{wb}")).await.1["workbook"]["space"].as_str().unwrap().to_string();
    let mut entries = Vec::new();
    for (kind, object) in [("query", q["id"].clone()), ("function", json!("TWICE")), ("view", v["id"].clone())] {
        let (code, e) = server.send(Method::POST, &format!("/api/spaces/{space}/library"), Some(json!({ "workbook": wb, "kind": kind, "object": object }))).await;
        assert_eq!(code, 200, "{e}");
        entries.push(e["id"].as_str().unwrap().to_string());
    }
    server.stop().await;

    let server = TestServer::start_with(dir.path(), |c| c.allow_sql = false).await;
    let take = |entry: String| {
        let (server, other) = (&server, &other);
        async move { server.send(Method::POST, &format!("/api/workbooks/{other}/library"), Some(json!({ "entry": entry }))).await }
    };
    for entry in &entries[..2] {
        let (code, body) = take(entry.clone()).await;
        assert_eq!(code, 400, "{body}");
        assert!(body.to_string().contains("SQL is disabled"), "{body}");
    }
    let (code, body) = take(entries[2].clone()).await;
    assert_eq!(code, 200, "a view is not SQL: {body}");
    // Nothing SQL arrived in the other workbook.
    let mut ws = server.ws(&other).await;
    assert_eq!(ws.ok("functions", json!({})).await, json!([]));
    ws.close().await;
    server.stop().await;
}

/// SEC.11: demo tables are capped over the protocol, a workbook at its size limit refuses
/// new data but still lets data be deleted, and the limit shows in the admin resources.
#[tokio::test(flavor = "multi_thread")]
async fn workbook_growth_is_budgeted() {
    let dir = tempfile::tempdir().unwrap();
    let server = TestServer::start(dir.path()).await;
    let wb = server.create_workbook("demo").await;
    let mut ws = server.ws(&wb).await;
    for _ in 0..3 {
        ws.ok("create_demo_table", json!({ "rows": 10 })).await;
    }
    let e = ws.call("create_demo_table", json!({ "rows": 10 })).await.unwrap_err();
    assert!(e.contains("at most 3 demo tables"), "{e}");
    ws.close().await;
    server.stop().await;

    let server = TestServer::start_with(dir.path(), |c| c.max_workbook_bytes = Some(1)).await;
    let mut ws = server.ws(&wb).await;
    let e = ws.call("create_table_from_query", json!({ "name": "more", "sql": "SELECT 1 AS x" })).await.unwrap_err();
    assert!(e.contains("its limit is"), "{e}");
    let (code, body) = server.upload(&wb, "more.csv", b"a\n1\n".to_vec()).await;
    assert_ne!(code, 200, "{body}");
    assert!(body.to_string().contains("its limit is"), "{body}");
    // Deleting is how a workbook gets back under its limit.
    ws.ok("drop_table", json!({ "table": "flights" })).await;
    ws.close().await;
    let (code, res) = server.get("/api/admin/resources").await;
    assert_eq!(code, 200, "{res}");
    assert_eq!(res["workbook_size_limit"], 1);
    server.stop().await;
}

/// SEC.11: online backups run one at a time, not more often than the interval, keep only
/// the newest few, and can be listed and deleted.
#[tokio::test(flavor = "multi_thread")]
async fn online_backups_are_rate_limited_pruned_and_listed() {
    let dir = tempfile::tempdir().unwrap();
    let server = TestServer::start_with(dir.path(), |c| {
        c.backups_kept = 2;
    })
    .await;
    server.create_workbook("w").await;
    let mut names = Vec::new();
    for _ in 0..3 {
        let (code, body) = server.send(Method::POST, "/api/admin/backups", None).await;
        assert_eq!(code, 200, "{body}");
        names.push(body["name"].as_str().unwrap().to_string());
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    let (code, list) = server.get("/api/admin/backups").await;
    assert_eq!(code, 200);
    let listed: Vec<&str> = list.as_array().unwrap().iter().map(|b| b["name"].as_str().unwrap()).collect();
    assert_eq!(listed, vec![names[2].as_str(), names[1].as_str()], "only the newest two are kept");
    let (code, _) = server.send(Method::DELETE, &format!("/api/admin/backups/{}", names[1]), None).await;
    assert_eq!(code, 204);
    for bad in ["backup-..", "manifest.json", "nope", "backup-1"] {
        let (code, _) = server.send(Method::DELETE, &format!("/api/admin/backups/{bad}"), None).await;
        assert_eq!(code, 404, "{bad}");
    }
    // `..` is resolved by the client before it is sent; whatever it reaches, no backup goes.
    let _ = server.send(Method::DELETE, "/api/admin/backups/..", None).await;
    assert!(dir.path().join(".backups").join(&names[2]).exists());
    server.stop().await;

    let server = TestServer::start_with(dir.path(), |c| c.backup_interval = std::time::Duration::from_secs(3600)).await;
    let (code, body) = server.send(Method::POST, "/api/admin/backups", None).await;
    assert_eq!(code, 200, "{body}");
    let (code, body) = server.send(Method::POST, "/api/admin/backups", None).await;
    assert_eq!(code, 409, "{body}");
    server.stop().await;
}

/// A control database can come from a backup archive someone else made: it is opened with
/// the engine locked down, so a view stored in it cannot read files on the host. (Here the
/// crafted `settings` view answers the schema check only if it could read the canary.)
#[test]
fn a_control_database_is_opened_locked_down() {
    let dir = tempfile::tempdir().unwrap();
    let canary = dir.path().join("canary.txt");
    std::fs::write(&canary, "canary").unwrap();
    let control = dir.path().join("data").join(".control");
    std::fs::create_dir_all(&control).unwrap();
    let path = control.join("control.duckdb");
    {
        let conn = duckdb::Connection::open(&path).unwrap();
        let sql = format!(
            "CREATE VIEW settings AS SELECT 'schema' AS key, (SELECT CASE WHEN content = 'canary' THEN '{}' END FROM read_text('{}')) AS value",
            alephgrid_server::auth::store::SCHEMA,
            canary.display()
        );
        conn.execute_batch(&sql).unwrap();
    }
    let opened = alephgrid_server::auth::Store::open(&path);
    let err = opened.err().expect("the crafted file must not open as a control database");
    let msg = format!("{err:#}");
    assert!(msg.contains("Permission") && msg.contains("disabled by configuration"), "{msg}");
    // And an ordinary control database still opens, and backs up into .backups.
    let fresh = dir.path().join("ok").join(".control");
    std::fs::create_dir_all(&fresh).unwrap();
    let store = alephgrid_server::auth::Store::open(&fresh.join("control.duckdb")).unwrap();
    let backups = dir.path().join("ok").join(".backups").join("b1");
    std::fs::create_dir_all(&backups).unwrap();
    store.backup_to(&backups.join("control.duckdb")).unwrap();
}
