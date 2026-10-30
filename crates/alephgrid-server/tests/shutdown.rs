//! 3.09: SIGTERM stops admitting work, drains, checkpoints, records allocations as stopped
//! and exits 0. Acknowledged changes survive; unacknowledged ones are complete or absent;
//! an import cut short leaves no table. A second signal exits at once. This is process
//! termination only (the OS keeps its page cache); it says nothing about power loss.

mod common;

use std::process::{Child, Command, Stdio};
use std::io::{BufRead, BufReader};
use std::time::{Duration, Instant};

use common::*;
use serde_json::{json, Value};

struct Proc {
    child: Child,
    addr: std::net::SocketAddr,
}

async fn spawn(data_dir: &std::path::Path, extra: &[&str]) -> Proc {
    let mut args = vec!["--data-dir", data_dir.to_str().unwrap(), "--threads", "2"];
    // Let the child reserve its own port. Releasing a probe listener first races
    // parallel tests and a TCP readiness probe can connect to the wrong child.
    args.extend(["--port", "0"]);
    args.extend(extra);
    let mut child = Command::new(env!("CARGO_BIN_EXE_alephgrid")).args(&args).stdout(Stdio::piped()).stderr(Stdio::inherit()).spawn().unwrap();
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let mut tx = Some(tx);
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Some(url) = line.split("Alephgrid is running → http://").nth(1) {
                if let Ok(addr) = url.split('/').next().unwrap_or("").parse::<std::net::SocketAddr>() {
                    if let Some(tx) = tx.take() { let _ = tx.send(addr); }
                }
            }
        }
    });
    let addr = match tokio::time::timeout(Duration::from_secs(30), rx).await {
        Ok(Ok(addr)) => addr,
        other => {
            let _ = child.kill();
            let _ = child.wait();
            panic!("server did not report its bound address: {other:?}");
        }
    };
    Proc { child, addr }
}

impl Proc {
    fn signal(&self, sig: &str) {
        let ok = Command::new("kill").args([sig, &self.child.id().to_string()]).status().unwrap().success();
        assert!(ok);
    }

    /// Wait for the process to exit (bounded) and return its exit code.
    async fn exit_code(mut self, within: Duration) -> Option<i32> {
        let deadline = Instant::now() + within;
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status.code();
            }
            if Instant::now() > deadline {
                let _ = self.child.kill();
                panic!("the server did not exit within {within:?}");
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

async fn create(addr: std::net::SocketAddr, name: &str) -> String {
    let r: Value = reqwest::Client::new().post(format!("http://{addr}/api/workbooks")).json(&json!({ "name": name })).send().await.unwrap().json().await.unwrap();
    r["id"].as_str().unwrap().to_string()
}

fn record(dir: &std::path::Path, wb: &str) -> Value {
    serde_json::from_slice(&std::fs::read(dir.join(".control/allocations").join(format!("{wb}.json"))).unwrap()).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn sigterm_when_idle_exits_cleanly_and_records_allocations_stopped() {
    let dir = tempfile::tempdir().unwrap();
    let p = spawn(dir.path(), &[]).await;
    let wb = create(p.addr, "idle").await;
    let mut ws = Ws::connect(p.addr, &wb).await;
    ws.ok("create_table_from_query", json!({ "name": "t", "sql": "SELECT 1 AS x" })).await;
    p.signal("-TERM");
    let closed = ws.event("workbook_closed").await.expect("sessions are told");
    assert_eq!(closed["reason"], "shutdown");
    assert_eq!(p.exit_code(Duration::from_secs(20)).await, Some(0));
    assert_eq!(record(dir.path(), &wb)["state"], "stopped", "a clean stop, not a lost allocation");
    assert!(!dir.path().join(".tmp").join(&wb).exists());
    let p = spawn(dir.path(), &[]).await;
    let mut ws = Ws::connect(p.addr, &wb).await;
    assert_eq!(ws.ok("query", json!({ "sql": "SELECT x FROM t" })).await["rows"], json!([[1]]));
    p.signal("-TERM");
    assert_eq!(p.exit_code(Duration::from_secs(20)).await, Some(0));
}

#[tokio::test(flavor = "multi_thread")]
async fn sigterm_during_a_stream_of_edits_keeps_every_acknowledged_one() {
    let dir = tempfile::tempdir().unwrap();
    let p = spawn(dir.path(), &[]).await;
    let wb = create(p.addr, "stream").await;
    let mut ws = Ws::connect(p.addr, &wb).await;
    ws.ok("create_table_from_query", json!({ "name": "t", "sql": "SELECT 0 AS a, 0 AS b FROM range(2000)" })).await;
    let view = ws.ok("open_view", json!({ "spec": { "table": "t" } })).await["view_id"].as_u64().unwrap();
    // Edits one after another; the signal arrives after 300 of them. Every edit that got
    // a success reply is acknowledged; the stream stops at the first failure or close.
    let mut acked = Vec::new();
    for k in 0..2000u64 {
        if k == 300 {
            p.signal("-TERM");
        }
        // Two cells per edit: each edit is all or nothing.
        let r = ws.call("edit", json!({ "view_id": view, "edits": [{ "row": k, "column": "a", "value": "1" }, { "row": k, "column": "b", "value": "1" }] })).await;
        match r {
            Ok(_) => acked.push(k),
            Err(_) => break,
        }
    }
    let code = p.exit_code(Duration::from_secs(30)).await;
    assert_eq!(code, Some(0));
    assert!(acked.len() >= 300, "{} acknowledged", acked.len());
    let p = spawn(dir.path(), &[]).await;
    let mut ws = Ws::connect(p.addr, &wb).await;
    let v = ws.ok("open_view", json!({ "spec": { "table": "t" } })).await["view_id"].as_u64().unwrap();
    let rows = ws.all_rows(v).await;
    for k in &acked {
        assert_eq!(rows[*k as usize], json!([1, 1]), "acknowledged edit of row {k} is present");
    }
    for (k, r) in rows.iter().enumerate() {
        assert!(r == &json!([0, 0]) || r == &json!([1, 1]), "row {k} is half-applied: {r}");
    }
    p.signal("-TERM");
    assert_eq!(p.exit_code(Duration::from_secs(20)).await, Some(0));
}

#[tokio::test(flavor = "multi_thread")]
async fn sigterm_during_an_import_leaves_a_whole_table_or_none() {
    let dir = tempfile::tempdir().unwrap();
    // A short grace period, so the import is interrupted rather than waited for.
    let p = spawn(dir.path(), &["--shutdown-grace", "1"]).await;
    let wb = create(p.addr, "import").await;
    let mut csv = String::with_capacity(80_000_000);
    csv.push_str("a,b,c\n");
    for i in 0..4_000_000u64 {
        csv.push_str(&format!("{i},{},{}\n", i % 97, i * 3));
    }
    let http = reqwest::Client::new();
    let staged: Value = http
        .post(format!("http://{}/api/workbooks/{wb}/uploads?filename=big.csv", p.addr))
        .body(csv)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let upload = staged["upload_id"].as_str().unwrap().to_string();
    let url = format!("http://{}/api/workbooks/{wb}/uploads/{upload}/import", p.addr);
    let import = tokio::spawn(async move { http.post(url).json(&json!({ "job": "j" })).send().await.map(|r| r.status().as_u16()) });
    // Signal once the import is running.
    let status_url = format!("http://{}/api/workbooks/{wb}/jobs/j", p.addr);
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let st: Value = reqwest::get(&status_url).await.unwrap().json().await.unwrap();
        if st["running"] == true {
            break;
        }
        assert!(Instant::now() < deadline, "the import never started");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    p.signal("-TERM");
    let reply = import.await.unwrap();
    assert_eq!(p.exit_code(Duration::from_secs(30)).await, Some(0));
    let p = spawn(dir.path(), &[]).await;
    let info: Value = reqwest::get(format!("http://{}/api/workbooks/{wb}", p.addr)).await.unwrap().json().await.unwrap();
    let tables = info["tables"].as_array().unwrap();
    match tables.as_slice() {
        [] => {}
        [t] => assert_eq!(t["row_count"], 4_000_000, "a table that exists is complete"),
        other => panic!("{other:?}"),
    }
    assert!(!matches!(reply, Ok(200)) || tables.len() == 1, "an acknowledged import must be there");
    eprintln!("import reply during shutdown: {reply:?}; tables after restart: {}", tables.len());
    assert!(files_under(&dir.path().join(".uploads")).is_empty(), "staged files do not survive a restart");
    p.signal("-TERM");
    assert_eq!(p.exit_code(Duration::from_secs(20)).await, Some(0));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_second_signal_exits_at_once_and_committed_changes_survive() {
    let dir = tempfile::tempdir().unwrap();
    let p = spawn(dir.path(), &["--shutdown-grace", "60"]).await;
    let wb = create(p.addr, "twice").await;
    let mut ws = Ws::connect(p.addr, &wb).await;
    ws.ok("create_table_from_query", json!({ "name": "t", "sql": "SELECT 5 AS x" })).await;
    // Something long is running, so the first signal starts a long drain.
    ws.send("query", json!({ "sql": "SELECT sum(i) FROM range(100000000000) r(i)" })).await;
    let resources = format!("http://{}/api/admin/resources", p.addr);
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let u: Value = reqwest::get(&resources).await.unwrap().json().await.unwrap();
        if u["allocations"].as_array().unwrap().iter().any(|a| a["jobs"].as_u64() > Some(0)) {
            break;
        }
        assert!(Instant::now() < deadline, "the query never started");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    p.signal("-TERM");
    // Draining has begun once the session is told the workbook is closing.
    ws.event("workbook_closed").await.expect("closing announced");
    p.signal("-TERM");
    let started = Instant::now();
    let code = p.exit_code(Duration::from_secs(10)).await;
    assert_eq!(code, Some(130));
    assert!(started.elapsed() < Duration::from_secs(10));
    let p = spawn(dir.path(), &[]).await;
    let mut ws = Ws::connect(p.addr, &wb).await;
    assert_eq!(ws.ok("query", json!({ "sql": "SELECT x FROM t" })).await["rows"], json!([[5]]));
    p.signal("-TERM");
    assert_eq!(p.exit_code(Duration::from_secs(20)).await, Some(0));
}
