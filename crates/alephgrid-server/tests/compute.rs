//! Phase 7 compute (7.13–7.16; 7.T03) over the real server: concurrent admission against
//! space and host budgets, forged profile requests, two collaborators opening a stopped
//! workbook, a failed start, the capacity queue, a controlled resize, heartbeat-only idle and
//! a long job during idle evaluation.
//!
//! Expected outcomes are written from docs/COMPUTE.md, not read from the controller.

mod common;

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use alephgrid_server::runtime::controller::StageHook;
use alephgrid_server::runtime::local::LocalProvider;
use alephgrid_server::runtime::{AllocationState, ComputeProfile, FailureKind, RuntimeFailure};
use common::*;
use reqwest::Method;
use serde_json::{json, Value};

const MIB: u64 = 1 << 20;
const GIB: u64 = 1 << 30;
const SLOW_SQL: &str = "SELECT sum(i) AS s FROM range(100000000000) r(i)";

/// Holds every start at one stage until released, counting arrivals; fails the next start
/// of chosen workbooks at `running`.
#[derive(Default)]
struct Hold {
    stage: Mutex<Option<AllocationState>>,
    cv: Condvar,
    arrived: AtomicUsize,
    fail: Mutex<HashSet<String>>,
}

impl Hold {
    fn hook(self: &Arc<Self>) -> StageHook {
        let h = self.clone();
        Arc::new(move |wb: &str, stage: AllocationState| {
            if stage == AllocationState::Running && h.fail.lock().unwrap().remove(wb) {
                return Err(RuntimeFailure::new(FailureKind::StartupFailed, "injected startup failure"));
            }
            let mut held = h.stage.lock().unwrap();
            if *held == Some(stage) {
                h.arrived.fetch_add(1, Ordering::SeqCst);
                while *held == Some(stage) {
                    held = h.cv.wait(held).unwrap();
                }
            }
            Ok(())
        })
    }

    fn hold(&self, stage: AllocationState) {
        self.arrived.store(0, Ordering::SeqCst);
        *self.stage.lock().unwrap() = Some(stage);
    }

    fn release(&self) {
        *self.stage.lock().unwrap() = None;
        self.cv.notify_all();
    }

    /// Releases held starts when dropped, so a failing test does not hang on them.
    fn guard(self: &Arc<Self>) -> Release {
        Release(self.clone())
    }

    fn fail_next(&self, workbook: &str) {
        self.fail.lock().unwrap().insert(workbook.to_string());
    }
}

struct Release(Arc<Hold>);

impl Drop for Release {
    fn drop(&mut self) {
        self.0.release();
    }
}

fn space_of(dir: &std::path::Path, wb: &str) -> String {
    let meta: Value = serde_json::from_slice(&std::fs::read(dir.join(wb).join("workbook.json")).unwrap()).unwrap();
    meta["space"].as_str().unwrap().to_string()
}

fn meta_of(dir: &std::path::Path, wb: &str) -> Value {
    serde_json::from_slice(&std::fs::read(dir.join(wb).join("workbook.json")).unwrap()).unwrap()
}

async fn runtime(server: &TestServer, wb: &str, body: Value) -> (u16, Value) {
    server.send(Method::POST, &format!("/api/workbooks/{wb}/runtime"), Some(body)).await
}

async fn status(server: &TestServer, wb: &str) -> Value {
    let (code, body) = server.get(&format!("/api/workbooks/{wb}/runtime")).await;
    assert_eq!(code, 200, "{body}");
    body
}

fn state_of(s: &Value) -> &str {
    s["allocation"]["state"].as_str().unwrap_or("none")
}

async fn stopped_workbooks(server: &TestServer, n: usize) -> Vec<String> {
    let mut ids = Vec::new();
    for i in 0..n {
        let id = server.create_workbook(&format!("w{i}")).await;
        let (code, body) = runtime(server, &id, json!({ "action": "stop" })).await;
        assert_eq!(code, 200, "{body}");
        ids.push(id);
    }
    ids
}

/// Open every workbook at once; the status code and body of each.
async fn open_all(server: &TestServer, ids: &[String]) -> Vec<(u16, Value)> {
    let tasks: Vec<_> = ids
        .iter()
        .map(|id| {
            let (http, url) = (server.http.clone(), server.url(&format!("/api/workbooks/{id}")));
            tokio::spawn(async move {
                let res = http.get(url).send().await.unwrap();
                (res.status().as_u16(), res.json::<Value>().await.unwrap_or(Value::Null))
            })
        })
        .collect();
    let mut out = Vec::new();
    for t in tasks {
        out.push(t.await.unwrap());
    }
    out
}

// ------------------------------------------------------------------ 7.13

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_starts_never_pass_the_space_budgets() {
    let dir = tempfile::tempdir().unwrap();
    let hold = Arc::new(Hold::default());
    let server = TestServer::start_with(dir.path(), |c| {
        c.memory_budget = Some(8 * GIB);
        c.threads = Some(16);
        c.min_workbook_memory = 256 * MIB;
        c.start_queue_timeout = Duration::ZERO;
        c.profiles = vec![ComputeProfile::parse("large=1GiB:2").unwrap()];
        c.stage_hook = Some(hold.hook());
    })
    .await;
    let _release = hold.guard();
    let ids = stopped_workbooks(&server, 6).await;
    let space = space_of(dir.path(), &ids[0]);

    // Open-workbooks limit 3: six opens at once, the admitted ones held while starting.
    let (code, body) = server.send(Method::PUT, &format!("/api/spaces/{space}/quota"), Some(json!({ "open_workbooks": 3 }))).await;
    assert_eq!(code, 200, "{body}");
    hold.hold(AllocationState::Starting);
    let opening = { let ids = ids.clone(); let s = &server; async move { open_all(s, &ids).await } };
    let watcher = {
        let state = server.state.clone();
        let space = space.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let (stop2, most) = (stop.clone(), Arc::new(AtomicUsize::new(0)));
        let most2 = most.clone();
        let t = std::thread::spawn(move || {
            while !stop2.load(Ordering::SeqCst) {
                most2.fetch_max(state.controller.space_usage(&space).0 as usize, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(1));
            }
        });
        (t, stop, most)
    };
    let releaser = {
        let hold = hold.clone();
        tokio::spawn(async move {
            eventually("three starts are admitted", || hold.arrived.load(Ordering::SeqCst) == 3).await;
            // Nobody else gets in while they start.
            tokio::time::sleep(Duration::from_millis(300)).await;
            let arrived = hold.arrived.load(Ordering::SeqCst);
            hold.release();
            arrived
        })
    };
    let results = opening.await;
    assert_eq!(releaser.await.unwrap(), 3);
    watcher.1.store(true, Ordering::SeqCst);
    watcher.0.join().unwrap();
    assert!(watcher.2.load(Ordering::SeqCst) <= 3, "the space never had more than 3 live allocations");
    let ok = results.iter().filter(|(c, _)| *c == 200).count();
    let refused: Vec<&Value> = results.iter().filter(|(c, _)| *c == 429).map(|(_, b)| b).collect();
    assert_eq!((ok, refused.len()), (3, 3), "{results:?}");
    assert!(refused.iter().all(|b| b["error"].as_str().unwrap().contains("workbook(s) open")), "{refused:?}");
    assert_eq!(server.state.controller.space_usage(&space).0, 3);

    // Compute budget 2.5 GiB: three large (1 GiB guaranteed each) and three standard
    // (256 MiB each) opened at once, 3.75 GiB of demand.
    for id in &ids {
        runtime(&server, id, json!({ "action": "stop" })).await;
    }
    let budget = 5 * GIB / 2;
    let (code, body) = server.send(Method::PUT, &format!("/api/spaces/{space}/quota"), Some(json!({ "compute_memory_bytes": budget }))).await;
    assert_eq!(code, 200, "{body}");
    let (code, body) = server.send(Method::PUT, &format!("/api/spaces/{space}/profiles"), Some(json!({ "profiles": ["large"] }))).await;
    assert_eq!(code, 200, "{body}");
    for id in &ids[..3] {
        let (code, body) = runtime(&server, id, json!({ "action": "restart", "profile": "large" })).await;
        assert_eq!(code, 200, "{body}");
        assert_eq!(body["allocation"]["profile"], "large");
        runtime(&server, id, json!({ "action": "stop" })).await;
    }
    hold.hold(AllocationState::Starting);
    let opening = { let ids = ids.clone(); let s = &server; async move { open_all(s, &ids).await } };
    let releaser = {
        let hold = hold.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(500)).await;
            hold.release();
        })
    };
    let results = opening.await;
    releaser.await.unwrap();
    let want = |i: usize| if i < 3 { GIB } else { 256 * MIB };
    let admitted: u64 = results.iter().enumerate().filter(|(_, (c, _))| *c == 200).map(|(i, _)| want(i)).sum();
    assert!(admitted <= budget, "{admitted} admitted over a budget of {budget}");
    let refused: Vec<usize> = results.iter().enumerate().filter(|(_, (c, _))| *c == 429).map(|(i, _)| i).collect();
    assert!(!refused.is_empty() && refused.len() + results.iter().filter(|(c, _)| *c == 200).count() == 6, "{results:?}");
    for i in &refused {
        // Necessary refusals only: the admitted set only grew, so each refused one would
        // still pass the budget now.
        assert!(admitted + want(*i) > budget, "workbook {i} was refused though it fits: {results:?}");
        assert!(results[*i].1["error"].as_str().unwrap().contains("compute budget"), "{:?}", results[*i]);
    }
    let (_, usage) = server.get("/api/admin/usage").await;
    let row = usage["spaces"].as_array().unwrap().iter().find(|s| s["space"]["id"] == space.as_str()).unwrap().clone();
    assert_eq!(row["compute_memory_bytes"].as_u64().unwrap(), admitted, "{row}");
    assert_eq!(row["profiles"], json!(["large"]));
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_starts_never_pass_the_host_budget() {
    let dir = tempfile::tempdir().unwrap();
    let hold = Arc::new(Hold::default());
    let server = TestServer::start_with(dir.path(), |c| {
        c.memory_budget = Some(GIB);
        c.threads = Some(8);
        c.min_workbook_memory = 256 * MIB;
        c.start_queue_timeout = Duration::ZERO;
        c.stage_hook = Some(hold.hook());
    })
    .await;
    let _release = hold.guard();
    let ids = stopped_workbooks(&server, 8).await;
    // All eight at once, held while starting, so none can be closed to make room: four fit
    // (4 × 256 MiB), and they are reserved before they open, not when they are ready.
    hold.hold(AllocationState::Starting);
    let opening = { let ids = ids.clone(); let s = &server; async move { open_all(s, &ids).await } };
    let releaser = {
        let hold = hold.clone();
        let state = server.state.clone();
        tokio::spawn(async move {
            eventually("four starts are admitted", || hold.arrived.load(Ordering::SeqCst) == 4).await;
            tokio::time::sleep(Duration::from_millis(300)).await;
            let reserved = state.controller.usage().reserved;
            let arrived = hold.arrived.load(Ordering::SeqCst);
            hold.release();
            (arrived, reserved)
        })
    };
    let results = opening.await;
    let (arrived, reserved) = releaser.await.unwrap();
    assert_eq!(arrived, 4);
    assert!(reserved.memory_bytes <= GIB && reserved.threads <= 8, "{reserved:?}");
    assert_eq!(results.iter().filter(|(c, _)| *c == 200).count(), 4, "{results:?}");
    assert!(results.iter().filter(|(c, _)| *c != 200).all(|(c, b)| *c == 503 && b["error"].as_str().unwrap().contains("capacity")), "{results:?}");
    let (_, usage) = server.get("/api/admin/resources").await;
    let total: u64 = usage["allocations"].as_array().unwrap().iter().map(|a| alephgrid_server::runtime::budget::parse_bytes(a["duckdb_memory_limit"].as_str().unwrap()).unwrap()).sum();
    assert!(total <= GIB, "{usage}");
    server.stop().await;
}

/// An organization with space Alpha holding workbook `wa`, and its owner and editor.
struct Team {
    server: TestServer,
    admin: Session,
    alpha: String,
    wa: String,
    owner: Session,
    editor: Session,
}

async fn team(dir: &std::path::Path, configure: impl FnOnce(&mut alephgrid_server::state::Config)) -> Team {
    let server = TestServer::start_accounts(dir, configure).await;
    let admin = server.admin().await;
    let org = admin.me["orgs"][0]["org"]["id"].as_str().unwrap().to_string();
    let alpha = admin.ok(Method::POST, "/api/spaces", Some(json!({ "name": "Alpha" }))).await["id"].as_str().unwrap().to_string();
    let wa = admin.ok(Method::POST, "/api/workbooks", Some(json!({ "name": "wa", "space": alpha }))).await["id"].as_str().unwrap().to_string();
    let mut people = Vec::new();
    for (email, role) in [("owner@example.com", "owner"), ("editor@example.com", "editor")] {
        let code = admin.invite(&org, email, Some(&alpha), Some(role)).await;
        people.push(server.accept(&code, email.split('@').next().unwrap(), "a long enough password").await);
    }
    let editor = people.pop().unwrap();
    let owner = people.pop().unwrap();
    Team { server, admin, alpha, wa, owner, editor }
}

#[tokio::test(flavor = "multi_thread")]
async fn forged_profile_requests_are_refused_and_change_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let t = team(dir.path(), |c| {
        c.memory_budget = Some(4 * GIB);
        c.threads = Some(8);
        c.profiles = ["large=512MiB:1", "xl=512MiB:1", "solo=512MiB:1:dedicated"].iter().map(|p| ComputeProfile::parse(p).unwrap()).collect();
    })
    .await;
    let (alpha, wa) = (t.alpha.clone(), t.wa.clone());
    let path = format!("/api/workbooks/{wa}/runtime");
    // Owners cannot grant profiles to their own space; unknown profiles cannot be granted.
    let (code, _) = t.owner.send(Method::PUT, &format!("/api/spaces/{alpha}/profiles"), Some(json!({ "profiles": ["large"] }))).await;
    assert_eq!(code, 403);
    let (code, body) = t.admin.send(Method::PUT, &format!("/api/spaces/{alpha}/profiles"), Some(json!({ "profiles": ["large", "huge"] }))).await;
    assert_eq!(code, 400, "{body}");
    let (code, body) = t.admin.send(Method::PUT, &format!("/api/spaces/{alpha}/profiles"), Some(json!({ "profiles": ["large", "solo"] }))).await;
    assert_eq!(code, 200, "{body}");
    let before = t.admin.ok(Method::GET, &path, None).await;
    let epoch = before["allocation"]["epoch"].clone();
    assert_eq!(state_of(&before), "ready");

    let forged: Vec<(&Session, Value, u16, &str)> = vec![
        (&t.editor, json!({ "action": "restart", "profile": "large" }), 403, "choose compute profiles"),
        (&t.owner, json!({ "action": "restart", "profile": "huge" }), 400, "unknown compute profile"),
        (&t.owner, json!({ "action": "restart", "profile": "xl" }), 403, "not granted to this space"),
        (&t.owner, json!({ "action": "restart", "profile": "solo" }), 403, "dedicated"),
        (&t.owner, json!({ "action": "restart", "profile": "large", "memory_bytes": 64 * GIB }), 400, "memory_bytes"),
        (&t.owner, json!({ "action": "restart", "threads": 64 }), 400, "threads"),
        (&t.owner, json!({ "action": "start", "profile": "large" }), 400, "restart"),
        (&t.owner, json!({ "action": "restart", "profile": { "id": "large", "memory_bytes": 1 } }), 400, "invalid request"),
    ];
    for (who, body, want, says) in forged {
        let (code, reply) = who.send(Method::POST, &path, Some(body.clone())).await;
        assert_eq!(code, want, "{body} → {reply}");
        assert!(reply["error"].as_str().unwrap_or("").contains(says), "{body} → {reply}");
    }
    // Nothing changed: same allocation, same profile, nothing recorded.
    let after = t.admin.ok(Method::GET, &path, None).await;
    assert_eq!(after["allocation"]["epoch"], epoch);
    assert_eq!(after["allocation"]["profile"], "standard");
    assert_eq!(after["profile"], "standard");
    assert!(meta_of(dir.path(), &wa).get("profile").is_none());

    // What each may choose, with the reason.
    let choices = t.editor.ok(Method::GET, &format!("/api/workbooks/{wa}/profiles"), None).await;
    let large = choices.as_array().unwrap().iter().find(|c| c["profile"]["id"] == "large").unwrap();
    assert_eq!((large["granted"].clone(), large["allowed"].clone()), (json!(true), json!(false)));
    let choices = t.owner.ok(Method::GET, &format!("/api/workbooks/{wa}/profiles"), None).await;
    let by_id = |id: &str| choices.as_array().unwrap().iter().find(|c| c["profile"]["id"] == id).unwrap().clone();
    assert_eq!(by_id("large")["allowed"], true);
    assert_eq!(by_id("xl")["granted"], false);
    assert_eq!(by_id("solo")["allowed"], false);
    assert!(by_id("solo")["reason"].as_str().unwrap().contains("organization admin"));

    // The authorized requests work: the owner picks large, an organization admin dedicated.
    let s = t.owner.ok(Method::POST, &path, Some(json!({ "action": "restart", "profile": "large" }))).await;
    assert_eq!((s["allocation"]["profile"].clone(), s["allocation"]["reservation"]["memory_bytes"].as_u64()), (json!("large"), Some(512 * MIB)));
    let s = t.admin.ok(Method::POST, &path, Some(json!({ "action": "restart", "profile": "solo" }))).await;
    assert_eq!(s["allocation"]["profile"], "solo");

    // A withdrawn grant: the workbook still opens, with standard, and says why.
    t.admin.ok(Method::PUT, &format!("/api/spaces/{alpha}/profiles"), Some(json!({ "profiles": [] }))).await;
    t.admin.ok(Method::POST, &path, Some(json!({ "action": "stop" }))).await;
    t.admin.ok(Method::POST, &path, Some(json!({ "action": "start" }))).await;
    let s = t.admin.ok(Method::GET, &path, None).await;
    assert_eq!(s["allocation"]["profile"], "standard");
    assert_eq!(s["profile"], "solo", "the workbook keeps asking for its profile");
    assert!(s["note"].as_str().unwrap().contains("no longer granted"), "{s}");
    t.server.stop().await;
}

// ------------------------------------------------------------------ 7.14

#[tokio::test(flavor = "multi_thread")]
async fn two_collaborators_opening_a_stopped_workbook_share_one_allocation() {
    let dir = tempfile::tempdir().unwrap();
    let hold = Arc::new(Hold::default());
    let server = TestServer::start_with(dir.path(), |c| c.stage_hook = Some(hold.hook())).await;
    let _release = hold.guard();
    let wb = server.create_workbook("shared").await;
    let mut ws = server.ws(&wb).await;
    ws.ok("create_table_from_query", json!({ "name": "t", "sql": "SELECT 1 AS n" })).await;
    ws.close().await;
    let stopped = runtime(&server, &wb, json!({ "action": "stop" })).await.1;
    let epoch = stopped["allocation"]["epoch"].as_u64().unwrap();

    hold.hold(AllocationState::Starting);
    let (addr, w1, w2) = (server.addr, wb.clone(), wb.clone());
    let a = tokio::spawn(async move { Ws::connect(addr, &w1).await });
    let b = tokio::spawn(async move { Ws::connect(addr, &w2).await });
    eventually("the start is under way", || hold.arrived.load(Ordering::SeqCst) == 1).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(hold.arrived.load(Ordering::SeqCst), 1, "the second collaborator waits for the first start");
    // Its record is there (the stage hook runs before the move to `starting`).
    let s = status(&server, &wb).await;
    assert_eq!(state_of(&s), "requested");
    assert_eq!(s["allocation"]["epoch"].as_u64().unwrap(), epoch + 1);
    hold.release();
    let (mut a, mut b) = (a.await.unwrap(), b.await.unwrap());
    let s = status(&server, &wb).await;
    assert_eq!(state_of(&s), "ready");
    assert_eq!(s["allocation"]["epoch"].as_u64().unwrap(), epoch + 1, "one allocation for both");
    assert_eq!(s["sessions"], 2);
    assert_eq!(s["ownership"]["holder"], s["allocation"]["holder"]);
    // Both are served by it: a change by one reaches the other.
    a.ok("sql_change", json!({ "sql": "UPDATE t SET n = 2" })).await;
    b.event("table_changed").await.unwrap();
    assert_eq!(b.ok("query", json!({ "sql": "SELECT n FROM t" })).await["rows"], json!([[2]]));
    a.close().await;
    b.close().await;
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_start_is_reported_with_its_reason_and_holds_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let hold = Arc::new(Hold::default());
    let server = TestServer::start_with(dir.path(), |c| c.stage_hook = Some(hold.hook())).await;
    let wb = stopped_workbooks(&server, 1).await.remove(0);
    hold.fail_next(&wb);
    // The page starts it without waiting and follows its status.
    let (code, first) = runtime(&server, &wb, json!({ "action": "start", "wait": false, "operation_id": "page-1" })).await;
    assert_eq!(code, 200, "{first}");
    let mut s = first;
    let deadline = Instant::now() + Duration::from_secs(20);
    while state_of(&s) != "failed" {
        assert!(Instant::now() < deadline, "{s}");
        assert!(["requested", "starting", "running"].contains(&state_of(&s)), "{s}");
        tokio::time::sleep(Duration::from_millis(20)).await;
        s = status(&server, &wb).await;
    }
    assert_eq!(s["allocation"]["failure"]["kind"], "startup_failed");
    assert!(s["allocation"]["failure"]["message"].as_str().unwrap().contains("injected"), "{s}");
    assert!(s.get("ownership").is_none());
    assert!(LocalProvider::lock(&dir.path().join(&wb)).is_ok(), "the lock was released");
    assert_eq!(server.state.controller.usage().reserved.memory_bytes, 0, "the reservation was released");
    // Try again: it opens.
    let (code, s) = runtime(&server, &wb, json!({ "action": "start" })).await;
    assert_eq!((code, state_of(&s)), (200, "ready"), "{s}");
    // A real failure names the file, but never the data directory (7.06).
    runtime(&server, &wb, json!({ "action": "stop" })).await;
    let file = dir.path().join(&wb).join("data.duckdb");
    let saved = std::fs::read(&file).unwrap();
    std::fs::write(&file, vec![0x5a; 64 * 1024]).unwrap();
    let (code, body) = runtime(&server, &wb, json!({ "action": "start" })).await;
    assert_eq!(code, 503, "{body}");
    let s = status(&server, &wb).await;
    let message = s["allocation"]["failure"]["message"].as_str().unwrap();
    let data = dir.path().to_string_lossy().to_string();
    assert!(message.contains("<data>/") && !message.contains(&data), "{message}");
    assert!(!body["error"].as_str().unwrap().contains(&data), "{body}");
    std::fs::write(&file, saved).unwrap();
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn starts_wait_in_line_for_capacity_in_order() {
    let dir = tempfile::tempdir().unwrap();
    let server = TestServer::start_with(dir.path(), |c| {
        c.memory_budget = Some(512 * MIB);
        c.threads = Some(4);
        c.min_workbook_memory = 256 * MIB;
        c.start_queue_timeout = Duration::from_secs(30);
    })
    .await;
    let ids = stopped_workbooks(&server, 4).await;
    // Two fit, both in use (sessions, not idle).
    let a = server.ws(&ids[0]).await;
    let b = server.ws(&ids[1]).await;
    let (code, c) = runtime(&server, &ids[2], json!({ "action": "start", "wait": false })).await;
    assert_eq!(code, 200, "{c}");
    assert_eq!((state_of(&c), c["queue_position"].as_u64()), ("requested", Some(1)), "{c}");
    let (_, d) = runtime(&server, &ids[3], json!({ "action": "start", "wait": false })).await;
    assert_eq!((state_of(&d), d["queue_position"].as_u64()), ("requested", Some(2)), "{d}");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(status(&server, &ids[2]).await["queue_position"], 1, "still waiting");
    // A is closed: the first in line gets its place, the second moves up.
    a.close().await;
    let c_id = ids[2].clone();
    let state = server.state.clone();
    eventually("C is ready", move || state.controller.handle(&c_id).is_some()).await;
    assert_eq!(state_of(&status(&server, &ids[2]).await), "ready");
    assert_eq!(state_of(&status(&server, &ids[0]).await), "stopped");
    let d = status(&server, &ids[3]).await;
    assert_eq!((state_of(&d), d["queue_position"].as_u64()), ("requested", Some(1)), "{d}");
    // C was opened for someone who has not connected yet: D does not take its place.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(state_of(&status(&server, &ids[2]).await), "ready");
    assert_eq!(state_of(&status(&server, &ids[3]).await), "requested");
    // Leaving the line.
    let (_, d) = runtime(&server, &ids[3], json!({ "action": "cancel" })).await;
    assert_eq!(state_of(&d), "stopped", "{d}");
    assert_eq!(d["allocation"]["failure"]["kind"], "cancelled");
    b.close().await;
    server.stop().await;

    // Waiting is bounded.
    let dir = tempfile::tempdir().unwrap();
    let server = TestServer::start_with(dir.path(), |c| {
        c.memory_budget = Some(512 * MIB);
        c.threads = Some(4);
        c.min_workbook_memory = 256 * MIB;
        c.start_queue_timeout = Duration::from_secs(1);
    })
    .await;
    let ids = stopped_workbooks(&server, 3).await;
    let a = server.ws(&ids[0]).await;
    let b = server.ws(&ids[1]).await;
    let t0 = Instant::now();
    let (code, body) = runtime(&server, &ids[2], json!({ "action": "start" })).await;
    assert!(t0.elapsed() >= Duration::from_secs(1));
    assert_eq!(code, 503, "{body}");
    assert!(body["error"].as_str().unwrap().contains("waited 1 s in line"), "{body}");
    assert_eq!(status(&server, &ids[2]).await["allocation"]["failure"]["kind"], "capacity");
    a.close().await;
    b.close().await;
    server.stop().await;
}

// ------------------------------------------------------------------ 7.15

#[tokio::test(flavor = "multi_thread")]
async fn a_controlled_resize_keeps_identity_data_pins_and_ownership() {
    let dir = tempfile::tempdir().unwrap();
    let hold = Arc::new(Hold::default());
    let server = TestServer::start_with(dir.path(), |c| {
        c.memory_budget = Some(GIB);
        c.threads = Some(4);
        c.profiles = ["large=512MiB:1", "xl=2GiB:1"].iter().map(|p| ComputeProfile::parse(p).unwrap()).collect();
        c.stage_hook = Some(hold.hook());
    })
    .await;
    let wb = server.create_workbook("resized").await;
    let space = space_of(dir.path(), &wb);
    server.send(Method::PUT, &format!("/api/spaces/{space}/profiles"), Some(json!({ "profiles": ["large", "xl"] }))).await;
    let mut a = server.ws(&wb).await;
    a.ok("create_table_from_query", json!({ "name": "t", "sql": "SELECT 41 AS x" })).await;
    a.ok("publish_function", json!({ "definition": { "name": "PLUSONE", "kind": "formula", "arguments": ["value"], "source": "value+1", "documentation": "", "tests": [] } })).await;
    a.ok("pin_functions", json!({ "pins": { "PLUSONE": 1 } })).await;
    a.ok("sql_change", json!({ "sql": "UPDATE t SET x = 42" })).await;
    let before = status(&server, &wb).await;
    let epoch = before["allocation"]["epoch"].as_u64().unwrap();
    let revision = Ws::connect(server.addr, &wb).await.hello["revision"].clone();

    // Nobody can take the workbook while it restarts: the lock never comes free.
    let wb_dir = dir.path().join(&wb);
    let (stop, free) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)));
    let sampler = {
        let (stop, free, d) = (stop.clone(), free.clone(), wb_dir.clone());
        std::thread::spawn(move || {
            while !stop.load(Ordering::SeqCst) {
                if LocalProvider::lock(&d).is_ok() {
                    free.store(true, Ordering::SeqCst);
                }
                std::thread::sleep(Duration::from_micros(200));
            }
        })
    };
    let (code, s) = runtime(&server, &wb, json!({ "action": "restart", "profile": "large" })).await;
    stop.store(true, Ordering::SeqCst);
    sampler.join().unwrap();
    assert_eq!(code, 200, "{s}");
    assert!(!free.load(Ordering::SeqCst), "the ownership lock was free during the restart");
    assert_eq!(state_of(&s), "ready");
    assert_eq!(s["profile"], "large");
    assert_eq!(s["allocation"]["profile"], "large");
    assert_eq!(s["allocation"]["reservation"]["memory_bytes"].as_u64(), Some(512 * MIB));
    assert_eq!(s["allocation"]["reservation"]["threads"].as_u64(), Some(1));
    assert_eq!(s["allocation"]["epoch"].as_u64().unwrap(), epoch + 1);
    assert_eq!(s["ownership"]["epoch"].as_u64().unwrap(), epoch + 1);
    assert_eq!(s["ownership"]["holder"], before["ownership"]["holder"]);
    assert_eq!(s["spec_id"], before["spec_id"]);
    assert_eq!(meta_of(dir.path(), &wb)["runtime_spec"], before["spec_id"]);
    assert_eq!(meta_of(dir.path(), &wb)["profile"], "large");
    // The collaborator was told and reconnects to the same workbook URL.
    let closed = a.event("workbook_closed").await.unwrap();
    assert_eq!(closed["reason"], "restarting");
    let mut b = server.ws(&wb).await;
    assert_eq!(b.hello["revision"], revision, "no change was lost or added");
    assert_eq!(b.ok("query", json!({ "sql": "SELECT x FROM t" })).await["rows"], json!([[42]]));
    let functions = b.ok("functions", json!({})).await;
    assert_eq!(functions[0]["pinned"], 1, "{functions}");
    b.close().await;
    let (_, usage) = server.get("/api/admin/resources").await;
    let mem = alephgrid_server::runtime::budget::parse_bytes(usage["allocations"][0]["duckdb_memory_limit"].as_str().unwrap()).unwrap();
    assert!(mem.abs_diff(512 * MIB) < 512 * MIB / 50, "the engine runs with the new size: {usage}");

    // A size that does not fit is refused before anything stops.
    let (code, body) = runtime(&server, &wb, json!({ "action": "restart", "profile": "xl" })).await;
    assert_eq!(code, 503, "{body}");
    let s = status(&server, &wb).await;
    assert_eq!((state_of(&s), s["allocation"]["epoch"].as_u64().unwrap()), ("ready", epoch + 1));

    // A replacement that fails to start: the workbook comes back as it was.
    hold.fail_next(&wb);
    let (code, body) = runtime(&server, &wb, json!({ "action": "restart", "profile": "standard" })).await;
    assert_eq!(code, 503, "{body}");
    assert!(body["error"].as_str().unwrap().contains("injected"), "{body}");
    let s = status(&server, &wb).await;
    assert_eq!(state_of(&s), "ready");
    assert_eq!(s["allocation"]["profile"], "large");
    assert_eq!(s["profile"], "large");
    assert_eq!(s["allocation"]["epoch"].as_u64().unwrap(), epoch + 3, "the failed one, then the restored one");
    let mut c = server.ws(&wb).await;
    assert_eq!(c.ok("query", json!({ "sql": "SELECT x FROM t" })).await["rows"], json!([[42]]));
    c.close().await;
    server.stop().await;
}

// ------------------------------------------------------------------ 7.16

#[tokio::test(flavor = "multi_thread")]
async fn heartbeats_alone_do_not_keep_compute_open() {
    let dir = tempfile::tempdir().unwrap();
    let server = TestServer::start_with(dir.path(), |c| c.idle_timeout = Duration::from_millis(400)).await;
    let wb = server.create_workbook("watched").await;
    let mut ws = server.ws(&wb).await;
    ws.ok("create_table_from_query", json!({ "name": "t", "sql": "SELECT 7 AS x" })).await;
    let last_op = alephgrid_core::storage::now_ms();
    // The connection stays open and pings, and does nothing else.
    let deadline = Instant::now() + Duration::from_secs(20);
    while server.state.controller.handle(&wb).is_some() {
        assert!(Instant::now() < deadline, "an open, pinging connection kept the workbook open");
        // It may close between the check and the ping.
        let _ = ws.ping().await;
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let s = status(&server, &wb).await;
    assert_eq!(state_of(&s), "stopped");
    let draining = s["allocation"]["transitions"].as_array().unwrap().iter().find(|t| t["to"] == "draining").unwrap().clone();
    assert!(draining["operation"].as_str().unwrap().starts_with("evict"));
    assert!(draining["at_ms"].as_u64().unwrap() >= last_op + 400, "closed only after the idle timeout: {draining}");
    // The session is told why.
    assert_eq!(ws.event("workbook_closed").await.unwrap()["reason"], "idle");
    // Checkpointed before it stopped: the data is in the file, not in a write-ahead log, and
    // the storage is kept.
    assert!(!dir.path().join(&wb).join("data.duckdb.wal").exists());
    assert!(s["storage_bytes"].as_u64().unwrap() > 0);
    let conn = duckdb::Connection::open_with_flags(dir.path().join(&wb).join("data.duckdb"), duckdb::Config::default().access_mode(duckdb::AccessMode::ReadOnly).unwrap()).unwrap();
    let x: i64 = conn.query_row("SELECT x FROM t", [], |r| r.get(0)).unwrap();
    assert_eq!(x, 7);
    drop(conn);
    // Using it again reopens it.
    let mut ws = server.ws(&wb).await;
    assert_eq!(ws.ok("query", json!({ "sql": "SELECT x FROM t" })).await["rows"], json!([[7]]));
    ws.close().await;
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_long_job_keeps_compute_open_and_idle_time_starts_when_it_ends() {
    let dir = tempfile::tempdir().unwrap();
    let server = TestServer::start_with(dir.path(), |c| c.idle_timeout = Duration::from_millis(300)).await;
    let wb = server.create_workbook("busy").await;
    let mut ws = server.ws(&wb).await;
    let epoch = status(&server, &wb).await["allocation"]["epoch"].clone();
    let slow = ws.send("query", json!({ "sql": SLOW_SQL })).await;
    // Five idle timeouts pass with nothing but the job running.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let s = status(&server, &wb).await;
    assert_eq!((state_of(&s), s["allocation"]["epoch"].clone()), ("ready", epoch.clone()), "{s}");
    // The job cannot end before its cancel is sent, so this is a lower bound on when it ended.
    // (Reading the clock after the reply instead adds the reply's delivery time, which is not
    // bounded on a loaded machine.) Had idle time counted from the job's start, the workbook
    // would close right after the cancel, well before this bound plus the timeout.
    let cancel_sent = alephgrid_core::storage::now_ms();
    ws.ok("cancel", json!({ "request": slow })).await;
    assert_eq!(ws.reply(slow).await.unwrap()["code"], "cancelled");
    let state = server.state.clone();
    let w = wb.clone();
    eventually("closed once idle", move || state.controller.handle(&w).is_none()).await;
    let s = status(&server, &wb).await;
    let draining = s["allocation"]["transitions"].as_array().unwrap().iter().find(|t| t["to"] == "draining").unwrap().clone();
    assert!(draining["at_ms"].as_u64().unwrap() + 20 >= cancel_sent + 300, "idle time counts from the end of the job: {draining}, cancel sent {cancel_sent}");
    ws.close().await;
    server.stop().await;
}

/// U.03: the workbook list says whether each workbook's compute is running or sleeping, from
/// the controller's memory (no workbook is opened to answer).
#[tokio::test(flavor = "multi_thread")]
async fn the_workbook_list_shows_compute_ready_then_sleeping() {
    let dir = tempfile::tempdir().unwrap();
    let server = TestServer::start_with(dir.path(), |c| c.idle_timeout = Duration::from_millis(400)).await;
    let wb = server.create_workbook("listed").await;
    let compute = |list: &serde_json::Value| list.as_array().unwrap().iter().find(|w| w["id"] == wb.as_str()).unwrap()["compute"].clone();

    let mut ws = server.ws(&wb).await;
    ws.ok("sheets", json!({})).await;
    let (_, list) = server.get("/api/workbooks").await;
    assert_eq!(compute(&list), "ready", "open and serving: {list}");

    ws.close().await;
    let mut asleep = false;
    for _ in 0..100 {
        let (_, list) = server.get("/api/workbooks").await;
        if compute(&list) == "sleeping" {
            asleep = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(asleep, "an idle workbook goes to sleep and the list says so");
    // Listing did not wake it.
    let (_, list) = server.get("/api/workbooks").await;
    assert_eq!(compute(&list), "sleeping");
    server.stop().await;
}
