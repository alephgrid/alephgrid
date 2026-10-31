//! 3.01, 3.02 and 3.07 over the wire: scheduled jobs (priority, fairness, cancellation,
//! deadlines, bounded queues), size limits applied before work, slow clients, and
//! revisions with idempotent mutation ids.

mod common;

use std::time::{Duration, Instant};

use common::*;
use serde_json::{json, Value};

/// Runs for minutes unless interrupted (a sum over 10^11 integers).
const SLOW_SQL: &str = "SELECT sum(i) AS s FROM range(100000000000) r(i)";

async fn table(ws: &mut Ws, name: &str, sql: &str) -> u64 {
    ws.ok("create_table_from_query", json!({ "name": name, "sql": sql })).await;
    ws.ok("open_view", json!({ "spec": { "table": name } })).await["view_id"].as_u64().unwrap()
}

/// Wait until the workbook's writer is busy with something (observable, not a sleep).
async fn until_writer_busy(server: &TestServer, wb: &str) {
    let state = server.state.clone();
    let wb = wb.to_string();
    eventually("the writer is busy", move || state.controller.handle(&wb).is_some_and(|h| h.sched.load() > 0 && h.active() > 0)).await;
}

fn revision_of(r: &Value) -> u64 {
    r["revision"].as_u64().unwrap_or_else(|| panic!("no revision in {r}"))
}

// ------------------------------------------------------------------ 3.01

#[tokio::test(flavor = "multi_thread")]
async fn reads_are_answered_while_a_long_change_runs_and_cancel_hits_only_its_target() {
    let dir = tempfile::tempdir().unwrap();
    let server = TestServer::start(dir.path()).await;
    let wb = server.create_workbook("sched").await;
    let mut a = server.ws(&wb).await;
    let mut b = server.ws(&wb).await;
    let view = table(&mut a, "t", "SELECT i AS n FROM range(1000) r(i)").await;
    let before = a.ok("history", json!({})).await.as_array().unwrap().len();
    // A starts a change that would run for minutes; B's change queues behind it.
    let slow = a.send("create_table_from_query", json!({ "name": "slow", "sql": SLOW_SQL })).await;
    until_writer_busy(&server, &wb).await;
    let queued = b.send("edit", json!({ "view_id": view, "edits": [{ "row": 0, "column": "n", "value": "-1" }] })).await;
    // Reads do not wait for the writer.
    let started = Instant::now();
    let v = b.ok("open_view", json!({ "spec": { "table": "t" } })).await["view_id"].as_u64().unwrap();
    let block = b.ok("fetch", json!({ "view_id": v, "row_start": 998, "row_count": 5 })).await;
    assert_eq!(block["rows"], json!([[998], [999]]));
    assert!(started.elapsed() < Duration::from_secs(5), "reads waited {:?}", started.elapsed());
    assert!(a.replies.is_empty() && b.buffered() == 0, "the long change is still running");
    // B cannot cancel A's request: request ids belong to their connection.
    assert_eq!(b.ok("cancel", json!({ "request": slow })).await, json!(false));
    // A cancels its own: the change is interrupted and leaves nothing.
    assert_eq!(a.ok("cancel", json!({ "request": slow })).await, json!(true));
    let r = a.reply(slow).await.unwrap();
    assert_eq!(r["ok"], false, "{r}");
    assert_eq!(r["code"], "cancelled", "{r}");
    // B's queued change then runs, unaffected.
    let r = b.reply(queued).await.unwrap();
    assert_eq!(r["ok"], true, "{r}");
    let tables: Vec<String> = a.ok("tables", json!({})).await.as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap().to_string()).collect();
    assert_eq!(tables, ["t"], "the cancelled table does not exist");
    let history = a.ok("history", json!({})).await;
    assert_eq!(history.as_array().unwrap().len(), before + 1, "only B's edit committed: {history}");
    assert_eq!(a.ok("fetch", json!({ "view_id": view, "row_start": 0, "row_count": 1 })).await["rows"], json!([[-1]]));
    a.close().await;
    b.close().await;
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn deadlines_apply_while_waiting_and_while_running() {
    let dir = tempfile::tempdir().unwrap();
    let server = TestServer::start(dir.path()).await;
    let wb = server.create_workbook("deadline").await;
    let mut ws = server.ws(&wb).await;
    let view = table(&mut ws, "t", "SELECT 1 AS n").await;
    // Running: a slow read with a short deadline is interrupted.
    let r = ws.send("query", json!({ "sql": SLOW_SQL, "timeout_ms": 300 })).await;
    let started = Instant::now();
    let reply = ws.reply(r).await.unwrap();
    assert_eq!(reply["code"], "deadline_exceeded", "{reply}");
    assert!(started.elapsed() < Duration::from_secs(10));
    // Waiting: a change behind a long one expires without running.
    let slow = ws.send("create_table_from_query", json!({ "name": "slow", "sql": SLOW_SQL })).await;
    until_writer_busy(&server, &wb).await;
    let late = ws.send("edit", json!({ "view_id": view, "edits": [{ "row": 0, "column": "n", "value": "2" }], "timeout_ms": 200 })).await;
    let reply = ws.reply(late).await.unwrap();
    assert_eq!(reply["code"], "deadline_exceeded", "{reply}");
    ws.ok("cancel", json!({ "request": slow })).await;
    assert_eq!(ws.reply(slow).await.unwrap()["code"], "cancelled");
    assert_eq!(ws.ok("fetch", json!({ "view_id": view, "row_start": 0, "row_count": 1 })).await["rows"], json!([[1]]), "the expired edit never ran");
    ws.close().await;
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_full_queue_refuses_with_overloaded_and_every_request_gets_one_reply() {
    let dir = tempfile::tempdir().unwrap();
    let server = TestServer::start(dir.path()).await;
    let wb = server.create_workbook("full").await;
    let mut ws = server.ws(&wb).await;
    let view = table(&mut ws, "t", "SELECT 0 AS n").await;
    let slow = ws.send("create_table_from_query", json!({ "name": "slow", "sql": SLOW_SQL })).await;
    until_writer_busy(&server, &wb).await;
    let ids: Vec<u64> = {
        let mut v = Vec::new();
        for k in 0..40 {
            v.push(ws.send("edit", json!({ "view_id": view, "edits": [{ "row": 0, "column": "n", "value": k.to_string() }] })).await);
        }
        v
    };
    ws.ok("cancel", json!({ "request": slow })).await;
    let mut ok = 0;
    let mut overloaded = 0;
    let mut revisions = Vec::new();
    for id in ids {
        let r = ws.reply(id).await.expect("every request gets a reply");
        if r["ok"] == true {
            ok += 1;
            revisions.push(revision_of(&r));
        } else {
            assert_eq!(r["code"], "overloaded", "{r}");
            overloaded += 1;
        }
    }
    assert_eq!(ws.reply(slow).await.unwrap()["code"], "cancelled");
    // 32 per connection: the slow change plus 31 edits fit, the other 9 were refused.
    assert_eq!((ok, overloaded), (31, 9));
    // Accepted edits ran in order, one revision each.
    assert!(revisions.windows(2).all(|w| w[1] == w[0] + 1), "{revisions:?}");
    assert_eq!(ws.ok("fetch", json!({ "view_id": view, "row_start": 0, "row_count": 1 })).await["rows"], json!([[30]]));
    ws.close().await;
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_flooding_session_does_not_hold_back_another() {
    let dir = tempfile::tempdir().unwrap();
    let server = TestServer::start(dir.path()).await;
    let wb = server.create_workbook("fair").await;
    let mut a = server.ws(&wb).await;
    let mut b = server.ws(&wb).await;
    let view = table(&mut a, "t", "SELECT 0 AS n, 0 AS m").await;
    let slow = a.send("create_table_from_query", json!({ "name": "slow", "sql": SLOW_SQL })).await;
    until_writer_busy(&server, &wb).await;
    let flood: Vec<u64> = {
        let mut v = Vec::new();
        for k in 0..20 {
            v.push(a.send("edit", json!({ "view_id": view, "edits": [{ "row": 0, "column": "n", "value": k.to_string() }] })).await);
        }
        v
    };
    let state = server.state.clone();
    let w = wb.clone();
    eventually("A's edits are queued", move || state.controller.handle(&w).is_some_and(|h| h.sched.waiting().1 >= 20)).await;
    let light = b.send("edit", json!({ "view_id": view, "edits": [{ "row": 0, "column": "m", "value": "1" }] })).await;
    a.ok("cancel", json!({ "request": slow })).await;
    let b_rev = revision_of(&b.reply(light).await.unwrap());
    let mut a_revs = Vec::new();
    for id in flood {
        a_revs.push(revision_of(&a.reply(id).await.unwrap()));
    }
    let first_a = *a_revs.iter().min().unwrap();
    // Round-robin: B's one request runs right after A's first, not after all twenty.
    assert!(b_rev <= first_a + 1, "B ran at revision {b_rev}, A's edits at {a_revs:?}");
    a.close().await;
    b.close().await;
    server.stop().await;
}

// ------------------------------------------------------------------ 3.07

#[tokio::test(flavor = "multi_thread")]
async fn revisions_are_contiguous_across_sessions_and_events_carry_them() {
    let dir = tempfile::tempdir().unwrap();
    let server = TestServer::start(dir.path()).await;
    let wb = server.create_workbook("rev").await;
    let mut a = server.ws(&wb).await;
    let mut watcher = server.ws(&wb).await;
    let view = table(&mut a, "t", "SELECT i AS n FROM range(10) r(i)").await;
    let mut b = server.ws(&wb).await;
    assert_eq!(b.hello["revision"], 1, "hello carries the current revision");
    let vb = b.ok("open_view", json!({ "spec": { "table": "t" } })).await["view_id"].as_u64().unwrap();
    let mut ids_a = Vec::new();
    let mut ids_b = Vec::new();
    for k in 0..10 {
        ids_a.push(a.send("edit", json!({ "view_id": view, "edits": [{ "row": k, "column": "n", "value": "100" }] })).await);
        ids_b.push(b.send("edit", json!({ "view_id": vb, "edits": [{ "row": k, "column": "n", "value": "200" }] })).await);
    }
    let mut revs = Vec::new();
    for id in ids_a {
        revs.push(revision_of(&a.reply(id).await.unwrap()));
    }
    for id in ids_b {
        revs.push(revision_of(&b.reply(id).await.unwrap()));
    }
    revs.sort();
    assert_eq!(revs, (2..=21).collect::<Vec<u64>>(), "one revision per change, no gaps, no repeats");
    // The watcher saw every change, in revision order.
    let mut seen = Vec::new();
    while seen.len() < 20 {
        let ev = watcher.event("cells_changed").await.unwrap();
        seen.push(ev["revision"].as_u64().unwrap());
    }
    assert_eq!(seen, (2..=21).collect::<Vec<u64>>());
    let history = a.ok("history", json!({ "after": 1 })).await;
    let h: Vec<u64> = history.as_array().unwrap().iter().map(|e| e["revision"].as_u64().unwrap()).collect();
    assert_eq!(h, (2..=21).collect::<Vec<u64>>());
    // History names who made each change: here the local user (docs/TEAMS.md).
    assert_eq!(history[0]["actor"], "local");
    assert_eq!(history[0]["op"], "edit");
    for s in [a, b, watcher] {
        s.close().await;
    }
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_retried_mutation_id_is_applied_once_across_reconnects_and_restarts() {
    let dir = tempfile::tempdir().unwrap();
    let server = TestServer::start(dir.path()).await;
    let wb = server.create_workbook("retry").await;
    let mut ws = server.ws(&wb).await;
    let view = table(&mut ws, "t", "SELECT 1 AS n").await;
    let insert = |view: u64| json!({ "view_id": view, "at": 0, "count": 2, "mutation_id": "client-7-op-1" });
    let id = ws.send("insert_rows", insert(view)).await;
    let first = ws.reply(id).await.unwrap();
    assert_eq!(first["ok"], true);
    assert!(first.get("replayed").is_none());
    // The same mutation again (the client did not see the reply and retries).
    let id = ws.send("insert_rows", insert(view)).await;
    let again = ws.reply(id).await.unwrap();
    assert_eq!(again["replayed"], true, "{again}");
    assert_eq!(again["revision"], first["revision"]);
    ws.close().await;
    // From a new connection, and after a restart.
    server.stop().await;
    let server = TestServer::start(dir.path()).await;
    let mut ws = server.ws(&wb).await;
    let view = ws.ok("open_view", json!({ "spec": { "table": "t" } })).await["view_id"].as_u64().unwrap();
    let id = ws.send("insert_rows", insert(view)).await;
    let replay = ws.reply(id).await.unwrap();
    assert_eq!(replay["replayed"], true);
    assert_eq!(replay["revision"], first["revision"]);
    assert_eq!(ws.ok("view_info", json!({ "view_id": view })).await["row_count"], 3, "inserted once");
    // A different id applies.
    let id = ws.send("insert_rows", json!({ "view_id": view, "at": 0, "count": 1, "mutation_id": "client-7-op-2" })).await;
    assert_eq!(revision_of(&ws.reply(id).await.unwrap()), revision_of(&first) + 1);
    ws.close().await;
    server.stop().await;
}

// ------------------------------------------------------------------ 3.02

#[tokio::test(flavor = "multi_thread")]
async fn requested_dimensions_are_clamped_and_replies_are_bounded() {
    let dir = tempfile::tempdir().unwrap();
    let server = TestServer::start(dir.path()).await;
    let wb = server.create_workbook("limits").await;
    let mut ws = server.ws(&wb).await;
    // Overflowing and absurd numbers: answered, clamped, nothing allocated for them.
    let narrow = table(&mut ws, "narrow", "SELECT i AS n FROM range(20000) r(i)").await;
    let b = ws.ok("fetch", json!({ "view_id": narrow, "row_start": u64::MAX, "row_count": u64::MAX })).await;
    assert_eq!(b["rows"], json!([]));
    let b = ws.ok("fetch", json!({ "view_id": narrow, "row_start": 0, "row_count": u64::MAX, "column_start": u32::MAX })).await;
    assert_eq!(b["rows"].as_array().unwrap().len(), 5000);
    assert_eq!(b["rows"][0], json!([]), "no columns at that offset");
    let b = ws.ok("fetch", json!({ "view_id": narrow, "row_start": u64::MAX - 1, "row_count": 10 })).await;
    assert_eq!(b["rows"], json!([]));
    // Wide: 500 columns → at most 500 rows per fetch (250,000 cells), and a column range
    // returns exactly that slice of the full width.
    let cols: Vec<String> = (0..500).map(|c| format!("i + {c} AS c{c}")).collect();
    let wide = table(&mut ws, "wide", &format!("SELECT {} FROM range(2000) r(i)", cols.join(", "))).await;
    let full = ws.ok("fetch", json!({ "view_id": wide, "row_start": 0, "row_count": 5000 })).await;
    assert_eq!(full["rows"].as_array().unwrap().len(), 500);
    let part = ws.ok("fetch", json!({ "view_id": wide, "row_start": 10, "row_count": 3, "column_start": 200, "column_count": 4 })).await;
    assert_eq!(part["column_start"], 200);
    for (i, row) in part["rows"].as_array().unwrap().iter().enumerate() {
        assert_eq!(row, &json!(full["rows"][10 + i].as_array().unwrap()[200..204]));
    }
    // Text-heavy: 100 rows of 1 MB each would be 100 MB; the reply stops near 8 MiB.
    let heavy = table(&mut ws, "heavy", "SELECT repeat('x', 1000000) AS s FROM range(100)").await;
    let id = ws.send("fetch", json!({ "view_id": heavy, "row_start": 0, "row_count": 100 })).await;
    let r = ws.reply(id).await.unwrap();
    let size = r.to_string().len();
    let rows = r["result"]["rows"].as_array().unwrap().len();
    assert_eq!(r["result"]["truncated"], true);
    assert!((1..100).contains(&rows) && size <= (8 << 20) + (1 << 20), "{rows} rows, {size} bytes");
    // Too many rows in one insert: refused before anything happens.
    let r = ws.send("insert_rows", json!({ "view_id": narrow, "at": 0, "count": 2_000_000 })).await;
    assert_eq!(ws.reply(r).await.unwrap()["code"], "too_large");
    assert_eq!(ws.ok("view_info", json!({ "view_id": narrow })).await["row_count"], 20000);
    // Oversized sheet fetch: clamped too.
    ws.ok("sheet_create", json!({ "name": "S" })).await;
    let b = ws.ok("sheet_fetch", json!({ "sheet": 0, "row_start": 0, "row_count": u32::MAX, "column_count": u32::MAX })).await;
    assert!(b["rows"].as_array().unwrap().len() <= 5000);
    ws.close().await;
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn messages_over_the_limit_close_only_that_connection() {
    let dir = tempfile::tempdir().unwrap();
    let server = TestServer::start_with(dir.path(), |c| c.max_message_bytes = 64 << 10).await;
    let wb = server.create_workbook("msg").await;
    let mut ok = server.ws(&wb).await;
    let view = table(&mut ok, "t", "SELECT 1 AS n").await;
    let mut big = server.ws(&wb).await;
    let edits: Vec<Value> = (0..5000).map(|i| json!({ "row": 0, "column": "n", "value": i.to_string() })).collect();
    big.send_raw(json!({ "id": 1, "op": "edit", "view_id": view, "edits": edits })).await;
    assert!(big.closed().await, "the server closes a connection that sends an oversized message");
    // Nothing was applied, and other connections are unaffected.
    assert_eq!(ok.ok("fetch", json!({ "view_id": view, "row_start": 0, "row_count": 1 })).await["rows"], json!([[1]]));
    ok.close().await;
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn uploads_are_bounded_by_size_and_by_the_temp_budget() {
    let dir = tempfile::tempdir().unwrap();
    let server = TestServer::start_with(dir.path(), |c| {
        c.max_upload_bytes = 1 << 20;
        c.temp_budget = Some(3 << 20); // 1.5 MiB for uploads
    })
    .await;
    let wb = server.create_workbook("up").await;
    let uploads = dir.path().join(".uploads");
    let staged_bytes = || async {
        let (_, usage) = server.get("/api/admin/resources").await;
        usage["upload_bytes"].as_u64().unwrap()
    };
    // Over the limit without Content-Length (streamed): refused mid-stream.
    let chunks: Vec<Result<Vec<u8>, std::io::Error>> = (0..20).map(|_| Ok(vec![b'x'; 100_000])).collect();
    let body = reqwest::Body::wrap_stream(futures_util::stream::iter(chunks));
    let res = server.http.post(server.url(&format!("/api/workbooks/{wb}/uploads?filename=c.csv"))).body(body).send().await;
    if let Ok(res) = res {
        assert_eq!(res.status(), 413);
    }
    assert_eq!(files_under(&uploads).len(), 0, "the partial file was removed");
    assert_eq!(staged_bytes().await, 0);
    // Exactly at the limit: accepted.
    let csv = |bytes: usize| {
        let mut s = String::from("s\n");
        while s.len() < bytes {
            s.push_str("abcdefghij\n");
        }
        s.truncate(bytes - 1);
        s.push('\n');
        s.into_bytes()
    };
    let res = server.http.post(server.url(&format!("/api/workbooks/{wb}/uploads?filename=a.csv"))).body(csv(1 << 20)).send().await.unwrap();
    assert_eq!(res.status(), 200);
    let first: Value = res.json().await.unwrap();
    assert_eq!(staged_bytes().await, 1 << 20, "a staged upload holds its bytes");
    // One byte over, with Content-Length: refused before the body is read. The client may
    // see the 413 or, still sending, a reset connection; either way nothing is stored.
    let res = server.http.post(server.url(&format!("/api/workbooks/{wb}/uploads?filename=b.csv"))).body(csv((1 << 20) + 1)).send().await;
    if let Ok(res) = res {
        assert_eq!(res.status(), 413);
    }
    assert_eq!(files_under(&uploads).len(), 1);
    // Within the limit but beyond what is left of the budget: 507, nothing left behind.
    // The server answers 507 as soon as the budget runs out, possibly while the client is
    // still sending (as for 413 above): it may see the 507 or a reset; nothing is stored.
    let res = server.http.post(server.url(&format!("/api/workbooks/{wb}/uploads?filename=d.csv"))).body(csv(900_000)).send().await;
    if let Ok(res) = res {
        assert_eq!(res.status(), 507);
    }
    assert_eq!(staged_bytes().await, 1 << 20, "failed uploads released their reservation");
    assert_eq!(files_under(&uploads).len(), 1, "only the staged upload is on disk");
    // Discarding the staged upload frees the space.
    let (code, _) = server.send(reqwest::Method::DELETE, &format!("/api/workbooks/{wb}/uploads/{}", first["upload_id"].as_str().unwrap()), None).await;
    assert_eq!(code, 204);
    assert_eq!(staged_bytes().await, 0);
    let res = server.http.post(server.url(&format!("/api/workbooks/{wb}/uploads?filename=d.csv"))).body(csv(900_000)).send().await.unwrap();
    assert_eq!(res.status(), 200);
    // A disk-full write during an upload: 507, the partial file is gone.
    alephgrid_server::state::arm_fault_in("upload_write", dir.path(), 1);
    let res = server.http.post(server.url(&format!("/api/workbooks/{wb}/uploads?filename=e.csv"))).body(csv(1000)).send().await.unwrap();
    assert_eq!(res.status(), 507);
    assert_eq!(files_under(&uploads).len(), 1);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_client_that_does_not_read_is_cut_off_with_bounded_buffering() {
    let dir = tempfile::tempdir().unwrap();
    let server = TestServer::start_with(dir.path(), |c| {
        c.outbox_pause_bytes = 1 << 20;
        c.outbox_max_bytes = 4 << 20;
    })
    .await;
    let wb = server.create_workbook("slow").await;
    let mut good = server.ws(&wb).await;
    let view = table(&mut good, "t", "SELECT repeat('y', 100000) AS s FROM range(200)").await;
    // A raw client that sends many large fetches and never reads.
    let (mut sink, stream) = {
        use futures_util::StreamExt;
        let (s, _) = tokio_tungstenite::connect_async(format!("ws://{}/ws/{wb}", server.addr)).await.unwrap();
        s.split()
    };
    // Registered before it sends anything (and so before it can be cut off); otherwise "one
    // session left" below could hold before the slow session even existed.
    let state = server.state.clone();
    eventually("the slow session is registered", move || state.sessions.lock().unwrap().len() == 2).await;
    {
        use futures_util::SinkExt;
        let v = server.state.controller.handle(&wb).unwrap();
        let _ = v;
        for i in 0..30 {
            let frame = json!({ "id": i + 1, "op": "fetch", "view_id": view, "row_start": 0, "row_count": 200 });
            if sink.send(tokio_tungstenite::tungstenite::Message::Text(frame.to_string().into())).await.is_err() {
                break;
            }
        }
    }
    // The server stops buffering for it and closes it.
    let state = server.state.clone();
    eventually("the slow session is closed", move || state.sessions.lock().unwrap().len() == 1).await;
    let (_, sessions) = server.get("/api/admin/sessions").await;
    assert_eq!(sessions.as_array().unwrap().len(), 1, "{sessions}");
    // The connection that reads is fine.
    assert_eq!(good.ok("view_info", json!({ "view_id": view })).await["row_count"], 200);
    drop(stream);
    drop(sink);
    good.close().await;
    server.stop().await;
}
