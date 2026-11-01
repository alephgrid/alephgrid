//! S.05: sheet layout: frozen rows and columns, column widths and row heights, hidden rows
//! and columns. Sizes are kept by the engine (and so in the workbook and in XLSX); the API
//! speaks the grid's pixels (104 × 24 by default) and converts to the engine's units.
//! S.06 range filters hide the rows that do not match, so they use the same hidden rows.

use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::{CellArea, Sheets, MAX_COLUMNS, MAX_ROWS};

/// The grid's default column width and row height, in pixels.
pub const GRID_COLUMN_WIDTH: f64 = 104.0;
pub const GRID_ROW_HEIGHT: f64 = 24.0;
/// The engine's defaults for the same sizes.
const ENGINE_COLUMN_WIDTH: f64 = 90.0;
const ENGINE_ROW_HEIGHT: f64 = 25.0;
/// The engine stores sizes in Excel's units: pixels divided by these.
const ENGINE_COLUMN_FACTOR: f64 = 9.0;
const ENGINE_ROW_FACTOR: f64 = 1.5625;
/// Rows changed by one resize, hide or show (each is one undo entry in the engine).
pub const MAX_LAYOUT_ROWS: u32 = 100_000;
/// Row entries a layout lists; rows past it keep their size but are not reported.
pub const MAX_LISTED_ROWS: usize = 200_000;
/// Rows read to auto-fit a column.
pub const FIT_SCAN_ROWS: u32 = 100_000;

/// Which lines a layout change addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
pub enum Axis {
    Rows,
    Columns,
}

/// A row or column that differs from the default: its size in grid pixels, or hidden.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct LineSize {
    /// 0-based row or column.
    pub index: u32,
    pub size: u32,
    #[serde(default)]
    pub hidden: bool,
}

/// A sheet's rows and columns that differ from the default, in index order.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
pub struct SheetLayout {
    pub frozen_rows: u32,
    pub frozen_columns: u32,
    pub columns: Vec<LineSize>,
    pub rows: Vec<LineSize>,
    /// True when more rows differ than a layout lists ([`MAX_LISTED_ROWS`]).
    #[serde(default)]
    pub truncated: bool,
    /// Merged ranges; the top-left cell holds the value and the others are covered.
    #[serde(default)]
    pub merged: Vec<CellArea>,
}

/// How cells are merged: one cell, one cell centred, or one per row (`across`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
pub enum MergeMode {
    Merge,
    Center,
    Across,
    Unmerge,
}

fn to_engine(axis: Axis, px: f64) -> f64 {
    match axis {
        Axis::Columns => px * ENGINE_COLUMN_WIDTH / GRID_COLUMN_WIDTH,
        Axis::Rows => px * ENGINE_ROW_HEIGHT / GRID_ROW_HEIGHT,
    }
}

fn to_grid(axis: Axis, engine: f64) -> u32 {
    let px = match axis {
        Axis::Columns => engine * GRID_COLUMN_WIDTH / ENGINE_COLUMN_WIDTH,
        Axis::Rows => engine * GRID_ROW_HEIGHT / ENGINE_ROW_HEIGHT,
    };
    px.round().max(0.0) as u32
}

impl Sheets {
    fn check_lines(&self, index: u32, axis: Axis, start: u32, count: u32) -> Result<(i32, i32)> {
        self.check_index(index)?;
        let limit = match axis {
            Axis::Rows => MAX_ROWS,
            Axis::Columns => MAX_COLUMNS,
        };
        if count == 0 || start >= limit || start as u64 + count as u64 > limit as u64 {
            bail!("{count} {} from {} are outside the sheet", if axis == Axis::Rows { "rows" } else { "columns" }, start + 1);
        }
        if axis == Axis::Rows && count > MAX_LAYOUT_ROWS {
            bail!("at most {MAX_LAYOUT_ROWS} rows can be resized, hidden or shown at once");
        }
        Ok((start as i32 + 1, (start + count) as i32))
    }

    /// Keep the first `rows` rows and `columns` columns in view while scrolling (0 unfreezes),
    /// as one undo step.
    pub fn freeze(&mut self, index: u32, rows: u32, columns: u32) -> Result<()> {
        self.check_index(index)?;
        if rows > 100 {
            bail!("at most 100 rows can be frozen");
        }
        if columns > 50 {
            bail!("at most 50 columns can be frozen");
        }
        let m = self.model_mut()?;
        let (old_rows, old_columns) = (m.get_frozen_rows_count(index).map_err(|e| anyhow!(e))?, m.get_frozen_columns_count(index).map_err(|e| anyhow!(e))?);
        let mut entries = 0;
        if old_rows != rows as i32 {
            m.set_frozen_rows_count(index, rows as i32).map_err(|e| anyhow!(e))?;
            entries += 1;
        }
        if old_columns != columns as i32 {
            m.set_frozen_columns_count(index, columns as i32).map_err(|e| anyhow!(e))?;
            entries += 1;
        }
        self.record(entries);
        Ok(())
    }

    /// Set the width (columns) or height (rows) of `count` lines from `start`, in grid
    /// pixels (4–2000), as one undo step.
    pub fn resize(&mut self, index: u32, axis: Axis, start: u32, count: u32, size: u32) -> Result<()> {
        let (first, last) = self.check_lines(index, axis, start, count)?;
        if !(4..=2000).contains(&size) {
            bail!("sizes are 4 to 2000 pixels");
        }
        let value = to_engine(axis, size as f64);
        let m = self.model_mut()?;
        match axis {
            Axis::Columns => m.set_columns_width(index, first, last, value),
            Axis::Rows => m.set_rows_height(index, first, last, value),
        }
        .map_err(|e| anyhow!(e))?;
        self.record(1);
        Ok(())
    }

    /// Hide or show `count` lines from `start`, as one undo step. Values and formulas stay.
    pub fn set_hidden(&mut self, index: u32, axis: Axis, start: u32, count: u32, hidden: bool) -> Result<()> {
        let (first, last) = self.check_lines(index, axis, start, count)?;
        let m = self.model_mut()?;
        match axis {
            Axis::Columns => m.set_columns_hidden(index, first, last, hidden),
            Axis::Rows => m.set_rows_hidden(index, first, last, hidden),
        }
        .map_err(|e| anyhow!(e))?;
        self.record(1);
        Ok(())
    }

    /// The sheet's frozen panes and the rows and columns that differ from the default.
    pub fn layout(&self, index: u32) -> Result<SheetLayout> {
        self.check_index(index)?;
        let m = self.model()?;
        let ws = m.get_model().workbook.worksheet(index).map_err(|e| anyhow!(e))?;
        let mut columns = Vec::new();
        for col in &ws.cols {
            if !(col.custom_width || col.hidden) {
                continue;
            }
            let size = to_grid(Axis::Columns, if col.custom_width { col.width * ENGINE_COLUMN_FACTOR } else { ENGINE_COLUMN_WIDTH });
            if !col.hidden && size == GRID_COLUMN_WIDTH as u32 {
                continue;
            }
            for c in col.min.max(1)..=col.max.min(MAX_COLUMNS as i32) {
                columns.push(LineSize { index: (c - 1) as u32, size, hidden: col.hidden });
            }
        }
        columns.sort_by_key(|l| l.index);
        let mut rows = Vec::new();
        let mut truncated = false;
        for row in &ws.rows {
            if row.r < 1 || !(row.hidden || row.custom_height) {
                continue;
            }
            let size = to_grid(Axis::Rows, if row.custom_height { row.height * ENGINE_ROW_FACTOR } else { ENGINE_ROW_HEIGHT });
            if !row.hidden && size == GRID_ROW_HEIGHT as u32 {
                continue;
            }
            if rows.len() == MAX_LISTED_ROWS {
                truncated = true;
                break;
            }
            rows.push(LineSize { index: (row.r - 1) as u32, size, hidden: row.hidden });
        }
        rows.sort_by_key(|l| l.index);
        let merged = ws
            .merged_cells
            .iter()
            .map(|m| CellArea { row: (m.row - 1).max(0) as u32, column: (m.column - 1).max(0) as u32, rows: m.height.max(1) as u32, columns: m.width.max(1) as u32 })
            .collect();
        Ok(SheetLayout { frozen_rows: ws.frozen_rows.max(0) as u32, frozen_columns: ws.frozen_columns.max(0) as u32, columns, rows, truncated, merged })
    }

    /// Merge an area into one cell (or one per row), or unmerge the merges inside it, as
    /// one undo step. Merging refuses areas where more than one cell holds a value, so no
    /// value is ever dropped.
    pub fn merge(&mut self, index: u32, a: &CellArea, mode: MergeMode) -> Result<()> {
        self.check_index(index)?;
        a.check("merge")?;
        if mode != MergeMode::Unmerge && a.rows * a.columns < 2 {
            bail!("select at least two cells to merge");
        }
        let range = super::format::area(index, a);
        let m = self.model_mut()?;
        match mode {
            MergeMode::Merge => m.merge_cells(&range),
            MergeMode::Center => m.merge_cells_center(&range),
            MergeMode::Across => m.merge_cells_across(&range),
            MergeMode::Unmerge => m.unmerge_cells(&range),
        }
        .map_err(|e| anyhow!(e))?;
        self.record(1);
        Ok(())
    }

    /// The longest shown texts of a column (at most 20, longest first), read from its first
    /// [`FIT_SCAN_ROWS`] used rows: the client measures them to auto-fit the column.
    pub fn column_texts(&self, index: u32, column: u32) -> Result<Vec<String>> {
        self.check_index(index)?;
        if column >= MAX_COLUMNS {
            bail!("column {} is outside the sheet", column + 1);
        }
        let m = self.model()?;
        let ws = m.get_model().workbook.worksheet(index).map_err(|e| anyhow!(e))?;
        let (rows, _) = super::used_size(m, index);
        let mut texts: Vec<String> = Vec::new();
        for r in 1..=rows.min(FIT_SCAN_ROWS) as i32 {
            if ws.cell(r, column as i32 + 1).is_none() {
                continue;
            }
            let t = m.get_formatted_cell_value(index, r, column as i32 + 1).unwrap_or_default();
            if t.is_empty() {
                continue;
            }
            texts.push(t);
            if texts.len() > 200 {
                texts.sort_by_key(|t| std::cmp::Reverse(t.chars().count()));
                texts.truncate(20);
            }
        }
        texts.sort_by_key(|t| std::cmp::Reverse(t.chars().count()));
        texts.truncate(20);
        Ok(texts)
    }

    /// The data rows of a range filter: `area` below its header row when `header`.
    fn filter_rows(&self, index: u32, area: &CellArea, header: bool, column: u32) -> Result<(u32, u32)> {
        self.check_index(index)?;
        area.check("filter")?;
        if column < area.column || column >= area.column + area.columns {
            bail!("the filter column is outside the range");
        }
        let first = area.row + header as u32;
        let end = area.row + area.rows;
        if first >= end {
            bail!("the range has no rows below its header");
        }
        if end - first > MAX_LAYOUT_ROWS {
            bail!("a range filter covers at most {MAX_LAYOUT_ROWS} rows");
        }
        Ok((first, end))
    }

    /// The distinct shown values of `column` within a range's data rows, with counts (at
    /// most 1,000, most frequent first; the empty value is ""), for a range filter's list.
    pub fn range_values(&self, index: u32, area: &CellArea, header: bool, column: u32) -> Result<Vec<crate::ValueCount>> {
        let (first, end) = self.filter_rows(index, area, header, column)?;
        let m = self.model()?;
        let mut counts: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
        for r in first..end {
            let t = m.get_formatted_cell_value(index, r as i32 + 1, column as i32 + 1).unwrap_or_default();
            *counts.entry(t).or_default() += 1;
        }
        let mut out: Vec<(String, u32)> = counts.into_iter().collect();
        out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        out.truncate(1000);
        Ok(out.into_iter().map(|(v, n)| crate::ValueCount { value: (!v.is_empty()).then_some(v), count: n as u64 }).collect())
    }

    /// S.06: filter a range: show its data rows whose shown value in `column` is one of
    /// `keep` and hide the others, as one undo step. Nothing is deleted; `keep` = None
    /// shows every data row again (clears the filter). Returns the number of rows hidden.
    pub fn filter_range(&mut self, index: u32, area: &CellArea, header: bool, column: u32, keep: Option<&[String]>) -> Result<u32> {
        let (first, end) = self.filter_rows(index, area, header, column)?;
        let hide: Vec<bool> = match keep {
            None => vec![false; (end - first) as usize],
            Some(keep) => {
                let keep: std::collections::HashSet<&str> = keep.iter().map(|s| s.as_str()).collect();
                let m = self.model()?;
                (first..end).map(|r| !keep.contains(m.get_formatted_cell_value(index, r as i32 + 1, column as i32 + 1).unwrap_or_default().as_str())).collect()
            }
        };
        let m = self.model_mut()?;
        m.set_rows_hidden(index, first as i32 + 1, end as i32, false).map_err(|e| anyhow!(e))?;
        let mut entries = 1;
        let mut i = 0;
        while i < hide.len() {
            if !hide[i] {
                i += 1;
                continue;
            }
            let j = (i..hide.len()).find(|&j| !hide[j]).unwrap_or(hide.len());
            m.set_rows_hidden(index, first as i32 + 1 + i as i32, first as i32 + j as i32, true).map_err(|e| anyhow!(e))?;
            entries += 1;
            i = j;
        }
        self.record(entries);
        Ok(hide.iter().filter(|h| **h).count() as u32)
    }
}
