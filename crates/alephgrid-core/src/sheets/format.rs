//! S.01 / S.02: cell formatting and number formats on sheets. A style patch changes only the
//! properties it names over an area, as one undo step; fetches return each styled cell's
//! style (empty cells included, so fills show). Number formats change what is shown, never
//! the stored value.

use anyhow::{anyhow, bail, Result};
use ironcalc::base::expressions::types::Area;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::{CellArea, Sheets};

fn is_false(b: &bool) -> bool {
    !*b
}

/// The look of a cell, as far as it differs from the default.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
pub struct CellStyle {
    #[serde(default, skip_serializing_if = "is_false")]
    #[ts(optional, as = "Option<bool>")]
    pub b: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    #[ts(optional, as = "Option<bool>")]
    pub i: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    #[ts(optional, as = "Option<bool>")]
    pub u: bool,
    /// Strikethrough.
    #[serde(default, skip_serializing_if = "is_false")]
    #[ts(optional, as = "Option<bool>")]
    pub s: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub fill: Option<String>,
    /// Font size in points, when not the default 12.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub size: Option<i32>,
    /// `left`, `center`, `right` (and other spreadsheet alignments).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub h: Option<String>,
    /// `top`, `center`, `bottom`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub v: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    #[ts(optional, as = "Option<bool>")]
    pub wrap: bool,
    /// The number format code, when not `general`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub num_fmt: Option<String>,
    /// Borders drawn on the cell: any of `t`, `r`, `b`, `l`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub borders: Option<String>,
}

/// An `#RRGGBB` colour from the engine's colour, if it is a plain RGB one.
pub(crate) fn rgb(c: &ironcalc::base::types::Color) -> Option<String> {
    match c {
        ironcalc::base::types::Color::Rgb(v) => {
            // A stored model may come from someone else's file: only hex digits, so slicing
            // cannot split a character and nothing but a colour reaches the page.
            let hex = v.trim_start_matches('#');
            if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
                return None;
            }
            let hex = if hex.len() == 8 { &hex[2..] } else { hex };
            (hex.len() == 6).then(|| format!("#{}", hex.to_uppercase()))
        }
        _ => None,
    }
}

impl CellStyle {
    pub(crate) fn of(s: &ironcalc::base::types::Style) -> CellStyle {
        let align = s.alignment.as_ref();
        let text = |v: String| (v != "general" && !v.is_empty()).then_some(v);
        CellStyle {
            b: s.font.b,
            i: s.font.i,
            u: s.font.u,
            s: s.font.strike,
            color: rgb(&s.font.color).filter(|c| c != "#000000"),
            fill: rgb(&s.fill.color),
            size: (s.font.sz != 12).then_some(s.font.sz),
            h: align.map(|a| format!("{:?}", a.horizontal).to_lowercase()).and_then(text),
            v: align.map(|a| format!("{:?}", a.vertical).to_lowercase()).filter(|v| v != "bottom" && !v.is_empty()),
            wrap: align.is_some_and(|a| a.wrap_text),
            num_fmt: text(s.num_fmt.clone()),
            borders: {
                let b = &s.border;
                let f: String = [(b.top.is_some(), 't'), (b.right.is_some(), 'r'), (b.bottom.is_some(), 'b'), (b.left.is_some(), 'l')].iter().filter(|x| x.0).map(|x| x.1).collect();
                (!f.is_empty()).then_some(f)
            },
        }
    }

    pub fn is_default(&self) -> bool {
        *self == CellStyle::default()
    }
}

/// A cell whose style differs from the default (see [`super::SheetBlock::styles`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct StyledCell {
    pub row: u32,
    pub column: u32,
    pub style: CellStyle,
}

/// Properties to set over an area; the ones left out stay as they are. An empty string
/// for a colour, alignment or number format sets it back to the default.
#[derive(Debug, Clone, Default, Deserialize, Serialize, TS)]
#[ts(optional_fields)]
pub struct StylePatch {
    pub b: Option<bool>,
    pub i: Option<bool>,
    pub u: Option<bool>,
    pub s: Option<bool>,
    pub color: Option<String>,
    pub fill: Option<String>,
    pub size: Option<i32>,
    pub h: Option<String>,
    pub v: Option<String>,
    pub wrap: Option<bool>,
    pub num_fmt: Option<String>,
}

const H: &[&str] = &["general", "left", "center", "right", "fill", "justify", "centercontinuous", "distributed"];
const V: &[&str] = &["bottom", "center", "top", "justify", "distributed"];

/// Whether `v` is a colour written `#RRGGBB`.
pub(crate) fn is_hex_color(v: &str) -> bool {
    v.len() == 7 && v.starts_with('#') && v[1..].chars().all(|c| c.is_ascii_hexdigit())
}

fn color(v: &str) -> Result<String> {
    if v.is_empty() {
        return Ok(String::new());
    }
    if !is_hex_color(v) {
        bail!("colours are written #RRGGBB, not {v:?}");
    }
    Ok(v.to_uppercase())
}

impl StylePatch {
    /// The engine's style paths and values for this patch.
    pub(super) fn paths(&self) -> Result<Vec<(&'static str, String)>> {
        let mut out = Vec::new();
        let flag = |b: bool| if b { "true".to_string() } else { "false".to_string() };
        if let Some(b) = self.b { out.push(("font.b", flag(b))); }
        if let Some(b) = self.i { out.push(("font.i", flag(b))); }
        if let Some(b) = self.u { out.push(("font.u", flag(b))); }
        if let Some(b) = self.s { out.push(("font.strike", flag(b))); }
        if let Some(c) = &self.color { out.push(("font.color", color(c)?)); }
        if let Some(c) = &self.fill { out.push(("fill.fg_color", color(c)?)); }
        if let Some(n) = self.size {
            if !(6..=96).contains(&n) {
                bail!("font sizes go from 6 to 96 points");
            }
            out.push(("font.size", n.to_string()));
        }
        if let Some(h) = &self.h {
            let h = if h.is_empty() { "general".to_string() } else { h.to_lowercase() };
            if !H.contains(&h.as_str()) { bail!("unknown horizontal alignment {h:?}"); }
            out.push(("alignment.horizontal", h));
        }
        if let Some(v) = &self.v {
            let v = if v.is_empty() { "bottom".to_string() } else { v.to_lowercase() };
            if !V.contains(&v.as_str()) { bail!("unknown vertical alignment {v:?}"); }
            out.push(("alignment.vertical", v));
        }
        if let Some(w) = self.wrap { out.push(("alignment.wrap_text", flag(w))); }
        if let Some(f) = &self.num_fmt {
            if f.len() > 255 { bail!("a number format is at most 255 characters"); }
            out.push(("num_fmt", if f.is_empty() { "general".into() } else { f.clone() }));
        }
        if out.is_empty() {
            bail!("the style change sets nothing");
        }
        Ok(out)
    }
}

pub(super) fn area(index: u32, a: &CellArea) -> Area {
    Area { sheet: index, row: a.row as i32 + 1, column: a.column as i32 + 1, width: a.columns as i32, height: a.rows as i32 }
}

impl Sheets {
    /// Apply `patch` to every cell of `a`, as one undo step (S.01, S.02).
    pub fn set_style(&mut self, index: u32, a: CellArea, patch: &StylePatch) -> Result<()> {
        self.check_index(index)?;
        a.check("format")?;
        let paths = patch.paths()?;
        let m = self.model_mut()?;
        let mut entries = 0;
        for (path, value) in paths {
            m.update_range_style(&area(index, &a), path, &value).map_err(|e| anyhow!(e))?;
            entries += 1;
        }
        self.record(entries);
        Ok(())
    }

    /// Clear every format of `a` (values stay), as one undo step.
    pub fn clear_format(&mut self, index: u32, a: CellArea) -> Result<()> {
        self.check_index(index)?;
        a.check("format")?;
        let m = self.model_mut()?;
        m.range_clear_formatting(&area(index, &a)).map_err(|e| anyhow!(e))?;
        self.record(1);
        Ok(())
    }

    /// Keep the first `columns` columns of a sheet in view while scrolling (S.05; 0 unfreezes);
    /// frozen rows stay.
    pub fn freeze_columns(&mut self, index: u32, columns: u32) -> Result<()> {
        let rows = self.info(index)?.frozen_rows;
        self.freeze(index, rows, columns)
    }

    /// Copy a sheet (cells, formats, layout) into a new sheet after it, as one undo step (S.07).
    pub fn duplicate(&mut self, index: u32) -> Result<super::SheetInfo> {
        self.check_index(index)?;
        let before: Vec<String> = self.list().into_iter().map(|s| s.name).collect();
        let m = self.model_mut()?;
        m.duplicate_sheet(index).map_err(|e| anyhow!(e))?;
        self.record(1);
        self.list().into_iter().find(|s| !before.contains(&s.name)).ok_or_else(|| anyhow!("the copy did not appear"))
    }

    /// Colour a sheet's tab (`#RRGGBB`, or empty for none), as one undo step (S.07).
    pub fn set_tab_color(&mut self, index: u32, value: &str) -> Result<()> {
        self.check_index(index)?;
        let c = color(value)?;
        let c = if c.is_empty() { ironcalc::base::types::Color::None } else { ironcalc::base::types::Color::Rgb(c) };
        let m = self.model_mut()?;
        m.set_sheet_color(index, &c).map_err(|e| anyhow!(e))?;
        self.record(1);
        Ok(())
    }

    /// Draw borders on an area (S.01): `kind` is all, outer, inner, top, bottom, left, right
    /// or none; `weight` thin, medium or thick. One undo step.
    pub fn set_border(&mut self, index: u32, a: CellArea, kind: &str, weight: &str) -> Result<()> {
        self.check_index(index)?;
        a.check("format")?;
        let kind = match kind {
            "all" => "All", "outer" => "Outer", "inner" => "Inner", "top" => "Top", "bottom" => "Bottom",
            "left" => "Left", "right" => "Right", "none" => "None",
            other => bail!("unknown border {other:?}"),
        };
        if !["thin", "medium", "thick"].contains(&weight) {
            bail!("borders are thin, medium or thick");
        }
        let border: ironcalc::base::BorderArea =
            serde_json::from_value(serde_json::json!({ "item": { "style": weight }, "type": kind })).map_err(|e| anyhow!("border: {e}"))?;
        let m = self.model_mut()?;
        m.set_area_with_border(&area(index, &a), &border).map_err(|e| anyhow!(e))?;
        self.record(1);
        Ok(())
    }

    /// Hide a sheet's tab (S.07); the last visible sheet stays. One undo step.
    pub fn hide_sheet(&mut self, index: u32) -> Result<()> {
        self.check_index(index)?;
        if self.list().len() < 2 {
            bail!("a workbook keeps at least one visible sheet");
        }
        let m = self.model_mut()?;
        m.hide_sheet(index).map_err(|e| anyhow!(e))?;
        self.record(1);
        Ok(())
    }

    /// Move a sheet's tab to the place of the visible sheet `to` (S.07). Formulas keep
    /// pointing at the same sheets (they refer by name). One undo step.
    pub fn move_tab(&mut self, index: u32, to: u32) -> Result<()> {
        self.check_index(index)?;
        self.check_index(to)?;
        if index == to {
            return Ok(());
        }
        let m = self.model_mut()?;
        m.move_sheet(index, to).map_err(|e| anyhow!(e))?;
        self.record(1);
        self.moved_all();
        Ok(())
    }

    /// The names of hidden sheets (Alephgrid's own are not listed).
    pub fn hidden_sheets(&self) -> Vec<String> {
        let Ok(m) = self.model() else { return vec![] };
        m.get_worksheets_properties().into_iter().filter(|p| p.state != "visible" && p.name != super::LINK_SHEET).map(|p| p.name).collect()
    }

    /// Show a hidden sheet again, by name. One undo step.
    pub fn unhide_sheet(&mut self, name: &str) -> Result<()> {
        if name == super::LINK_SHEET || !self.hidden_sheets().iter().any(|n| n == name) {
            bail!("no hidden sheet {name:?}");
        }
        let index = self.model()?.get_worksheets_properties().iter().position(|p| p.name == name).ok_or_else(|| anyhow!("no sheet {name:?}"))? as u32;
        let m = self.model_mut()?;
        m.unhide_sheet(index).map_err(|e| anyhow!(e))?;
        self.record(1);
        Ok(())
    }

    /// The engine's full style of a cell (for tests of what the engine stores).
    #[cfg(test)]
    pub fn engine_style(&self, index: u32, row: u32, column: u32) -> Result<ironcalc::base::types::Style> {
        self.model()?.get_cell_style(index, row as i32 + 1, column as i32 + 1).map_err(|e| anyhow!(e))
    }

    /// The engine's stable id of a sheet (kept through renames and moves).
    pub fn sheet_id(&self, index: u32) -> Result<u32> {
        Ok(self.model()?.get_model().workbook.worksheet(index).map_err(|e| anyhow!(e))?.sheet_id)
    }

    /// The style of one cell (row and column 0-based).
    #[cfg(test)]
    pub fn cell_style(&self, index: u32, row: u32, column: u32) -> Result<CellStyle> {
        let m = self.model()?;
        Ok(CellStyle::of(&m.get_cell_style(index, row as i32 + 1, column as i32 + 1).map_err(|e| anyhow!(e))?))
    }
}

#[cfg(test)]
mod tests {
    use ironcalc::base::types::Color;

    #[test]
    fn only_hex_colours_come_out_and_odd_text_does_not_panic() {
        assert_eq!(super::rgb(&Color::Rgb("#ff00aa".into())).as_deref(), Some("#FF00AA"));
        assert_eq!(super::rgb(&Color::Rgb("FFFF00AA".into())).as_deref(), Some("#FF00AA"));
        for bad in ["#é1234567", "red;x:y", "#12345", "#zzzzzz"] {
            assert_eq!(super::rgb(&Color::Rgb(bad.into())), None, "{bad}");
        }
    }
}
