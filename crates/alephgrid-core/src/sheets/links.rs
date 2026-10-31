//! Sheet cells that table formulas read ("links", F.09).
//!
//! Each linked cell has an anchor: a formula such as `=Settings!$B$2` in column A of a very
//! hidden sheet, written outside the undo history. The sheet engine keeps an anchor pointing
//! at its cell as rows and columns are inserted, deleted or moved, as sheets are renamed, and
//! through undo and redo, exactly as it does for any formula; `Workbook::sync_links` then
//! rewrites the table formulas to match and records each cell's value.
//!
//! Sheet cells that read tables ("table cells") go the other way: the sheet engine holds
//! `=_rf_links!$B$<slot>` for them, column B of the link sheet holds the value Alephgrid
//! computed for the slot's table formula, and the formula is shown in place of the
//! reference. See `crate::links`.

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};

use anyhow::{anyhow, Result};
use ironcalc::base::cell::CellValue as SheetValue;
use ironcalc::base::expressions::lexer::{Lexer, LexerMode};
use ironcalc::base::expressions::token::TokenType;
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

/// Column of the link sheet holding table-cell results; anchors are in column A.
const RESULT_COLUMN: i32 = 2;

/// A sheet cell holding a table formula, and its result slot.
#[derive(Debug, Clone, PartialEq)]
pub struct SlotCell {
    pub cell: CellRef,
    pub slot: u32,
}

/// What the sheet engine holds for the table cell of `slot`: a reference to its value, or for
/// a table result (`spills`) a spill reference to the array in its own very hidden sheet.
pub(crate) fn slot_input(slot: u32, spills: bool) -> String {
    if spills { format!("={SPILL_PREFIX}{slot}!$A$1#") } else { format!("={LINK_SHEET}!$B${slot}") }
}

/// Very hidden sheets holding table results, `_rf_spill_<slot>`, one array each at A1.
pub(crate) const SPILL_PREFIX: &str = "_rf_spill_";
/// Most cells a table result may spill into a sheet.
pub const MAX_SPILL_CELLS: usize = 10_000;

/// The slot of a cell whose content is exactly a slot reference (either form).
pub(crate) fn slot_of(content: &str) -> Option<u32> {
    let (sheet, cell) = content.strip_prefix('=')?.split_once('!')?;
    let sheet = sheet.trim_matches('\'');
    if sheet.eq_ignore_ascii_case(LINK_SHEET) {
        return cell.strip_prefix("$B$")?.parse().ok().filter(|n| *n >= 1);
    }
    let n = sheet.get(..SPILL_PREFIX.len()).filter(|p| p.eq_ignore_ascii_case(SPILL_PREFIX)).map(|_| &sheet[SPILL_PREFIX.len()..])?;
    (cell == "$A$1#").then(|| n.parse().ok()).flatten().filter(|n| *n >= 1)
}

/// An array as an array constant the sheet engine spills (blanks become "").
fn array_constant(rows: &[Vec<CellValue>]) -> String {
    let one = |v: &CellValue| match v {
        CellValue::Number(n) => format!("{n}"),
        CellValue::Text(t) => format!("\"{}\"", t.replace('"', "\"\"")),
        CellValue::Bool(b) => if *b { "TRUE".into() } else { "FALSE".into() },
        CellValue::Error(e) => e.clone(),
        CellValue::Blank | CellValue::Array(_) => "\"\"".into(),
    };
    format!("={{{}}}", rows.iter().map(|r| r.iter().map(one).collect::<Vec<_>>().join(",")).collect::<Vec<_>>().join(";"))
}

fn tokens(formula: &str) -> Vec<TokenType> {
    let (Ok(locale), Ok(language)) = (ironcalc::base::locale::get_locale("en"), ironcalc::base::language::get_language("en")) else { return Vec::new() };
    let body = formula.trim_start().strip_prefix('=').unwrap_or(formula);
    let mut lexer = Lexer::new(body, LexerMode::A1, locale, language);
    let mut out = Vec::new();
    loop {
        match lexer.next_token() {
            TokenType::EOF | TokenType::Illegal(_) => return out,
            t => out.push(t),
        }
    }
}

/// Whether a formula refers to the link sheet by name.
pub(crate) fn mentions_link_sheet(formula: &str) -> bool {
    tokens(formula).iter().any(|t| match t {
        TokenType::Reference { sheet: Some(s), .. } | TokenType::Range { sheet: Some(s), .. } => {
            s.eq_ignore_ascii_case(LINK_SHEET) || s.get(..SPILL_PREFIX.len()).is_some_and(|p| p.eq_ignore_ascii_case(SPILL_PREFIX))
        }
        _ => false,
    })
}

/// Cells visited at most while looking for what a sheet cell depends on.
pub(crate) const MAX_PRECEDENT_CELLS: usize = 200_000;

impl Sheets {
    /// Table-cell formulas by slot, shown in place of their slot references.
    pub(crate) fn set_table_cells(&mut self, formulas: HashMap<u32, String>) {
        self.table_cells = formulas;
    }

    /// What a cell's content on sheet `index` is shown as: a table cell's formula instead of
    /// its reference, with cells on that sheet written without the sheet's name.
    pub(crate) fn shown_input(&self, index: u32, content: String) -> String {
        match slot_of(&content).and_then(|s| self.table_cells.get(&s)) {
            Some(f) => {
                let home = self.info(index).map(|i| i.name).unwrap_or_default();
                format!("={}", crate::table_formula::unqualify_cells(f, &home))
            }
            None => content,
        }
    }

    /// Every table cell on the visible sheets, in sheet, row, column order.
    pub fn slot_cells(&self) -> Vec<SlotCell> {
        let Some(m) = self.model.as_ref() else { return Vec::new() };
        let mut out = Vec::new();
        for info in self.list() {
            let Ok(ws) = m.get_model().workbook.worksheet(info.index) else { continue };
            for (row, column, cell) in ws.sheet_data.cells() {
                if !matches!(*cell, Cell::CellFormula { .. } | Cell::ArrayFormula { .. }) {
                    continue;
                }
                let content = m.get_cell_content(info.index, row, column).unwrap_or_default();
                if let Some(slot) = slot_of(&content) {
                    out.push(SlotCell { cell: CellRef { sheet: info.name.clone(), row: row as u32, column: column as u32 }, slot });
                }
            }
        }
        out
    }

    /// Create the very hidden sheet of table result `slot` (before anything refers to it).
    pub(crate) fn ensure_spill_sheet(&mut self, slot: u32) -> Result<()> {
        self.model_mut()?.ensure_unrecorded_sheet(&format!("{SPILL_PREFIX}{slot}")).map_err(|e| anyhow!(e))?;
        Ok(())
    }

    /// Write table-cell results into their slots, outside the undo history, and evaluate.
    pub(crate) fn set_slot_values(&mut self, values: &[(u32, CellValue)]) -> Result<()> {
        if values.is_empty() {
            return Ok(());
        }
        let m = self.model_mut()?;
        let sheet = m.ensure_unrecorded_sheet(LINK_SHEET).map_err(|e| anyhow!(e))?;
        m.pause_evaluation();
        let out = values.iter().try_for_each(|(slot, v)| {
            if let CellValue::Array(rows) = v {
                let spill = m.ensure_unrecorded_sheet(&format!("{SPILL_PREFIX}{slot}"))?;
                return m.set_unrecorded_input(spill, 1, 1, &array_constant(rows));
            }
            let input = match v {
                CellValue::Blank => String::new(),
                // Shortest text that reads back as the same number.
                CellValue::Number(n) => format!("{n}"),
                CellValue::Text(t) => format!("'{t}"),
                CellValue::Bool(b) => if *b { "TRUE".into() } else { "FALSE".into() },
                CellValue::Error(e) => format!("={e}"),
                CellValue::Array(_) => unreachable!("written above"),
            };
            m.set_unrecorded_input(sheet, *slot as i32, RESULT_COLUMN, &input)
        });
        m.resume_evaluation();
        m.evaluate();
        out.map_err(|e| anyhow!(e))
    }

    /// The slots whose values the given cells depend on through the sheet engine's
    /// formulas: references, ranges, defined names and spilled arrays, followed
    /// transitively. `Err` when more than [`MAX_PRECEDENT_CELLS`] cells would be visited.
    /// References computed while evaluating (`INDIRECT`, `OFFSET`) are not followed.
    pub(crate) fn slots_feeding(&self, cells: &[CellRef]) -> std::result::Result<BTreeSet<u32>, String> {
        let Some(m) = self.model.as_ref() else { return Ok(BTreeSet::new()) };
        let model = m.get_model();
        let wb = &model.workbook;
        let names = wb.get_defined_names_with_scope();
        let sheet_index = |name: &str| -> Option<u32> { self.visible_sheet(name).map(|(i, _)| i) };
        let mut found = BTreeSet::new();
        let mut seen: HashSet<(u32, i32, i32)> = HashSet::new();
        let mut queue: VecDeque<(u32, i32, i32)> = VecDeque::new();
        for c in cells {
            if let Some(i) = sheet_index(&c.sheet) {
                queue.push_back((i, c.row as i32, c.column as i32));
            }
        }
        let mut dims: HashMap<u32, (i32, i32, i32, i32)> = HashMap::new();
        // Formula texts to scan, with the sheet they are read on.
        let mut texts: Vec<(u32, String)> = Vec::new();
        let mut names_done: HashSet<(String, Option<u32>)> = HashSet::new();
        while let Some((s, r, c)) = queue.pop_front() {
            if !seen.insert((s, r, c)) {
                continue;
            }
            if seen.len() > MAX_PRECEDENT_CELLS {
                return Err(format!("more than {MAX_PRECEDENT_CELLS} sheet cells feed this table formula; Alephgrid cannot check it for circular references"));
            }
            let Ok(ws) = wb.worksheet(s) else { continue };
            match ws.cell(r, c).map(|c| c.into_owned()) {
                Some(Cell::CellFormula { .. } | Cell::ArrayFormula { .. }) => {
                    let content = m.get_cell_content(s, r, c).unwrap_or_default();
                    if let Some(slot) = slot_of(&content) {
                        found.insert(slot);
                        continue;
                    }
                    texts.push((s, content));
                }
                Some(Cell::SpillCell { a, .. }) => queue.push_back((s, a.0, a.1)),
                _ => {}
            }
            while let Some((on, text)) = texts.pop() {
                for t in tokens(&text) {
                    match t {
                        TokenType::Reference { sheet, row, column, .. } => {
                            let target = match &sheet { Some(n) => sheet_index(n), None => Some(on) };
                            if let Some(t) = target {
                                queue.push_back((t, row, column));
                            }
                        }
                        TokenType::Range { sheet, left, right } => {
                            let Some(t) = (match &sheet { Some(n) => sheet_index(n), None => Some(on) }) else { continue };
                            let Ok(ws) = wb.worksheet(t) else { continue };
                            let dim = *dims.entry(t).or_insert_with(|| { let d = ws.dimension(); (d.min_row, d.max_row, d.min_column, d.max_column) });
                            let (r1, r2) = (left.row.min(right.row).max(dim.0), left.row.max(right.row).min(dim.1));
                            let (c1, c2) = (left.column.min(right.column).max(dim.2), left.column.max(right.column).min(dim.3));
                            for row in r1..=r2 {
                                for (col, _) in ws.sheet_data.cells_in_row_between(row, c1, c2) {
                                    queue.push_back((t, row, col));
                                }
                                if queue.len() > MAX_PRECEDENT_CELLS {
                                    return Err(format!("more than {MAX_PRECEDENT_CELLS} sheet cells feed this table formula; Alephgrid cannot check it for circular references"));
                                }
                            }
                        }
                        TokenType::Ident(name) => {
                            // A defined name: the sheet's own first, then the workbook's.
                            let lower = name.to_lowercase();
                            let found_name = names.iter().find(|(n, scope, _)| n.to_lowercase() == lower && *scope == Some(on))
                                .or_else(|| names.iter().find(|(n, scope, _)| n.to_lowercase() == lower && scope.is_none()));
                            if let Some((n, scope, f)) = found_name {
                                if names_done.insert((n.to_lowercase(), *scope)) {
                                    texts.push((on, f.clone()));
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        Ok(found)
    }
}
