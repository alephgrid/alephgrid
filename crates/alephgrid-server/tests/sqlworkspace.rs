//! Phase 4 over the wire: the SQL workspace through real HTTP and WebSocket requests that
//! skip the UI. Parameters, error codes and redaction (4.02); bounded previews, cancellation
//! of one query among several, interactive reads during long queries, complete exports
//! downloaded once (4.03); saved queries and materializations across restarts and sessions
//! (4.04–4.06); the database lockdown of every open workbook and internal tables (4.07); SQL
//! changes with mutation ids and events (4.08).

mod common;

use std::time::{Duration, Instant};

use common::*;
use reqwest::Method;
use serde_json::{json, Value};

/// Runs for minutes unless interrupted (a sum over 10^11 integers).
const SLOW_SQL: &str = "SELECT sum(i) AS s FROM range(100000000000) r(i)";

fn param(name: &str, ty: &str, value: Option<&str>) -> Value {
    match value {
        Some(v) => json!({ "name": name, "type": ty, "value": v }),
        None => json!({ "name": name, "type": ty }),
    }
}

async fn reply_code(ws: &mut Ws, id: u64) -> Value {
    ws.reply(id).await.expect("a reply")
}

/// Wait until `n` jobs run or wait on the workbook (observable, not a sleep).
async fn until_load(server: &TestServer, wb: &str, n: usize) {
    let state = server.state.clone();
    let wb = wb.to_string();
    eventually("jobs are running", move || state.controller.handle(&wb).is_some_and(|h| h.sched.load() >= n && h.active() > 0)).await;
}

fn export_files(server: &TestServer, wb: &str) -> Vec<std::path::PathBuf> {
    files_under(&server.data_dir.join(".exports").join(wb))
}

#[tokio::test(flavor = "multi_thread")]
async fn queries_bind_parameters_and_explain_refusals_with_codes() {
    let dir = tempfile::tempdir().unwrap();
    let server = TestServer::start(dir.path()).await;
    let wb = server.create_workbook("params").await;
    let mut ws = server.ws(&wb).await;
    ws.ok("create_table_from_query", json!({ "name": "people", "sql": "SELECT * FROM (VALUES (1, 'Ada'), (2, 'O''Brien; DROP TABLE people; --'), (3, NULL)) v(id, name)" })).await;

    let schema = ws.ok("schema", json!({})).await;
    assert_eq!(schema, json!([{ "name": "people", "row_count": 3, "columns": [
        { "name": "id", "sql_type": "INTEGER", "kind": "int" }, { "name": "name", "sql_type": "VARCHAR", "kind": "text" }] }]));

    let r = ws.ok("query", json!({ "sql": "SELECT id FROM people WHERE name = $n", "params": [param("n", "text", Some("O'Brien; DROP TABLE people; --"))] })).await;
    assert_eq!(r["rows"], json!([[2]]));
    assert_eq!(r["truncated"], false);
    assert_eq!(r["sources"], json!([{ "table": "people", "data_rev": 1 }]));
    assert_eq!(r["revision"], 1);
    assert!(r["elapsed_ms"].is_u64());
    let r = ws.ok("query", json!({ "sql": "SELECT count(*) FROM people WHERE name IS NOT DISTINCT FROM $n", "params": [param("n", "text", None)] })).await;
    assert_eq!(r["rows"], json!([[1]]), "an omitted value binds NULL");
    let r = ws.ok("query", json!({ "sql": "SELECT $big + 1, $d", "params": [param("big", "integer", Some("9007199254740993")), param("d", "decimal", Some("-0.000001"))] })).await;
    assert_eq!(r["rows"], json!([["9007199254740994", "-0.000001"]]));

    let history = ws.ok("history", json!({})).await;
    for (frame, code, text) in [
        (json!({ "sql": "SELECT id FROM people WHERE name = $n" }), "bad_parameter", "$n has no value"),
        (json!({ "sql": "SELECT $n", "params": [param("n", "integer", Some("1; DROP TABLE people"))] }), "bad_parameter", "is not an integer"),
        (json!({ "sql": "SELECT * FROM people WHERE id = ?" }), "bad_parameter", "named parameters"),
        (json!({ "sql": "SELECT * FROM read_csv('/etc/passwd')" }), "not_allowed", "read_csv"),
        (json!({ "sql": "SELECT * FROM _rf_history" }), "not_allowed", "internal"),
        (json!({ "sql": "SELECT 1; DROP TABLE people" }), "not_allowed", "one statement"),
        (json!({ "sql": "SET threads = 64" }), "not_allowed", "only queries"),
        (json!({ "sql": "UPDATE people SET name = 'x'" }), "sql_change", "changes data"),
    ] {
        let id = ws.send("query", frame.clone()).await;
        let r = reply_code(&mut ws, id).await;
        assert_eq!(r["ok"], false, "{frame}");
        assert_eq!(r["code"], code, "{frame}: {r}");
        assert!(r["error"].as_str().unwrap().contains(text), "{frame}: {r}");
    }
    assert_eq!(ws.ok("history", json!({})).await, history, "refusals changed nothing");
    // Paths inside the data directory are never echoed back.
    let secret = dir.path().join("secret.csv");
    let id = ws.send("query", json!({ "sql": format!("SELECT * FROM '{}'", secret.display()) })).await;
    let r = reply_code(&mut ws, id).await;
    let msg = r["error"].as_str().unwrap();
    assert!(!msg.contains(&dir.path().to_string_lossy().to_string()) && msg.contains("<data>/secret.csv"), "{msg}");
    // Errors point into the user's text.
    let err = ws.call("query", json!({ "sql": "SELECT id,\n  nope\nFROM people" })).await.unwrap_err();
    assert!(err.contains("nope") && err.contains("LINE 2:"), "{err}");
    ws.close().await;
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn cancel_hits_one_query_and_interactive_reads_are_not_starved() {
    let dir = tempfile::tempdir().unwrap();
    let server = TestServer::start(dir.path()).await;
    let wb = server.create_workbook("cancel").await;
    let mut a = server.ws(&wb).await;
    let mut b = server.ws(&wb).await;
    a.ok("create_table_from_query", json!({ "name": "t", "sql": "SELECT i AS n FROM range(1000) r(i)" })).await;
    let view = a.ok("open_view", json!({ "spec": { "table": "t" } })).await["view_id"].as_u64().unwrap();
    // A: one long query running, a second one queued behind it on the bulk reader.
    let running = a.send("query", json!({ "sql": SLOW_SQL })).await;
    let queued = a.send("query", json!({ "sql": "SELECT 42 AS answer" })).await;
    // B runs its own long query too.
    let other = b.send("query", json!({ "sql": SLOW_SQL })).await;
    until_load(&server, &wb, 3).await;
    // Interactive reads still answer at once.
    let started = Instant::now();
    let block = a.ok("fetch", json!({ "view_id": view, "row_start": 995, "row_count": 10 })).await;
    assert_eq!(block["rows"], json!([[995], [996], [997], [998], [999]]));
    assert!(started.elapsed() < Duration::from_secs(5), "a fetch waited {:?} behind long queries", started.elapsed());
    // Cancelling the queued query removes it without touching the running ones.
    assert_eq!(a.ok("cancel", json!({ "request": queued })).await, json!(true));
    let r = reply_code(&mut a, queued).await;
    assert_eq!(r["code"], "cancelled", "{r}");
    assert_eq!(a.buffered(), 0, "the running query is still running");
    // B cannot cancel A's query; A cancels its own; B's keeps running until B cancels it.
    assert_eq!(b.ok("cancel", json!({ "request": running })).await, json!(false));
    assert_eq!(a.ok("cancel", json!({ "request": running })).await, json!(true));
    assert_eq!(reply_code(&mut a, running).await["code"], "cancelled");
    assert_eq!(b.buffered(), 0, "B's query is unaffected");
    assert_eq!(b.ok("cancel", json!({ "request": other })).await, json!(true));
    assert_eq!(reply_code(&mut b, other).await["code"], "cancelled");
    // A fresh query afterwards gets a fresh result.
    assert_eq!(a.ok("query", json!({ "sql": "SELECT 7" })).await["rows"], json!([[7]]));
    // A deadline interrupts a running query.
    let id = a.send("query", json!({ "sql": SLOW_SQL, "timeout_ms": 200 })).await;
    assert_eq!(reply_code(&mut a, id).await["code"], "deadline_exceeded");
    // Closing the connection cancels what it runs; the workbook stays usable.
    a.send("query", json!({ "sql": SLOW_SQL })).await;
    until_load(&server, &wb, 1).await;
    a.close().await;
    let state = server.state.clone();
    let wbid = wb.clone();
    eventually("the closed session's query stopped", move || state.controller.handle(&wbid).is_some_and(|h| h.sched.load() == 0)).await;
    assert_eq!(b.ok("query", json!({ "sql": "SELECT count(*) FROM t" })).await["rows"], json!([[1000]]));
    b.close().await;
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn exports_hold_every_row_download_once_and_leave_nothing_behind() {
    let dir = tempfile::tempdir().unwrap();
    let server = TestServer::start(dir.path()).await;
    let wb = server.create_workbook("export").await;
    let mut ws = server.ws(&wb).await;
    let n: i64 = 120_000;
    ws.ok("create_table_from_query", json!({ "name": "big", "sql": format!("SELECT i, 'row ' || i AS label, (i * 10)::DECIMAL(20,2) AS amount FROM range({n}) r(i)") })).await;
    let preview = ws.ok("query", json!({ "sql": "SELECT * FROM big WHERE i >= $lo", "params": [param("lo", "integer", Some("0"))] })).await;
    assert_eq!((preview["rows"].as_array().unwrap().len(), preview["truncated"].clone()), (500, json!(true)), "the preview is bounded");

    let e = ws.ok("export_query", json!({ "sql": "SELECT * FROM big WHERE i >= $lo ORDER BY i", "params": [param("lo", "integer", Some("0"))], "format": "csv" })).await;
    assert_eq!(e["rows"], n);
    assert_eq!(e["revision"], preview["revision"]);
    let url = format!("/api/workbooks/{wb}/downloads/{}", e["download"].as_str().unwrap());
    let (status, headers, bytes) = server.download(&url).await;
    assert_eq!(status, 200);
    assert_eq!(bytes.len() as u64, e["bytes"].as_u64().unwrap());
    assert!(headers["content-disposition"].to_str().unwrap().contains("query.csv"));
    let rows = parse_csv(&String::from_utf8(bytes).unwrap());
    assert_eq!(rows[0], ["i", "label", "amount"]);
    assert_eq!(rows.len() as i64, n + 1, "every row, not the preview's 500");
    let sum: i64 = rows[1..].iter().map(|r| r[0].parse::<i64>().unwrap()).sum();
    assert_eq!(sum, n * (n - 1) / 2);
    assert_eq!(rows[n as usize], [(n - 1).to_string(), format!("row {}", n - 1), format!("{}.00", (n - 1) * 10)]);
    // Once only, and the file is gone.
    assert_eq!(server.download(&url).await.0, 404);
    assert!(export_files(&server, &wb).is_empty());
    // Another workbook's download id does not work here.
    let e = ws.ok("export_query", json!({ "sql": "SELECT 1", "format": "json" })).await;
    let other = server.create_workbook("other").await;
    assert_eq!(server.download(&format!("/api/workbooks/{other}/downloads/{}", e["download"].as_str().unwrap())).await.0, 404);
    // Parquet keeps exact decimals and big integers.
    let e = ws.ok("export_query", json!({ "sql": "SELECT 9007199254740993::BIGINT AS b, 1.10::DECIMAL(5,2) AS d", "format": "parquet" })).await;
    let (status, _, bytes) = server.download(&format!("/api/workbooks/{wb}/downloads/{}", e["download"].as_str().unwrap())).await;
    assert_eq!(status, 200);
    assert!(bytes.starts_with(b"PAR1"));
    // Unclaimed exports expire (the janitor runs this hourly).
    assert_eq!(server.state.downloads.expire(Duration::ZERO), 1);
    assert!(export_files(&server, &wb).is_empty());

    // A cancelled export, a failing export and a disconnect during an export leave no file.
    let id = ws.send("export_query", json!({ "sql": SLOW_SQL, "format": "csv" })).await;
    until_load(&server, &wb, 1).await;
    ws.ok("cancel", json!({ "request": id })).await;
    assert_eq!(reply_code(&mut ws, id).await["code"], "cancelled");
    let err = ws.call("export_query", json!({ "sql": "SELECT CASE WHEN i = 100000 THEN error('boom') ELSE i END FROM big", "format": "csv" })).await.unwrap_err();
    assert!(err.contains("boom"), "{err}");
    let mut gone = server.ws(&wb).await;
    gone.send("export_query", json!({ "sql": SLOW_SQL, "format": "csv" })).await;
    until_load(&server, &wb, 1).await;
    gone.close().await;
    let state = server.state.clone();
    let wbid = wb.clone();
    eventually("the abandoned export stopped", move || state.controller.handle(&wbid).is_some_and(|h| h.sched.load() == 0)).await;
    assert!(export_files(&server, &wb).is_empty(), "{:?}", export_files(&server, &wb));
    assert!(server.state.downloads.is_empty());
    // Cancel racing completion: either a usable download or a cancellation with no file.
    for _ in 0..20 {
        let id = ws.send("export_query", json!({ "sql": "SELECT * FROM big LIMIT 1000", "format": "csv" })).await;
        let cancelled = ws.ok("cancel", json!({ "request": id })).await;
        let r = reply_code(&mut ws, id).await;
        if r["ok"] == true {
            assert_eq!(cancelled, json!(false), "a reply after a successful cancel must not be a result: {r}");
            let (status, _, bytes) = server.download(&format!("/api/workbooks/{wb}/downloads/{}", r["result"]["download"].as_str().unwrap())).await;
            assert_eq!((status, parse_csv(&String::from_utf8(bytes).unwrap()).len()), (200, 1001));
        } else {
            assert_eq!(r["code"], "cancelled", "{r}");
        }
        assert!(export_files(&server, &wb).is_empty());
    }
    ws.close().await;
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn saved_queries_and_materializations_persist_and_notify() {
    let dir = tempfile::tempdir().unwrap();
    let server = TestServer::start(dir.path()).await;
    let wb = server.create_workbook("saved").await;
    let mut a = server.ws(&wb).await;
    let mut b = server.ws(&wb).await;
    a.ok("create_table_from_query", json!({ "name": "sales", "sql": "SELECT i AS id, i % 2 AS team, i * 100 AS amount FROM range(10) r(i)" })).await;
    let sql = "SELECT team, sum(amount) AS total FROM sales WHERE amount >= $min GROUP BY team ORDER BY team";
    let params = json!([param("min", "integer", Some("0"))]);
    let q = a.ok("save_query", json!({ "name": "Team totals", "kind": "live", "sql": sql, "params": params })).await;
    assert_eq!((q["version"].clone(), q["kind"].clone()), (json!(1), json!("live")));
    let ev = b.event("queries_changed").await.unwrap();
    assert!(ev["revision"].as_u64().unwrap() > 0);
    let q2 = a.ok("save_query", json!({ "saved": q["id"], "name": "Team totals", "kind": "live", "sql": sql, "params": [param("min", "integer", Some("300"))] })).await;
    assert_eq!(q2["version"], 2);
    let t = a.ok("create_table_from_query", json!({ "name": "totals", "sql": sql, "params": params, "saved": q["id"], "version": 1 })).await;
    assert_eq!(t["row_count"], 2);
    // Team 0: 0+200+400+600+800, team 1: 100+300+500+700+900.
    assert_eq!(a.ok("query", json!({ "sql": "SELECT * FROM totals ORDER BY team" })).await["rows"], json!([[0, 2000], [1, 2500]]));

    // B edits the source: the materialization is stale; A refreshes it; B hears about it.
    let v = b.ok("open_view", json!({ "spec": { "table": "sales" } })).await["view_id"].as_u64().unwrap();
    b.ok("edit", json!({ "view_id": v, "edits": [{ "row": 0, "column": "amount", "value": "5" }] })).await;
    let m = a.ok("materialized", json!({})).await;
    let totals = m.as_array().unwrap().iter().find(|m| m["table"] == "totals").unwrap();
    assert_eq!((totals["stale"].clone(), totals["query"].clone(), totals["query_version"].clone()), (json!(true), q["id"].clone(), json!(1)));
    let refreshed = a.ok("refresh_materialized", json!({ "table": "totals" })).await;
    assert_eq!(refreshed["stale"], false);
    // B's edit was a cell change (cells_changed); the refresh tells everyone the table changed.
    let ev = b.event("table_changed").await.unwrap();
    assert_eq!(ev["table"], "totals");
    assert_eq!(a.ok("query", json!({ "sql": "SELECT * FROM totals ORDER BY team" })).await["rows"], json!([[0, 2005], [1, 2500]]));
    a.close().await;
    b.close().await;
    server.stop().await;

    // Everything is there after a restart.
    let server = TestServer::start(dir.path()).await;
    let mut ws = server.ws(&wb).await;
    let saved = ws.ok("saved_queries", json!({})).await;
    assert_eq!(saved.as_array().unwrap().len(), 1);
    assert_eq!((saved[0]["name"].clone(), saved[0]["version"].clone(), saved[0]["kind"].clone()), (json!("Team totals"), json!(2), json!("live")));
    let versions = ws.ok("query_versions", json!({ "saved": q["id"] })).await;
    assert_eq!(versions.as_array().unwrap().iter().map(|v| v["params"][0]["value"].clone()).collect::<Vec<_>>(), [json!("0"), json!("300")]);
    let m = ws.ok("materialized", json!({})).await;
    assert!(m.as_array().unwrap().iter().any(|m| m["table"] == "totals" && m["stale"] == false));
    assert_eq!(ws.ok("query", json!({ "sql": "SELECT * FROM totals ORDER BY team" })).await["rows"], json!([[0, 2005], [1, 2500]]));
    ws.close().await;
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn sql_changes_apply_once_with_revisions_and_events() {
    let dir = tempfile::tempdir().unwrap();
    let server = TestServer::start(dir.path()).await;
    let wb = server.create_workbook("changes").await;
    let mut a = server.ws(&wb).await;
    let mut b = server.ws(&wb).await;
    a.ok("create_table_from_query", json!({ "name": "t", "sql": "SELECT i AS id, 'v' || i AS s FROM range(5) r(i)" })).await;
    let frame = json!({ "sql": "UPDATE t SET s = $s WHERE id >= $min", "params": [param("s", "text", Some("x")), param("min", "integer", Some("3"))], "mutation_id": "change-1" });
    let id = a.send("sql_change", frame.clone()).await;
    let first = a.reply(id).await.unwrap();
    assert_eq!(first["result"], json!({ "table": "t", "kind": "update", "rows": 2 }));
    let ev = b.event("table_changed").await.unwrap();
    assert_eq!((ev["table"].clone(), ev["revision"].clone()), (json!("t"), first["revision"].clone()));
    // A retry with the same mutation id (e.g. after a reconnect) is not applied again.
    let mut c = server.ws(&wb).await;
    let id = c.send("sql_change", frame).await;
    let second = c.reply(id).await.unwrap();
    assert_eq!((second["replayed"].clone(), second["revision"].clone()), (json!(true), first["revision"].clone()));
    assert_eq!(a.ok("query", json!({ "sql": "SELECT string_agg(s, ',' ORDER BY id) FROM t" })).await["rows"], json!([["v0,v1,v2,x,x"]]));
    // Refused statements change nothing and say why.
    let history = a.ok("history", json!({})).await;
    for sql in ["DROP TABLE t", "UPDATE _rf_meta SET value = '9'", "UPDATE t SET _rf_id = 0", "DELETE FROM t USING t AS u", "UPDATE t SET id = 'x'"] {
        let id = a.send("sql_change", json!({ "sql": sql })).await;
        let r = a.reply(id).await.unwrap();
        assert_eq!(r["ok"], false, "{sql}");
    }
    assert_eq!(a.ok("history", json!({})).await, history);
    a.close().await;
    b.close().await;
    c.close().await;
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn every_open_workbook_is_locked_down_but_application_files_still_work() {
    let dir = tempfile::tempdir().unwrap();
    let server = TestServer::start(dir.path()).await;
    let wb = server.create_workbook("locked").await;
    let (status, _) = server.upload(&wb, "data.csv", b"a,b\n1,x\n2,y\n".to_vec()).await;
    assert_eq!(status, 200, "imports go through the workbook's own upload directory");
    let h = server.state.controller.handle(&wb).unwrap();
    let settings = h.with_reader(|r| Ok((r.setting("enable_external_access")?, r.setting("allowed_directories")?, r.setting("autoload_known_extensions")?))).unwrap();
    assert_eq!(settings.0, "false");
    assert_eq!(settings.2, "false");
    let allowed = settings.1;
    assert!(allowed.contains(&format!(".uploads/{wb}/")) && allowed.contains(&format!(".exports/{wb}/")), "{allowed}");
    assert!(!allowed.contains(".backups") && !allowed.contains(".trash"), "{allowed}");
    // A second workbook's directories are not in this one's list, and vice versa.
    let other = server.create_workbook("other").await;
    let mut ws = server.ws(&other).await;
    ws.ok("tables", json!({})).await;
    let other_allowed = server.state.controller.handle(&other).unwrap().with_reader(|r| r.setting("allowed_directories")).unwrap();
    assert!(!other_allowed.contains(&wb), "{other_allowed}");
    ws.close().await;
    // Even reached past the SQL policy, the database refuses files and settings.
    let outside = dir.path().join("outside.csv");
    std::fs::write(&outside, "x\n1\n").unwrap();
    let r = h.with_reader(|r| r.backup_to(&dir.path().join("stolen.duckdb")));
    assert!(r.is_err(), "the database cannot write outside its directories");
    assert!(!dir.path().join("stolen.duckdb").exists());
    drop(h);

    // Exports, xlsx export, duplicate and backup still work.
    let spec = urlencode(r#"{"table":"data"}"#);
    let (status, _, bytes) = server.download(&format!("/api/workbooks/{wb}/export?spec={spec}&format=csv")).await;
    assert_eq!((status, String::from_utf8(bytes).unwrap()), (200, "a,b\n1,x\n2,y\n".to_string()));
    let (status, copy) = server.send(Method::POST, &format!("/api/workbooks/{wb}/duplicate"), None).await;
    assert_eq!(status, 200, "{copy}");
    let (status, backup) = server.send(Method::POST, "/api/admin/backups", None).await;
    assert_eq!(status, 200, "{backup}");
    assert!(files_under(&dir.path().join(".exports")).is_empty(), "staging copies are cleaned up");

    // Internal tables are unreachable through every table operation over the wire.
    let mut ws = server.ws(&wb).await;
    let history = ws.ok("history", json!({})).await;
    for (op, params) in [
        ("open_view", json!({ "spec": { "table": "_rf_history" } })),
        ("drop_table", json!({ "table": "_rf_views" })),
        ("rename_table", json!({ "table": "_rf_meta", "new_name": "m" })),
        ("add_column", json!({ "table": "_rf_meta", "name": "x", "sql_type": "INTEGER" })),
        ("add_formula_column", json!({ "table": "_rf_tables", "name": "x", "formula": "1" })),
        ("save_view", json!({ "name": "v", "spec": { "table": "_rf_history" } })),
        ("create_table_from_query", json!({ "name": "leak", "sql": "SELECT * FROM _rf_history" })),
        ("create_sheet_from_query", json!({ "name": "leak", "sql": "SELECT * FROM duckdb_settings()" })),
        ("export_query", json!({ "sql": "SELECT * FROM _rf_meta" })),
        ("sql_change", json!({ "sql": "DELETE FROM _rf_history" })),
    ] {
        assert!(ws.call(op, params.clone()).await.is_err(), "{op} {params}");
    }
    let spec = urlencode(r#"{"table":"_rf_history"}"#);
    assert_ne!(server.download(&format!("/api/workbooks/{wb}/export?spec={spec}&format=csv")).await.0, 200);
    assert_eq!(ws.ok("history", json!({})).await, history);
    ws.close().await;
    server.stop().await;
}
