//! Free-form, Excel-compatible sheets powered by IronCalc.
//!
//! All sheets of a Alephgrid workbook live in one IronCalc model, so formulas can
//! reference each other (`=Budget!B4 * 2`). The model is persisted as a blob inside
//! the workbook's DuckDB file, next to the big columnar tables. Rows and columns
//! are 0-based on the wire and 1-based inside IronCalc.

use anyhow::{anyhow, bail, Result};
use ironcalc::base::cell::CellValue;
use ironcalc::base::formatter::dates::from_excel_date;
use ironcalc::base::formatter::lexer::is_likely_date_number_format;
use ironcalc::base::types::CellType;
use ironcalc::base::UserModel;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

mod editing;
mod links;
pub use links::{Anchor, SlotCell, LINK_SHEET, MAX_SPILL_CELLS};
pub(crate) use links::{slot_input, slot_of};
pub use editing::{CellArea, CellDetail, CellError, FillDirection, FormulaProblem, NamedRange, PasteMode};

const LANG: &str = "en";
/// Hard cap on cells written in one go (paste, query → sheet).
pub const MAX_BULK_CELLS: usize = 1_000_000;
/// Excel's grid size, which IronCalc enforces.
pub const MAX_ROWS: u32 = 1_048_576;
pub const MAX_COLUMNS: u32 = 16_384;
/// Largest used range converted to a table in one go.
pub const MAX_CONVERT_CELLS: u64 = 20_000_000;

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct SheetInfo {
    pub index: u32,
    pub name: String,
    /// Used range, 0-based exclusive bounds.
    pub rows: u32,
    pub columns: u32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
pub enum Align {
    Left,
    Right,
    Center,
}

/// One rendered cell: `[display, align, input]`. `input` is present only when it
/// differs from `display` (formulas, formatted numbers), to keep frames small.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SheetCell(pub String, pub Align, #[serde(default, skip_serializing_if = "Option::is_none")] pub Option<String>);

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct SheetBlock {
    pub row_start: u32,
    /// `rows[i][j]` is `null` for empty cells.
    #[ts(type = "(SheetCell | null)[][]")]
    pub rows: Vec<Vec<Option<SheetCell>>>,
}

#[derive(Debug, Clone, Deserialize, TS)]
pub struct SheetInput {
    pub row: u32,
    pub column: u32,
    /// Text exactly as typed: `42`, `hello`, `=SUM(A1:A9)`. Empty clears the cell.
    pub value: String,
}

pub struct Sheets {
    model: Option<UserModel<'static>>,
    /// IronCalc keeps one history entry per primitive change; these group them into
    /// user-level steps so one paste is one undo.
    undo_groups: Vec<u32>,
    redo_groups: Vec<u32>,
    /// Bumped whenever the model is about to change (after validation), so a caller can
    /// tell a rejected request from a failure part-way through a change.
    changes: u64,
    /// Formulas of the table cells (F.09), by result slot: shown in place of the
    /// `=_rf_links!$B$n` reference the sheet engine holds for them.
    table_cells: std::collections::HashMap<u32, String>,
}

/// A sheet column converted for a table: its inferred SQL type and canonical text values.
pub struct TypedColumn {
    pub name: String,
    pub sql_type: &'static str,
    pub values: Vec<Option<String>>,
}

impl Sheets {
    pub fn from_bytes(bytes: Option<Vec<u8>>) -> Result<Sheets> {
        let model = match bytes {
            Some(b) => Some(UserModel::from_bytes(&b, LANG).map_err(|e| anyhow!("corrupt sheet data: {e}"))?),
            None => None,
        };
        Ok(Sheets { model, undo_groups: vec![], redo_groups: vec![], changes: 0, table_cells: Default::default() })
    }

    /// Record that the last user operation added `entries` IronCalc history entries.
    fn record(&mut self, entries: u32) {
        if entries > 0 {
            self.undo_groups.push(entries);
            self.redo_groups.clear();
        }
    }

    pub fn has_function_name(&self, name: &str) -> bool {
        self.model.as_ref().is_some_and(|m| m.get_model().workbook.defined_names.iter().any(|n| n.name.eq_ignore_ascii_case(name)))
    }

    pub fn has_function_definition(&self, d: &crate::functions::FunctionDefinition) -> bool {
        self.model.as_ref().is_some_and(|m| m.get_model().workbook.defined_names.iter().any(|n| n.name.eq_ignore_ascii_case(&d.name) && n.sheet_id.is_none() && n.formula == crate::functions::canonical_lambda(d)) && !m.get_model().workbook.defined_names.iter().any(|n| n.name.eq_ignore_ascii_case(&d.name) && n.sheet_id.is_some()))
    }

    /// Registry changes clear cell undo history so undo cannot change a function pin.
    pub fn install_function(&mut self, d: &crate::functions::FunctionDefinition) -> Result<()> {
        if self.model.as_ref().is_some_and(|m| m.get_model().workbook.defined_names.iter().any(|n| n.name.eq_ignore_ascii_case(&d.name) && n.sheet_id.is_some())) {
            bail!("sheet-local imported name conflicts with workbook function {}", d.name);
        }
        if self.has_function_definition(d) { return Ok(()); }
        if self.model.is_none() { self.create("Sheet1")?; }
        let model = self.model_mut()?;
        let exists = model.get_model().workbook.defined_names.iter().any(|n| n.name.eq_ignore_ascii_case(&d.name));
        let formula = crate::functions::lambda(d);
        if exists { model.update_defined_name(&d.name, None, &d.name, None, &formula).map_err(|e| anyhow!(e))?; }
        else { model.new_defined_name(&d.name, None, &formula).map_err(|e| anyhow!(e))?; }
        model.evaluate();
        self.undo_groups.clear(); self.redo_groups.clear();
        Ok(())
    }

    pub fn test_function_call(&mut self, d: &crate::functions::FunctionDefinition, args: &[String]) -> Result<String> {
        // Scratch model only. Values cannot inject formulas; ranges are entered in real sheets.
        let mut inputs = Vec::new();
        for (i, value) in args.iter().enumerate() {
            inputs.push(SheetInput { row: 0, column: i as u32, value: if value.starts_with('=') { format!("'{value}") } else { value.clone() } });
        }
        let refs = (0..args.len()).map(|i| format!("{}1", column_name(i as u32))).collect::<Vec<_>>().join(",");
        inputs.push(SheetInput { row: 1, column: 0, value: format!("={}({refs})", d.name) });
        self.set(0, &inputs)?;
        self.model()?.get_formatted_cell_value(0, 2, 1).map_err(|e| anyhow!(e))
    }

    pub fn to_bytes(&self) -> Option<Vec<u8>> {
        self.model.as_ref().map(|m| m.to_bytes())
    }

    fn model(&self) -> Result<&UserModel<'static>> {
        self.model.as_ref().ok_or_else(|| anyhow!("this workbook has no sheets"))
    }

    fn model_mut(&mut self) -> Result<&mut UserModel<'static>> {
        self.changes += 1;
        self.model.as_mut().ok_or_else(|| anyhow!("this workbook has no sheets"))
    }

    /// Number of (attempted) changes so far; see `changes` above.
    pub fn changes(&self) -> u64 {
        self.changes
    }

    /// Only visible sheets can be addressed; the link sheet (see `links.rs`) is internal.
    fn check_index(&self, index: u32) -> Result<()> {
        let visible = self.model()?.get_worksheets_properties().get(index as usize).is_some_and(|p| p.state == "visible");
        if !visible {
            bail!("unknown sheet {index}");
        }
        Ok(())
    }

    pub fn list(&self) -> Vec<SheetInfo> {
        let Some(m) = &self.model else { return vec![] };
        m.get_worksheets_properties()
            .into_iter()
            .enumerate()
            .filter(|(_, p)| p.state == "visible")
            .map(|(i, p)| {
                let (rows, columns) = used_size(m, i as u32);
                SheetInfo { index: i as u32, name: p.name, rows, columns }
            })
            .collect()
    }

    pub fn info(&self, index: u32) -> Result<SheetInfo> {
        self.list().into_iter().find(|s| s.index == index).ok_or_else(|| anyhow!("unknown sheet {index}"))
    }

    pub fn create(&mut self, name: &str) -> Result<SheetInfo> {
        let name = name.trim();
        let mut entries = 0;
        self.changes += 1;
        let index = match &mut self.model {
            None => {
                self.model = Some(UserModel::new_empty("workbook", "en", "UTC", LANG).map_err(|e| anyhow!(e))?);
                self.undo_groups.clear();
                self.redo_groups.clear();
                0
            }
            Some(m) => {
                m.new_sheet().map_err(|e| anyhow!(e))?;
                entries += 1;
                m.get_worksheets_properties().len() as u32 - 1
            }
        };
        if !name.is_empty() {
            let unique = self.unique_name(name, index);
            entries += self.rename_raw(index, &unique)?;
        }
        self.record(entries);
        self.info(index)
    }

    /// Rename without recording; returns the number of history entries added.
    fn rename_raw(&mut self, index: u32, name: &str) -> Result<u32> {
        let m = self.model_mut()?;
        if m.get_worksheets_properties()[index as usize].name == name {
            return Ok(0);
        }
        m.rename_sheet(index, name).map_err(|e| anyhow!(e))?;
        Ok(1)
    }

    fn unique_name(&self, base: &str, skip: u32) -> String {
        let taken: Vec<String> = self
            .model
            .as_ref()
            .map(|m| {
                m.get_worksheets_properties()
                    .into_iter()
                    .enumerate()
                    .filter(|(i, _)| *i as u32 != skip)
                    .map(|(_, p)| p.name.to_lowercase())
                    .collect()
            })
            .unwrap_or_default();
        let mut name = base.to_string();
        let mut i = 2;
        while taken.contains(&name.to_lowercase()) {
            name = format!("{base} {i}");
            i += 1;
        }
        name
    }

    pub fn rename(&mut self, index: u32, name: &str) -> Result<()> {
        self.check_index(index)?;
        let name = name.trim();
        if name.is_empty() {
            bail!("sheet name cannot be empty");
        }
        let n = self.rename_raw(index, name)?;
        self.record(n);
        Ok(())
    }

    pub fn delete(&mut self, index: u32) -> Result<()> {
        self.check_index(index)?;
        if self.list().len() == 1 {
            // IronCalc needs at least one worksheet; drop the whole model instead.
            self.changes += 1;
            self.model = None;
            self.undo_groups.clear();
            self.redo_groups.clear();
            return Ok(());
        }
        self.model_mut()?.delete_sheet(index).map_err(|e| anyhow!(e))?;
        self.record(1);
        Ok(())
    }

    pub fn fetch(&self, index: u32, row_start: u32, row_count: u32, column_count: u32) -> Result<SheetBlock> {
        self.check_index(index)?;
        let m = self.model()?;
        let ws = m.get_model().workbook.worksheet(index).map_err(|e| anyhow!(e))?;
        // Clamp before allocating: rows by the fetch limits for this width, the range at
        // the grid's edge.
        let columns = column_count.min(MAX_COLUMNS);
        let row_count = crate::limits::fetch_rows(row_count as u64, columns as usize) as u32;
        let end = row_start.saturating_add(row_count).min(MAX_ROWS);
        let mut rows = Vec::with_capacity(end.saturating_sub(row_start) as usize);
        for r in row_start..end {
            let mut out: Vec<Option<SheetCell>> = Vec::new();
            let mut any = false;
            for (c, _) in ws.sheet_data.cells_in_row(r as i32 + 1) {
                let ci = (c - 1) as u32;
                if ci >= columns {
                    continue;
                }
                let (row, col) = (r as i32 + 1, c);
                let display = m.get_formatted_cell_value(index, row, col).unwrap_or_default();
                let input = self.shown_input(index, m.get_cell_content(index, row, col).unwrap_or_default());
                if display.is_empty() && input.is_empty() {
                    continue;
                }
                let align = match m.get_cell_type(index, row, col) {
                    Ok(CellType::Number) => Align::Right,
                    Ok(CellType::LogicalValue) | Ok(CellType::ErrorValue) => Align::Center,
                    _ => Align::Left,
                };
                let input = (input != display).then_some(input);
                // Rows are only as wide as their last cell; the client pads.
                if out.len() <= ci as usize {
                    out.resize(ci as usize + 1, None);
                }
                out[ci as usize] = Some(SheetCell(display, align, input));
                any = true;
            }
            rows.push(if any { out } else { vec![] });
        }
        Ok(SheetBlock { row_start, rows })
    }

    /// Write cell inputs as one undoable step. Every coordinate is validated before the
    /// first change; if IronCalc still fails part-way, the caller (`Workbook::with_sheets`)
    /// restores the last saved model, so a batch is all-or-nothing.
    pub fn set(&mut self, index: u32, inputs: &[SheetInput]) -> Result<()> {
        self.check_index(index)?;
        if inputs.is_empty() {
            return Ok(());
        }
        if inputs.len() > MAX_BULK_CELLS {
            bail!(crate::limits::TooLarge(format!("too many cells in one edit (max {MAX_BULK_CELLS})")));
        }
        for i in inputs {
            if i.row >= MAX_ROWS || i.column >= MAX_COLUMNS {
                bail!("cell row {} column {} is outside the sheet ({MAX_ROWS} rows × {MAX_COLUMNS} columns)", i.row + 1, i.column + 1);
            }
            // Table cells hold exactly a slot reference; nothing else may read the link sheet.
            if i.value.starts_with('=') && links::slot_of(&i.value).is_none() && links::mentions_link_sheet(&i.value) {
                bail!("{}{}: {LINK_SHEET} is reserved for Alephgrid's own use", column_name(i.column), i.row + 1);
            }
        }
        let m = self.model_mut()?;
        m.pause_evaluation();
        let mut entries = 0;
        let result = (|| -> Result<()> {
            for i in inputs {
                m.set_user_input(index, i.row as i32 + 1, i.column as i32 + 1, &i.value)
                    .map_err(|e| anyhow!("{}{}: {e}", column_name(i.column), i.row + 1))?;
                entries += 1;
            }
            Ok(())
        })();
        m.resume_evaluation();
        m.evaluate();
        self.record(entries);
        result
    }

    /// Like [`Sheets::set`], but cells flagged as text are forced to stay text (a quote
    /// prefix is added when IronCalc would otherwise read them as numbers, dates or
    /// booleans). Used when copying query results, where `"00123"` must not become 123.
    pub fn set_values(&mut self, index: u32, inputs: &[(SheetInput, bool)]) -> Result<()> {
        let plain: Vec<SheetInput> = inputs.iter().map(|(i, _)| i.clone()).collect();
        self.set(index, &plain)?;
        let m = self.model()?;
        let fix: Vec<SheetInput> = inputs
            .iter()
            .filter(|(i, text)| {
                *text && !matches!(m.get_cell_type(index, i.row as i32 + 1, i.column as i32 + 1), Ok(CellType::Text))
            })
            .map(|(i, _)| SheetInput { value: format!("'{}", i.value), ..i.clone() })
            .collect();
        if !fix.is_empty() {
            self.set(index, &fix)?;
            // Both writes form one user step.
            let second = self.undo_groups.pop().unwrap_or(0);
            if let Some(first) = self.undo_groups.last_mut() {
                *first += second;
            }
        }
        Ok(())
    }

    pub fn can_undo(&self) -> bool {
        !self.undo_groups.is_empty()
    }

    pub fn undo(&mut self) -> Result<bool> {
        let Some(n) = self.undo_groups.pop() else { return Ok(false) };
        let m = self.model_mut()?;
        for _ in 0..n {
            m.undo().map_err(|e| anyhow!(e))?;
        }
        m.evaluate();
        self.redo_groups.push(n);
        Ok(true)
    }

    pub fn redo(&mut self) -> Result<bool> {
        let Some(n) = self.redo_groups.pop() else { return Ok(false) };
        let m = self.model_mut()?;
        for _ in 0..n {
            m.redo().map_err(|e| anyhow!(e))?;
        }
        m.evaluate();
        self.undo_groups.push(n);
        Ok(true)
    }

    fn check_span(&self, at: u32, count: u32, max: u32, what: &str) -> Result<()> {
        if count == 0 || count > max || at >= max || at as u64 + count as u64 > max as u64 {
            bail!("invalid {what} range {} + {count}", at + 1);
        }
        Ok(())
    }

    pub fn insert_rows(&mut self, index: u32, at: u32, count: u32) -> Result<()> {
        self.check_index(index)?;
        self.check_span(at, count, MAX_ROWS, "row")?;
        self.model_mut()?.insert_rows(index, at as i32 + 1, count as i32).map_err(|e| anyhow!(e))?;
        self.record(1);
        Ok(())
    }

    pub fn delete_rows(&mut self, index: u32, at: u32, count: u32) -> Result<()> {
        self.check_index(index)?;
        self.check_span(at, count, MAX_ROWS, "row")?;
        self.model_mut()?.delete_rows(index, at as i32 + 1, count as i32).map_err(|e| anyhow!(e))?;
        self.record(1);
        Ok(())
    }

    pub fn insert_columns(&mut self, index: u32, at: u32, count: u32) -> Result<()> {
        self.check_index(index)?;
        self.check_span(at, count, MAX_COLUMNS, "column")?;
        self.model_mut()?.insert_columns(index, at as i32 + 1, count as i32).map_err(|e| anyhow!(e))?;
        self.record(1);
        Ok(())
    }

    pub fn delete_columns(&mut self, index: u32, at: u32, count: u32) -> Result<()> {
        self.check_index(index)?;
        self.check_span(at, count, MAX_COLUMNS, "column")?;
        self.model_mut()?.delete_columns(index, at as i32 + 1, count as i32).map_err(|e| anyhow!(e))?;
        self.record(1);
        Ok(())
    }

    /// Import every sheet of an .xlsx file. Into an empty workbook the IronCalc model
    /// is adopted as-is (styles kept); otherwise cell inputs are copied into new sheets.
    /// Returns the new sheets and plain-language notes about what did not come across.
    pub fn import_xlsx(&mut self, path: &std::path::Path) -> Result<(Vec<SheetInfo>, Vec<String>)> {
        let name = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "import".into());
        let bytes = std::fs::read(path)?;
        let mut warnings = xlsx_part_warnings(&bytes);
        let workbook = ironcalc::import::load_from_xlsx_bytes(&bytes, &name, "en", "UTC")
            .map_err(|e| anyhow!("could not read xlsx: {e}"))?;
        let imported = ironcalc::base::Model::from_workbook(workbook, LANG).map_err(|e| anyhow!(e))?;
        let mut imported = UserModel::from_model(imported);
        imported.evaluate();
        warnings.extend(unsupported_function_warning(&imported));
        if self.model.is_none() {
            self.changes += 1;
            self.model = Some(imported);
            self.undo_groups.clear();
            self.redo_groups.clear();
            return Ok((self.list(), warnings));
        }
        let groups_before = self.undo_groups.len();
        // Give clashing sheets unique names inside the imported model first: IronCalc
        // rewrites references on rename, so formulas keep pointing at the copied sheet.
        let props = imported.get_worksheets_properties();
        let mut taken: Vec<String> = self.list().into_iter().map(|s| s.name.to_lowercase()).collect();
        for (i, p) in props.iter().enumerate() {
            let mut name = p.name.clone();
            let mut n = 2;
            while taken.contains(&name.to_lowercase()) {
                name = format!("{} {n}", p.name);
                n += 1;
            }
            taken.push(name.to_lowercase());
            if name != p.name {
                imported.rename_sheet(i as u32, &name).map_err(|e| anyhow!(e))?;
            }
        }
        // Create every sheet before writing cells, so cross-sheet references resolve.
        let mut targets = Vec::new();
        for p in imported.get_worksheets_properties() {
            targets.push(self.create(&p.name)?.index);
        }
        let mut created = Vec::new();
        for (i, target) in targets.into_iter().enumerate() {
            let ws = imported.get_model().workbook.worksheet(i as u32).map_err(|e| anyhow!(e))?;
            let mut inputs = Vec::new();
            for (row, col, _) in ws.sheet_data.cells() {
                if let Ok(content) = imported.get_cell_content(i as u32, row, col) {
                    if !content.is_empty() {
                        inputs.push(SheetInput { row: row as u32 - 1, column: col as u32 - 1, value: content });
                    }
                }
            }
            self.set(target, &inputs)?;
            created.push(self.info(target)?);
        }
        // The whole import is one undo step.
        let total: u32 = self.undo_groups.drain(groups_before..).sum();
        self.record(total);
        warnings.push("Cell values and formulas were copied into new sheets; formatting is kept only when importing into a workbook without sheets.".into());
        Ok((created, warnings))
    }

    /// The sheets an .xlsx file would add, and its compatibility notes, without importing.
    pub fn preview_xlsx(path: &std::path::Path) -> Result<(Vec<SheetInfo>, Vec<String>)> {
        let mut scratch = Sheets::from_bytes(None)?;
        scratch.import_xlsx(path)
    }

    /// Without the internal link sheet. Table cells (F.09), which other spreadsheets cannot
    /// compute, are written as their current values; each one is listed in the returned
    /// notes, so the replacement is never silent.
    pub fn export_xlsx(&self, path: &std::path::Path) -> Result<Vec<String>> {
        let file = std::fs::File::create(path)?;
        let m = self.model()?;
        let hidden = m.get_worksheets_properties().iter().position(|p| p.name == LINK_SHEET && p.state != "visible");
        let mut notes = Vec::new();
        match hidden {
            None => ironcalc::export::save_xlsx_to_writer(m.get_model(), file),
            Some(i) => {
                let mut copy = UserModel::from_bytes(&m.to_bytes(), LANG).map_err(|e| anyhow!(e))?;
                for placed in self.slot_cells() {
                    let Some((index, _)) = self.visible_sheet(&placed.cell.sheet) else { continue };
                    let (row, column) = (placed.cell.row as i32, placed.cell.column as i32);
                    let shown = m.get_formatted_cell_value(index, row, column).unwrap_or_default();
                    let value = match (m.get_cell_type(index, row, column), m.get_model().get_cell_value_by_index(index, row, column)) {
                        (Ok(CellType::ErrorValue), _) => shown.clone(),
                        (_, Ok(CellValue::Number(n))) => format!("{n}"),
                        (_, Ok(CellValue::Boolean(b))) => if b { "TRUE".into() } else { "FALSE".into() },
                        (_, Ok(CellValue::String(t))) => format!("'{t}"),
                        _ => String::new(),
                    };
                    let content = m.get_cell_content(index, row, column).unwrap_or_default();
                    let formula = self.shown_input(index, content.clone());
                    // A table result: every spilled cell becomes its value.
                    let (rows, columns) = if content.ends_with('#') {
                        let spill = m.get_worksheets_properties().iter().position(|p| p.name == format!("{}{}", links::SPILL_PREFIX, placed.slot));
                        match spill.and_then(|k| m.get_model().workbook.worksheet(k as u32).ok()).map(|w| w.dimension()) {
                            Some(d) => (d.max_row, d.max_column),
                            None => (1, 1),
                        }
                    } else {
                        (1, 1)
                    };
                    if rows == 1 && columns == 1 {
                        copy.set_user_input(index, row, column, &value).map_err(|e| anyhow!(e))?;
                    }
                    for r in 0..rows {
                        for c in 0..columns {
                            if rows == 1 && columns == 1 { continue; }
                            let (rr, cc) = (row + r, column + c);
                            let v = match (m.get_cell_type(index, rr, cc), m.get_model().get_cell_value_by_index(index, rr, cc)) {
                                (Ok(CellType::ErrorValue), _) => m.get_formatted_cell_value(index, rr, cc).unwrap_or_default(),
                                (_, Ok(CellValue::Number(n))) => format!("{n}"),
                                (_, Ok(CellValue::Boolean(b))) => if b { "TRUE".into() } else { "FALSE".into() },
                                (_, Ok(CellValue::String(t))) => format!("'{t}"),
                                _ => String::new(),
                            };
                            copy.set_user_input(index, rr, cc, &v).map_err(|e| anyhow!(e))?;
                        }
                    }
                    let place = if rows == 1 && columns == 1 {
                        placed.cell.text((false, false))
                    } else {
                        format!("{}:{}{}", placed.cell.text((false, false)), column_name((column + columns - 2) as u32), row + rows - 1)
                    };
                    let what = if rows == 1 && columns == 1 { format!("its value {shown:?}") } else { format!("its {rows}×{columns} values") };
                    notes.push(format!("{place}: the table formula {formula} is exported as {what}; other spreadsheets cannot read Alephgrid tables"));
                }
                // Without the hidden sheets: table results, then the link sheet.
                let hidden: Vec<usize> = copy.get_worksheets_properties().iter().enumerate()
                    .filter(|(_, p)| p.name.starts_with(links::SPILL_PREFIX) && p.state != "visible").map(|(k, _)| k).collect();
                for k in hidden.into_iter().rev() {
                    copy.delete_sheet(k as u32).map_err(|e| anyhow!(e))?;
                }
                let i = copy.get_worksheets_properties().iter().position(|p| p.name == LINK_SHEET).unwrap_or(i);
                copy.delete_sheet(i as u32).map_err(|e| anyhow!(e))?;
                ironcalc::export::save_xlsx_to_writer(copy.get_model(), file)
            }
        }
        .map_err(|e| anyhow!("could not write xlsx: {e}"))?;
        Ok(notes)
    }

    /// What an XLSX export of the sheets cannot keep (F.13), one note per finding, without
    /// writing a file the user sees: table cells exported as values, dynamic arrays that need
    /// Excel 365/2021 to spill, and functions Excel does not have.
    pub fn xlsx_compatibility(&self) -> Result<Vec<String>> {
        let path = std::env::temp_dir().join(format!("alephgrid-compat-{}-{}.xlsx", std::process::id(), crate::storage::now_ms()));
        let notes = self.export_xlsx(&path);
        let _ = std::fs::remove_file(&path);
        let mut notes = notes?;
        let m = self.model()?;
        let catalog: serde_json::Value = serde_json::from_str(include_str!("../data/function_catalog.json"))?;
        let not_excel: Vec<String> = catalog["functions"].as_array().into_iter().flatten()
            .filter(|f| f["sheet_status"] == "registered" && !f["sources"].as_array().is_some_and(|s| s.iter().any(|x| x == "excel")))
            .filter_map(|f| f["name"].as_str().map(str::to_string)).collect();
        for info in self.list() {
            let Ok(ws) = m.get_model().workbook.worksheet(info.index) else { continue };
            for (row, column, cell) in ws.sheet_data.cells() {
                let array = matches!(*cell, ironcalc::base::types::Cell::ArrayFormula { .. });
                if !array && !matches!(*cell, ironcalc::base::types::Cell::CellFormula { .. }) {
                    continue;
                }
                let content = m.get_cell_content(info.index, row, column).unwrap_or_default();
                if links::slot_of(&content).is_some() {
                    continue;
                }
                let at = format!("{}!{}{}", info.name, column_name(column as u32 - 1), row);
                if array {
                    notes.push(format!("{at}: {content} is a dynamic array formula; Excel 365 or 2021 spills it, older versions show one value"));
                }
                let upper = content.to_uppercase();
                for f in &not_excel {
                    let hit = upper.match_indices(&format!("{f}(")).any(|(i, _)| i == 0 || !upper.as_bytes()[i - 1].is_ascii_alphanumeric() && upper.as_bytes()[i - 1] != b'.' && upper.as_bytes()[i - 1] != b'_');
                    if hit {
                        notes.push(format!("{at}: {f} is not an Excel function; Excel will show #NAME?"));
                    }
                }
            }
        }
        Ok(notes)
    }

    /// The used range as typed table columns (first row = header). Reads every row,
    /// independent of viewport fetch limits.
    ///
    /// Type rules per column, over the data rows: all numbers with a date format →
    /// DATE (whole days) or TIMESTAMP; all whole numbers within ±(2^53 − 1) → BIGINT;
    /// all numbers → DOUBLE; all booleans → BOOLEAN; anything else (including errors)
    /// → VARCHAR holding the displayed text. Empty cells become NULL.
    pub fn typed_columns(&self, index: u32) -> Result<Vec<TypedColumn>> {
        let info = self.info(index)?;
        if info.rows < 2 {
            bail!("the sheet needs a header row and at least one data row");
        }
        if info.rows as u64 * info.columns as u64 > MAX_CONVERT_CELLS {
            bail!("the used range is too large to convert ({} rows × {} columns)", info.rows, info.columns);
        }
        let m = self.model()?;
        let model = m.get_model();
        let ws = model.workbook.worksheet(index).map_err(|e| anyhow!(e))?;
        let width = info.columns as usize;
        let data_rows = info.rows as usize - 1;
        #[derive(Clone)]
        enum Cell {
            Empty,
            Number(f64, bool),
            Bool(bool),
            Text(String),
        }
        let mut cells: Vec<Vec<Cell>> = vec![vec![Cell::Empty; data_rows]; width];
        let mut header: Vec<String> = vec![String::new(); width];
        for r in 1..=info.rows as i32 {
            for (c, _) in ws.sheet_data.cells_in_row(r) {
                let ci = (c - 1) as usize;
                if ci >= width {
                    continue;
                }
                let display = m.get_formatted_cell_value(index, r, c).unwrap_or_default();
                if r == 1 {
                    header[ci] = display;
                    continue;
                }
                let cell = match (m.get_cell_type(index, r, c), model.get_cell_value_by_index(index, r, c)) {
                    (Ok(CellType::ErrorValue), _) => Cell::Text(display),
                    (_, Ok(CellValue::Number(n))) => {
                        let fmt = m.get_cell_style(index, r, c).map(|s| s.num_fmt).unwrap_or_default();
                        Cell::Number(n, is_likely_date_number_format(&fmt))
                    }
                    (_, Ok(CellValue::Boolean(b))) => Cell::Bool(b),
                    (_, Ok(CellValue::String(s))) if s.is_empty() => Cell::Empty,
                    (_, Ok(CellValue::String(_))) => Cell::Text(display),
                    _ => Cell::Empty,
                };
                cells[ci][r as usize - 2] = cell;
            }
        }
        let names = column_names(&header);
        let mut out = Vec::with_capacity(width);
        for (ci, column) in cells.into_iter().enumerate() {
            let filled: Vec<&Cell> = column.iter().filter(|c| !matches!(c, Cell::Empty)).collect();
            let all = |f: fn(&Cell) -> bool| !filled.is_empty() && filled.iter().all(|c| f(c));
            let sql_type = if all(|c| matches!(c, Cell::Number(_, true))) {
                if all(|c| matches!(c, Cell::Number(n, _) if n.fract() == 0.0)) { "DATE" } else { "TIMESTAMP" }
            } else if all(|c| matches!(c, Cell::Number(n, _) if n.fract() == 0.0 && n.abs() <= crate::wire::MAX_SAFE_INTEGER as f64)) {
                "BIGINT"
            } else if all(|c| matches!(c, Cell::Number(..))) {
                "DOUBLE"
            } else if all(|c| matches!(c, Cell::Bool(_))) {
                "BOOLEAN"
            } else {
                "VARCHAR"
            };
            let values = column
                .into_iter()
                .map(|c| match c {
                    Cell::Empty => Ok(None),
                    Cell::Number(n, _) if sql_type == "DATE" => Ok(Some(serial_to_date(n)?)),
                    Cell::Number(n, _) if sql_type == "TIMESTAMP" => Ok(Some(serial_to_timestamp(n)?)),
                    Cell::Number(n, _) if sql_type == "BIGINT" => Ok(Some((n as i64).to_string())),
                    // Rust prints the shortest text that parses back to the same double.
                    Cell::Number(n, _) if sql_type == "DOUBLE" => Ok(Some(format!("{n:?}"))),
                    Cell::Number(n, _) => Ok(Some(format_general(n))),
                    Cell::Bool(b) => Ok(Some(if sql_type == "BOOLEAN" { b.to_string() } else { b.to_string().to_uppercase() })),
                    Cell::Text(s) => Ok(Some(s)),
                })
                .collect::<Result<Vec<_>>>()?;
            out.push(TypedColumn { name: names[ci].clone(), sql_type, values });
        }
        Ok(out)
    }
}

/// Parts of an .xlsx package that sheets do not carry over, by package path prefix.
const UNSUPPORTED_PARTS: &[(&str, &str)] = &[
    ("xl/charts/", "Charts are not shown and are left out when you export."),
    ("xl/pivotTables/", "Pivot tables were imported as their last calculated values; they no longer refresh."),
    ("xl/drawings/", "Pictures, shapes and drawings are not shown and are left out when you export."),
    ("xl/vbaProject.bin", "Macros (VBA) were ignored; Alephgrid never runs them."),
    ("xl/externalLinks/", "Links to other workbooks were not followed; their cells keep the last saved values."),
    ("xl/connections.xml", "Data connections and queries were ignored."),
    ("xl/comments", "Comments and notes were not imported."),
    ("xl/threadedComments/", "Threaded comments were not imported."),
    ("xl/slicers/", "Slicers were not imported."),
];

fn xlsx_part_warnings(bytes: &[u8]) -> Vec<String> {
    let Ok(archive) = zip::ZipArchive::new(std::io::Cursor::new(bytes)) else { return vec![] };
    let names: Vec<String> = archive.file_names().filter_map(|n| n.ok().map(|n| n.into_owned())).collect();
    UNSUPPORTED_PARTS
        .iter()
        .filter(|(prefix, _)| names.iter().any(|n| n.starts_with(prefix)))
        .map(|(_, msg)| msg.to_string())
        .collect()
}

/// Cells that evaluate to #NAME? usually call a function the engine does not have.
fn unsupported_function_warning(m: &UserModel<'static>) -> Option<String> {
    let mut count = 0usize;
    let mut names: Vec<String> = Vec::new();
    for (index, _) in m.get_worksheets_properties().iter().enumerate() {
        let Ok(ws) = m.get_model().workbook.worksheet(index as u32) else { continue };
        for (row, col, _) in ws.sheet_data.cells() {
            let index = index as u32;
            if !matches!(m.get_cell_type(index, row, col), Ok(CellType::ErrorValue)) {
                continue;
            }
            if m.get_formatted_cell_value(index, row, col).ok().as_deref() != Some("#NAME?") {
                continue;
            }
            count += 1;
            // For the message only: the identifiers called in the formula text.
            let formula = m.get_cell_content(index, row, col).unwrap_or_default();
            for name in called_names(&formula) {
                if !names.contains(&name) && names.len() < 5 {
                    names.push(name);
                }
            }
        }
    }
    (count > 0).then(|| {
        let list = if names.is_empty() { String::new() } else { format!(" (for example {})", names.join(", ")) };
        format!("{count} cell{} show #NAME? because they use functions or names Alephgrid does not support yet{list}.", if count == 1 { "" } else { "s" })
    })
}

/// Upper-cased identifiers directly followed by `(` outside string literals.
fn called_names(formula: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut word = String::new();
    let mut in_string = false;
    for c in formula.chars() {
        if c == '"' {
            in_string = !in_string;
            word.clear();
            continue;
        }
        if in_string {
            continue;
        }
        if c.is_alphanumeric() || c == '_' || c == '.' {
            word.push(c);
        } else {
            if c == '(' && !word.is_empty() && !word.chars().next().unwrap().is_ascii_digit() {
                out.push(word.to_uppercase());
            }
            word.clear();
        }
    }
    out
}

fn format_general(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

fn serial_to_date(n: f64) -> Result<String> {
    Ok(from_excel_date(n as i64).map_err(|e| anyhow!("invalid date serial {n}: {e}"))?.to_string())
}

fn serial_to_timestamp(n: f64) -> Result<String> {
    let days = n.floor();
    let date = from_excel_date(days as i64).map_err(|e| anyhow!("invalid date serial {n}: {e}"))?;
    let secs = ((n - days) * 86_400.0).round() as u32;
    let time = date
        .and_hms_opt((secs / 3600).min(23), (secs % 3600) / 60, secs % 60)
        .ok_or_else(|| anyhow!("invalid time in serial {n}"))?;
    Ok(time.to_string())
}

/// Table column names from header cells: blanks become `column_N`, duplicates get a
/// suffix, and the internal row id name is never produced.
fn column_names(header: &[String]) -> Vec<String> {
    let mut taken: Vec<String> = Vec::new();
    header
        .iter()
        .enumerate()
        .map(|(i, h)| {
            let base = match h.trim() {
                "" => format!("column_{}", i + 1),
                h if h.eq_ignore_ascii_case("_rf_id") => "rf_id".to_string(),
                h if h.eq_ignore_ascii_case("_rf_ord") => "rf_ord".to_string(),
                h => h.to_string(),
            };
            let mut name = base.clone();
            let mut n = 2;
            while taken.contains(&name.to_lowercase()) {
                name = format!("{base}_{n}");
                n += 1;
            }
            taken.push(name.to_lowercase());
            name
        })
        .collect()
}

fn used_size(m: &UserModel<'static>, index: u32) -> (u32, u32) {
    match m.get_model().workbook.worksheet(index) {
        Ok(ws) if !ws.sheet_data.is_empty() => {
            let d = ws.dimension();
            (d.max_row as u32, d.max_column as u32)
        }
        _ => (0, 0),
    }
}

pub fn column_name(index: u32) -> String {
    let mut n = index + 1;
    let mut s = Vec::new();
    while n > 0 {
        let r = ((n - 1) % 26) as u8;
        s.push((b'A' + r) as char);
        n = (n - 1) / 26;
    }
    s.iter().rev().collect()
}
