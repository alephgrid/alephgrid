//! Sheet cells that table formulas read ("links", F.09).
//!
//! Each linked cell has an anchor: a formula such as `=Settings!$B$2` in column A of a very
//! hidden sheet, written outside the undo history. The sheet engine keeps an anchor pointing
//! at its cell as rows and columns are inserted, deleted or moved, as sheets are renamed, and
//! through undo and redo, exactly as it does for any formula; `Workbook::sync_links` then
//! rewrites the table formulas to match and records each cell's value.

use anyhow::{anyhow, Result};
use ironcalc::base::cell::CellValue as SheetValue;
use ironcalc::base::types::{Cell, CellType};

use super::Sheets;
use crate::table_formula::{CellRef, CellValue, ExprKind};

/// Name of the very hidden sheet holding the anchors. Clients never see it.
pub const LINK_SHEET: &str = "_rf_links";

/// Where an anchor points now.
#[derive(Debug, Clone, PartialEq)]
pub enum Anchor {
    At(CellRef),
    /// Its cell was deleted (the anchor shows `#REF!`).
    Gone,
    /// The anchor itself is missing (the sheets were replaced or never had it).
    Missing,
}

impl Sheets {
    fn link_sheet(&self) -> Option<u32> {
        let m = self.model.as_ref()?;
        m.get_worksheets_properties().iter().position(|p| p.name == LINK_SHEET && p.state != "visible").map(|i| i as u32)
    }

    /// The index and actual name of the visible sheet called `name` (case-insensitive).
    pub fn visible_sheet(&self, name: &str) -> Option<(u32, String)> {
        let wanted = name.to_lowercase();
        self.list().into_iter().find(|s| s.name.to_lowercase() == wanted).map(|s| (s.index, s.name))
    }

    pub fn anchor(&self, anchor: u32) -> Anchor {
        let (Some(m), Some(sheet)) = (self.model.as_ref(), self.link_sheet()) else { return Anchor::Missing };
        let content = m.get_cell_content(sheet, anchor as i32, 1).unwrap_or_default();
        if content.is_empty() {
            return Anchor::Missing;
        }
        match crate::table_formula::parse(&content).map(|e| e.kind) {
            Ok(ExprKind::Cell { cell, .. }) => Anchor::At(cell),
            _ => Anchor::Gone,
        }
    }

    /// Point anchors at cells (`None` clears one), creating the link sheet when needed. Not
    /// recorded in the undo history; evaluated once at the end.
    pub fn set_anchors(&mut self, changes: &[(u32, Option<CellRef>)]) -> Result<()> {
        if changes.is_empty() {
            return Ok(());
        }
        let m = self.model_mut()?;
        let sheet = m.ensure_unrecorded_sheet(LINK_SHEET).map_err(|e| anyhow!(e))?;
        m.pause_evaluation();
        let out = changes.iter().try_for_each(|(anchor, cell)| {
            let input = cell.as_ref().map(|c| format!("={}", c.text((true, true)))).unwrap_or_default();
            m.set_unrecorded_input(sheet, *anchor as i32, 1, &input)
        });
        m.resume_evaluation();
        m.evaluate();
        out.map_err(|e| anyhow!(e))
    }

    /// The computed value of a cell on a visible sheet; `None` when there is no such sheet.
    pub fn cell_value(&self, cell: &CellRef) -> Option<CellValue> {
        let (index, _) = self.visible_sheet(&cell.sheet)?;
        let m = self.model.as_ref()?;
        let model = m.get_model();
        let (row, column) = (cell.row as i32, cell.column as i32);
        let present = model.workbook.worksheet(index).ok()?.cell(row, column).is_some_and(|c| !matches!(*c, Cell::EmptyCell { .. }));
        if !present {
            return Some(CellValue::Blank);
        }
        Some(match (m.get_cell_type(index, row, column), model.get_cell_value_by_index(index, row, column)) {
            (Ok(CellType::ErrorValue), _) => CellValue::Error(m.get_formatted_cell_value(index, row, column).unwrap_or_else(|_| "#ERROR!".into())),
            (_, Ok(SheetValue::Number(n))) => CellValue::Number(n),
            (_, Ok(SheetValue::Boolean(b))) => CellValue::Bool(b),
            (_, Ok(SheetValue::String(s))) => CellValue::Text(s),
            _ => CellValue::Blank,
        })
    }
}
