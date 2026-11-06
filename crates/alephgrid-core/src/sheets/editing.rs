//! Spreadsheet editing on top of IronCalc: named ranges, formula problems and error
//! origins, fill down/right and formula-aware copy, move and paste special.
//!
//! Formula translation reuses IronCalc's parser and stringifier: a formula is parsed in
//! the context of its source cell and written out in the context of its target cell, so
//! relative references shift, absolute ones stay and references qualified with a sheet
//! keep that sheet. Every operation validates its whole area before the first change and
//! is one undo step.

use anyhow::{anyhow, bail, Result};
use ironcalc::base::expressions::parser::stringify::to_localized_string;
use ironcalc::base::expressions::parser::{new_parser_english, Node};
use ironcalc::base::expressions::types::CellReferenceRC;
use ironcalc::base::types::{Cell, FormulaValue};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::{SheetInput, Sheets, MAX_BULK_CELLS, MAX_COLUMNS, MAX_ROWS};

/// A named cell or range. `scope` is the sheet index of a sheet-level name, `null` for a
/// workbook-level name. Workbook functions (LAMBDAs) are listed separately.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct NamedRange {
    pub name: String,
    pub scope: Option<u32>,
    /// What the name refers to, e.g. `=Sheet1!$A$1:$B$4`.
    pub refers_to: String,
}

/// Where an error shown in a cell comes from.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct CellError {
    /// The error as displayed, e.g. `#DIV/0!`.
    pub code: String,
    /// The cell that produced it, e.g. `Sheet1!B3`; empty for a typed error value.
    pub origin: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct CellDetail {
    pub input: String,
    pub display: String,
    pub error: Option<CellError>,
}

/// A formula the parser rejects, with the character offset (in the text as typed,
/// including a leading `=`) where it stopped.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct FormulaProblem {
    pub message: String,
    pub position: u32,
}

/// A rectangle of cells, 0-based.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, TS)]
pub struct CellArea {
    pub row: u32,
    pub column: u32,
    pub rows: u32,
    pub columns: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, TS)]
#[serde(rename_all = "lowercase")]
pub enum FillDirection {
    Down,
    Right,
}

/// What a paste writes. `all`: values, formulas and formats; `formulas`: values and
/// formulas without formats; `values`: calculated values only; `formats`: formats only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, TS)]
#[serde(rename_all = "lowercase")]
pub enum PasteMode {
    All,
    Formulas,
    Values,
    Formats,
}

/// One source cell, read before anything is written.
struct Source {
    input: Option<String>,
    is_text: bool,
    value: Option<String>,
    value_is_text: bool,
}

impl CellArea {
    fn cells(&self) -> u64 {
        self.rows as u64 * self.columns as u64
    }

    pub(crate) fn check(&self, what: &str) -> Result<()> {
        if self.rows == 0 || self.columns == 0 {
            bail!("the {what} area is empty");
        }
        if self.row as u64 + self.rows as u64 > MAX_ROWS as u64 || self.column as u64 + self.columns as u64 > MAX_COLUMNS as u64 {
            bail!("the {what} area extends outside the sheet ({MAX_ROWS} rows × {MAX_COLUMNS} columns)");
        }
        if self.cells() > MAX_BULK_CELLS as u64 {
            bail!(crate::limits::TooLarge(format!("too many cells in one edit (max {MAX_BULK_CELLS})")));
        }
        Ok(())
    }

}

fn is_lambda(formula: &str) -> bool {
    formula.trim().trim_start_matches('=').trim_start().to_ascii_uppercase().starts_with("LAMBDA(")
}

impl Sheets {
    // ------------------------------------------------------------ named ranges

    /// Named cells and ranges, sorted by name then scope. LAMBDA definitions (workbook
    /// functions) are not named ranges and are left out.
    pub fn names(&self) -> Vec<NamedRange> {
        let Some(m) = &self.model else { return vec![] };
        let mut out: Vec<NamedRange> = m
            .get_defined_name_list()
            .into_iter()
            .filter(|(_, _, f)| !is_lambda(f))
            .map(|(name, scope, f)| NamedRange { name, scope, refers_to: if f.starts_with('=') { f } else { format!("={f}") } })
            .collect();
        out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then(a.scope.cmp(&b.scope)));
        out
    }

    fn check_name(&self, name: &str, scope: Option<u32>, refers_to: &str) -> Result<()> {
        if name.is_empty() {
            bail!("a name cannot be empty");
        }
        if let Some(s) = scope {
            self.check_index(s)?;
        }
        if super::links::mentions_link_sheet(refers_to) {
            bail!("{} is reserved for Alephgrid's own use", super::LINK_SHEET);
        }
        if ironcalc::base::language::get_default_language().functions.lookup(name).is_some() {
            bail!("{name} is a built-in function name; choose another name");
        }
        let m = self.model()?;
        if m.get_defined_name_list().iter().any(|(n, _, f)| n.eq_ignore_ascii_case(name) && is_lambda(f)) {
            bail!("{name} is a workbook function; choose another name");
        }
        if is_lambda(refers_to) {
            bail!("a named range must refer to cells; define functions in the Functions panel");
        }
        Ok(())
    }

    /// IronCalc accepts any reference or LAMBDA; this API accepts references only.
    fn name_error(e: String, refers_to: &str) -> anyhow::Error {
        match e.split_once(": ") {
            Some(("Formula", _)) => anyhow!("{refers_to:?} is not a cell or range reference (use e.g. =Sheet1!$A$1:$B$4)"),
            Some(("Name", "Invalid defined name")) => anyhow!("invalid name: use letters, digits, _ or ., starting with a letter or _, and not a cell reference such as A1"),
            Some(("Name", "Defined name already exists")) => anyhow!("that name already exists in this scope"),
            Some((_, rest)) => anyhow!("{rest}"),
            None => anyhow!(e),
        }
    }

    pub fn define_name(&mut self, name: &str, scope: Option<u32>, refers_to: &str) -> Result<()> {
        let name = name.trim();
        let refers_to = refers_to.trim();
        self.check_name(name, scope, refers_to)?;
        // IronCalc takes the reference without the leading `=` that users type.
        let target = refers_to.strip_prefix('=').unwrap_or(refers_to);
        let groups = self.undo_groups.len();
        self.model_mut()?.new_defined_name(name, scope, target).map_err(|e| Self::name_error(e, refers_to))?;
        self.record(1);
        let result = self.respill_array_results();
        self.merge_groups_since(groups);
        result
    }

    /// IronCalc fixes a cell's scalar/array kind when the formula is entered, so a formula
    /// typed before its name referred to several cells shows #VALUE! afterwards. Enter such
    /// cells again (same text) so they spill as they would if typed now. Bounded by the
    /// used cells of the workbook; the caller folds these writes into its undo step.
    fn respill_array_results(&mut self) -> Result<()> {
        let mut redo = Vec::new();
        {
            let m = self.model()?;
            for (index, ws) in m.get_model().workbook.worksheets.iter().enumerate() {
                for row in ws.sheet_data.rows() {
                    for (column, cell) in ws.sheet_data.cells_in_row(row) {
                        if let Cell::CellFormula { v: FormulaValue::Error { d, .. }, .. } = cell.as_ref() {
                            if d.m == "Array result in scalar context" {
                                redo.push((index as u32, row, column, m.get_cell_content(index as u32, row, column).map_err(|e| anyhow!(e))?));
                            }
                        }
                    }
                }
            }
        }
        if redo.is_empty() {
            return Ok(());
        }
        let m = self.model_mut()?;
        m.pause_evaluation();
        let mut entries = 0;
        let result = (|| -> Result<()> {
            for (index, row, column, content) in &redo {
                m.set_user_input(*index, *row, *column, content).map_err(|e| anyhow!(e))?;
                entries += 1;
            }
            Ok(())
        })();
        m.resume_evaluation();
        m.evaluate();
        self.record(entries);
        result
    }

    /// Rename, rescope or repoint a name. Formulas using it follow a rename.
    pub fn update_name(&mut self, name: &str, scope: Option<u32>, new_name: &str, new_scope: Option<u32>, refers_to: &str) -> Result<()> {
        let new_name = new_name.trim();
        let refers_to = refers_to.trim();
        if !self.names().iter().any(|n| n.name.eq_ignore_ascii_case(name) && n.scope == scope) {
            bail!("no named range {name:?} in that scope");
        }
        self.check_name(new_name, new_scope, refers_to)?;
        let target = refers_to.strip_prefix('=').unwrap_or(refers_to);
        let groups = self.undo_groups.len();
        self.model_mut()?.update_defined_name(name, scope, new_name, new_scope, target).map_err(|e| Self::name_error(e, refers_to))?;
        self.record(1);
        let result = self.respill_array_results();
        self.merge_groups_since(groups);
        result
    }

    /// Formulas that used the name show #NAME? afterwards, as in other spreadsheets.
    pub fn delete_name(&mut self, name: &str, scope: Option<u32>) -> Result<()> {
        if !self.names().iter().any(|n| n.name.eq_ignore_ascii_case(name) && n.scope == scope) {
            bail!("no named range {name:?} in that scope");
        }
        self.model_mut()?.delete_defined_name(name, scope).map_err(|e| anyhow!(e))?;
        self.record(1);
        Ok(())
    }

    // ------------------------------------------------------------ problems and origins

    /// The input, display and (for an error) its origin and message.
    pub fn cell_detail(&self, index: u32, row: u32, column: u32) -> Result<CellDetail> {
        self.check_index(index)?;
        let m = self.model()?;
        let (r, c) = (row as i32 + 1, column as i32 + 1);
        let display = m.get_formatted_cell_value(index, r, c).map_err(|e| anyhow!(e))?;
        let input = self.shown_input(index, m.get_cell_content(index, r, c).map_err(|e| anyhow!(e))?);
        let cell = m.get_model().workbook.worksheet(index).map_err(|e| anyhow!(e))?.cell(r, c).map(|c| c.into_owned());
        let error = match cell {
            Some(Cell::CellFormula { v: FormulaValue::Error { d, .. }, .. } | Cell::ArrayFormula { v: FormulaValue::Error { d, .. }, .. }) => {
                Some(CellError { code: display.clone(), origin: d.o.clone(), message: d.m.clone() })
            }
            Some(Cell::ErrorCell { .. }) => Some(CellError { code: display.clone(), origin: String::new(), message: "Typed error value".into() }),
            _ => None,
        };
        Ok(CellDetail { input, display, error })
    }

    /// Parse `formula` as if typed into the cell; `None` when it parses (or is not a formula).
    pub fn check_formula(&self, index: u32, row: u32, column: u32, formula: &str) -> Result<Option<FormulaProblem>> {
        self.check_index(index)?;
        let lead = formula.len() - formula.trim_start().len();
        let Some(body) = formula.trim_start().strip_prefix('=') else { return Ok(None) };
        let m = self.model()?.get_model();
        let wb = &m.workbook;
        let mut parser = new_parser_english(wb.get_worksheet_names(), wb.get_defined_names_with_scope(), wb.tables.clone());
        let context = CellReferenceRC { sheet: wb.worksheet(index).map_err(|e| anyhow!(e))?.get_name(), row: row as i32 + 1, column: column as i32 + 1 };
        let first = parser.parse(body, &context);
        // IronCalc completes one missing final `)` when it stores a formula; accept what it stores.
        if matches!(first, Node::ParseErrorKind { .. }) && !matches!(parser.parse(&format!("{body})"), &context), Node::ParseErrorKind { .. }) {
            return Ok(None);
        }
        Ok(match first {
            Node::ParseErrorKind { message, position, .. } => {
                // `position` counts characters of `body`; report it in the typed text.
                let before = formula[..lead].chars().count() + 1;
                Some(FormulaProblem { message, position: (before + position.min(body.chars().count())) as u32 })
            }
            _ => None,
        })
    }

    // ------------------------------------------------------------ fill, copy, move

    /// Read the source cells of a copy before anything changes.
    fn read_area(&self, index: u32, area: &CellArea) -> Result<Vec<Vec<Source>>> {
        let m = self.model()?;
        let ws = m.get_model().workbook.worksheet(index).map_err(|e| anyhow!(e))?;
        let mut out = Vec::with_capacity(area.rows as usize);
        for dr in 0..area.rows {
            let mut row = Vec::with_capacity(area.columns as usize);
            for dc in 0..area.columns {
                let (r, c) = ((area.row + dr) as i32 + 1, (area.column + dc) as i32 + 1);
                let cell = ws.cell(r, c).map(|c| c.into_owned());
                let source = match cell {
                    None | Some(Cell::EmptyCell { .. }) => Source { input: None, is_text: false, value: None, value_is_text: false },
                    // Spilled values belong to their anchor formula, which is copied instead.
                    Some(Cell::SpillCell { .. }) => Source { input: None, is_text: false, value: None, value_is_text: false },
                    Some(cell) => {
                        let input = m.get_cell_content(index, r, c).map_err(|e| anyhow!(e))?;
                        let is_text = matches!(cell, Cell::SharedString { .. });
                        let value = m.get_model().get_cell_value_by_index(index, r, c).map_err(|e| anyhow!(e))?;
                        let (value, value_is_text) = match value {
                            ironcalc::base::cell::CellValue::None => (None, false),
                            ironcalc::base::cell::CellValue::String(s) => {
                                // Error results read back as their code and paste as that error.
                                let is_error = matches!(&cell, Cell::CellFormula { v: FormulaValue::Error { .. }, .. } | Cell::ErrorCell { .. });
                                (Some(s), !is_error)
                            }
                            ironcalc::base::cell::CellValue::Number(n) => (Some(format!("{n}")), false),
                            ironcalc::base::cell::CellValue::Boolean(b) => (Some(if b { "TRUE" } else { "FALSE" }.to_string()), false),
                        };
                        Source { input: Some(input), is_text, value, value_is_text }
                    }
                };
                row.push(source);
            }
            out.push(row);
        }
        Ok(out)
    }

    /// The formula `input`, typed at source cell `from`, as it reads when copied to `to`
    /// (sheet, row, column). The source sheet does not matter: see below.
    pub(super) fn translate(&self, input: &str, from: (u32, u32), to: (u32, u32, u32)) -> Result<String> {
        let Some(body) = input.strip_prefix('=') else { return Ok(input.to_string()) };
        let m = self.model()?.get_model();
        let wb = &m.workbook;
        let mut parser = new_parser_english(wb.get_worksheet_names(), wb.get_defined_names_with_scope(), wb.tables.clone());
        let sheet_name = |i: u32| wb.worksheet(i).map(|w| w.get_name()).map_err(|e| anyhow!(e));
        // Parse as if the source cell were on the target sheet: unqualified references then
        // stay unqualified (relative to the destination sheet), as in other spreadsheets,
        // while references naming a sheet keep naming it.
        let source = CellReferenceRC { sheet: sheet_name(to.0)?, row: from.0 as i32 + 1, column: from.1 as i32 + 1 };
        let target = CellReferenceRC { sheet: sheet_name(to.0)?, row: to.1 as i32 + 1, column: to.2 as i32 + 1 };
        let node = parser.parse(body, &source);
        if let Node::ParseErrorKind { .. } = node {
            // An unparsable formula is copied as typed, as IronCalc stores it.
            return Ok(input.to_string());
        }
        let language = ironcalc::base::language::get_default_language();
        let locale = ironcalc::base::locale::get_default_locale();
        Ok(format!("={}", to_localized_string(&node, &target, locale, language)))
    }

    fn style_grid(&self, index: u32, area: &CellArea) -> Result<Vec<Vec<ironcalc::base::types::Style>>> {
        let m = self.model()?;
        (0..area.rows)
            .map(|dr| (0..area.columns).map(|dc| m.get_cell_style(index, (area.row + dr) as i32 + 1, (area.column + dc) as i32 + 1).map_err(|e| anyhow!(e))).collect())
            .collect()
    }

    /// Write `inputs` (text flags keep text text) and optionally `styles` over `target`,
    /// as one undo step.
    fn write(&mut self, index: u32, inputs: Vec<(SheetInput, bool)>, styles: Option<(CellArea, Vec<Vec<ironcalc::base::types::Style>>)>) -> Result<()> {
        let groups = self.undo_groups.len();
        let result = (|| {
            self.set_values(index, &inputs)?;
            if let Some((area, styles)) = styles {
                let m = self.model_mut()?;
                m.set_selected_sheet(index).map_err(|e| anyhow!(e))?;
                let (r0, c0) = (area.row as i32 + 1, area.column as i32 + 1);
                m.set_selected_cell(r0, c0).map_err(|e| anyhow!(e))?;
                m.set_selected_range(r0, c0, r0 + area.rows as i32 - 1, c0 + area.columns as i32 - 1).map_err(|e| anyhow!(e))?;
                m.on_paste_styles(&styles).map_err(|e| anyhow!(e))?;
                self.record(1);
            }
            Ok(())
        })();
        self.merge_groups_since(groups);
        result
    }

    /// Fold the undo groups recorded since `len` into one user step.
    fn merge_groups_since(&mut self, len: usize) {
        if self.undo_groups.len() > len + 1 {
            let total: u32 = self.undo_groups.drain(len..).sum();
            self.undo_groups.push(total);
        }
    }

    /// Ctrl+D / Ctrl+R: copy the first row (down) or first column (right) of `area` over
    /// the rest of it, translating formulas and copying formats.
    pub fn fill(&mut self, index: u32, area: CellArea, direction: FillDirection) -> Result<()> {
        self.check_index(index)?;
        area.check("fill")?;
        let (span, len) = match direction {
            FillDirection::Down => (CellArea { rows: 1, ..area }, area.rows),
            FillDirection::Right => (CellArea { columns: 1, ..area }, area.columns),
        };
        if len < 2 {
            return Ok(());
        }
        let sources = self.read_area(index, &span)?;
        let source_styles = self.style_grid(index, &span)?;
        let mut inputs = Vec::new();
        let mut styles = vec![vec![ironcalc::base::types::Style::default(); area.columns as usize]; area.rows as usize];
        for dr in 0..area.rows {
            for dc in 0..area.columns {
                let (sr, sc) = match direction { FillDirection::Down => (0, dc), FillDirection::Right => (dr, 0) };
                let s = &sources[sr as usize][sc as usize];
                styles[dr as usize][dc as usize] = source_styles[sr as usize][sc as usize].clone();
                if (dr, dc) == (sr, sc) {
                    continue;
                }
                let (row, column) = (area.row + dr, area.column + dc);
                let value = match s.input.clone() {
                    Some(i) => self.copy_input(&i, (span.row + sr, span.column + sc), (index, row, column))?,
                    None => String::new(),
                };
                inputs.push((SheetInput { row, column, value, base: None }, s.is_text));
            }
        }
        self.write(index, inputs, Some((area, styles)))
    }

    /// Drag the fill handle: extend `source` to `target`, which contains it and grows it in
    /// one direction (down, up, right or left). Along each line a run of two or more plain
    /// numbers continues as a linear trend (1, 2 → 3, 4; 10, 7 → 4, 1), and a single text
    /// cell ending in a whole number counts on (Item 1 → Item 2). Weekday, month and
    /// quarter names continue by name (Mon → Tue, Jan → Feb, Q4 → Q1; two names set the
    /// step). Dates (numbers with a date format) count on by day from one cell, by month
    /// when the cells share their day of the month (2024-01-15, 2024-02-15 → 2024-03-15),
    /// and otherwise follow the trend. Anything else repeats the source pattern,
    /// translating formulas as a copy would. Formats repeat too. One undo step; the source
    /// cells are not rewritten.
    pub fn drag_fill(&mut self, index: u32, source: CellArea, target: CellArea) -> Result<()> {
        self.check_index(index)?;
        source.check("fill source")?;
        target.check("fill")?;
        let same_columns = target.column == source.column && target.columns == source.columns;
        let same_rows = target.row == source.row && target.rows == source.rows;
        let end = |a: u32, n: u32| a + n;
        let vertical = same_columns && target.rows > source.rows
            && (target.row == source.row || end(target.row, target.rows) == end(source.row, source.rows));
        let horizontal = same_rows && target.columns > source.columns
            && (target.column == source.column || end(target.column, target.columns) == end(source.column, source.columns));
        if !vertical && !horizontal {
            if target == source {
                return Ok(());
            }
            bail!("a fill extends the selected cells in one direction");
        }
        let sources = self.read_area(index, &source)?;
        let source_styles = self.style_grid(index, &source)?;
        // Position along the fill axis relative to the source's first line; negative when
        // filling up or left.
        let (n, offset) = if vertical {
            (source.rows as i64, target.row as i64 - source.row as i64)
        } else {
            (source.columns as i64, target.column as i64 - source.column as i64)
        };
        let lines = if vertical { source.columns } else { source.rows };
        let cell = |line: u32, k: usize| -> &Source { if vertical { &sources[k][line as usize] } else { &sources[line as usize][k] } };
        let style = |line: u32, k: usize| -> &ironcalc::base::types::Style {
            if vertical { &source_styles[k][line as usize] } else { &source_styles[line as usize][k] }
        };
        enum Plan { Trend(f64, f64), Count(String, u64, usize), Names(&'static [&'static str], usize, usize, Case), Months(i64, u32, i64), Repeat }
        let mut plans = Vec::with_capacity(lines as usize);
        for line in 0..lines {
            let numbers: Option<Vec<f64>> = (0..n as usize).map(|k| plain_number(cell(line, k))).collect();
            let dates = numbers.is_some()
                && (0..n as usize).all(|k| ironcalc::base::formatter::lexer::is_likely_date_number_format(&style(line, k).num_fmt));
            let names: Option<Vec<(&'static [&'static str], usize, Case)>> = (0..n as usize)
                .map(|k| { let s = cell(line, k); if s.is_text { s.input.as_deref().and_then(series_name) } else { None } })
                .collect();
            let plan = match (numbers, names) {
                (Some(ys), _) if dates && ys.len() == 1 => Plan::Trend(ys[0], 1.0),
                (Some(ys), _) if dates && month_step(&ys).is_some() => {
                    let (y, m, d) = civil_from_serial(ys[0] as i64);
                    Plan::Months(y * 12 + m as i64 - 1, d, month_step(&ys).unwrap_or(1))
                }
                (_, Some(names)) => {
                    let (list, first, case) = names[0];
                    let len = list.len();
                    let steps: Vec<usize> = names.windows(2).map(|w| (w[1].1 + len - w[0].1) % len).collect();
                    let same_list = names.iter().all(|(l, _, _)| std::ptr::eq(*l, list));
                    match steps.first().copied() {
                        None => Plan::Names(list, first, 1, case),
                        Some(step) if same_list && step != 0 && steps.iter().all(|s| *s == step) => Plan::Names(list, first, step, case),
                        _ => Plan::Repeat,
                    }
                }
                (Some(ys), _) if ys.len() >= 2 => {
                    // Least-squares line through (0, y0), (1, y1), …: exact for evenly spaced runs.
                    let m = ys.len() as f64;
                    let mean_x = (m - 1.0) / 2.0;
                    let mean_y = ys.iter().sum::<f64>() / m;
                    let (mut sxy, mut sxx) = (0.0, 0.0);
                    for (i, y) in ys.iter().enumerate() {
                        let dx = i as f64 - mean_x;
                        sxy += dx * (y - mean_y);
                        sxx += dx * dx;
                    }
                    let slope = sxy / sxx;
                    Plan::Trend(mean_y - slope * mean_x, slope)
                }
                _ => match (n, cell(line, 0)) {
                    (1, s) if s.is_text => match s.input.as_deref().and_then(trailing_number) {
                        Some((prefix, number, width)) => Plan::Count(prefix, number, width),
                        None => Plan::Repeat,
                    },
                    _ => Plan::Repeat,
                },
            };
            plans.push(plan);
        }
        let mut inputs = Vec::new();
        let mut styles = vec![vec![ironcalc::base::types::Style::default(); target.columns as usize]; target.rows as usize];
        for dr in 0..target.rows {
            for dc in 0..target.columns {
                let (row, column) = (target.row + dr, target.column + dc);
                let (line, along) = if vertical { (dc, dr as i64) } else { (dr, dc as i64) };
                let k = along + offset; // index relative to the source's first line
                let src = k.rem_euclid(n) as usize;
                let (sr, sc) = if vertical { (src, line as usize) } else { (line as usize, src) };
                styles[dr as usize][dc as usize] = source_styles[sr][sc].clone();
                if (0..n).contains(&k) {
                    continue;
                }
                let s = &sources[sr][sc];
                let (value, text) = match &plans[line as usize] {
                    Plan::Trend(a, b) => (format_number(a + b * k as f64), false),
                    Plan::Count(prefix, number, width) => {
                        let v = (*number as i64 + k).unsigned_abs();
                        (format!("{prefix}{v:0width$}"), true)
                    }
                    Plan::Names(list, first, step, case) => {
                        let len = list.len() as i64;
                        let at = (*first as i64 + *step as i64 * k).rem_euclid(len) as usize;
                        (case.apply(list[at]), true)
                    }
                    Plan::Months(first, day, step) => {
                        let month = first + step * k;
                        let (y, m) = (month.div_euclid(12), month.rem_euclid(12) as u32 + 1);
                        (format!("{}", serial_from_civil(y, m, (*day).min(days_in_month(y, m)))), false)
                    }
                    Plan::Repeat => match s.input.clone() {
                        Some(i) => {
                            let from = (source.row + sr as u32, source.column + sc as u32);
                            (self.copy_input(&i, from, (index, row, column))?, s.is_text)
                        }
                        None => (String::new(), false),
                    },
                };
                inputs.push((SheetInput { row, column, value, base: None }, text));
            }
        }
        self.write(index, inputs, Some((target, styles)))
    }

    /// The block of filled cells around `(row, column)`, bounded by empty rows and columns
    /// (what Ctrl+A selects first). A lone empty cell is its own region.
    pub fn current_region(&self, index: u32, row: u32, column: u32) -> Result<CellArea> {
        self.check_index(index)?;
        let m = self.model()?;
        let ws = m.get_model().workbook.worksheet(index).map_err(|e| anyhow!(e))?;
        let filled = |r: i64, c: i64| -> bool {
            r >= 0 && c >= 0 && r < MAX_ROWS as i64 && c < MAX_COLUMNS as i64
                && !matches!(ws.cell(r as i32 + 1, c as i32 + 1).as_deref(), None | Some(Cell::EmptyCell { .. }))
        };
        let (mut r0, mut r1, mut c0, mut c1) = (row as i64, row as i64, column as i64, column as i64);
        loop {
            let mut grew = false;
            if (c0 - 1..=c1 + 1).any(|c| filled(r0 - 1, c)) { r0 -= 1; grew = true; }
            if (c0 - 1..=c1 + 1).any(|c| filled(r1 + 1, c)) { r1 += 1; grew = true; }
            if (r0 - 1..=r1 + 1).any(|r| filled(r, c0 - 1)) { c0 -= 1; grew = true; }
            if (r0 - 1..=r1 + 1).any(|r| filled(r, c1 + 1)) { c1 += 1; grew = true; }
            if !grew { break; }
        }
        Ok(CellArea { row: r0 as u32, column: c0 as u32, rows: (r1 - r0 + 1) as u32, columns: (c1 - c0 + 1) as u32 })
    }

    /// Paste the `source` area of sheet `from` at `at` on sheet `index`. Formulas are
    /// translated as when copying; `transpose` swaps rows and columns.
    pub fn paste(&mut self, from: u32, source: CellArea, index: u32, at: (u32, u32), mode: PasteMode, transpose: bool) -> Result<CellArea> {
        self.check_index(from)?;
        self.check_index(index)?;
        source.check("copied")?;
        let target = if transpose {
            CellArea { row: at.0, column: at.1, rows: source.columns, columns: source.rows }
        } else {
            CellArea { row: at.0, column: at.1, rows: source.rows, columns: source.columns }
        };
        target.check("paste")?;
        let sources = self.read_area(from, &source)?;
        let source_styles = if matches!(mode, PasteMode::All | PasteMode::Formats) { Some(self.style_grid(from, &source)?) } else { None };
        let mut inputs = Vec::new();
        let mut styles = vec![vec![ironcalc::base::types::Style::default(); target.columns as usize]; target.rows as usize];
        for dr in 0..source.rows {
            for dc in 0..source.columns {
                let (tr, tc) = if transpose { (dc, dr) } else { (dr, dc) };
                if let Some(st) = &source_styles {
                    styles[tr as usize][tc as usize] = st[dr as usize][dc as usize].clone();
                }
                if mode == PasteMode::Formats {
                    continue;
                }
                let s = &sources[dr as usize][dc as usize];
                let (row, column) = (target.row + tr, target.column + tc);
                let (value, text) = match mode {
                    PasteMode::Values => (s.value.clone().unwrap_or_default(), s.value_is_text),
                    _ => match &s.input {
                        Some(i) => (self.copy_input(i, (source.row + dr, source.column + dc), (index, row, column))?, s.is_text),
                        None => (String::new(), false),
                    },
                };
                inputs.push((SheetInput { row, column, value, base: None }, text));
            }
        }
        let styles = source_styles.map(|_| (target, styles));
        self.write(index, inputs, styles)?;
        Ok(target)
    }

    /// Cut and paste within one sheet: cells move with their formats, formulas elsewhere
    /// that refer to them follow, and references inside the moved area keep pointing at
    /// the same cells. Uses IronCalc's own cut/paste.
    pub fn move_cells(&mut self, index: u32, source: CellArea, at: (u32, u32)) -> Result<CellArea> {
        self.check_index(index)?;
        source.check("moved")?;
        let target = CellArea { row: at.0, column: at.1, ..source };
        target.check("paste")?;
        if (target.row, target.column) == (source.row, source.column) {
            return Ok(target);
        }
        let m = self.model_mut()?;
        m.set_selected_sheet(index).map_err(|e| anyhow!(e))?;
        let (r0, c0) = (source.row as i32 + 1, source.column as i32 + 1);
        let (r1, c1) = (r0 + source.rows as i32 - 1, c0 + source.columns as i32 - 1);
        m.set_selected_cell(r0, c0).map_err(|e| anyhow!(e))?;
        m.set_selected_range(r0, c0, r1, c1).map_err(|e| anyhow!(e))?;
        let clipboard = serde_json::to_value(m.copy_to_clipboard().map_err(|e| anyhow!(e))?)?;
        let data = serde_json::from_value(clipboard["data"].clone())?;
        // The clipboard's range is clamped to the used area; pass the full source range
        // so the whole area moves (empty cells included).
        m.set_selected_cell(target.row as i32 + 1, target.column as i32 + 1).map_err(|e| anyhow!(e))?;
        m.paste_from_clipboard(index, (r0, c0, r1, c1), &data, true).map_err(|e| anyhow!(e))?;
        self.record(1);
        // Cells moved: addresses read before now name other content.
        self.moved(index);
        Ok(target)
    }
}

/// A constant number cell (not text, a formula or a boolean), for series fills.
fn plain_number(s: &Source) -> Option<f64> {
    let input = s.input.as_deref()?;
    if s.is_text || s.value_is_text || input.starts_with('=') {
        return None;
    }
    s.value.as_deref()?.parse::<f64>().ok().filter(|v| v.is_finite())
}

/// Names that continue as a series when dragged (F.04c).
const SERIES_NAMES: [&[&str]; 5] = [
    &["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"],
    &["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"],
    &["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"],
    &["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"],
    &["Q1", "Q2", "Q3", "Q4"],
];

/// How a series name was written, so the names after it are written the same way.
#[derive(Clone, Copy)]
enum Case { Upper, Lower, AsListed }

impl Case {
    fn apply(self, name: &str) -> String {
        match self {
            Case::Upper => name.to_uppercase(),
            Case::Lower => name.to_lowercase(),
            Case::AsListed => name.to_string(),
        }
    }
}

/// `"wed"` → (the short weekday names, 2, lower case).
fn series_name(text: &str) -> Option<(&'static [&'static str], usize, Case)> {
    SERIES_NAMES.iter().find_map(|list| {
        let at = list.iter().position(|n| n.eq_ignore_ascii_case(text))?;
        let letters = || text.chars().filter(|c| c.is_alphabetic());
        let case = if text == list[at] {
            Case::AsListed
        } else if letters().all(|c| c.is_uppercase()) {
            Case::Upper
        } else if letters().all(|c| c.is_lowercase()) {
            Case::Lower
        } else {
            Case::AsListed
        };
        Some((*list, at, case))
    })
}

/// Serial numbers of the dates the sheet engine numbers like other spreadsheets
/// (1900-03-01 to 9999-12-31).
const DATES: std::ops::RangeInclusive<i64> = 61..=2_958_465;

/// The month step of dates that share their day of the month (2024-01-31, 2024-02-29
/// do not; 2023-12-31, 2024-01-31 do) and are evenly spaced in months.
fn month_step(serials: &[f64]) -> Option<i64> {
    if serials.len() < 2 || serials.iter().any(|v| v.fract() != 0.0 || !DATES.contains(&(*v as i64))) {
        return None;
    }
    let dates: Vec<(i64, u32, u32)> = serials.iter().map(|v| civil_from_serial(*v as i64)).collect();
    if dates.iter().any(|d| d.2 != dates[0].2) {
        return None;
    }
    let months: Vec<i64> = dates.iter().map(|(y, m, _)| y * 12 + *m as i64).collect();
    let step = months[1] - months[0];
    (step != 0 && months.windows(2).all(|w| w[1] - w[0] == step)).then_some(step)
}

fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        2 if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Serial 25569 is 1970-01-01; civil dates from and to days since then use the
/// proleptic Gregorian algorithms of H. Hinnant ("chrono-Compatible Low-Level Date
/// Algorithms").
const SERIAL_1970: i64 = 25_569;

fn civil_from_serial(serial: i64) -> (i64, u32, u32) {
    let z = serial - SERIAL_1970 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

fn serial_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let m = month as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + day as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468 + SERIAL_1970
}

/// `"Item 07"` → (`"Item "`, 7, 2): text that ends in a whole number, and its digit width.
fn trailing_number(text: &str) -> Option<(String, u64, usize)> {
    let digits = text.bytes().rev().take_while(|b| b.is_ascii_digit()).count();
    if digits == 0 || digits == text.len() || digits > 15 {
        return None;
    }
    let (prefix, number) = text.split_at(text.len() - digits);
    Some((prefix.to_string(), number.parse().ok()?, digits))
}

/// A series value as typed input: whole numbers without a decimal point, others to 12
/// decimals so that 0.1 steps do not show binary noise.
fn format_number(v: f64) -> String {
    if v == v.trunc() && v.abs() < 1e15 {
        return format!("{}", v as i64);
    }
    let s = format!("{v:.12}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ironcalc::base::expressions::types::Area;

    fn sheets() -> Sheets {
        let mut s = Sheets::from_bytes(None).unwrap();
        s.create("Data").unwrap();
        s
    }

    fn input(row: u32, column: u32, value: &str) -> SheetInput {
        SheetInput { row, column, value: value.to_string(), base: None }
    }

    fn content(s: &Sheets, sheet: u32, row: u32, column: u32) -> String {
        s.model().unwrap().get_cell_content(sheet, row as i32 + 1, column as i32 + 1).unwrap()
    }

    fn display(s: &Sheets, sheet: u32, row: u32, column: u32) -> String {
        s.model().unwrap().get_formatted_cell_value(sheet, row as i32 + 1, column as i32 + 1).unwrap()
    }

    #[test]
    fn formula_problems_report_where_parsing_stopped() {
        let s = sheets();
        assert!(s.check_formula(0, 0, 0, "=SUM(A1:A3)").unwrap().is_none());
        assert!(s.check_formula(0, 0, 0, "plain text").unwrap().is_none());
        // Positions count characters of the text as typed, including `=`.
        // A missing final parenthesis is completed when stored, so it is not a problem.
        assert!(s.check_formula(0, 0, 0, "=SUM(A1").unwrap().is_none());
        assert!(s.check_formula(0, 0, 0, "=SUM(MAX(A1,2)").unwrap().is_none());
        assert!(s.check_formula(0, 0, 0, "=SUM(1,,").unwrap().is_none(), "stored as =SUM(1,,)");
        for (formula, position) in [("=1+)", 3), ("=SUM(1,+", 8), ("=SUM((A1", 8), ("=1 +* 2", 4), ("=1+2)", 4), ("=1 2", 2), ("  =\"abc", 7)] {
            let p = s.check_formula(0, 0, 0, formula).unwrap().unwrap_or_else(|| panic!("{formula} should not parse"));
            assert!(!p.message.is_empty());
            assert_eq!(p.position, position, "{formula}: {p:?}");
        }
    }

    /// Input after a complete expression used to be dropped silently (`=1 2` showed 1).
    #[test]
    fn trailing_input_is_an_error_not_a_shorter_formula() {
        let mut s = sheets();
        s.set(0, &[input(0, 0, "=1+2)"), input(1, 0, "=1 2"), input(2, 0, "=SUM(1,2"), input(3, 0, "=1+)")]).unwrap();
        assert_eq!((content(&s, 0, 0, 0), display(&s, 0, 0, 0)), ("=1+2)".into(), "#ERROR!".into()));
        assert_eq!((content(&s, 0, 1, 0), display(&s, 0, 1, 0)), ("=1 2".into(), "#ERROR!".into()));
        // A missing closing parenthesis at the end is still completed, as before.
        assert_eq!((content(&s, 0, 2, 0), display(&s, 0, 2, 0)), ("=SUM(1,2)".into(), "3".into()));
        assert_eq!(display(&s, 0, 3, 0), "#ERROR!");
    }

    fn reopen(s: &Sheets) -> Sheets {
        Sheets::from_bytes(s.to_bytes()).unwrap()
    }

    #[test]
    fn named_ranges_follow_renames_survive_reopen_and_undo() {
        let mut s = sheets();
        s.set(0, &[input(0, 1, "10"), input(1, 1, "20"), input(2, 1, "30"), input(0, 3, "=SUM(Sales)"), input(1, 3, "=Sales")]).unwrap();
        assert_eq!(display(&s, 0, 0, 3), "#NAME?");
        s.define_name("Sales", None, "=Data!$B$1:$B$3").unwrap();
        assert_eq!(display(&s, 0, 0, 3), "60");
        // A formula typed before the name existed spills like one typed now.
        assert_eq!((display(&s, 0, 1, 3), display(&s, 0, 2, 3), display(&s, 0, 3, 3)), ("10".into(), "20".into(), "30".into()));
        assert_eq!(s.names(), vec![NamedRange { name: "Sales".into(), scope: None, refers_to: "=Data!$B$1:$B$3".into() }]);
        s.update_name("Sales", None, "Revenue", None, "=Data!$B$1:$B$2").unwrap();
        assert_eq!(content(&s, 0, 0, 3), "=SUM(Revenue)");
        assert_eq!(display(&s, 0, 0, 3), "30");
        assert_eq!((display(&s, 0, 2, 3), display(&s, 0, 3, 3)), ("20".into(), "".into()), "the spill shrinks with the range");
        let reopened = reopen(&s);
        assert_eq!(reopened.names()[0].name, "Revenue");
        assert_eq!(display(&reopened, 0, 0, 3), "30");
        s.delete_name("Revenue", None).unwrap();
        assert!(s.names().is_empty());
        assert_eq!(display(&s, 0, 0, 3), "#NAME?");
        assert!(s.undo().unwrap());
        assert_eq!(display(&s, 0, 0, 3), "30");
        assert!(s.undo().unwrap());
        assert_eq!((s.names()[0].name.as_str(), display(&s, 0, 0, 3).as_str()), ("Sales", "60"));
        assert!(s.redo().unwrap());
        assert_eq!(content(&s, 0, 0, 3), "=SUM(Revenue)");
    }

    #[test]
    fn sheet_scoped_names_shadow_workbook_names_on_their_sheet() {
        let mut s = sheets();
        s.create("Other").unwrap();
        s.set(0, &[input(0, 0, "1"), input(0, 1, "2"), input(1, 0, "=Pick")]).unwrap();
        s.set(1, &[input(0, 0, "=Pick")]).unwrap();
        s.define_name("Pick", None, "=Data!$A$1").unwrap();
        s.define_name("Pick", Some(0), "=Data!$B$1").unwrap();
        assert_eq!(display(&s, 0, 1, 0), "2", "the sheet's own name wins on that sheet");
        assert_eq!(display(&s, 1, 0, 0), "1", "other sheets use the workbook name");
        assert_eq!(s.names().len(), 2);
        assert!(s.define_name("pick", Some(0), "=Data!$A$1").is_err(), "names are case-insensitive within a scope");
    }

    #[test]
    fn names_refuse_functions_constants_and_cell_like_names() {
        let mut s = sheets();
        s.model_mut().unwrap().new_defined_name("DOUBLE", None, "=LAMBDA(x,x*2)").unwrap();
        let err = |r: Result<()>| r.unwrap_err().to_string();
        assert!(err(s.define_name("Double", None, "=Data!$A$1")).contains("workbook function"));
        assert!(err(s.define_name("SUM", None, "=Data!$A$1")).contains("built-in function"));
        assert!(err(s.define_name("Half", None, "=LAMBDA(x,x/2)")).contains("must refer to cells"));
        let e = err(s.define_name("TaxRate", None, "=5")); assert!(e.contains("not a cell or range reference"), "{e}");
        assert!(err(s.define_name("A1", None, "=Data!$A$1")).contains("invalid name"));
        assert!(err(s.define_name("", None, "=Data!$A$1")).contains("empty"));
        assert!(err(s.define_name("Ok", Some(7), "=Data!$A$1")).contains("unknown sheet"));
        assert!(err(s.delete_name("DOUBLE", None)).contains("no named range"), "functions are not managed as names");
        assert!(s.names().is_empty(), "LAMBDAs are not listed as named ranges");
        s.define_name("Ok", None, "=Data!$A$1").unwrap();
        assert!(err(s.define_name("OK", None, "=Data!$A$2")).contains("already exists"));
    }

    #[test]
    fn fill_down_and_right_copy_the_first_row_or_column() {
        let mut s = sheets();
        s.set(0, &[input(0, 0, "1"), input(0, 1, "=A1*2"), input(0, 2, "'007"), input(0, 3, "=$A$1+A$1")]).unwrap();
        s.fill(0, CellArea { row: 0, column: 0, rows: 4, columns: 4 }, FillDirection::Down).unwrap();
        for r in 1..4 {
            // Ctrl+D copies; it does not extend a series.
            assert_eq!(display(&s, 0, r, 0), "1");
            assert_eq!(content(&s, 0, r, 1), format!("=A{}*2", r + 1));
            assert_eq!(display(&s, 0, r, 1), "2");
            assert_eq!(display(&s, 0, r, 2), "007", "text stays text");
            assert_eq!(content(&s, 0, r, 3), "=$A$1+A$1");
        }
        assert!(s.undo().unwrap(), "one undo step");
        assert!((1..4).all(|r| content(&s, 0, r, 1).is_empty() && content(&s, 0, r, 0).is_empty()));
        assert_eq!(content(&s, 0, 0, 1), "=A1*2", "the source row stays");
        s.set(0, &[input(2, 0, "=A1+$A1"), input(3, 0, "x")]).unwrap();
        s.fill(0, CellArea { row: 2, column: 0, rows: 2, columns: 3 }, FillDirection::Right).unwrap();
        assert_eq!(content(&s, 0, 2, 1), "=B1+$A1");
        assert_eq!(content(&s, 0, 2, 2), "=C1+$A1");
        assert_eq!(display(&s, 0, 3, 2), "x");
        assert!(s.fill(0, CellArea { row: 0, column: 0, rows: MAX_ROWS, columns: 2 }, FillDirection::Down).is_err(), "bounded");
        assert!(s.fill(0, CellArea { row: MAX_ROWS - 1, column: 0, rows: 2, columns: 1 }, FillDirection::Down).is_err());
    }

    #[test]
    fn drag_fill_extends_series_counts_text_and_repeats_patterns() {
        let mut s = sheets();
        let a = |row, column, rows, columns| CellArea { row, column, rows, columns };
        // Column A: 1, 3 → 5, 7, 9. Column B: one number copies. Column C: Item 9 counts on
        // and keeps its digit width. Column D: a formula repeats, shifted.
        s.set(0, &[input(0, 0, "1"), input(1, 0, "3"), input(0, 1, "4"), input(0, 2, "Item 09"), input(0, 3, "=A1*2")]).unwrap();
        s.drag_fill(0, a(0, 0, 2, 1), a(0, 0, 5, 1)).unwrap();
        assert_eq!((2..5).map(|r| content(&s, 0, r, 0)).collect::<Vec<_>>(), ["5", "7", "9"]);
        s.drag_fill(0, a(0, 1, 1, 3), a(0, 1, 3, 3)).unwrap();
        assert_eq!((content(&s, 0, 1, 1), content(&s, 0, 2, 1)), ("4".into(), "4".into()));
        assert_eq!((display(&s, 0, 1, 2), display(&s, 0, 2, 2)), ("Item 10".into(), "Item 11".into()));
        assert_eq!((content(&s, 0, 1, 3), content(&s, 0, 2, 3)), ("=A2*2".into(), "=A3*2".into()));
        assert_eq!(display(&s, 0, 2, 3), "10", "=A3*2 with A3 = 5");
        assert!(s.undo().unwrap(), "one undo step per drag");
        assert!(content(&s, 0, 1, 2).is_empty() && content(&s, 0, 0, 2) == "Item 09");

        // Decreasing run filled right; 0.1 steps without binary noise.
        s.set(0, &[input(10, 0, "10"), input(10, 1, "7"), input(11, 0, "0.1"), input(11, 1, "0.2")]).unwrap();
        s.drag_fill(0, a(10, 0, 2, 2), a(10, 0, 2, 5)).unwrap();
        assert_eq!((2..5).map(|c| content(&s, 0, 10, c)).collect::<Vec<_>>(), ["4", "1", "-2"]);
        assert_eq!((2..5).map(|c| content(&s, 0, 11, c)).collect::<Vec<_>>(), ["0.3", "0.4", "0.5"]);

        // Up: the trend runs backwards from the first cell; a text pair repeats.
        s.set(0, &[input(25, 5, "100"), input(26, 5, "110"), input(25, 6, "a"), input(26, 6, "b")]).unwrap();
        s.drag_fill(0, a(25, 5, 2, 2), a(22, 5, 5, 2)).unwrap();
        assert_eq!((22..25).map(|r| content(&s, 0, r, 5)).collect::<Vec<_>>(), ["70", "80", "90"]);
        assert_eq!((22..25).map(|r| display(&s, 0, r, 6)).collect::<Vec<_>>(), ["b", "a", "b"]);

        // Uneven runs follow the least-squares line: 1, 2, 4 → slope 1.5, intercept 0.8333…
        s.set(0, &[input(40, 0, "1"), input(41, 0, "2"), input(42, 0, "4")]).unwrap();
        s.drag_fill(0, a(40, 0, 3, 1), a(40, 0, 4, 1)).unwrap();
        assert_eq!(content(&s, 0, 43, 0), "5.333333333333");

        let e = s.drag_fill(0, a(0, 0, 1, 1), a(0, 0, 2, 2)).unwrap_err().to_string();
        assert!(e.contains("one direction"), "{e}");
        assert!(s.drag_fill(0, a(0, 0, 1, 1), a(0, 0, MAX_ROWS, 1)).is_err(), "bounded");
    }

    /// F.04c: weekday, month and quarter names continue by name, keeping their case, and
    /// wrap around; dates (numbers with a date format) continue by day, or by month when
    /// the cells share their day of the month. Expected values worked out on a calendar.
    #[test]
    fn drag_fill_continues_named_and_date_series() {
        let mut s = sheets();
        let a = |row, column, rows, columns| CellArea { row, column, rows, columns };
        let shown = |s: &Sheets, rows: std::ops::Range<u32>, column: u32| rows.map(|r| display(s, 0, r, column)).collect::<Vec<_>>();
        s.set(0, &[input(0, 0, "Mon"), input(0, 1, "Friday"), input(0, 2, "Nov"), input(0, 3, "December"),
            input(0, 4, "MON"), input(0, 5, "q3"), input(0, 6, "Mon"), input(1, 6, "Wed")]).unwrap();
        s.drag_fill(0, a(0, 0, 1, 6), a(0, 0, 4, 6)).unwrap();
        assert_eq!(shown(&s, 1..4, 0), ["Tue", "Wed", "Thu"]);
        assert_eq!(shown(&s, 1..4, 1), ["Saturday", "Sunday", "Monday"], "wraps after Sunday");
        assert_eq!(shown(&s, 1..4, 2), ["Dec", "Jan", "Feb"]);
        assert_eq!(shown(&s, 1..4, 3), ["January", "February", "March"]);
        assert_eq!(shown(&s, 1..4, 4), ["TUE", "WED", "THU"], "capitals stay capitals");
        assert_eq!(shown(&s, 1..4, 5), ["q4", "q1", "q2"], "quarters wrap, not Q5");
        // Two names set the step: Mon, Wed → Fri, Sun, Tue.
        s.drag_fill(0, a(0, 6, 2, 1), a(0, 6, 5, 1)).unwrap();
        assert_eq!(shown(&s, 2..5, 6), ["Fri", "Sun", "Tue"]);
        // Filling up runs backwards; filling right works the same way.
        s.set(0, &[input(12, 0, "Wed"), input(20, 0, "Mar")]).unwrap();
        s.drag_fill(0, a(12, 0, 1, 1), a(10, 0, 3, 1)).unwrap();
        assert_eq!(shown(&s, 10..12, 0), ["Mon", "Tue"]);
        s.drag_fill(0, a(20, 0, 1, 1), a(20, 0, 1, 3)).unwrap();
        assert_eq!((display(&s, 0, 20, 1), display(&s, 0, 20, 2)), ("Apr".into(), "May".into()));
        // Text that only looks like a name still counts on or repeats as before.
        s.set(0, &[input(30, 0, "Monday 1"), input(30, 1, "Mo")]).unwrap();
        s.drag_fill(0, a(30, 0, 1, 2), a(30, 0, 2, 2)).unwrap();
        assert_eq!((display(&s, 0, 31, 0), display(&s, 0, 31, 1)), ("Monday 2".into(), "Mo".into()));
        assert!(s.undo().unwrap(), "one undo step per drag");
        assert!(content(&s, 0, 31, 0).is_empty());

        // Dates: serial numbers shown as yyyy-mm-dd (2024-01-30 is serial 45321).
        let date = |s: &mut Sheets, row: u32, column: u32, serial: &str| {
            s.set(0, &[input(row, column, serial)]).unwrap();
            let cell = Area { sheet: 0, row: row as i32 + 1, column: column as i32 + 1, width: 1, height: 1 };
            s.model_mut().unwrap().update_range_style(&cell, "num_fmt", "yyyy-mm-dd").unwrap();
        };
        date(&mut s, 40, 0, "45321");
        assert_eq!(display(&s, 0, 40, 0), "2024-01-30");
        // One date counts on by day, across the month end.
        s.drag_fill(0, a(40, 0, 1, 1), a(40, 0, 4, 1)).unwrap();
        assert_eq!(shown(&s, 41..44, 0), ["2024-01-31", "2024-02-01", "2024-02-02"]);
        // 2024-01-15, 2024-02-15 (same day, one month apart) → the 15th of each month.
        date(&mut s, 40, 1, "45306");
        date(&mut s, 41, 1, "45337");
        s.drag_fill(0, a(40, 1, 2, 1), a(40, 1, 5, 1)).unwrap();
        assert_eq!(shown(&s, 42..45, 1), ["2024-03-15", "2024-04-15", "2024-05-15"]);
        // 2023-12-31, 2024-01-31: the day is kept where the month has it, else its last day.
        date(&mut s, 40, 2, "45291");
        date(&mut s, 41, 2, "45322");
        s.drag_fill(0, a(40, 2, 2, 1), a(40, 2, 4, 1)).unwrap();
        assert_eq!(shown(&s, 42..44, 2), ["2024-02-29", "2024-03-31"]);
        // A year apart: yearly.
        date(&mut s, 40, 3, "44985");
        date(&mut s, 41, 3, "45350");
        assert_eq!((display(&s, 0, 40, 3), display(&s, 0, 41, 3)), ("2023-02-28".into(), "2024-02-28".into()));
        s.drag_fill(0, a(40, 3, 2, 1), a(40, 3, 3, 1)).unwrap();
        assert_eq!(display(&s, 0, 42, 3), "2025-02-28");
        // Upwards from 2024-02-15, 2024-03-15: back one month per row.
        s.drag_fill(0, a(41, 1, 2, 1), a(39, 1, 4, 1)).unwrap();
        assert_eq!((display(&s, 0, 39, 1), display(&s, 0, 40, 1)), ("2023-12-15".into(), "2024-01-15".into()));
        // Dates a week apart continue the week (the trend), still shown as dates.
        date(&mut s, 50, 0, "45292");
        date(&mut s, 51, 0, "45299");
        s.drag_fill(0, a(50, 0, 2, 1), a(50, 0, 3, 1)).unwrap();
        assert_eq!(display(&s, 0, 52, 0), "2024-01-15");
    }

    #[test]
    fn current_region_stops_at_empty_rows_and_columns() {
        let mut s = sheets();
        let a = |row, column, rows, columns| CellArea { row, column, rows, columns };
        // B2:D4 filled except C3, plus E5 touching it diagonally; G1 is separate.
        s.set(0, &[input(1, 1, "x"), input(1, 2, "x"), input(1, 3, "x"), input(2, 1, "x"), input(2, 3, "x"),
            input(3, 1, "x"), input(3, 2, "x"), input(3, 3, "x"), input(4, 4, "x"), input(0, 6, "y")]).unwrap();
        assert_eq!(s.current_region(0, 2, 2).unwrap(), a(1, 1, 4, 4), "from the empty middle cell");
        assert_eq!(s.current_region(0, 0, 6).unwrap(), a(0, 6, 1, 1));
        assert_eq!(s.current_region(0, 20, 20).unwrap(), a(20, 20, 1, 1), "an empty cell alone");
    }

    #[test]
    fn paste_special_modes_and_transpose() {
        let mut s = sheets();
        s.set(0, &[input(0, 0, "2"), input(0, 1, "=A1*10"), input(1, 0, "'05"), input(1, 1, "=1/0")]).unwrap();
        let bold = Area { sheet: 0, row: 1, column: 2, width: 1, height: 1 };
        s.model_mut().unwrap().update_range_style(&bold, "font.b", "true").unwrap();
        let src = CellArea { row: 0, column: 0, rows: 2, columns: 2 };
        let is_bold = |s: &Sheets, r: u32, c: u32| s.model().unwrap().get_cell_style(0, r as i32 + 1, c as i32 + 1).unwrap().font.b;
        // Values: calculated results, errors kept as errors, text kept as text, no formats.
        s.paste(0, src, 0, (4, 0), PasteMode::Values, false).unwrap();
        assert_eq!((content(&s, 0, 4, 0), content(&s, 0, 4, 1)), ("2".into(), "20".into()));
        assert_eq!(display(&s, 0, 5, 0), "05");
        assert_eq!(display(&s, 0, 5, 1), "#DIV/0!");
        assert!(!is_bold(&s, 4, 1));
        // Formulas: translated formulas, no formats.
        s.paste(0, src, 0, (8, 2), PasteMode::Formulas, false).unwrap();
        assert_eq!(content(&s, 0, 8, 3), "=C9*10");
        assert!(!is_bold(&s, 8, 3));
        // All: formulas and formats.
        s.paste(0, src, 0, (12, 0), PasteMode::All, false).unwrap();
        assert_eq!(content(&s, 0, 12, 1), "=A13*10");
        assert!(is_bold(&s, 12, 1));
        // Formats only leave the target's values alone.
        s.set(0, &[input(20, 1, "keep")]).unwrap();
        s.paste(0, src, 0, (20, 0), PasteMode::Formats, false).unwrap();
        assert_eq!(display(&s, 0, 20, 1), "keep");
        assert!(is_bold(&s, 20, 1));
        // Transpose: A1:B2 at C31 swaps rows and columns; each formula is translated like
        // an ordinary copy to the cell it lands in (B1 → C32 shifts A1 to B32).
        let t = s.paste(0, src, 0, (30, 2), PasteMode::All, true).unwrap();
        assert_eq!((t.row, t.column, t.rows, t.columns), (30, 2, 2, 2));
        assert_eq!(content(&s, 0, 30, 2), "2");
        assert_eq!(display(&s, 0, 30, 3), "05");
        assert_eq!(content(&s, 0, 31, 2), "=B32*10");
        assert!(!is_bold(&s, 30, 3) && is_bold(&s, 31, 2));
        // A reference shifted off the grid becomes #REF!, as when copying.
        s.paste(0, CellArea { row: 0, column: 1, rows: 1, columns: 1 }, 0, (40, 0), PasteMode::All, false).unwrap();
        assert_eq!(content(&s, 0, 40, 0), "=#REF!*10");
        // Each paste is one undo step.
        assert!(s.undo().unwrap());
        assert!(s.undo().unwrap());
        assert!(content(&s, 0, 31, 2).is_empty() && content(&s, 0, 20, 1) == "keep");
    }

    #[test]
    fn overlapping_paste_reads_sources_before_writing() {
        let mut s = sheets();
        s.set(0, &[input(0, 0, "1"), input(1, 0, "2"), input(2, 0, "=A1+A2")]).unwrap();
        s.paste(0, CellArea { row: 0, column: 0, rows: 3, columns: 1 }, 0, (1, 0), PasteMode::All, false).unwrap();
        assert_eq!((display(&s, 0, 1, 0), display(&s, 0, 2, 0)), ("1".into(), "2".into()));
        assert_eq!(content(&s, 0, 3, 0), "=A2+A3");
        assert_eq!(display(&s, 0, 3, 0), "3");
    }

    #[test]
    fn move_updates_references_to_the_moved_cells() {
        let mut s = sheets();
        s.set(0, &[input(0, 0, "5"), input(1, 0, "=A1*2"), input(0, 1, "=A1+1"), input(0, 2, "=SUM(A1:A2)")]).unwrap();
        let t = s.move_cells(0, CellArea { row: 0, column: 0, rows: 2, columns: 1 }, (4, 3)).unwrap();
        assert_eq!((t.row, t.column), (4, 3));
        assert!(content(&s, 0, 0, 0).is_empty() && content(&s, 0, 1, 0).is_empty());
        assert_eq!(content(&s, 0, 4, 3), "5");
        assert_eq!(content(&s, 0, 5, 3), "=D5*2", "references inside the moved area move with it");
        assert_eq!(content(&s, 0, 0, 1), "=D5+1", "references from outside follow the moved cell");
        assert_eq!(display(&s, 0, 0, 2), "15");
        assert!(s.undo().unwrap());
        assert_eq!((content(&s, 0, 0, 0), content(&s, 0, 0, 1)), ("5".into(), "=A1+1".into()));
        assert!(content(&s, 0, 4, 3).is_empty());
    }

    #[test]
    fn errors_report_their_origin() {
        let mut s = sheets();
        s.set(0, &[input(0, 0, "=1/0"), input(0, 1, "=A1+1"), input(0, 2, "#N/A"), input(0, 3, "7")]).unwrap();
        let d = s.cell_detail(0, 0, 1).unwrap();
        let e = d.error.expect("error detail");
        assert_eq!((e.code.as_str(), e.origin.as_str()), ("#DIV/0!", "Data!A1"));
        assert!(!e.message.is_empty());
        assert_eq!(d.input, "=A1+1");
        let typed = s.cell_detail(0, 0, 2).unwrap().error.expect("typed error");
        assert_eq!((typed.code.as_str(), typed.origin.as_str()), ("#N/A", ""));
        assert!(s.cell_detail(0, 0, 3).unwrap().error.is_none());
        assert!(s.cell_detail(3, 0, 0).is_err());
    }

    #[test]
    fn copy_translation_keeps_absolute_and_qualified_references() {
        let mut s = sheets();
        s.create("Other").unwrap();
        s.set(0, &[input(0, 1, "=A1+$A$1+A$1+$A1+Data!A1+Other!B2")]).unwrap();
        s.paste(0, CellArea { row: 0, column: 1, rows: 1, columns: 1 }, 0, (2, 3), PasteMode::All, false).unwrap();
        assert_eq!(content(&s, 0, 2, 3), "=C3+$A$1+C$1+$A3+Data!C3+Other!D4");
        // Into another sheet: unqualified references are relative to the new sheet.
        s.paste(0, CellArea { row: 0, column: 1, rows: 1, columns: 1 }, 1, (0, 1), PasteMode::All, false).unwrap();
        assert_eq!(content(&s, 1, 0, 1), "=A1+$A$1+A$1+$A1+Data!A1+Other!B2");
    }
}
