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
