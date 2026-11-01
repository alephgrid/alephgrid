//! In-process test server and clients. Each test gets its own data directory and port.

#![allow(dead_code)]

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use alephgrid_server::state::{AppState, Config, Shared};
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
    janitor: Option<tokio::task::JoinHandle<()>>,
    pub http: reqwest::Client,
}

impl TestServer {
    pub async fn start(data_dir: &Path) -> TestServer {
        Self::start_with(data_dir, |_| {}).await
    }

    pub async fn start_with(data_dir: &Path, configure: impl FnOnce(&mut Config)) -> TestServer {
        let mut config = Config::new(data_dir);
        config.memory_budget = Some(1 << 30);
        config.threads = Some(2);
        config.temp_budget = Some(4 << 30);
        // Tests take backups back to back.
        config.backup_interval = std::time::Duration::ZERO;
        configure(&mut config);
        let state = AppState::new(config).unwrap();
        let janitor = alephgrid_server::state::start_janitor(&state);
        let app = alephgrid_server::http::router(state.clone(), None);
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
        TestServer { addr, state, data_dir: data_dir.to_path_buf(), shutdown: Some(tx), task: Some(task), janitor: Some(janitor), http: reqwest::Client::new() }
    }

    /// Graceful stop, as on SIGTERM: stop admitting, drain and close every workbook (which
    /// ends their sessions), then stop serving.
    pub async fn stop(mut self) {
        if let Some(j) = self.janitor.take() {
            j.abort();
        }
        let state = self.state.clone();
        tokio::task::spawn_blocking(move || state.checkpoint_all()).await.unwrap();
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        if let Some(task) = self.task.take() {
            let _ = tokio::time::timeout(Duration::from_secs(10), task).await;
        }
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
    /// Replies received while waiting for another one.
    pub replies: std::collections::HashMap<u64, Value>,
    pub hello: Value,
}

impl Ws {
    pub async fn connect(addr: SocketAddr, workbook: &str) -> Ws {
        Self::try_connect(addr, workbook).await.expect("websocket connect")
    }

    pub async fn try_connect(addr: SocketAddr, workbook: &str) -> Result<Ws, String> {
        let (stream, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws/{workbook}")).await.map_err(|e| e.to_string())?;
        let mut ws = Ws { stream, next_id: 1, session: 0, events: vec![], replies: Default::default(), hello: Value::Null };
        let hello = ws.next_message().await.ok_or("closed before hello")?;
        assert_eq!(hello["event"], "hello");
        ws.session = hello["session"].as_u64().unwrap();
        ws.hello = hello;
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
            if let Some(other) = msg["id"].as_u64() {
                self.replies.insert(other, msg);
            }
        }
    }

    /// Send a request frame (`params` may include `mutation_id`, `timeout_ms`) and return
    /// its id without waiting.
    pub async fn send(&mut self, op: &str, params: Value) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        let mut frame = params.as_object().cloned().unwrap_or_default();
        frame.insert("id".into(), id.into());
        frame.insert("op".into(), op.into());
        self.stream.send(Message::Text(Value::Object(frame).to_string().into())).await.unwrap();
        id
    }

    /// The whole reply frame for request `id`, waiting for it (None if the connection closed).
    pub async fn reply(&mut self, id: u64) -> Option<Value> {
        if let Some(r) = self.replies.remove(&id) {
            return Some(r);
        }
        loop {
            let msg = self.next_message().await?;
            if msg.get("event").is_some() {
                self.events.push(msg);
                continue;
            }
            match msg["id"].as_u64() {
                Some(i) if i == id => return Some(msg),
                Some(i) => {
                    self.replies.insert(i, msg);
                }
                None => {}
            }
        }
    }

    /// Ids of replies received so far, in arrival order is not kept; use `reply` per id.
    pub fn buffered(&self) -> usize {
        self.replies.len()
    }

    /// A WebSocket ping (what keepalives send). The server answers it; it is not activity.
    /// False once the connection is closed.
    pub async fn ping(&mut self) -> bool {
        self.stream.send(Message::Ping(Vec::new().into())).await.is_ok()
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

    /// The next event, whatever its name (those kept while waiting for replies first).
    pub async fn event_any(&mut self) -> Option<Value> {
        if !self.events.is_empty() {
            return Some(self.events.remove(0));
        }
        loop {
            let msg = self.next_message().await?;
            if msg.get("event").is_some() {
                return Some(msg);
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

/// One sample request of every operation, in an order that runs on a workbook holding table
/// `t` (`n,s` with 3 rows). Ids written as `PLACEHOLDER`, `QUERY`, `ANALYSIS`, `DASHBOARD`,
/// `PREVIEW_REV` and `UNDO_REV` are filled in by the caller from earlier replies.
pub fn operation_script() -> Vec<(&'static str, Value)> {
    vec![
        ("tables", json!({})),
        ("open_view", json!({ "spec": { "table": "t", "sort": [], "filters": [] } })),
        ("update_view", json!({ "view_id": 1, "spec": { "table": "t", "sort": [{ "column": "n", "desc": true }], "filters": [] } })),
        ("view_info", json!({ "view_id": 1 })),
        ("fetch", json!({ "view_id": 1, "row_start": 0, "row_count": 10 })),
        ("update_view", json!({ "view_id": 1, "spec": { "table": "t" } })),
        ("edit", json!({ "view_id": 1, "edits": [{ "row": 0, "column": "s", "value": "z" }] })),
        ("insert_rows", json!({ "view_id": 1, "at": 1, "count": 1 })),
        ("delete_rows", json!({ "view_id": 1, "start": 1, "count": 1 })),
        // Take the delete back and redo it: the state that follows is unchanged.
        ("undo", json!({})),
        ("undo", json!({ "redo": true })),
        ("stats", json!({ "view_id": 1, "column": "n", "start": 0, "end": 3 })),
        ("find_in_view", json!({ "view_id": 1, "text": "1", "count": true })),
        ("column_values", json!({ "spec": { "table": "t" }, "column": "n" })),
        ("add_column", json!({ "table": "t", "name": "extra", "sql_type": "INTEGER" })),
        ("add_formula_column", json!({ "table": "t", "name": "twice", "formula": "[n] * 2" })),
        ("update_formula", json!({ "table": "t", "name": "twice", "formula": "[n] * 3" })),
        ("rename_column", json!({ "table": "t", "name": "extra", "new_name": "more" })),
        ("drop_column", json!({ "table": "t", "name": "more" })),
        ("query", json!({ "sql": "SELECT n, twice FROM t" })),
        ("create_table_from_query", json!({ "name": "copy", "sql": "SELECT * FROM t" })),
        ("create_demo_table", json!({ "rows": 10 })),
        ("rename_table", json!({ "table": "copy", "new_name": "copy2" })),
        ("drop_table", json!({ "table": "copy2" })),
        ("sheet_create", json!({ "name": "S" })),
        ("sheets", json!({})),
        ("sheet_rename", json!({ "sheet": 0, "name": "Sheet A" })),
        ("sheet_set", json!({ "sheet": 0, "inputs": [{ "row": 0, "column": 0, "value": "1" }, { "row": 1, "column": 0, "value": "=A1+1" }] })),
        ("sheet_fetch", json!({ "sheet": 0, "row_start": 0, "row_count": 5, "column_count": 3 })),
        ("sheet_undo", json!({})),
        ("sheet_redo", json!({})),
        ("sheet_insert_rows", json!({ "sheet": 0, "at": 0, "count": 1 })),
        ("sheet_delete_rows", json!({ "sheet": 0, "at": 0, "count": 1 })),
        ("sheet_insert_columns", json!({ "sheet": 0, "at": 0, "count": 1 })),
        ("sheet_delete_columns", json!({ "sheet": 0, "at": 0, "count": 1 })),
        ("sheet_define_name", json!({ "name": "FirstCells", "refers_to": "='Sheet A'!$A$1:$A$2" })),
        ("sheet_update_name", json!({ "name": "FirstCells", "new_name": "Anchor", "new_scope": 0, "refers_to": "='Sheet A'!$A$1" })),
        ("sheet_names", json!({})),
        ("sheet_delete_name", json!({ "name": "Anchor", "scope": 0 })),
        ("sheet_cell_detail", json!({ "sheet": 0, "row": 1, "column": 0 })),
        ("sheet_check_formula", json!({ "sheet": 0, "row": 0, "column": 1, "formula": "=SUM(1,,"})),
        ("sheet_fill", json!({ "sheet": 0, "area": { "row": 1, "column": 0, "rows": 3, "columns": 1 }, "direction": "down" })),
        ("sheet_style", json!({ "sheet": 0, "area": { "row": 0, "column": 0, "rows": 2, "columns": 2 }, "style": { "b": true, "fill": "#FFEEAA", "num_fmt": "0.00" } })),
        ("sheet_find", json!({ "sheet": 0, "search": { "text": "1" }, "count": true })),
        ("sheet_replace", json!({ "sheet": 0, "search": { "text": "zzz-none" }, "replacement": "y" })),
        ("sheet_find_all", json!({ "search": { "text": "a" } })),
        ("sheet_replace_all", json!({ "search": { "text": "zzz-none" }, "replacement": "y" })),
        ("add_comment", json!({ "sheet": 0, "row": 0, "column": 0, "text": "Check this" })),
        ("comments", json!({ "sheet": 0 })),
        ("edit_comment", json!({ "sheet": 0, "comment": "COMMENT", "text": "Checked" })),
        ("resolve_comment", json!({ "sheet": 0, "comment": "COMMENT", "resolved": true })),
        ("delete_comment", json!({ "sheet": 0, "comment": "COMMENT" })),
        ("sheet_border", json!({ "sheet": 0, "area": { "row": 0, "column": 0, "rows": 2, "columns": 2 }, "kind": "outer", "weight": "thin" })),
        ("sheet_sort", json!({ "sheet": 0, "area": { "row": 40, "column": 20, "rows": 2, "columns": 1 }, "by": [{ "column": 0, "desc": true }] })),
        ("sheet_color", json!({ "sheet": 0, "color": "#2E75B6" })),
        ("sheet_freeze", json!({ "sheet": 0, "columns": 1 })),
        ("sheet_layout", json!({ "sheet": 0 })),
        ("sheet_resize", json!({ "sheet": 0, "axis": "columns", "start": 0, "count": 2, "size": 150 })),
        ("sheet_set_hidden", json!({ "sheet": 0, "axis": "rows", "start": 40, "count": 2, "hidden": true })),
        ("sheet_merge", json!({ "sheet": 0, "area": { "row": 70, "column": 0, "rows": 1, "columns": 2 }, "mode": "center" })),
        ("sheet_column_texts", json!({ "sheet": 0, "column": 0 })),
        ("sheet_range_values", json!({ "sheet": 0, "area": { "row": 0, "column": 0, "rows": 3, "columns": 1 }, "header": false, "column": 0 })),
        ("sheet_filter", json!({ "sheet": 0, "area": { "row": 60, "column": 0, "rows": 3, "columns": 1 }, "header": false, "column": 0 })),
        ("sheet_clear_format", json!({ "sheet": 0, "area": { "row": 1, "column": 1, "rows": 1, "columns": 1 } })),
        ("sheet_paste", json!({ "from_sheet": 0, "source": { "row": 0, "column": 0, "rows": 2, "columns": 1 }, "sheet": 0, "row": 0, "column": 2, "mode": "values", "transpose": true })),
        ("sheet_move", json!({ "sheet": 0, "source": { "row": 0, "column": 2, "rows": 1, "columns": 2 }, "row": 5, "column": 2 })),
        ("create_sheet_from_query", json!({ "name": "Q", "sql": "SELECT * FROM t" })),
        ("open_view", json!({ "spec": { "table": "t" } })),
        ("table_to_sheet", json!({ "view_id": 2, "name": "Copy" })),
        ("sheet_delete", json!({ "sheet": 2 })),
        ("save_view", json!({ "name": "Top", "spec": { "table": "t", "sort": [{ "column": "n", "desc": true }], "filters": [], "hidden": ["s"] }, "widths": { "n": 120.0 } })),
        ("saved_views", json!({ "table": "t" })),
        ("table_from_view", json!({ "view_id": 2, "name": "snap" })),
        ("sheet_set", json!({ "sheet": 1, "inputs": [{ "row": 0, "column": 0, "value": "h" }, { "row": 1, "column": 0, "value": "1" }] })),
        ("sheet_to_table", json!({ "sheet": 1, "name": "from_sheet" })),
        ("sheet_delete", json!({ "sheet": 1 })),
        ("rename_saved_view", json!({ "saved": "PLACEHOLDER", "name": "Top two" })),
        ("delete_saved_view", json!({ "saved": "PLACEHOLDER" })),
        ("close_view", json!({ "view_id": 1 })),
        ("history", json!({ "after": 0, "limit": 5 })),
        ("schema", json!({})),
        ("save_query", json!({ "name": "Big", "sql": "SELECT n FROM t WHERE n > $min", "params": [{ "name": "min", "type": "integer", "value": "1" }] })),
        ("query_versions", json!({ "saved": "QUERY" })),
        ("saved_queries", json!({})),
        ("rename_query", json!({ "saved": "QUERY", "name": "Bigger" })),
        ("query", json!({ "sql": "SELECT n FROM t WHERE n > $min", "params": [{ "name": "min", "type": "integer", "value": "1" }], "limit": 1 })),
        ("export_query", json!({ "sql": "SELECT * FROM t", "format": "csv" })),
        ("create_table_from_query", json!({ "name": "mat", "sql": "SELECT n FROM t WHERE n > $min", "params": [{ "name": "min", "type": "integer", "value": "1" }], "saved": "QUERY", "version": 1 })),
        ("materialized", json!({})),
        ("sql_change", json!({ "sql": "UPDATE t SET s = 'q' WHERE n = 1" })),
        ("refresh_materialized", json!({ "table": "mat" })),
        ("delete_query", json!({ "saved": "QUERY" })),
        ("analyze", json!({"spec":{"kind":"profile","table":"t","columns":["s"]}})),
        ("analyses", json!({})),
        ("save_analysis", json!({"saved":null,"name":"Profile","spec":{"kind":"profile","table":"t","columns":["s"]},"chart":false})),
        ("materialize_analysis", json!({"name":"analysis_copy","spec":{"kind":"profile","table":"t","columns":["s"]}})),
        ("save_dashboard", json!({"saved":null,"name":"Board","tiles":[{"analysis":"ANALYSIS","x":0,"y":0,"w":6,"h":6}],"slicers":[]})),
        ("dashboards", json!({})),
        ("dashboard", json!({"dashboard":"DASHBOARD"})),
        ("set_validations", json!({"table":"t","rules":[{"column":"s","rule":{"kind":"required"}}]})),
        ("validations", json!({"table":"t"})),
        ("preview_transform", json!({"table":"t","transform":{"kind":"trim","column":"s"}})),
        ("apply_transform", json!({"table":"t","transform":{"kind":"trim","column":"s"},"name":"t_trimmed"})),
        ("set_validations", json!({"table":"t","rules":[]})),
        ("inspect_formula", json!({"table":"t","column":"twice"})),
        ("xlsx_compatibility", json!({})),
        ("drill", json!({"spec":{"kind":"summary","table":"t","groups":["s"],"aggregate":"count","value":null,"pivot":null},"row":["x",1]})),
        ("delete_dashboard", json!({"dashboard":"DASHBOARD"})),
        ("delete_analysis", json!({"analysis":"ANALYSIS"})),
        ("presentation", json!({"table":"t"})),
        ("save_presentation", json!({"table":"t","presentation":{"hidden":[],"frozen":1,"formats":{},"rules":[]}})),
        ("replace_preview", json!({"spec":{"table":"t","column":"s","find":"q","replacement":"changed"}})),
        ("replace_apply", json!({"spec":{"table":"t","column":"s","find":"q","replacement":"changed"},"revision":"PREVIEW_REV"})),
        ("replace_undo", json!({"revision":"UNDO_REV"})),
        ("functions", json!({})),
        ("test_function", json!({ "definition": { "name": "PLUSONE", "kind": "formula", "arguments": ["value"], "source": "value+1", "documentation": "", "tests": [{"arguments":["2"],"expected":"3"}] } })),
        ("publish_function", json!({ "definition": { "name": "PLUSONE", "kind": "formula", "arguments": ["value"], "source": "value+1", "documentation": "", "tests": [] } })),
        ("pin_functions", json!({ "pins": { "PLUSONE": 1 } })),
        ("refresh_functions", json!({})),
        ("cancel", json!({ "request": 1 })),
        ("recalculate", json!({ "table": "t" })),
        ("cancel_recalculation", json!({ "table": "t" })),
        ("sheet_duplicate", json!({ "sheet": 0 })),
        ("set_sheet_rules", json!({ "sheet": 0, "rules": { "colors": [{ "area": { "row": 0, "column": 0, "rows": 5, "columns": 1 }, "condition": "greater", "value": "5", "color": "#FFC7CE" }], "validations": [{ "area": { "row": 30, "column": 30, "rows": 2, "columns": 1 }, "list": ["yes", "no"] }] } })),
        ("sheet_rules", json!({ "sheet": 0 })),
        ("sheet_reorder", json!({ "sheet": 1, "to": 0 })),
        ("sheet_hide", json!({ "sheet": 1 })),
        ("hidden_sheets", json!({})),
        ("sheet_unhide", json!({ "name": "HIDDEN" })),
    ]
}

// ------------------------------------------------------------- accounts mode

pub const ADMIN_EMAIL: &str = "admin@example.com";
pub const ADMIN_PASSWORD: &str = "correct horse battery";

impl TestServer {
    /// `--auth accounts` with the instance admin created at start.
    pub async fn start_accounts(data_dir: &Path, configure: impl FnOnce(&mut Config)) -> TestServer {
        Self::start_with(data_dir, |c| {
            c.auth.mode = alephgrid_server::auth::AuthMode::Accounts;
            c.auth.bootstrap = Some((ADMIN_EMAIL.into(), ADMIN_PASSWORD.into()));
            configure(c);
        })
        .await
    }

    pub fn origin(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// Sign in with a password.
    pub async fn login(&self, email: &str, password: &str) -> Result<Session, (u16, Value)> {
        let res = self
            .http
            .post(self.url("/api/auth/login"))
            .header("Origin", self.origin())
            .json(&json!({ "email": email, "password": password }))
            .send()
            .await
            .unwrap();
        let status = res.status().as_u16();
        let cookie = session_cookie(res.headers());
        let body: Value = res.json().await.unwrap_or(Value::Null);
        match cookie {
            Some(c) if status == 200 => Ok(Session { addr: self.addr, http: self.http.clone(), cookie: Some(c), bearer: None, me: body }),
            _ => Err((status, body)),
        }
    }

    pub async fn admin(&self) -> Session {
        self.login(ADMIN_EMAIL, ADMIN_PASSWORD).await.expect("admin signs in")
    }

    /// A client with no credentials at all.
    pub fn anonymous(&self) -> Session {
        Session { addr: self.addr, http: self.http.clone(), cookie: None, bearer: None, me: Value::Null }
    }

    /// A client sending `Authorization: Bearer <token>`.
    pub fn bearer(&self, token: &str) -> Session {
        Session { addr: self.addr, http: self.http.clone(), cookie: None, bearer: Some(token.into()), me: Value::Null }
    }

    /// Accept an invitation by creating a password user; returns the signed-in session.
    pub async fn accept(&self, code: &str, name: &str, password: &str) -> Session {
        let res = self
            .http
            .post(self.url("/api/invites/accept"))
            .header("Origin", self.origin())
            .json(&json!({ "code": code, "name": name, "password": password }))
            .send()
            .await
            .unwrap();
        let status = res.status().as_u16();
        let cookie = session_cookie(res.headers());
        let body: Value = res.json().await.unwrap_or(Value::Null);
        assert_eq!(status, 200, "accepting the invitation: {body}");
        Session { addr: self.addr, http: self.http.clone(), cookie, bearer: None, me: body }
    }
}

/// The session cookie's value from a response's `Set-Cookie`.
pub fn session_cookie(headers: &reqwest::header::HeaderMap) -> Option<String> {
    headers.get_all("set-cookie").iter().filter_map(|v| v.to_str().ok()).find_map(|v| {
        let first = v.split(';').next()?;
        let (k, val) = first.split_once('=')?;
        (k == "alephgrid_session" && !val.is_empty()).then(|| val.to_string())
    })
}

/// A client of one principal: its cookie or bearer token on every request, and this
/// server's `Origin` (as a browser page of the server would send).
#[derive(Clone)]
pub struct Session {
    pub addr: SocketAddr,
    pub http: reqwest::Client,
    pub cookie: Option<String>,
    pub bearer: Option<String>,
    /// What signing in returned (`/api/me`).
    pub me: Value,
}

impl Session {
    pub fn url(&self, path: &str) -> String {
        format!("http://{}{}", self.addr, path)
    }

    pub fn user_id(&self) -> String {
        self.me["user"]["id"].as_str().unwrap_or_default().to_string()
    }

    fn credentials(&self, mut req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if let Some(c) = &self.cookie {
            req = req.header("Cookie", format!("alephgrid_session={c}"));
        }
        if let Some(b) = &self.bearer {
            req = req.header("Authorization", format!("Bearer {b}"));
        }
        req.header("Origin", format!("http://{}", self.addr))
    }

    pub async fn send(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> (u16, Value) {
        let mut req = self.credentials(self.http.request(method, self.url(path)));
        if let Some(b) = body {
            req = req.json(&b);
        }
        let res = req.send().await.unwrap();
        let status = res.status().as_u16();
        let text = res.text().await.unwrap();
        (status, serde_json::from_str(&text).unwrap_or(Value::String(text)))
    }

    pub async fn send_bytes(&self, method: reqwest::Method, path: &str, bytes: Vec<u8>) -> (u16, Value) {
        let res = self.credentials(self.http.request(method, self.url(path))).body(bytes).send().await.unwrap();
        let status = res.status().as_u16();
        let text = res.text().await.unwrap();
        (status, serde_json::from_str(&text).unwrap_or(Value::String(text)))
    }

    pub async fn get(&self, path: &str) -> (u16, Value) {
        self.send(reqwest::Method::GET, path, None).await
    }

    pub async fn post(&self, path: &str, body: Value) -> (u16, Value) {
        self.send(reqwest::Method::POST, path, Some(body)).await
    }

    /// `body` of a 2xx reply, or a panic naming the request.
    pub async fn ok(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> Value {
        let (status, v) = self.send(method.clone(), path, body).await;
        assert!((200..300).contains(&status), "{method} {path}: {status} {v}");
        v
    }

    pub async fn ws(&self, workbook: &str) -> Result<Ws, String> {
        let mut headers = vec![("Origin".to_string(), format!("http://{}", self.addr))];
        if let Some(c) = &self.cookie {
            headers.push(("Cookie".into(), format!("alephgrid_session={c}")));
        }
        if let Some(b) = &self.bearer {
            headers.push(("Authorization".into(), format!("Bearer {b}")));
        }
        Ws::try_connect_with(self.addr, workbook, &headers).await
    }

    /// Invite `email` to `org`, optionally with a role in `space`; returns the code.
    pub async fn invite(&self, org: &str, email: &str, space: Option<&str>, role: Option<&str>) -> String {
        let v = self
            .ok(reqwest::Method::POST, &format!("/api/orgs/{org}/invites"), Some(json!({ "email": email, "space": space, "space_role": role })))
            .await;
        v["code"].as_str().unwrap().to_string()
    }
}

impl Ws {
    /// Connect with extra request headers (credentials, `Origin`).
    pub async fn try_connect_with(addr: SocketAddr, workbook: &str, headers: &[(String, String)]) -> Result<Ws, String> {
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;
        let mut req = format!("ws://{addr}/ws/{workbook}").into_client_request().map_err(|e| e.to_string())?;
        for (k, v) in headers {
            req.headers_mut().insert(
                tokio_tungstenite::tungstenite::http::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                tokio_tungstenite::tungstenite::http::HeaderValue::from_str(v).unwrap(),
            );
        }
        let (stream, _) = tokio_tungstenite::connect_async(req).await.map_err(|e| match e {
            tokio_tungstenite::tungstenite::Error::Http(r) => format!("HTTP {}", r.status().as_u16()),
            other => other.to_string(),
        })?;
        let mut ws = Ws { stream, next_id: 1, session: 0, events: vec![], replies: Default::default(), hello: Value::Null };
        let hello = ws.next_message().await.ok_or("closed before hello")?;
        assert_eq!(hello["event"], "hello");
        ws.session = hello["session"].as_u64().unwrap();
        ws.hello = hello;
        Ok(ws)
    }

    /// The error code of a request's reply (`None` if it succeeded or the connection closed).
    pub async fn code(&mut self, op: &str, params: Value) -> Option<String> {
        let id = self.send(op, params).await;
        let r = self.reply(id).await?;
        (r["ok"] != true).then(|| r["code"].as_str().unwrap_or("").to_string())
    }
}
