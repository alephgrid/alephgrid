//! In-process test server and clients. Each test gets its own data directory and port.

#![allow(dead_code)]

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use rowfinity_server::state::{AppState, Config, Shared};
use serde_json::{json, Value};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

pub struct TestServer {
    pub addr: SocketAddr,
    pub state: Shared,
    pub data_dir: PathBuf,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    task: Option<tokio::task::JoinHandle<()>>,
    pub http: reqwest::Client,
}

impl TestServer {
    pub async fn start(data_dir: &Path) -> TestServer {
        Self::start_with(data_dir, |_| {}).await
    }

    pub async fn start_with(data_dir: &Path, configure: impl FnOnce(&mut Config)) -> TestServer {
        let mut config = Config {
            data_dir: data_dir.to_path_buf(),
            token: None,
            allow_sql: true,
            memory_limit: Some("1GB".into()),
            threads: Some(2),
        };
        configure(&mut config);
        let state = AppState::new(config).unwrap();
        let app = rowfinity_server::http::router(state.clone(), None);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let task = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = rx.await;
                })
                .await
                .unwrap();
        });
        TestServer { addr, state, data_dir: data_dir.to_path_buf(), shutdown: Some(tx), task: Some(task), http: reqwest::Client::new() }
    }

    /// Graceful stop, as on SIGTERM: stop serving, checkpoint, close every database.
    pub async fn stop(mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        if let Some(task) = self.task.take() {
            let _ = tokio::time::timeout(Duration::from_secs(10), task).await;
        }
        self.state.checkpoint_all();
        let state = self.state.clone();
        drop(self);
        // WebSocket sessions finish on their own once their client is gone; wait for them,
        // so that dropping the state really closes every DuckDB file.
        let deadline = Instant::now() + Duration::from_secs(10);
        while std::sync::Arc::strong_count(&state) > 1 {
            assert!(Instant::now() < deadline, "server state still referenced after stop");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        drop(state);
    }

    pub fn url(&self, path: &str) -> String {
        format!("http://{}{}", self.addr, path)
    }

    pub async fn get(&self, path: &str) -> (u16, Value) {
        let res = self.http.get(self.url(path)).send().await.unwrap();
        let status = res.status().as_u16();
        let text = res.text().await.unwrap();
        (status, serde_json::from_str(&text).unwrap_or(Value::String(text)))
    }

    pub async fn send(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> (u16, Value) {
        let mut req = self.http.request(method, self.url(path));
        if let Some(b) = body {
            req = req.json(&b);
        }
        let res = req.send().await.unwrap();
        let status = res.status().as_u16();
        let text = res.text().await.unwrap();
        (status, serde_json::from_str(&text).unwrap_or(Value::String(text)))
    }

    pub async fn create_workbook(&self, name: &str) -> String {
        let (status, body) = self.send(reqwest::Method::POST, "/api/workbooks", Some(json!({ "name": name }))).await;
        assert_eq!(status, 200, "{body}");
        body["id"].as_str().unwrap().to_string()
    }

    pub async fn upload(&self, workbook: &str, filename: &str, bytes: Vec<u8>) -> (u16, Value) {
        let res = self
            .http
            .post(self.url(&format!("/api/workbooks/{workbook}/import?filename={}", urlencode(filename))))
            .body(bytes)
            .send()
            .await
            .unwrap();
        let status = res.status().as_u16();
        let text = res.text().await.unwrap();
        (status, serde_json::from_str(&text).unwrap_or(Value::String(text)))
    }

    pub async fn download(&self, path: &str) -> (u16, reqwest::header::HeaderMap, Vec<u8>) {
        let res = self.http.get(self.url(path)).send().await.unwrap();
        let status = res.status().as_u16();
        let headers = res.headers().clone();
        (status, headers, res.bytes().await.unwrap().to_vec())
    }

    pub async fn ws(&self, workbook: &str) -> Ws {
        Ws::connect(self.addr, workbook).await
    }
}

pub fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| if b.is_ascii_alphanumeric() || b"-._~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") })
        .collect()
}

pub struct Ws {
    stream: WebSocketStream<MaybeTlsStream<TcpStream>>,
    next_id: u64,
    pub session: u64,
    /// Events received while waiting for replies.
    pub events: Vec<Value>,
}

impl Ws {
    pub async fn connect(addr: SocketAddr, workbook: &str) -> Ws {
        Self::try_connect(addr, workbook).await.expect("websocket connect")
    }

    pub async fn try_connect(addr: SocketAddr, workbook: &str) -> Result<Ws, String> {
        let (stream, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws/{workbook}")).await.map_err(|e| e.to_string())?;
        let mut ws = Ws { stream, next_id: 1, session: 0, events: vec![] };
        let hello = ws.next_message().await.ok_or("closed before hello")?;
        assert_eq!(hello["event"], "hello");
        ws.session = hello["session"].as_u64().unwrap();
        Ok(ws)
    }

    async fn next_message(&mut self) -> Option<Value> {
        loop {
            match tokio::time::timeout(Duration::from_secs(60), self.stream.next()).await.expect("websocket timed out")? {
                Ok(Message::Text(t)) => return Some(serde_json::from_str(&t).unwrap()),
                Ok(Message::Close(_)) | Err(_) => return None,
                Ok(_) => continue,
            }
        }
    }

    /// Send one operation and wait for its reply; events that arrive meanwhile are kept.
    pub async fn call(&mut self, op: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        let mut frame = params.as_object().cloned().unwrap_or_default();
        frame.insert("id".into(), id.into());
        frame.insert("op".into(), op.into());
        self.stream.send(Message::Text(Value::Object(frame).to_string().into())).await.map_err(|e| e.to_string())?;
        loop {
            let msg = self.next_message().await.ok_or_else(|| "connection closed".to_string())?;
            if msg.get("event").is_some() {
                self.events.push(msg);
                continue;
            }
            if msg["id"].as_u64() == Some(id) {
                return if msg["ok"] == true { Ok(msg["result"].clone()) } else { Err(msg["error"].as_str().unwrap_or("").to_string()) };
            }
        }
    }

    /// Send a frame without waiting for its reply.
    pub async fn send_raw(&mut self, frame: Value) {
        self.stream.send(Message::Text(frame.to_string().into())).await.unwrap();
    }

    pub async fn ok(&mut self, op: &str, params: Value) -> Value {
        match self.call(op, params.clone()).await {
            Ok(v) => v,
            Err(e) => panic!("{op} {params} failed: {e}"),
        }
    }

    /// Wait for the next event with this name (or connection close → None).
    pub async fn event(&mut self, name: &str) -> Option<Value> {
        if let Some(i) = self.events.iter().position(|e| e["event"] == name) {
            return Some(self.events.remove(i));
        }
        loop {
            let msg = self.next_message().await?;
            if msg["event"] == name {
                return Some(msg);
            }
            if msg.get("event").is_some() {
                self.events.push(msg);
            }
        }
    }

    /// True once the server has closed the connection.
    pub async fn closed(&mut self) -> bool {
        loop {
            match self.next_message().await {
                None => return true,
                Some(m) if m.get("event").is_some() => self.events.push(m),
                Some(_) => {}
            }
        }
    }

    pub async fn close(mut self) {
        let _ = self.stream.close(None).await;
        // Drain until the server acknowledges, so its session (and handle) is released.
        while let Ok(Some(_)) = tokio::time::timeout(Duration::from_secs(5), self.stream.next()).await {}
    }

    /// Every row of a view, read in fetch windows.
    pub async fn all_rows(&mut self, view: u64) -> Vec<Value> {
        let info = self.ok("view_info", json!({ "view_id": view })).await;
        let n = info["row_count"].as_u64().unwrap();
        let mut out = Vec::new();
        let mut start = 0;
        while start < n {
            let block = self.ok("fetch", json!({ "view_id": view, "row_start": start, "row_count": 5000 })).await;
            let rows = block["rows"].as_array().unwrap().clone();
            assert!(!rows.is_empty());
            start += rows.len() as u64;
            out.extend(rows);
        }
        out
    }
}

/// Poll a condition until it holds (observable state, not a fixed sleep).
pub async fn eventually(what: &str, mut f: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !f() {
        assert!(Instant::now() < deadline, "timed out waiting for: {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

pub fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                out.extend(files_under(&p));
            } else {
                out.push(p);
            }
        }
    }
    out
}

/// Parse simple CSV text (quoted fields, doubled quotes) into rows of strings.
pub fn parse_csv(text: &str) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match (c, quoted) {
            ('"', true) if chars.peek() == Some(&'"') => {
                field.push('"');
                chars.next();
            }
            ('"', true) => quoted = false,
            ('"', false) if field.is_empty() => quoted = true,
            (',', false) => row.push(std::mem::take(&mut field)),
            ('\n', false) => {
                row.push(std::mem::take(&mut field));
                rows.push(std::mem::take(&mut row));
            }
            ('\r', false) => {}
            (c, _) => field.push(c),
        }
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        rows.push(row);
    }
    rows
}
