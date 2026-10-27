//! 3.03, 3.04 and 3.11–3.14 over the real server: runtime specs, the lifecycle of
//! allocations (single-flight starts, cancel before ready, failed starts, stop, restart
//! reconciliation, fail-closed ownership), the host budget and idle eviction.

mod common;

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use common::*;
use reqwest::Method;
use rowfinity_server::runtime::budget::parse_bytes;
use rowfinity_server::runtime::controller::StageHook;
use rowfinity_server::runtime::local::LocalProvider;
use rowfinity_server::runtime::{AllocationState, FailureKind, RuntimeFailure, RuntimeSpec};
use serde_json::{json, Value};

/// Holds starts of one workbook at one stage until opened; counts arrivals.
#[derive(Default)]
struct Gate {
    target: Mutex<Option<(String, AllocationState)>>,
    open: Mutex<bool>,
    cv: Condvar,
    reached: AtomicUsize,
    fail: AtomicBool,
}

impl Gate {
    fn hook(self: &Arc<Self>) -> StageHook {
        let g = self.clone();
        Arc::new(move |wb: &str, stage: AllocationState| {
            let hit = g.target.lock().unwrap().as_ref().is_some_and(|(w, s)| w == wb && *s == stage);
            if !hit {
                return Ok(());
            }
            g.reached.fetch_add(1, Ordering::SeqCst);
            let mut open = g.open.lock().unwrap();
            while !*open {
                open = g.cv.wait(open).unwrap();
            }
            if g.fail.load(Ordering::SeqCst) {
                return Err(RuntimeFailure::new(FailureKind::StartupFailed, "injected startup failure"));
            }
            Ok(())
        })
    }

    fn arm(&self, workbook: &str, stage: AllocationState) {
        *self.target.lock().unwrap() = Some((workbook.to_string(), stage));
        *self.open.lock().unwrap() = false;
    }

    fn release(&self) {
        *self.open.lock().unwrap() = true;
        self.cv.notify_all();
    }
}

/// Create a workbook's files without opening it (as an earlier run would have left them).
async fn closed_workbook(server: &TestServer, name: &str) -> String {
    let id = server.create_workbook(name).await;
    let (status, _) = server.send(Method::POST, &format!("/api/workbooks/{id}/runtime"), Some(json!({ "action": "stop" }))).await;
    assert_eq!(status, 200);
    id
}

async fn status(server: &TestServer, wb: &str) -> Value {
    let (code, body) = server.get(&format!("/api/workbooks/{wb}/runtime")).await;
    assert_eq!(code, 200, "{body}");
    body
}

fn state_of(s: &Value) -> String {
    s["allocation"]["state"].as_str().unwrap_or("none").to_string()
}

fn lock_is_free(dir: &std::path::Path) -> bool {
    LocalProvider::lock(dir).is_ok()
}

// ------------------------------------------------------------------ 3.11

#[tokio::test(flavor = "multi_thread")]
async fn specs_are_content_addressed_pinned_and_data_outlives_compute() {
    let dir = tempfile::tempdir().unwrap();
    let server = TestServer::start(dir.path()).await;
    let wb = server.create_workbook("pinned").await;
    let mut ws = server.ws(&wb).await;
    ws.ok("create_table_from_query", json!({ "name": "t", "sql": "SELECT 42 AS answer" })).await;
    ws.close().await;
    let s = status(&server, &wb).await;
    let current = RuntimeSpec::current();
    assert_eq!(s["spec_id"], current.id());
    assert_eq!(s["spec"]["storage_format"], rowfinity_engine::storage::FORMAT);
    assert_eq!(s["spec"]["capabilities"], json!(["tables", "sheets", "sql"]));
    let meta: Value = serde_json::from_slice(&std::fs::read(dir.path().join(&wb).join("workbook.json")).unwrap()).unwrap();
    assert_eq!(meta["runtime_spec"], current.id());
    let stored: RuntimeSpec =
        serde_json::from_slice(&std::fs::read(dir.path().join(".control/specs").join(format!("{}.json", current.id()))).unwrap()).unwrap();
    assert_eq!(stored, current);
    // Same content, same id; any change, another id.
    let mut other = current.clone();
    other.engine_version.push('+');
    assert_ne!(other.id(), current.id());
    assert_eq!(RuntimeSpec::current().id(), current.id());

    // Stopping the compute keeps the workbook: identity, data, revision.
    let (code, stopped) = server.send(Method::POST, &format!("/api/workbooks/{wb}/runtime"), Some(json!({ "action": "stop" }))).await;
    assert_eq!(code, 200, "{stopped}");
    assert_eq!(state_of(&stopped), "stopped");
    assert!(lock_is_free(&dir.path().join(&wb)));
    let first_epoch = stopped["allocation"]["epoch"].as_u64().unwrap();
    let mut ws = server.ws(&wb).await;
    let q = ws.ok("query", json!({ "sql": "SELECT answer FROM t" })).await;
    assert_eq!(q["rows"], json!([[42]]));
    ws.close().await;
    let again = status(&server, &wb).await;
    assert_eq!(state_of(&again), "ready");
    assert_eq!(again["allocation"]["epoch"].as_u64().unwrap(), first_epoch + 1, "a new allocation, a new epoch");
    assert_ne!(again["allocation"]["id"], stopped["allocation"]["id"]);
    assert_eq!(again["ownership"]["epoch"].as_u64().unwrap(), first_epoch + 1);
    assert_eq!(again["ownership"]["mechanism"], "flock");
    server.stop().await;
}

// --------------------------------------------------------------- 3.12–3.14

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_opens_share_one_allocation() {
    let dir = tempfile::tempdir().unwrap();
    let gate = Arc::new(Gate::default());
    let hook = gate.hook();
    let server = TestServer::start_with(dir.path(), |c| c.stage_hook = Some(hook)).await;
    let wb = closed_workbook(&server, "busy").await;
    gate.arm(&wb, AllocationState::Starting);
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let state = server.state.clone();
        let wb = wb.clone();
        tasks.push(tokio::task::spawn_blocking(move || state.get(&wb).map(|h| Arc::as_ptr(&h) as usize)));
    }
    eventually("the first start reaches the gate", || gate.reached.load(Ordering::SeqCst) == 1).await;
    assert_eq!(state_of(&status(&server, &wb).await), "requested");
    gate.release();
    let mut ptrs = Vec::new();
    for t in tasks {
        ptrs.push(t.await.unwrap().unwrap());
    }
    assert!(ptrs.windows(2).all(|w| w[0] == w[1]), "every caller got the same handle");
    assert_eq!(gate.reached.load(Ordering::SeqCst), 1, "only one start ran");
    let s = status(&server, &wb).await;
    let stages: Vec<&str> = s["allocation"]["transitions"].as_array().unwrap().iter().map(|t| t["to"].as_str().unwrap()).collect();
    assert_eq!(stages, ["starting", "running", "ready"]);
    // Starting again with an operation id already used does nothing; so does a second stop.
    let op = s["allocation"]["transitions"][0]["operation"].as_str().unwrap().to_string();
    let (_, same) = server.send(Method::POST, &format!("/api/workbooks/{wb}/runtime"), Some(json!({ "action": "start", "operation_id": op }))).await;
    assert_eq!(same["allocation"]["id"], s["allocation"]["id"]);
    for _ in 0..2 {
        let (code, st) =
            server.send(Method::POST, &format!("/api/workbooks/{wb}/runtime"), Some(json!({ "action": "stop", "operation_id": "stop-1" }))).await;
        assert_eq!(code, 200);
        assert_eq!(state_of(&st), "stopped");
    }
    let s = status(&server, &wb).await;
    assert_eq!(s["allocation"]["transitions"].as_array().unwrap().iter().filter(|t| t["to"] == "stopped").count(), 1);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_start_cancelled_before_ready_releases_everything() {
    let dir = tempfile::tempdir().unwrap();
    let gate = Arc::new(Gate::default());
    let hook = gate.hook();
    let server = TestServer::start_with(dir.path(), |c| c.stage_hook = Some(hook)).await;
    let wb = closed_workbook(&server, "cancel").await;
    gate.arm(&wb, AllocationState::Running);
    let state = server.state.clone();
    let w = wb.clone();
    let opening = tokio::task::spawn_blocking(move || state.get(&w).map(|_| ()).map_err(|e| format!("{e:#}")));
    eventually("the engine is open, not ready yet", || gate.reached.load(Ordering::SeqCst) == 1).await;
    assert_eq!(state_of(&status(&server, &wb).await), "starting", "requested → starting, waiting before running");
    assert!(!lock_is_free(&dir.path().join(&wb)), "the starting allocation holds the lock");
    // Cancel (from another request) while it waits; then let it continue.
    let state = server.state.clone();
    let w = wb.clone();
    let cancel = tokio::task::spawn_blocking(move || state.controller.cancel(&w));
    let state = server.state.clone();
    let w = wb.clone();
    eventually("the cancellation is requested", move || state.controller.cancel_pending(&w)).await;
    gate.release();
    assert_eq!(cancel.await.unwrap(), Some(AllocationState::Stopped));
    let err = opening.await.unwrap().unwrap_err();
    assert!(err.contains("cancelled"), "{err}");
    let s = status(&server, &wb).await;
    assert_eq!(state_of(&s), "stopped");
    assert_eq!(s["allocation"]["failure"]["kind"], "cancelled");
    assert!(lock_is_free(&dir.path().join(&wb)), "lock released");
    assert!(!server.state.controller.temp_dir(&wb).exists(), "scratch removed");
    let (_, usage) = server.get("/api/admin/resources").await;
    assert_eq!(usage["reserved"]["memory_bytes"], 0);
    // The next open is a fresh allocation.
    gate.arm("nobody", AllocationState::Running);
    let mut ws = server.ws(&wb).await;
    ws.ok("tables", json!({})).await;
    ws.close().await;
    assert_eq!(state_of(&status(&server, &wb).await), "ready");
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn failed_starts_leave_no_lock_reservation_or_scratch() {
    let dir = tempfile::tempdir().unwrap();
    let gate = Arc::new(Gate::default());
    let hook = gate.hook();
    let server = TestServer::start_with(dir.path(), |c| c.stage_hook = Some(hook)).await;
    let wb = closed_workbook(&server, "broken").await;
    let wbdir = dir.path().join(&wb);
    let check_clean = |s: &Value, kind: &str| {
        assert_eq!(state_of(s), "failed", "{s}");
        assert_eq!(s["allocation"]["failure"]["kind"], kind);
        assert!(lock_is_free(&wbdir));
        assert!(!server.state.controller.temp_dir(&wb).exists());
    };
    // 1. A corrupt database file.
    let good = std::fs::read(wbdir.join("data.duckdb")).unwrap();
    std::fs::write(wbdir.join("data.duckdb"), b"this is not a database").unwrap();
    let (code, err) = server.get(&format!("/api/workbooks/{wb}")).await;
    assert_eq!(code, 503, "{err}");
    assert!(err["error"].as_str().unwrap().contains("could not open the workbook"), "{err}");
    check_clean(&status(&server, &wb).await, "startup_failed");
    // 2. A failure injected between stages.
    std::fs::write(wbdir.join("data.duckdb"), &good).unwrap();
    gate.arm(&wb, AllocationState::Running);
    gate.fail.store(true, Ordering::SeqCst);
    gate.release();
    let (code, _) = server.get(&format!("/api/workbooks/{wb}")).await;
    assert_eq!(code, 503);
    check_clean(&status(&server, &wb).await, "startup_failed");
    // 3. The allocation record cannot be written (disk full): nothing starts, nothing left.
    gate.arm("nobody", AllocationState::Running);
    rowfinity_server::state::arm_fault_in("write_durably", dir.path(), 1);
    let (code, err) = server.get(&format!("/api/workbooks/{wb}")).await;
    assert_eq!(code, 503, "{err}");
    assert!(err["error"].as_str().unwrap().contains("no space left"), "{err}");
    assert!(lock_is_free(&wbdir));
    let leftovers: Vec<_> = files_under(&dir.path().join(".control")).into_iter().filter(|p| p.to_string_lossy().ends_with(".tmp")).collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
    // Repaired: it opens, with the data it had.
    let (code, body) = server.get(&format!("/api/workbooks/{wb}")).await;
    assert_eq!(code, 200, "{body}");
    assert_eq!(state_of(&status(&server, &wb).await), "ready");
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn incompatible_requirements_are_refused_without_touching_data() {
    let dir = tempfile::tempdir().unwrap();
    let server = TestServer::start(dir.path()).await;
    let wb = closed_workbook(&server, "needs python").await;
    let wbdir = dir.path().join(&wb);
    let mut spec = RuntimeSpec::current();
    spec.capabilities.insert(rowfinity_server::runtime::Capability::Python);
    let id = rowfinity_server::runtime::store_spec(&dir.path().join(".control"), &spec).unwrap();
    let mut meta: Value = serde_json::from_slice(&std::fs::read(wbdir.join("workbook.json")).unwrap()).unwrap();
    meta["runtime_spec"] = json!(id);
    std::fs::write(wbdir.join("workbook.json"), serde_json::to_vec(&meta).unwrap()).unwrap();
    let before = std::fs::read(wbdir.join("data.duckdb")).unwrap();
    let (code, err) = server.get(&format!("/api/workbooks/{wb}")).await;
    assert_eq!(code, 409, "{err}");
    assert!(err["error"].as_str().unwrap().contains("python"), "{err}");
    let s = status(&server, &wb).await;
    assert_eq!(s["allocation"]["failure"]["kind"], "incompatible");
    assert_eq!(std::fs::read(wbdir.join("data.duckdb")).unwrap(), before);
    assert!(lock_is_free(&wbdir));
    // A file from a newer storage format is refused the same way.
    meta["runtime_spec"] = Value::Null;
    std::fs::write(wbdir.join("workbook.json"), serde_json::to_vec(&meta).unwrap()).unwrap();
    {
        let conn = duckdb::Connection::open(wbdir.join("data.duckdb")).unwrap();
        conn.execute_batch("UPDATE _rf_meta SET value = '7' WHERE key = 'format'; CHECKPOINT;").unwrap();
    }
    let (code, err) = server.get(&format!("/api/workbooks/{wb}")).await;
    assert_eq!(code, 409, "{err}");
    assert!(err["error"].as_str().unwrap().contains("newer version"), "{err}");
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_restarted_controller_reconciles_and_fails_closed_on_foreign_owners() {
    let dir = tempfile::tempdir().unwrap();
    let server = TestServer::start(dir.path()).await;
    let a = server.create_workbook("a").await;
    let b = server.create_workbook("b").await;
    let mut ws = server.ws(&a).await;
    ws.ok("create_table_from_query", json!({ "name": "t", "sql": "SELECT 1 AS x" })).await;
    ws.close().await;
    server.stop().await;
    // As if the previous process died: records claim live allocations, scratch is left over.
    for (wb, state) in [(&a, "ready"), (&b, "starting")] {
        let path = dir.path().join(".control/allocations").join(format!("{wb}.json"));
        let mut rec: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        rec["state"] = json!(state);
        std::fs::write(&path, serde_json::to_vec(&rec).unwrap()).unwrap();
        std::fs::create_dir_all(dir.path().join(".tmp").join(wb)).unwrap();
        std::fs::write(dir.path().join(".tmp").join(wb).join("spill.tmp"), b"x").unwrap();
    }
    // Workbook b is still held by "another process" (a lock taken through another file).
    let foreign = LocalProvider::lock(&dir.path().join(&b)).unwrap();
    let server = TestServer::start(dir.path()).await;
    let sa = status(&server, &a).await;
    assert_eq!(state_of(&sa), "failed");
    assert_eq!(sa["allocation"]["failure"]["kind"], "lost");
    assert!(!dir.path().join(".tmp").join(&a).exists(), "dead allocation's scratch removed");
    // b: ownership is uncertain, so it is not opened.
    let (code, err) = server.get(&format!("/api/workbooks/{b}")).await;
    assert_eq!(code, 423, "{err}");
    assert!(err["error"].as_str().unwrap().contains("another process"), "{err}");
    drop(foreign);
    let (code, _) = server.get(&format!("/api/workbooks/{b}")).await;
    assert_eq!(code, 200);
    // a reopens with its data, under a newer epoch.
    let mut ws = server.ws(&a).await;
    assert_eq!(ws.ok("query", json!({ "sql": "SELECT x FROM t" })).await["rows"], json!([[1]]));
    ws.close().await;
    assert!(status(&server, &a).await["allocation"]["epoch"].as_u64().unwrap() > sa["allocation"]["epoch"].as_u64().unwrap());
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn one_server_per_data_directory() {
    let dir = tempfile::tempdir().unwrap();
    let server = TestServer::start(dir.path()).await;
    let second = rowfinity_server::state::AppState::new(rowfinity_server::state::Config::new(dir.path()));
    let err = second.err().expect("a second server must be refused").to_string();
    assert!(err.contains("another Rowfinity server"), "{err}");
    server.stop().await;
}

// ------------------------------------------------------------------ 3.03

fn sum_limits(usage: &Value) -> (u64, u64) {
    let allocs = usage["allocations"].as_array().unwrap();
    let mem = allocs.iter().map(|a| parse_bytes(a["duckdb_memory_limit"].as_str().unwrap()).unwrap()).sum();
    let threads = allocs.iter().map(|a| a["duckdb_threads"].as_str().unwrap().parse::<u64>().unwrap()).sum();
    (mem, threads)
}

#[tokio::test(flavor = "multi_thread")]
async fn open_workbooks_share_the_budget_and_never_exceed_it() {
    let dir = tempfile::tempdir().unwrap();
    let server = TestServer::start_with(dir.path(), |c| {
        c.memory_budget = Some(1 << 30);
        c.threads = Some(4);
        c.min_workbook_memory = 256 << 20;
    })
    .await;
    let budget = 1u64 << 30;
    let mut sessions = Vec::new();
    let mut ids = Vec::new();
    for i in 0..4 {
        let wb = server.create_workbook(&format!("w{i}")).await;
        sessions.push(server.ws(&wb).await);
        ids.push(wb);
        let (_, usage) = server.get("/api/admin/resources").await;
        let (mem, threads) = sum_limits(&usage);
        // Read back from every DuckDB instance: the sum stays within the budget.
        assert!(mem <= budget, "{i}: {mem} > {budget}: {usage}");
        assert!(threads <= 4, "{usage}");
        assert_eq!(usage["allocations"].as_array().unwrap().len(), i + 1);
        let each = parse_bytes(usage["allocations"][0]["duckdb_memory_limit"].as_str().unwrap()).unwrap();
        // Rebalanced: each gets about budget / n (DuckDB rounds when reporting).
        let share = budget / (i as u64 + 1);
        assert!(each.abs_diff(share) < share / 50, "{each} vs {share}");
    }
    // A fifth does not fit (4 × 256 MiB = 1 GiB) and nothing is idle: refused, nothing changes.
    let (status, body) = server.send(Method::POST, "/api/workbooks", Some(json!({ "name": "w4" }))).await;
    assert_eq!(status, 503, "{body}");
    assert!(body["error"].as_str().unwrap().contains("capacity"), "{body}");
    let (_, usage) = server.get("/api/admin/resources").await;
    assert_eq!(usage["allocations"].as_array().unwrap().len(), 4);
    // Once one is idle it is closed to make room.
    sessions.remove(0).close().await;
    eventually("w0 has no session", || server.state.controller.handle(&ids[0]).is_none_or(|h| h.sessions() == 0)).await;
    let (code, body) = server.get(&format!("/api/workbooks/{}", ids[0])).await;
    assert_eq!(code, 200, "{body}");
    let (status, body) = server.send(Method::POST, "/api/workbooks", Some(json!({ "name": "w6" }))).await;
    assert_eq!(status, 200, "{body}");
    let (_, usage) = server.get("/api/admin/resources").await;
    let (mem, threads) = sum_limits(&usage);
    assert!(mem <= budget && threads <= 4, "{usage}");
    assert_eq!(usage["allocations"].as_array().unwrap().len(), 4);
    for s in sessions {
        s.close().await;
    }
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn sheet_memory_comes_out_of_duckdbs_share() {
    let dir = tempfile::tempdir().unwrap();
    let server = TestServer::start_with(dir.path(), |c| c.memory_budget = Some(1 << 30)).await;
    let wb = server.create_workbook("sheets").await;
    let mut ws = server.ws(&wb).await;
    ws.ok("sheet_create", json!({ "name": "S" })).await;
    let inputs: Vec<Value> = (0..20000).map(|i| json!({ "row": i, "column": 0, "value": format!("text value number {i}") })).collect();
    ws.ok("sheet_set", json!({ "sheet": 0, "inputs": inputs })).await;
    ws.close().await;
    // Reopen so the estimate is taken at start.
    server.send(Method::POST, &format!("/api/workbooks/{wb}/runtime"), Some(json!({ "action": "stop" }))).await;
    let ws = server.ws(&wb).await;
    let (_, usage) = server.get("/api/admin/resources").await;
    let mem = parse_bytes(usage["allocations"][0]["duckdb_memory_limit"].as_str().unwrap()).unwrap();
    // Without sheets the limit equals the whole share (see the budget test); with them it is lower.
    assert!(mem < 1 << 30, "DuckDB's limit {mem} must be below the whole 1 GiB share");
    assert!(mem > (1 << 30) - (256 << 20), "{mem}");
    ws.close().await;
    server.stop().await;
}

// ------------------------------------------------------------------ 3.04

#[tokio::test(flavor = "multi_thread")]
async fn idle_workbooks_close_and_reopen_but_not_while_in_use() {
    let dir = tempfile::tempdir().unwrap();
    let server = TestServer::start_with(dir.path(), |c| c.idle_timeout = Duration::from_millis(200)).await;
    let idle = server.create_workbook("idle").await;
    let busy = server.create_workbook("busy").await;
    let mut ws = server.ws(&busy).await;
    ws.ok("create_table_from_query", json!({ "name": "t", "sql": "SELECT 7 AS x" })).await;
    let state = server.state.clone();
    let i = idle.clone();
    eventually("the idle workbook is closed", move || state.controller.handle(&i).is_none()).await;
    let s = status(&server, &idle).await;
    assert_eq!(state_of(&s), "stopped");
    assert!(s["allocation"]["transitions"].as_array().unwrap().iter().any(|t| t["operation"].as_str().unwrap().starts_with("evict")));
    // The workbook with a session stays open well past the timeout.
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert_eq!(state_of(&status(&server, &busy).await), "ready");
    assert_eq!(ws.ok("query", json!({ "sql": "SELECT x FROM t" })).await["rows"], json!([[7]]));
    ws.close().await;
    // Closed workbooks reopen on demand.
    let (code, _) = server.get(&format!("/api/workbooks/{idle}")).await;
    assert_eq!(code, 200);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn eviction_racing_new_work_never_serves_from_a_closed_workbook() {
    let dir = tempfile::tempdir().unwrap();
    // Evict as soon as a workbook is unused.
    let server = TestServer::start_with(dir.path(), |c| c.idle_timeout = Duration::ZERO).await;
    let wb = server.create_workbook("race").await;
    let mut ws = server.ws(&wb).await;
    ws.ok("create_table_from_query", json!({ "name": "t", "sql": "SELECT i AS n FROM range(100) r(i)" })).await;
    ws.close().await;
    // Run until enough evictions have raced the requests (or a cap), rather than for a
    // fixed time: under load fewer windows open, and the test must still see them.
    let stop = Arc::new(AtomicBool::new(false));
    let evicted = Arc::new(AtomicUsize::new(0));
    let cap = std::time::Instant::now() + Duration::from_secs(60);
    let state = server.state.clone();
    let (stop2, evicted2) = (stop.clone(), evicted.clone());
    let evictor = tokio::task::spawn_blocking(move || {
        while !stop2.load(Ordering::SeqCst) {
            evicted2.fetch_add(state.controller.evict_idle().len(), Ordering::SeqCst);
        }
    });
    let mut workers = Vec::new();
    for k in 0..3 {
        let http = server.http.clone();
        let addr = server.addr;
        let wb = wb.clone();
        let stop = stop.clone();
        workers.push(tokio::spawn(async move {
            let mut ok = 0;
            while !stop.load(Ordering::SeqCst) {
                if k % 2 == 0 {
                    let res = http.get(format!("http://{addr}/api/workbooks/{wb}")).send().await.unwrap();
                    let status = res.status().as_u16();
                    let body: Value = res.json().await.unwrap();
                    assert_eq!(status, 200, "{body}");
                    assert_eq!(body["tables"][0]["row_count"], 100);
                } else {
                    let mut ws = Ws::connect(addr, &wb).await;
                    let v = ws.ok("open_view", json!({ "spec": { "table": "t" } })).await;
                    let block = ws.ok("fetch", json!({ "view_id": v["view_id"], "row_start": 98, "row_count": 5 })).await;
                    assert_eq!(block["rows"], json!([[98], [99]]));
                    ws.close().await;
                }
                ok += 1;
                // Short pauses leave the workbook idle now and then.
                tokio::time::sleep(Duration::from_millis((ok * 7 + k) % 10)).await;
            }
            ok
        }));
    }
    while evicted.load(Ordering::SeqCst) < 10 && std::time::Instant::now() < cap {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    stop.store(true, Ordering::SeqCst);
    evictor.await.unwrap();
    let mut total = 0;
    for w in workers {
        total += w.await.unwrap();
    }
    let evictions = evicted.load(Ordering::SeqCst);
    let s = status(&server, &wb).await;
    let epoch = s["allocation"]["epoch"].as_u64().unwrap();
    eprintln!("eviction race: {total} requests, {evictions} evictions, final epoch {epoch}");
    assert!(total > 20, "{total} requests");
    assert!(evictions >= 10 && epoch > 10, "evictions must actually have raced the requests ({evictions} evictions, epoch {epoch})");
    server.stop().await;
}
