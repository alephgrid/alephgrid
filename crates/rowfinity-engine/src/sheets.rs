//! Free-form, Excel-compatible sheets powered by IronCalc.
//!
//! All sheets of a Rowfinity workbook live in one IronCalc model, so formulas can
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

const LANG: &str = "en";
/// Hard cap on cells written in one go (paste, query → sheet).
pub const MAX_BULK_CELLS: usize = 1_000_000;
/// Excel's grid size, which IronCalc enforces.
pub const MAX_ROWS: u32 = 1_048_576;
pub const MAX_COLUMNS: u32 = 16_384;
/// Largest used range converted to a table in one go.
pub const MAX_CONVERT_CELLS: u64 = 20_000_000;

#[derive(Debug, Clone, Serialize, TS)]
pub struct SheetInfo {
    pub index: u32,
    pub name: String,
    /// Used range, 0-based exclusive bounds.
    pub rows: u32,
    pub columns: u32,
}

#[derive(Debug, Clone, Copy, Serialize, TS)]
#[serde(rename_all = "lowercase")]
pub enum Align {
    Left,
    Right,
    Center,
}

/// One rendered cell: `[display, align, input]`. `input` is present only when it
/// differs from `display` (formulas, formatted numbers), to keep frames small.
#[derive(Debug, Clone, Serialize)]
pub struct SheetCell(pub String, pub Align, #[serde(skip_serializing_if = "Option::is_none")] pub Option<String>);

#[derive(Debug, Clone, Serialize, TS)]
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
        Ok(Sheets { model, undo_groups: vec![], redo_groups: vec![] })
    }

    /// Record that the last user operation added `entries` IronCalc history entries.
    fn record(&mut self, entries: u32) {
        if entries > 0 {
            self.undo_groups.push(entries);
            self.redo_groups.clear();
        }
    }

    pub fn to_bytes(&self) -> Option<Vec<u8>> {
        self.model.as_ref().map(|m| m.to_bytes())
    }

    fn model(&self) -> Result<&UserModel<'static>> {
        self.model.as_ref().ok_or_else(|| anyhow!("this workbook has no sheets"))
    }

    fn model_mut(&mut self) -> Result<&mut UserModel<'static>> {
        self.model.as_mut().ok_or_else(|| anyhow!("this workbook has no sheets"))
    }

    fn check_index(&self, index: u32) -> Result<()> {
        let n = self.model()?.get_worksheets_properties().len() as u32;
        if index >= n {
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
        if self.model()?.get_worksheets_properties().len() == 1 {
            // IronCalc needs at least one worksheet; drop the whole model instead.
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
        let columns = column_count.min(16_384);
        let mut rows = Vec::with_capacity(row_count as usize);
        for r in row_start..row_start.saturating_add(row_count.min(5_000)) {
            let mut out: Vec<Option<SheetCell>> = vec![None; columns as usize];
            let mut any = false;
            for (c, _) in ws.sheet_data.cells_in_row(r as i32 + 1) {
                let ci = (c - 1) as u32;
                if ci >= columns {
                    continue;
                }
                let (row, col) = (r as i32 + 1, c);
                let display = m.get_formatted_cell_value(index, row, col).unwrap_or_default();
                let input = m.get_cell_content(index, row, col).unwrap_or_default();
                if display.is_empty() && input.is_empty() {
                    continue;
                }
                let align = match m.get_cell_type(index, row, col) {
                    Ok(CellType::Number) => Align::Right,
                    Ok(CellType::LogicalValue) | Ok(CellType::ErrorValue) => Align::Center,
                    _ => Align::Left,
                };
                let input = (input != display).then_some(input);
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
            bail!("too many cells in one edit (max {MAX_BULK_CELLS})");
        }
        for i in inputs {
            if i.row >= MAX_ROWS || i.column >= MAX_COLUMNS {
                bail!("cell row {} column {} is outside the sheet ({MAX_ROWS} rows × {MAX_COLUMNS} columns)", i.row + 1, i.column + 1);
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
    pub fn import_xlsx(&mut self, path: &std::path::Path) -> Result<Vec<SheetInfo>> {
        let name = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "import".into());
        let bytes = std::fs::read(path)?;
        let workbook = ironcalc::import::load_from_xlsx_bytes(&bytes, &name, "en", "UTC")
            .map_err(|e| anyhow!("could not read xlsx: {e}"))?;
        let imported = ironcalc::base::Model::from_workbook(workbook, LANG).map_err(|e| anyhow!(e))?;
        let mut imported = UserModel::from_model(imported);
        imported.evaluate();
        if self.model.is_none() {
            self.model = Some(imported);
            self.undo_groups.clear();
            self.redo_groups.clear();
            return Ok(self.list());
        }
        let mut created = Vec::new();
        let groups_before = self.undo_groups.len();
        for (i, props) in imported.get_worksheets_properties().into_iter().enumerate() {
            let sheet = self.create(&props.name)?;
            let ws = imported.get_model().workbook.worksheet(i as u32).map_err(|e| anyhow!(e))?;
            let mut inputs = Vec::new();
            for (row, col, _) in ws.sheet_data.cells() {
                if let Ok(content) = imported.get_cell_content(i as u32, row, col) {
                    if !content.is_empty() {
                        inputs.push(SheetInput { row: row as u32 - 1, column: col as u32 - 1, value: content });
                    }
                }
            }
            self.set(sheet.index, &inputs)?;
            created.push(self.info(sheet.index)?);
        }
        // The whole import is one undo step.
        let total: u32 = self.undo_groups.drain(groups_before..).sum();
        self.record(total);
        Ok(created)
    }

    pub fn export_xlsx(&self, path: &std::path::Path) -> Result<()> {
        let file = std::fs::File::create(path)?;
        ironcalc::export::save_xlsx_to_writer(self.model()?.get_model(), file)
            .map_err(|e| anyhow!("could not write xlsx: {e}"))?;
        Ok(())
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
