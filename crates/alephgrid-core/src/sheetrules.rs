//! S.10: conditional formatting and data validation on sheets. Rules are kept per sheet (by
//! its stable id) in the workbook. Colour rules are drawn by the client from the shown
//! values; list and number validation is enforced here, before an edit is applied.

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::sheets::CellArea;
use crate::workbook::Workbook;

/// A colour rule: cells of `area` meeting `condition` against `value` get `color` as fill.
/// Conditions: greater, less, equal, contains, empty, not_empty.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct SheetColorRule {
    pub area: CellArea,
    pub condition: String,
    #[serde(default)]
    pub value: String,
    pub color: String,
}

/// A validation rule: values typed into `area` must be one of `list`, or a number between
/// `min` and `max` (when given).
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct SheetValidation {
    pub area: CellArea,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(optional, as = "Option<Vec<String>>")]
    pub list: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub min: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub max: Option<f64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, TS)]
pub struct SheetRules {
    #[serde(default)]
    pub colors: Vec<SheetColorRule>,
    #[serde(default)]
    pub validations: Vec<SheetValidation>,
}

const CONDITIONS: &[&str] = &["greater", "less", "equal", "contains", "empty", "not_empty", "scale"];

fn inside(a: &CellArea, row: u32, column: u32) -> bool {
    row >= a.row && row < a.row + a.rows && column >= a.column && column < a.column + a.columns
}

impl SheetValidation {
    /// Why `value` is refused, if it is. Formulas and empty cells are not checked.
    pub fn refuses(&self, value: &str) -> Option<String> {
        if value.is_empty() || value.starts_with('=') {
            return None;
        }
        if !self.list.is_empty() && !self.list.iter().any(|v| v == value) {
            return Some(format!("{value:?} is not one of the allowed values: {}", self.list.join(", ")));
        }
        if self.min.is_some() || self.max.is_some() {
            let Ok(n) = value.trim().parse::<f64>() else { return Some(format!("{value:?} is not a number")) };
            if self.min.is_some_and(|m| n < m) || self.max.is_some_and(|m| n > m) {
                return Some(format!("{value} is outside {}–{}", self.min.map(|m| m.to_string()).unwrap_or_default(), self.max.map(|m| m.to_string()).unwrap_or_default()));
            }
        }
        None
    }
}

impl Workbook {
    fn rules_key(&self, sheet: u32) -> Result<String> {
        let s = self.sheets();
        s.info(sheet)?;
        Ok(format!("sheet_rules:{}", s.sheet_id(sheet)?))
    }

    /// The colour and validation rules of a sheet.
    pub fn sheet_rules(&self, sheet: u32) -> Result<SheetRules> {
        let key = self.rules_key(sheet)?;
        Ok(crate::storage::meta_get(&self.conn, &key)?.and_then(|v| serde_json::from_str(&v).ok()).unwrap_or_default())
    }

    /// Replace a sheet's rules (at most 100 of each kind).
    pub fn set_sheet_rules(&mut self, sheet: u32, rules: SheetRules) -> Result<()> {
        if rules.colors.len() > 100 || rules.validations.len() > 100 {
            bail!("a sheet takes at most 100 colour rules and 100 validation rules");
        }
        for r in &rules.colors {
            if !CONDITIONS.contains(&r.condition.as_str()) {
                bail!("unknown condition {:?}", r.condition);
            }
            if !crate::sheets::is_hex_color(&r.color) {
                bail!("colours are written #RRGGBB");
            }
        }
        for v in &rules.validations {
            if v.list.is_empty() && v.min.is_none() && v.max.is_none() {
                bail!("a validation rule needs a list or a number range");
            }
            if v.list.len() > 500 {
                bail!("a list takes at most 500 values");
            }
        }
        let key = self.rules_key(sheet)?;
        let json = serde_json::to_string(&rules)?;
        self.mutating("set_sheet_rules", None, |wb| crate::storage::meta_set(&wb.conn, &key, &json))
    }

    /// Refuse cell inputs the sheet's validation rules do not allow (before writing them).
    pub fn check_sheet_inputs(&self, sheet: u32, inputs: &[crate::sheets::SheetInput]) -> Result<()> {
        let rules = self.sheet_rules(sheet)?;
        if rules.validations.is_empty() {
            return Ok(());
        }
        for i in inputs {
            for v in rules.validations.iter().filter(|v| inside(&v.area, i.row, i.column)) {
                if let Some(why) = v.refuses(&i.value) {
                    bail!("{}{}: {why}", crate::sheets::column_name(i.column), i.row + 1);
                }
            }
        }
        Ok(())
    }
}

impl crate::workbook::Reader {
    /// The colour and validation rules of a sheet (S.10).
    pub fn sheet_rules(&self, sheet: u32) -> Result<SheetRules> {
        let key = {
            let s = self.sheets();
            s.info(sheet)?;
            format!("sheet_rules:{}", s.sheet_id(sheet)?)
        };
        Ok(crate::storage::meta_get(&self.conn, &key)?.and_then(|v| serde_json::from_str(&v).ok()).unwrap_or_default())
    }
}
