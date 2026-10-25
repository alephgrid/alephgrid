//! Free-form, Excel-compatible sheets powered by IronCalc.
//!
//! All sheets of a Rowfinity workbook live in one IronCalc model, so formulas can
//! reference each other (`=Budget!B4 * 2`). The model is persisted as a blob inside
//! the workbook's DuckDB file, next to the big columnar tables. Rows and columns
//! are 0-based on the wire and 1-based inside IronCalc.

use anyhow::{anyhow, bail, Result};
use ironcalc::base::types::CellType;
use ironcalc::base::UserModel;
use serde::{Deserialize, Serialize};

const LANG: &str = "en";
/// Hard cap on cells written in one go (paste, query → sheet).
pub const MAX_BULK_CELLS: usize = 1_000_000;

#[derive(Debug, Clone, Serialize)]
pub struct SheetInfo {
    pub index: u32,
    pub name: String,
    /// Used range, 0-based exclusive bounds.
    pub rows: u32,
    pub columns: u32,
}

#[derive(Debug, Clone, Copy, Serialize)]
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

#[derive(Debug, Clone, Serialize)]
pub struct SheetBlock {
    pub row_start: u32,
    /// `rows[i][j]` is `null` for empty cells.
    pub rows: Vec<Vec<Option<SheetCell>>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SheetInput {
    pub row: u32,
    pub column: u32,
    /// Text exactly as typed: `42`, `hello`, `=SUM(A1:A9)`. Empty clears the cell.
    pub value: String,
}

pub struct Sheets {
    model: Option<UserModel<'static>>,
}

impl Sheets {
    pub fn from_bytes(bytes: Option<Vec<u8>>) -> Result<Sheets> {
        let model = match bytes {
            Some(b) => Some(UserModel::from_bytes(&b, LANG).map_err(|e| anyhow!("corrupt sheet data: {e}"))?),
            None => None,
        };
        Ok(Sheets { model })
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
        let index = match &mut self.model {
            None => {
                self.model = Some(UserModel::new_empty("workbook", "en", "UTC", LANG).map_err(|e| anyhow!(e))?);
                0
            }
            Some(m) => {
                m.new_sheet().map_err(|e| anyhow!(e))?;
                m.get_worksheets_properties().len() as u32 - 1
            }
        };
        if !name.is_empty() {
            let unique = self.unique_name(name, index);
            self.model_mut()?.rename_sheet(index, &unique).map_err(|e| anyhow!(e))?;
        }
        self.info(index)
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
        self.model_mut()?.rename_sheet(index, name.trim()).map_err(|e| anyhow!(e))
    }

    pub fn delete(&mut self, index: u32) -> Result<()> {
        self.check_index(index)?;
        if self.model()?.get_worksheets_properties().len() == 1 {
            // IronCalc needs at least one worksheet; drop the whole model instead.
            self.model = None;
            return Ok(());
        }
        self.model_mut()?.delete_sheet(index).map_err(|e| anyhow!(e))
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

    pub fn set(&mut self, index: u32, inputs: &[SheetInput]) -> Result<()> {
        self.check_index(index)?;
        if inputs.len() > MAX_BULK_CELLS {
            bail!("too many cells in one edit (max {MAX_BULK_CELLS})");
        }
        let m = self.model_mut()?;
        m.pause_evaluation();
        let result = (|| -> Result<()> {
            for i in inputs {
                m.set_user_input(index, i.row as i32 + 1, i.column as i32 + 1, &i.value)
                    .map_err(|e| anyhow!("{}{}: {e}", column_name(i.column), i.row + 1))?;
            }
            Ok(())
        })();
        m.resume_evaluation();
        m.evaluate();
        result
    }

    pub fn undo(&mut self) -> Result<bool> {
        let m = self.model_mut()?;
        if !m.can_undo() {
            return Ok(false);
        }
        m.undo().map_err(|e| anyhow!(e))?;
        Ok(true)
    }

    pub fn redo(&mut self) -> Result<bool> {
        let m = self.model_mut()?;
        if !m.can_redo() {
            return Ok(false);
        }
        m.redo().map_err(|e| anyhow!(e))?;
        Ok(true)
    }

    pub fn insert_rows(&mut self, index: u32, at: u32, count: u32) -> Result<()> {
        self.check_index(index)?;
        self.model_mut()?.insert_rows(index, at as i32 + 1, count as i32).map_err(|e| anyhow!(e))
    }

    pub fn delete_rows(&mut self, index: u32, at: u32, count: u32) -> Result<()> {
        self.check_index(index)?;
        self.model_mut()?.delete_rows(index, at as i32 + 1, count as i32).map_err(|e| anyhow!(e))
    }

    pub fn insert_columns(&mut self, index: u32, at: u32, count: u32) -> Result<()> {
        self.check_index(index)?;
        self.model_mut()?.insert_columns(index, at as i32 + 1, count as i32).map_err(|e| anyhow!(e))
    }

    pub fn delete_columns(&mut self, index: u32, at: u32, count: u32) -> Result<()> {
        self.check_index(index)?;
        self.model_mut()?.delete_columns(index, at as i32 + 1, count as i32).map_err(|e| anyhow!(e))
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
            return Ok(self.list());
        }
        let mut created = Vec::new();
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
        Ok(created)
    }

    pub fn export_xlsx(&self, path: &std::path::Path) -> Result<()> {
        let file = std::fs::File::create(path)?;
        ironcalc::export::save_xlsx_to_writer(self.model()?.get_model(), file)
            .map_err(|e| anyhow!("could not write xlsx: {e}"))?;
        Ok(())
    }

    /// Displayed values of a sheet's used range, row-major (for sheet → table).
    pub fn used_values(&self, index: u32) -> Result<Vec<Vec<String>>> {
        let info = self.info(index)?;
        let block = self.fetch(index, 0, info.rows, info.columns)?;
        Ok(block
            .rows
            .into_iter()
            .map(|row| {
                let mut out: Vec<String> = row.into_iter().map(|c| c.map(|c| c.0).unwrap_or_default()).collect();
                out.resize(info.columns as usize, String::new());
                out
            })
            .collect())
    }
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
