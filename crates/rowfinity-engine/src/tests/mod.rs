//! Engine tests. Layers (see docs/TEST_STRATEGY.md):
//! `basic` smoke coverage, `integrity` table mutations, `sheets` IronCalc integration,
//! `precision` exact values, `formulas` dependency-aware schema changes, `sql` query
//! expansion, `props` property/state-machine tests against a reference model.

mod basic;
mod formulas;
mod integrity;
mod precision;
mod props;
mod sheets;
mod sql;

use crate::*;
use serde_json::Value;

/// A file-backed workbook in its own temporary directory.
pub(crate) struct TempWb {
    pub dir: tempfile::TempDir,
    pub wb: Workbook,
}

impl TempWb {
    pub fn new() -> TempWb {
        let dir = tempfile::tempdir().unwrap();
        let wb = Workbook::open(&dir.path().join("wb.duckdb")).unwrap();
        TempWb { dir, wb }
    }

    pub fn path(&self) -> std::path::PathBuf {
        self.dir.path().join("wb.duckdb")
    }

    /// Close and reopen the database file, as a server restart would.
    pub fn reopen(&mut self) {
        let path = self.path();
        // Replace the workbook first so the old connection is dropped before reopening.
        self.wb = Workbook::open_in_memory().unwrap();
        self.wb = Workbook::open(&path).unwrap();
    }
}

pub(crate) fn spec(table: &str) -> ViewSpec {
    ViewSpec { table: table.into(), ..Default::default() }
}

pub(crate) fn edit(row: u64, column: &str, value: &str) -> CellEdit {
    CellEdit { row, column: column.into(), value: Some(value.into()) }
}

/// All rows of a view as JSON values, read in fetch-sized windows.
pub(crate) fn all_rows(wb: &Workbook, view: u64) -> Vec<Vec<Value>> {
    let n = wb.view_info(view).unwrap().row_count;
    let mut out = Vec::new();
    let mut start = 0;
    while start < n {
        let block = wb.fetch(view, start, 5_000).unwrap();
        assert_eq!(block.row_start, start);
        assert!(!block.rows.is_empty(), "fetch returned no rows at {start} of {n}");
        start += block.rows.len() as u64;
        out.extend(block.rows);
    }
    out
}

/// Run a scalar query through the public preview API and return the single value.
pub(crate) fn scalar(wb: &Workbook, sql: &str) -> Value {
    let p = wb.preview_query(sql, 2).unwrap();
    assert_eq!(p.rows.len(), 1, "{sql} returned {} rows", p.rows.len());
    p.rows[0][0].clone()
}

pub(crate) fn sheet_input(row: u32, column: u32, value: &str) -> SheetInput {
    SheetInput { row, column, value: value.into() }
}

pub(crate) fn cell_text(wb: &Workbook, sheet: u32, row: u32, column: u32) -> Option<String> {
    wb.sheets().fetch(sheet, row, 1, column + 1).unwrap().rows[0].get(column as usize).cloned().flatten().map(|c| c.0)
}
