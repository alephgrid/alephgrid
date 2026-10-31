//! Gate 7.12 on one host with policy-managed local compute: two spaces cannot reach each
//! other's data, and concurrent, reconnecting users cannot silently lose or misapply edits,
//! while the workbook's compute is restarted with another size and closed when idle.
//!
//! The oracle is the clients' own record of what the server acknowledged: every cell ends
//! with the value of the latest acknowledged edit of it (by revision), no refused edit's
//! value appears anywhere, and a resent change is applied once.

mod common;

use std::collections::HashMap;
use std::time::Duration;

use alephgrid_server::runtime::ComputeProfile;
use common::*;
use reqwest::Method;
use serde_json::{json, Value};

const ROWS: i64 = 12;
const EDITS: usize = 60;

fn csv(rows: i64, word: &str) -> Vec<u8> {
    let mut s = String::from("id,v,n\n");
    for i in 1..=rows {
        s.push_str(&format!("{i},{word}{i},0\n"));
    }
    s.into_bytes()
}

/// Deterministic choices per client (xorshift).
struct Rng(u64);

impl Rng {
    fn next(&mut self, n: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % n
    }
}

/// One person's connection, as a browser keeps it: what it last saw of each record, and
/// changes resent with their mutation id after a reconnect.
struct Client {
    who: Session,
    wb: String,
    ws: Ws,
    view: u64,
    seen: HashMap<i64, Value>,
    reconnects: usize,
}

enum Outcome {
    Applied(u64),
    Refused,
}

impl Client {
    async fn open(who: &Session, wb: &str) -> Client {
        let mut c = Client { who: who.clone(), wb: wb.to_string(), ws: who.ws(wb).await.unwrap(), view: 0, seen: HashMap::new(), reconnects: 0 };
        c.reopen().await;
        c
    }

    async fn reopen(&mut self) {
        match self.ws.call("open_view", json!({ "spec": { "table": "t", "sort": [], "filters": [], "hidden": [] } })).await {
            Ok(v) => self.view = v["view_id"].as_u64().unwrap(),
            Err(e) => {
                assert!(e.contains("closed"), "open_view: {e}");
                return Box::pin(self.reconnect()).await;
            }
        }
        self.refresh().await;
    }

    async fn refresh(&mut self) {
        let block = loop {
            match self.ws.call("fetch", json!({ "view_id": self.view, "row_start": 0, "row_count": 1000 })).await {
                Ok(b) => break b,
                Err(e) => {
                    assert!(e.contains("closed"), "fetch: {e}");
                    Box::pin(self.reconnect()).await;
                }
            }
        };
        self.seen.clear();
        for (id, row) in block["ids"].as_array().unwrap().iter().zip(block["rows"].as_array().unwrap()) {
            self.seen.insert(id.as_i64().unwrap(), row[1].clone());
        }
    }

    /// Reconnect after the server closed the workbook (idle, restart) or the connection.
    async fn reconnect(&mut self) {
        for _ in 0..100 {
            if let Ok(ws) = self.who.ws(&self.wb).await {
                self.ws = ws;
                self.reconnects += 1;
                Box::pin(self.reopen()).await;
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("could not reconnect");
    }

    /// Send `op` with a mutation id until the server answers it: a change cut off by a
    /// closed workbook is resent as it was, as the browser does.
    async fn change(&mut self, op: &str, mut frame: Value, uses_view: bool) -> Outcome {
        loop {
            if uses_view {
                frame["view_id"] = json!(self.view);
            }
            let id = self.ws.send(op, frame.clone()).await;
            match self.ws.reply(id).await {
                Some(r) if r["ok"] == true => return Outcome::Applied(r["revision"].as_u64().unwrap()),
                Some(r) if r["code"] == "conflict" => return Outcome::Refused,
                Some(r) => assert!(r["code"] == "unavailable", "{op} {frame}: {r}"),
                None => {}
            }
            self.reconnect().await;
        }
    }
}

/// `count` edits by one client of random cells, each based on what it last saw. Returns
/// the acknowledged (revision, record, value) and the refused values.
async fn edit_cells(c: &mut Client, name: &str, seed: u64, count: usize, from: usize) -> (Vec<(u64, i64, String)>, Vec<String>) {
    let mut rng = Rng(seed);
    let (mut acked, mut refused) = (Vec::new(), Vec::new());
    let mut records: Vec<i64> = c.seen.keys().copied().collect();
    records.sort();
    assert_eq!(records.len() as i64, ROWS);
    for k in from..from + count {
        let record = records[rng.next(ROWS as u64) as usize];
        let value = format!("{name}-{k}");
        let base = c.seen.get(&record).cloned().unwrap_or(Value::Null);
        let frame = json!({ "mutation_id": format!("{name}-{k}"), "edits": [{ "row": 0, "record": record, "column": "v", "value": value, "base": base }] });
        match c.change("edit", frame, true).await {
            Outcome::Applied(rev) => {
                acked.push((rev, record, value.clone()));
                c.seen.insert(record, json!(value));
            }
            Outcome::Refused => {
                refused.push(value);
                c.refresh().await;
            }
        }
        if k % 7 == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }
    (acked, refused)
}

#[tokio::test(flavor = "multi_thread")]
async fn gate_two_spaces_and_concurrent_reconnecting_users() {
    let dir = tempfile::tempdir().unwrap();
    let server = TestServer::start_accounts(dir.path(), |c| {
        c.idle_timeout = Duration::from_millis(600);
        c.memory_budget = Some(2 << 30);
        c.threads = Some(8);
        c.profiles = vec![ComputeProfile::parse("large=512MiB:2").unwrap()];
    })
    .await;
    let admin = server.admin().await;
    let org = admin.me["orgs"][0]["org"]["id"].as_str().unwrap().to_string();
    let mut spaces = Vec::new();
    let mut books = Vec::new();
    for (name, word) in [("Alpha", "alpha"), ("Beta", "beta-secret")] {
        let space = admin.ok(Method::POST, "/api/spaces", Some(json!({ "name": name }))).await["id"].as_str().unwrap().to_string();
        let wb = admin.ok(Method::POST, "/api/workbooks", Some(json!({ "name": name, "space": space }))).await["id"].as_str().unwrap().to_string();
        let (s, v) = admin.send_bytes(Method::POST, &format!("/api/workbooks/{wb}/import?filename=t.csv"), csv(ROWS, word)).await;
        assert_eq!(s, 200, "{v}");
        spaces.push(space);
        books.push(wb);
    }
    let (alpha, wa, wb) = (spaces[0].clone(), books[0].clone(), books[1].clone());
    admin.ok(Method::PUT, &format!("/api/spaces/{alpha}/profiles"), Some(json!({ "profiles": ["large"] }))).await;
    let mut people = Vec::new();
    for (email, space) in [("alice@example.com", &spaces[0]), ("bob@example.com", &spaces[1])] {
        let code = admin.invite(&org, email, Some(space), Some("editor")).await;
        people.push(server.accept(&code, email.split('@').next().unwrap(), "a long enough password").await);
    }
    let (alice, bob) = (people[0].clone(), people[1].clone());

    // ---------------------------------------------- the other space's data is out of reach
    for path in [
        format!("/api/workbooks/{wb}"),
        format!("/api/workbooks/{wb}/runtime"),
        format!("/api/workbooks/{wb}/profiles"),
        format!("/api/workbooks/{wb}/export?spec={}&format=csv", urlencode(&json!({ "table": "t" }).to_string())),
    ] {
        let (code, body) = alice.get(&path).await;
        assert_eq!(code, 404, "{path}: {body}");
        assert!(!body.to_string().contains("beta-secret"), "{path}: {body}");
    }
    for body in [json!({ "action": "start" }), json!({ "action": "stop" }), json!({ "action": "restart", "profile": "large" })] {
        let (code, reply) = alice.post(&format!("/api/workbooks/{wb}/runtime"), body.clone()).await;
        assert_eq!(code, 404, "{body}: {reply}");
    }
    assert!(alice.ws(&wb).await.is_err(), "no connection to the other space's workbook");
    // Nor through SQL in her own workbook: its database reaches only its own files.
    let mut probe = alice.ws(&wa).await.unwrap();
    let other = dir.path().join(&wb);
    for sql in [
        format!("SELECT * FROM '{}'", other.join("data.duckdb").display()),
        format!("SELECT * FROM read_csv('{}')", other.join("workbook.json").display()),
        format!("SELECT * FROM read_parquet('{}/*.parquet')", dir.path().join(".exports").join(&wb).display()),
        format!("ATTACH '{}' AS b", other.join("data.duckdb").display()),
    ] {
        let id = probe.send("query", json!({ "sql": sql })).await;
        let r = probe.reply(id).await.unwrap();
        assert_eq!(r["ok"], false, "{sql}: {r}");
        assert!(!r.to_string().contains("beta-secret"), "{sql}: {r}");
        let id = probe.send("sql_change", json!({ "sql": format!("COPY t TO '{}'", other.join("stolen.csv").display()) })).await;
        assert_eq!(probe.reply(id).await.unwrap()["ok"], false);
    }
    assert!(!other.join("stolen.csv").exists());
    probe.close().await;

    // ------------------------------------------- concurrent, reconnecting edits on Alpha
    let mut a1 = Client::open(&alice, &wa).await;
    let mut a2 = Client::open(&alice, &wa).await;
    let mut b1 = Client::open(&bob, &wb).await;
    let restarter = {
        let admin = admin.clone();
        let wa = wa.clone();
        tokio::spawn(async move {
            // Two controlled restarts while the edits run: to large and back.
            for profile in ["large", "standard"] {
                tokio::time::sleep(Duration::from_millis(150)).await;
                let s = admin.ok(Method::POST, &format!("/api/workbooks/{wa}/runtime"), Some(json!({ "action": "restart", "profile": profile }))).await;
                assert_eq!(s["allocation"]["profile"], profile);
            }
        })
    };
    let (r1, r2, rb) = tokio::join!(
        edit_cells(&mut a1, "a1", 0x9e3779b97f4a7c15, EDITS, 0),
        edit_cells(&mut a2, "a2", 0xd1b54a32d192ed03, EDITS, 0),
        edit_cells(&mut b1, "b1", 0x2545f4914f6cdd1d, EDITS / 2, 0),
    );
    restarter.await.unwrap();
    let mut acked = [r1.0, r2.0].concat();
    let mut refused = [r1.1, r2.1].concat();
    assert!(a1.reconnects + a2.reconnects >= 2, "the restarts closed the connections ({} and {} reconnects)", a1.reconnects, a2.reconnects);

    // Idle: nobody does anything, the connections stay open; compute is closed anyway.
    let state = server.state.clone();
    let w = wa.clone();
    eventually("Alpha's workbook is closed when idle", move || state.controller.handle(&w).is_none()).await;
    let s = admin.ok(Method::GET, &format!("/api/workbooks/{wa}/runtime"), None).await;
    assert_eq!(s["allocation"]["state"], "stopped");
    assert_eq!(a1.ws.event("workbook_closed").await.unwrap()["reason"], "idle");
    // They continue: their next changes reconnect and are checked against what they saw.
    let (more1, more2) = tokio::join!(edit_cells(&mut a1, "a1", 7, 20, EDITS), edit_cells(&mut a2, "a2", 11, 20, EDITS));
    acked.extend(more1.0.into_iter().chain(more2.0));
    refused.extend(more1.1.into_iter().chain(more2.1));

    // A batch of increments sent with mutation ids, cut off mid-flight, then resent whole
    // after reconnecting: each applies once.
    let mut ids = Vec::new();
    for k in 0..10 {
        ids.push(a1.ws.send("sql_change", json!({ "mutation_id": format!("inc-{k}"), "sql": "UPDATE t SET n = n + 1 WHERE id = 1" })).await);
    }
    let _ = a1.ws.reply(ids[2]).await;
    let cut = std::mem::replace(&mut a1.ws, alice.ws(&wa).await.unwrap());
    drop(cut);
    a1.reopen().await;
    for k in 0..10 {
        let frame = json!({ "mutation_id": format!("inc-{k}"), "sql": "UPDATE t SET n = n + 1 WHERE id = 1" });
        assert!(matches!(a1.change("sql_change", frame, false).await, Outcome::Applied(_)));
    }

    // ------------------------------------------------------------------- the oracle
    // Each record starts with its imported value, then has the latest acknowledged edit.
    acked.sort();
    let block = a1.ws.ok("fetch", json!({ "view_id": a1.view, "row_start": 0, "row_count": 1000 })).await;
    let (ids, rows) = (block["ids"].as_array().unwrap().clone(), block["rows"].as_array().unwrap().clone());
    assert_eq!(rows.len() as i64, ROWS);
    let mut expected: HashMap<i64, String> =
        ids.iter().zip(&rows).map(|(id, row)| (id.as_i64().unwrap(), format!("alpha{}", row[0].as_i64().unwrap()))).collect();
    for (_, record, value) in &acked {
        expected.insert(*record, value.clone());
    }
    for (id, row) in ids.iter().zip(&rows) {
        let id = id.as_i64().unwrap();
        assert_eq!(row[1].as_str().unwrap(), expected[&id], "record {id}: the latest acknowledged edit");
        assert!(!refused.iter().any(|r| r == row[1].as_str().unwrap()), "a refused edit was applied: {row}");
    }
    let n = a1.ws.ok("query", json!({ "sql": "SELECT n FROM t WHERE id = 1" })).await["rows"].clone();
    assert_eq!(n, json!([[10]]), "ten increments, each once");
    assert!(!refused.is_empty(), "the clients did overlap");
    let revisions: std::collections::HashSet<u64> = acked.iter().map(|(r, _, _)| *r).collect();
    assert_eq!(revisions.len(), acked.len(), "one revision per acknowledged change");
    eprintln!("gate: {} acknowledged, {} refused, {} + {} reconnects", acked.len(), refused.len(), a1.reconnects, a2.reconnects);

    // Beta got Bob's edits only (it closed when idle meanwhile: read it afresh).
    drop(b1);
    let mut b2 = Client::open(&bob, &wb).await;
    let block = b2.ws.ok("fetch", json!({ "view_id": b2.view, "row_start": 0, "row_count": 1000 })).await;
    let mut acked_b = rb.0;
    acked_b.sort();
    let (ids, rows) = (block["ids"].as_array().unwrap().clone(), block["rows"].as_array().unwrap().clone());
    let mut expected_b: HashMap<i64, String> =
        ids.iter().zip(&rows).map(|(id, row)| (id.as_i64().unwrap(), format!("beta-secret{}", row[0].as_i64().unwrap()))).collect();
    for (_, record, value) in &acked_b {
        expected_b.insert(*record, value.clone());
    }
    for (id, row) in ids.iter().zip(&rows) {
        assert_eq!(row[1].as_str().unwrap(), expected_b[&id.as_i64().unwrap()], "{row}");
        assert!(!row[1].as_str().unwrap().starts_with('a'), "{row}");
    }
    server.stop().await;
}
