//! Phase 8 (8.T01, 8.T02): Python functions called from sheet cells and table columns,
//! computed in sandboxed workers in batches outside the workbook's lock.
//!
//! Expected values are written out from docs/PYTHON_FUNCTIONS.md; they are not computed by
//! the code under test. Needs a CPython 3.9+ (`ALEPHGRID_TEST_PYTHON`, else the first of a
//! few usual places); the tests fail, not skip, without one.

mod common;

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use common::*;
use serde_json::{json, Value};

fn python() -> PathBuf {
    if let Ok(p) = std::env::var("ALEPHGRID_TEST_PYTHON") {
        return PathBuf::from(p);
    }
    for p in ["/opt/homebrew/bin/python3.12", "/opt/homebrew/bin/python3", "/usr/local/bin/python3", "/usr/bin/python3"] {
        if Path::new(p).exists() {
            return PathBuf::from(p);
        }
    }
    panic!("no Python interpreter found: set ALEPHGRID_TEST_PYTHON");
}

/// A runtime without packages, made once per test process.
fn runtime() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("alephgrid-pyrt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        alephgrid_server::python::create::create(&alephgrid_server::python::create::Options { python: python(), wheels: None, requirements: None, dir: dir.clone() })
            .expect("creating the test runtime");
        dir
    })
}

async fn server(dir: &Path, configure: impl FnOnce(&mut alephgrid_server::state::Config)) -> TestServer {
    TestServer::start_with(dir, |c| {
        c.python_runtime = Some(runtime().to_path_buf());
        c.python_limits.timeout_ms = 5_000;
        c.python_limits.cpu_ms = 5_000;
        configure(c);
    })
    .await
}

fn function(name: &str, args: &[&str], returns: &str, source: &str, tests: Value) -> Value {
    json!({ "definition": {
        "name": name, "kind": "python", "arguments": args, "source": source, "returns": returns,
        "documentation": "", "tests": tests,
    } })
}

/// Formatted text of a sheet cell (row, column from 0).
async fn cell(ws: &mut Ws, row: u32, column: u32) -> String {
    let block = ws.ok("sheet_fetch", json!({ "sheet": 0, "row_start": row, "row_count": 1, "column_count": column + 1 })).await;
    block["rows"][0][column as usize][0].as_str().unwrap_or("").to_string()
}

/// Wait until the cell shows something other than "calculating".
async fn computed(ws: &mut Ws, row: u32, column: u32) -> String {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let v = cell(ws, row, column).await;
        if v != "#CALC!" {
            return v;
        }
        // `#CALC!` is also a final answer (a limit reached); pending cells say "Calculating".
        let detail = ws.ok("sheet_cell_detail", json!({ "sheet": 0, "row": row, "column": column })).await;
        if !detail.to_string().contains("Calculating") {
            // Read again: the result may have arrived between the two reads.
            return cell(ws, row, column).await;
        }
        assert!(Instant::now() < deadline, "cell ({row}, {column}) still calculating");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_python_function_runs_from_sheet_cells() {
    let dir = tempfile::tempdir().unwrap();
    let server = server(dir.path(), |_| {}).await;
    let wb = server.create_workbook("py").await;
    let mut ws = server.ws(&wb).await;
    let source = "def normalize_sku(sku):\n    return sku.strip().upper().replace('-', '')\n";
    let published = ws.ok("publish_function", function("NORMALIZE_SKU", &["sku"], "text", source, json!([{ "arguments": [" ab-12 "], "expected": "AB12" }]))).await;
    assert_eq!(published["versions"][0]["runtime"]["kind"], "python", "{published}");
    ws.ok("sheet_create", json!({ "name": "Plan" })).await;
    ws.ok("sheet_set", json!({ "sheet": 0, "inputs": [
        { "row": 0, "column": 0, "value": " ab-12 " },
        { "row": 0, "column": 1, "value": "=NORMALIZE_SKU(A1)" },
    ] })).await;
    assert_eq!(computed(&mut ws, 0, 1).await, "AB12");
    ws.close().await;
    server.stop().await;
}

async fn publish(ws: &mut Ws, name: &str, args: &[&str], returns: &str, source: &str) -> Value {
    ws.ok("publish_function", function(name, args, returns, source, json!([]))).await
}

async fn set(ws: &mut Ws, inputs: Value) {
    ws.ok("sheet_set", json!({ "sheet": 0, "inputs": inputs })).await;
}

fn started(server: &TestServer) -> u64 {
    server.state.python.as_ref().unwrap().started.load(std::sync::atomic::Ordering::Relaxed)
}

// ------------------------------------------------------------- 8.01, 8.02

#[tokio::test(flavor = "multi_thread")]
async fn values_ranges_and_errors_follow_the_contract() {
    let dir = tempfile::tempdir().unwrap();
    let server = server(dir.path(), |_| {}).await;
    let wb = server.create_workbook("values").await;
    let mut ws = server.ws(&wb).await;
    publish(&mut ws, "KIND", &["x"], "text", "def kind(x):\n    return type(x).__name__\n").await;
    publish(&mut ws, "TOTAL", &["r"], "number", "def total(r):\n    return sum(v for row in r for v in row if isinstance(v, float))\n").await;
    publish(&mut ws, "FAILS", &["x"], "text", "def fails(x):\n    raise ValueError('bad sku ' + str(x))\n").await;
    publish(&mut ws, "WRONG", &["x"], "number", "def wrong(x):\n    return [x]\n").await;
    publish(&mut ws, "NOTNUM", &["x"], "number", "def notnum(x):\n    return float('nan')\n").await;
    publish(&mut ws, "HUGE", &["x"], "number", "def huge(x):\n    return 2 ** 60\n").await;
    publish(&mut ws, "LIMIT53", &["x"], "number", "def limit53(x):\n    return 2 ** 53\n").await;
    publish(&mut ws, "NOTHING", &["x"], "text", "def nothing(x):\n    return None\n").await;
    ws.ok("sheet_create", json!({ "name": "Plan" })).await;
    set(&mut ws, json!([
        { "row": 0, "column": 0, "value": "1.5" }, { "row": 1, "column": 0, "value": "2" }, { "row": 2, "column": 0, "value": "text" },
        { "row": 0, "column": 1, "value": "=KIND(A1)" }, { "row": 1, "column": 1, "value": "=KIND(A3)" }, { "row": 2, "column": 1, "value": "=KIND(TRUE)" },
        { "row": 3, "column": 1, "value": "=KIND(A9)" }, { "row": 4, "column": 1, "value": "=KIND(A1:A3)" }, { "row": 5, "column": 1, "value": "=TOTAL(A1:A3)" },
        { "row": 6, "column": 1, "value": "=FAILS(A1)" }, { "row": 7, "column": 1, "value": "=WRONG(1)" }, { "row": 8, "column": 1, "value": "=NOTNUM(1)" },
        { "row": 9, "column": 1, "value": "=HUGE(1)" }, { "row": 10, "column": 1, "value": "=LIMIT53(1)=9007199254740992" }, { "row": 11, "column": 1, "value": "=KIND(1/0)" },
        { "row": 12, "column": 1, "value": "=NOTHING(1)" }, { "row": 13, "column": 1, "value": "=LEN(KIND(A1))+1" },
    ])).await;
    let want = [
        "float", "str", "bool", "NoneType", "list", "3.5", "#VALUE!", "#VALUE!", "#NUM!", "#NUM!", "TRUE", "#DIV/0!", "", "6",
    ];
    for (row, w) in want.iter().enumerate() {
        assert_eq!(computed(&mut ws, row as u32, 1).await, *w, "row {}", row + 1);
    }
    // The exception's type and message are the cell's.
    let detail = ws.ok("sheet_cell_detail", json!({ "sheet": 0, "row": 6, "column": 1 })).await;
    assert!(detail.to_string().contains("ValueError: bad sku 1.5"), "{detail}");
    let detail = ws.ok("sheet_cell_detail", json!({ "sheet": 0, "row": 7, "column": 1 })).await;
    assert!(detail.to_string().contains("returned a list, expected a number"), "{detail}");
    // A test that fails stops publication.
    let r = ws.call("publish_function", function("TWICE", &["x"], "number", "def twice(x):\n    return x * 3\n", json!([{ "arguments": ["2"], "expected": "4" }]))).await;
    assert!(r.unwrap_err().contains("tests failed"));
    ws.close().await;
    server.stop().await;
}

// ------------------------------------------------------------- 8.06, 8.08

#[tokio::test(flavor = "multi_thread")]
async fn table_columns_are_computed_in_batches_and_kept_across_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let server = server(dir.path(), |_| {}).await;
    let wb = server.create_workbook("table").await;
    let mut ws = server.ws(&wb).await;
    publish(&mut ws, "NORMALIZE_SKU", &["sku"], "text", "def normalize_sku(sku):\n    return sku.strip().upper().replace('-', '')\n").await;
    ws.ok("create_table_from_query", json!({ "name": "t", "sql": "SELECT i AS n, ' ab-' || (i % 50) || ' ' AS sku FROM range(20000) r(i)" })).await;
    let before = started(&server);
    ws.ok("add_formula_column", json!({ "table": "t", "name": "clean", "formula": "NORMALIZE_SKU([sku])" })).await;
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let r = ws.ok("query", json!({ "sql": "SELECT count(*) FILTER (WHERE clean IS NULL), count(DISTINCT clean), min(clean) FROM t" })).await["rows"].clone();
        if r[0][0] == json!(0) {
            assert_eq!(r[0][1], json!(50));
            assert_eq!(r[0][2], json!("AB0"));
            break;
        }
        assert!(Instant::now() < deadline, "{r}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(started(&server) - before, 1, "50 distinct values: one batch, not one call per row");
    // Another arrangement of the same values computes nothing new.
    let r = ws.ok("query", json!({ "sql": "SELECT clean FROM t WHERE n = 12345" })).await;
    assert_eq!(r["rows"], json!([["AB45"]]));
    ws.close().await;
    // Reopened: the results are there at once, without a worker.
    let (code, _) = server.send(reqwest::Method::POST, &format!("/api/workbooks/{wb}/runtime"), Some(json!({ "action": "stop" }))).await;
    assert_eq!(code, 200);
    let before = started(&server);
    let mut ws = server.ws(&wb).await;
    let r = ws.ok("query", json!({ "sql": "SELECT count(*) FILTER (WHERE clean IS NULL) FROM t" })).await;
    assert_eq!(r["rows"], json!([[0]]));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(started(&server), before, "nothing computed again");
    // Only a whole-formula call is accepted for a column.
    let e = ws.call("add_formula_column", json!({ "table": "t", "name": "bad", "formula": "NORMALIZE_SKU([sku]) || 'x'" })).await.unwrap_err();
    assert!(e.contains("whole formula"), "{e}");
    ws.close().await;
    server.stop().await;
}

// ------------------------------------------------------------- 8.03, 8.04

#[tokio::test(flavor = "multi_thread")]
async fn workers_cannot_reach_files_network_processes_or_credentials() {
    let dir = tempfile::tempdir().unwrap();
    std::env::set_var("ALEPHGRID_TEST_SECRET", "s3cret");
    let server = server(dir.path(), |_| {}).await;
    let wb = server.create_workbook("escape").await;
    let mut ws = server.ws(&wb).await;
    let secret_file = dir.path().join("secret.txt");
    std::fs::write(&secret_file, "top secret").unwrap();
    let source = format!(
        "import os, socket, subprocess\ndef escape(what):\n    tries = {{\n        'read': lambda: open({:?}).read(),\n        'passwd': lambda: open('/etc/passwd').read(),\n        'list': lambda: os.listdir({:?}),\n        'write': lambda: open('/tmp/alephgrid-escape.txt', 'w').write('x'),\n        'connect': lambda: socket.create_connection(('127.0.0.1', {}), timeout=2),\n        'dns': lambda: socket.getaddrinfo('example.com', 80),\n        'spawn': lambda: subprocess.run(['/bin/echo', 'hi'], check=True),\n        'fork': lambda: os.fork(),\n    }}\n    if what == 'env':\n        return ','.join(sorted(k for k in os.environ if not k.startswith('__CF')))\n    try:\n        tries[what]()\n        return 'ALLOWED'\n    except BaseException as e:\n        return 'denied ' + type(e).__name__\n",
        secret_file.to_string_lossy(),
        dir.path().to_string_lossy(),
        server.addr.port(),
    );
    publish(&mut ws, "ESCAPE", &["what"], "text", &source).await;
    ws.ok("sheet_create", json!({ "name": "Plan" })).await;
    let tries = ["read", "passwd", "list", "write", "connect", "dns", "spawn", "fork", "env"];
    let inputs: Vec<Value> = tries.iter().enumerate().map(|(i, t)| json!({ "row": i, "column": 0, "value": format!("=ESCAPE(\"{t}\")") })).collect();
    set(&mut ws, json!(inputs)).await;
    for (i, t) in tries.iter().enumerate() {
        let v = computed(&mut ws, i as u32, 0).await;
        if *t == "env" {
            assert!(!v.contains("ALEPHGRID_TEST_SECRET") && !v.contains("HOME") && !v.contains("PATH"), "environment inherited: {v}");
        } else {
            assert!(v.starts_with("denied"), "{t}: {v}");
        }
    }
    assert!(!Path::new("/tmp/alephgrid-escape.txt").exists());
    ws.close().await;
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn limits_stop_runaway_functions_without_hanging_other_workbooks() {
    let dir = tempfile::tempdir().unwrap();
    let server = server(dir.path(), |c| {
        c.python_limits.timeout_ms = 1_500;
        c.python_limits.cpu_ms = 1_500;
        c.python_limits.memory_bytes = 200 << 20;
        c.python_limits.output_bytes = 4096;
    })
    .await;
    let wb = server.create_workbook("limits").await;
    let other = server.create_workbook("other").await;
    let mut ws = server.ws(&wb).await;
    publish(&mut ws, "SPIN", &["x"], "number", "def spin(x):\n    while True:\n        pass\n").await;
    publish(&mut ws, "HOG", &["x"], "number", "def hog(x):\n    b = []\n    while True:\n        b.append(bytearray(16 << 20))\n").await;
    publish(&mut ws, "LOUD", &["x"], "text", "def loud(x):\n    return 'x' * 8000\n").await;
    publish(&mut ws, "CRASH", &["x"], "number", "import os\ndef crash(x):\n    os._exit(3)\n").await;
    ws.ok("sheet_create", json!({ "name": "Plan" })).await;
    let t0 = Instant::now();
    set(&mut ws, json!([
        { "row": 0, "column": 0, "value": "=SPIN(1)" }, { "row": 1, "column": 0, "value": "=HOG(1)" },
        { "row": 2, "column": 0, "value": "=LOUD(1)" }, { "row": 3, "column": 0, "value": "=CRASH(1)" },
    ])).await;
    // Meanwhile another workbook answers at once.
    let mut o = server.ws(&other).await;
    let q = Instant::now();
    o.ok("query", json!({ "sql": "SELECT 1" })).await;
    assert!(q.elapsed() < Duration::from_secs(1), "{:?}", q.elapsed());
    o.close().await;
    let reasons = ["more than 1.5 s", "MiB of memory", "bytes of results", "worker stopped"];
    for row in 0..reasons.len() {
        assert_eq!(computed(&mut ws, row as u32, 0).await, "#CALC!", "row {row}");
    }
    eprintln!("limits reached in {:?}", t0.elapsed());
    for (row, why) in reasons.iter().enumerate() {
        let detail = ws.ok("sheet_cell_detail", json!({ "sheet": 0, "row": row, "column": 0 })).await;
        assert!(detail.to_string().contains(why), "row {row}: {detail}");
    }
    ws.close().await;
    server.stop().await;
}

// ------------------------------------------------------------- 8.07, 8.08

#[tokio::test(flavor = "multi_thread")]
async fn workers_run_outside_the_lock_and_stale_versions_are_discarded() {
    let dir = tempfile::tempdir().unwrap();
    let server = server(dir.path(), |_| {}).await;
    let wb = server.create_workbook("lock").await;
    let mut ws = server.ws(&wb).await;
    publish(&mut ws, "SLOW", &["x"], "number", "import time\ndef slow(x):\n    time.sleep(1.5)\n    return x * 2\n").await;
    ws.ok("sheet_create", json!({ "name": "Plan" })).await;
    set(&mut ws, json!([{ "row": 0, "column": 0, "value": "=SLOW(10)" }])).await;
    // While the worker sleeps, the workbook takes changes and reads at once.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let t = Instant::now();
    set(&mut ws, json!([{ "row": 5, "column": 5, "value": "edited meanwhile" }])).await;
    assert_eq!(cell(&mut ws, 5, 5).await, "edited meanwhile");
    assert!(t.elapsed() < Duration::from_millis(700), "a change waited for the worker: {:?}", t.elapsed());
    // A new version pinned while version 1 computes: version 1's result is not stored.
    let v2 = ws.ok("publish_function", function("SLOW", &["x"], "number", "import time\ndef slow(x):\n    time.sleep(0.2)\n    return x * 3\n", json!([]))).await;
    assert_eq!(v2["versions"].as_array().unwrap().len(), 2);
    ws.ok("pin_functions", json!({ "pins": { "SLOW": 2 } })).await;
    assert_eq!(computed(&mut ws, 0, 0).await, "30", "only the pinned version's result shows");
    // Version 1's late result was discarded, not kept for it either.
    tokio::time::sleep(Duration::from_millis(1600)).await;
    let r = ws.ok("query", json!({ "sql": "SELECT 1" })).await;
    assert_eq!(r["rows"], json!([[1]]));
    ws.ok("pin_functions", json!({ "pins": { "SLOW": 1 } })).await;
    assert_eq!(computed(&mut ws, 0, 0).await, "20", "version 1 computed afresh when pinned again");
    ws.close().await;
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn volatile_functions_change_only_on_refresh() {
    let dir = tempfile::tempdir().unwrap();
    let server = server(dir.path(), |_| {}).await;
    let wb = server.create_workbook("volatile").await;
    let mut ws = server.ws(&wb).await;
    let mut f = function("STAMP", &["x"], "number", "import time\ndef stamp(x):\n    return time.time_ns() % 1000000007\n", json!([]));
    f["definition"]["volatile"] = json!(true);
    ws.ok("publish_function", f).await;
    ws.ok("sheet_create", json!({ "name": "Plan" })).await;
    set(&mut ws, json!([{ "row": 0, "column": 0, "value": "=STAMP(1)" }])).await;
    let first = computed(&mut ws, 0, 0).await;
    // Other changes recalculate the sheet; the stamp stays.
    set(&mut ws, json!([{ "row": 3, "column": 3, "value": "7" }])).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(cell(&mut ws, 0, 0).await, first);
    // Refresh computes it again.
    ws.ok("refresh_functions", json!({})).await;
    let second = computed(&mut ws, 0, 0).await;
    assert_ne!(second, first);
    ws.close().await;
    server.stop().await;
}

// ------------------------------------------------------------------ 8.10

#[tokio::test(flavor = "multi_thread")]
async fn revocation_duplicates_and_a_missing_runtime_are_handled() {
    let dir = tempfile::tempdir().unwrap();
    let server = server(dir.path(), |_| {}).await;
    let wb = server.create_workbook("policy").await;
    let space = server.get(&format!("/api/workbooks/{wb}")).await.1["workbook"]["space"].as_str().unwrap().to_string();
    let mut ws = server.ws(&wb).await;
    publish(&mut ws, "SLOW", &["x"], "number", "import time\ndef slow(x):\n    time.sleep(1)\n    return x + 1\n").await;
    publish(&mut ws, "PLUS", &["x"], "number", "def plus(x):\n    return x + 100\n").await;
    ws.ok("sheet_create", json!({ "name": "Plan" })).await;
    set(&mut ws, json!([{ "row": 0, "column": 0, "value": "=PLUS(1)" }])).await;
    assert_eq!(computed(&mut ws, 0, 0).await, "101");
    // Turned off for the space while a call computes: its result is not stored, and cells
    // say why.
    set(&mut ws, json!([{ "row": 1, "column": 0, "value": "=SLOW(1)" }])).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let (code, body) = server.send(reqwest::Method::PATCH, &format!("/api/spaces/{space}"), Some(json!({ "python": false }))).await;
    assert_eq!(code, 200, "{body}");
    assert_eq!(body["python"], false);
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(cell(&mut ws, 1, 0).await, "#NAME?");
    let detail = ws.ok("sheet_cell_detail", json!({ "sheet": 0, "row": 1, "column": 0 })).await;
    assert!(detail.to_string().contains("turned off for this space"), "{detail}");
    assert_eq!(cell(&mut ws, 0, 0).await, "101", "results computed before stay");
    let h = server.state.controller.handle(&wb).unwrap();
    assert!(h.functions.get("SLOW", 1, &alephgrid_core::external::sheet_key(&[alephgrid_core::external::WorkerValue::Number(1.0)])).is_none(), "the late result was not stored");
    // Back on: it computes.
    server.send(reqwest::Method::PATCH, &format!("/api/spaces/{space}"), Some(json!({ "python": true }))).await;
    assert_eq!(computed(&mut ws, 1, 0).await, "2");
    // The same results delivered twice are stored once; the first stands.
    let key = alephgrid_core::external::sheet_key(&[alephgrid_core::external::WorkerValue::Number(5.0)]);
    let first = h.with_engine({ let k = key.clone(); move |wb| wb.store_function_results("PLUS", 1, &[(k, alephgrid_core::external::CallResult::Number(105.0))]) }).unwrap();
    let again = h.with_engine({ let k = key.clone(); move |wb| wb.store_function_results("PLUS", 1, &[(k, alephgrid_core::external::CallResult::Number(-1.0))]) }).unwrap();
    assert_eq!((first.stored, again.stored), (1, 0));
    assert_eq!(h.functions.get("PLUS", 1, &key), Some(alephgrid_core::external::CallResult::Number(105.0)));
    drop(h);
    ws.close().await;
    server.stop().await;

    // The server without a Python runtime: cached results show, new calls say why.
    let server = TestServer::start(dir.path()).await;
    let mut ws = server.ws(&wb).await;
    assert_eq!(cell(&mut ws, 0, 0).await, "101");
    set(&mut ws, json!([{ "row": 2, "column": 0, "value": "=PLUS(2)" }])).await;
    assert_eq!(cell(&mut ws, 2, 0).await, "#NAME?");
    let detail = ws.ok("sheet_cell_detail", json!({ "sheet": 0, "row": 2, "column": 0 })).await;
    assert!(detail.to_string().contains("no Python runtime"), "{detail}");
    ws.close().await;
    server.stop().await;
}

// ------------------------------------------------------------------ 8.05

/// A wheel of one pure-Python module, made by hand (no build tools, no network).
fn wheel(dir: &Path) -> (PathBuf, String) {
    use base64::Engine;
    use sha2::{Digest, Sha256};
    use std::io::Write;
    let files = [
        ("skutools/__init__.py", "def clean(s):\n    return s.strip().upper()\n".to_string()),
        ("skutools-1.0.dist-info/METADATA", "Metadata-Version: 2.1\nName: skutools\nVersion: 1.0\n".to_string()),
        ("skutools-1.0.dist-info/WHEEL", "Wheel-Version: 1.0\nGenerator: test\nRoot-Is-Purelib: true\nTag: py3-none-any\n".to_string()),
    ];
    let path = dir.join("skutools-1.0-py3-none-any.whl");
    let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
    let mut record = String::new();
    for (name, body) in &files {
        zip.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
        zip.write_all(body.as_bytes()).unwrap();
        let h = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(body.as_bytes()));
        record.push_str(&format!("{name},sha256={h},{}\n", body.len()));
    }
    record.push_str("skutools-1.0.dist-info/RECORD,,\n");
    zip.start_file("skutools-1.0.dist-info/RECORD", zip::write::SimpleFileOptions::default()).unwrap();
    zip.write_all(record.as_bytes()).unwrap();
    zip.finish().unwrap();
    let sha = alephgrid_server::runtime::hex(&Sha256::digest(std::fs::read(&path).unwrap()));
    (path, sha)
}

#[tokio::test(flavor = "multi_thread")]
async fn runtimes_install_pinned_packages_offline() {
    let dir = tempfile::tempdir().unwrap();
    let wheels = dir.path().join("wheels");
    std::fs::create_dir_all(&wheels).unwrap();
    let (_, sha) = wheel(&wheels);
    let req = dir.path().join("requirements.txt");
    std::fs::write(&req, format!("skutools==1.0 --hash=sha256:{sha}\n")).unwrap();
    let rt = dir.path().join("rt");
    let m = alephgrid_server::python::create::create(&alephgrid_server::python::create::Options { python: python(), wheels: Some(wheels.clone()), requirements: Some(req.clone()), dir: rt.clone() }).unwrap();
    assert_eq!(m.packages.len(), 1);
    assert_eq!((m.packages[0].name.as_str(), m.packages[0].version.as_str(), m.packages[0].sha256.as_str()), ("skutools", "1.0", sha.as_str()));
    let plain = alephgrid_server::python::Manifest::load(runtime()).unwrap();
    assert_ne!(m.id(), plain.id(), "packages are part of the runtime's identity");
    // A wrong hash is refused, and nothing is fetched from anywhere.
    std::fs::write(&req, "skutools==1.0 --hash=sha256:0000000000000000000000000000000000000000000000000000000000000000\n").unwrap();
    let e = alephgrid_server::python::create::create(&alephgrid_server::python::create::Options { python: python(), wheels: Some(wheels), requirements: Some(req), dir: dir.path().join("rt2") });
    assert!(e.is_err());
    // The package is importable from functions run in this runtime.
    let data = dir.path().join("data");
    let server = TestServer::start_with(&data, |c| c.python_runtime = Some(rt.clone())).await;
    let wb = server.create_workbook("pkg").await;
    let mut ws = server.ws(&wb).await;
    let f = publish(&mut ws, "SKUCLEAN", &["s"], "text", "import skutools\ndef skuclean(s):\n    return skutools.clean(s)\n").await;
    assert_eq!(f["versions"][0]["runtime"]["packages"], json!(["skutools==1.0"]));
    ws.ok("sheet_create", json!({ "name": "Plan" })).await;
    set(&mut ws, json!([{ "row": 0, "column": 0, "value": "=SKUCLEAN(\" ab \")" }])).await;
    assert_eq!(computed(&mut ws, 0, 0).await, "AB");
    ws.close().await;
    server.stop().await;
}
