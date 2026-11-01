//! S.09: charts placed on a sheet. Each chart reads a cell range of its sheet (first column
//! the categories, first row the series names) and sits over the grid at an anchor cell
//! with a size in grid pixels. Charts are kept per sheet (by its stable id) in the workbook;
//! their values are always read from the cells, so they follow every edit. XLSX files do not
//! carry them (the compatibility list says so).

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::sheets::CellArea;
use crate::workbook::Workbook;

/// Kinds a sheet chart can be drawn as.
pub const CHART_KINDS: &[&str] = &["column", "bar", "line", "area", "scatter", "pie"];
/// Charts per sheet.
pub const MAX_SHEET_CHARTS: usize = 50;
/// Rows of data a chart reads (categories).
pub const MAX_CHART_ROWS: u32 = 1_000;
/// Columns of data a chart reads (one category column plus series).
pub const MAX_CHART_COLUMNS: u32 = 21;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct SheetChart {
    /// Chosen by the client; unique within the sheet.
    pub chart: String,
    pub kind: String,
    #[serde(default)]
    pub title: String,
    /// The data: first column categories, first row series names.
    pub data: CellArea,
    /// Top-left cell the chart is placed at, and its size in grid pixels.
    pub row: u32,
    pub column: u32,
    pub width: u32,
    pub height: u32,
}

impl SheetChart {
    fn check(&self) -> Result<()> {
        if self.chart.is_empty() || self.chart.len() > 64 {
            bail!("a chart needs an id of 1 to 64 characters");
        }
        if !CHART_KINDS.contains(&self.kind.as_str()) {
            bail!("unknown chart kind {:?}", self.kind);
        }
        if self.title.chars().count() > 200 {
            bail!("a chart title takes at most 200 characters");
        }
        if self.data.rows < 2 || self.data.columns < 2 {
            bail!("a chart reads at least two rows and two columns (categories and a series)");
        }
        if self.data.rows > MAX_CHART_ROWS + 1 || self.data.columns > MAX_CHART_COLUMNS {
            bail!("a chart reads at most {MAX_CHART_ROWS} rows and {} series", MAX_CHART_COLUMNS - 1);
        }
        if self.data.row as u64 + self.data.rows as u64 > crate::sheets::MAX_ROWS as u64
            || self.data.column as u64 + self.data.columns as u64 > crate::sheets::MAX_COLUMNS as u64
            || self.row >= crate::sheets::MAX_ROWS
            || self.column >= crate::sheets::MAX_COLUMNS
        {
            bail!("the chart is outside the sheet");
        }
        if !(160..=2000).contains(&self.width) || !(120..=2000).contains(&self.height) {
            bail!("a chart is 160 to 2000 pixels wide and 120 to 2000 high");
        }
        Ok(())
    }
}

fn key(sheet_id: u32) -> String {
    format!("sheet_charts:{sheet_id}")
}

impl Workbook {
    /// The charts placed on a sheet.
    pub fn sheet_charts(&self, sheet: u32) -> Result<Vec<SheetChart>> {
        let id = { let s = self.sheets(); s.info(sheet)?; s.sheet_id(sheet)? };
        Ok(crate::storage::meta_get(&self.conn, &key(id))?.and_then(|v| serde_json::from_str(&v).ok()).unwrap_or_default())
    }

    /// Replace a sheet's charts (at most [`MAX_SHEET_CHARTS`]).
    pub fn set_sheet_charts(&mut self, sheet: u32, charts: Vec<SheetChart>) -> Result<()> {
        if charts.len() > MAX_SHEET_CHARTS {
            bail!("a sheet holds at most {MAX_SHEET_CHARTS} charts");
        }
        let mut ids = std::collections::HashSet::new();
        for c in &charts {
            c.check()?;
            if !ids.insert(c.chart.as_str()) {
                bail!("two charts share the id {:?}", c.chart);
            }
        }
        let id = { let s = self.sheets(); s.info(sheet)?; s.sheet_id(sheet)? };
        let json = serde_json::to_string(&charts)?;
        self.mutating("set_sheet_charts", None, |wb| crate::storage::meta_set(&wb.conn, &key(id), &json))
    }
}

impl crate::workbook::Reader {
    /// The charts placed on a sheet (S.09).
    pub fn sheet_charts(&self, sheet: u32) -> Result<Vec<SheetChart>> {
        let id = { let s = self.sheets(); s.info(sheet)?; s.sheet_id(sheet)? };
        Ok(crate::storage::meta_get(&self.conn, &key(id))?.and_then(|v| serde_json::from_str(&v).ok()).unwrap_or_default())
    }
}
