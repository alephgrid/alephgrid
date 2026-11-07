//! S.04: find and replace in sheets. Matches are cells whose shown value (or formula, when
//! asked) contains the text, or equals it with `whole`; found in row then column order.
//! Replacing writes every changed cell in one edit, so it is one undo step.

use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::{SheetInput, Sheets, MAX_BULK_CELLS};
use crate::{FindHit, FindResult};

/// What to look for in a sheet.
#[derive(Debug, Clone, Default, Deserialize, Serialize, TS)]
pub struct SheetSearch {
    pub text: String,
    #[serde(default)]
    #[ts(optional, as = "Option<bool>")]
    pub match_case: bool,
    /// The whole cell must equal the text.
    #[serde(default)]
    #[ts(optional, as = "Option<bool>")]
    pub whole: bool,
    /// Look in formulas (the cell's input) instead of shown values.
    #[serde(default)]
    #[ts(optional, as = "Option<bool>")]
    pub formulas: bool,
}

impl SheetSearch {
    fn hits(&self, cell: &str) -> bool {
        if self.text.is_empty() {
            return false;
        }
        let (c, t) = if self.match_case { (cell.to_string(), self.text.clone()) } else { (cell.to_lowercase(), self.text.to_lowercase()) };
        if self.whole { c == t } else { c.contains(&t) }
    }
}

/// How many cells of a sheet match a search (S.04, all sheets).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct SheetMatches {
    pub sheet: u32,
    pub name: String,
    pub matches: u64,
}

impl Sheets {
    /// The visible sheets with matching cells, in tab order, with their counts.
    pub fn find_all_sheets(&self, search: &SheetSearch) -> Result<Vec<SheetMatches>> {
        let mut out = Vec::new();
        for s in self.list() {
            let n = self.matches(s.index, search)?.len() as u64;
            if n > 0 {
                out.push(SheetMatches { sheet: s.index, name: s.name, matches: n });
            }
        }
        Ok(out)
    }

    /// [`Sheets::replace_all`] in every visible sheet, as one undo step. Returns how many
    /// cells changed.
    pub fn replace_all_sheets(&mut self, search: &SheetSearch, replacement: &str) -> Result<usize> {
        let found = self.find_all_sheets(search)?;
        let total: u64 = found.iter().map(|s| s.matches).sum();
        if total as usize > MAX_BULK_CELLS {
            bail!(crate::limits::TooLarge(format!("{total} cells match; at most {MAX_BULK_CELLS} can be replaced at once")));
        }
        let before = self.undo_groups.len();
        let mut n = 0;
        for s in found {
            n += self.replace_all(s.sheet, search, replacement)?;
        }
        // Every sheet's write forms one user step.
        while self.undo_groups.len() > before + 1 {
            let last = self.undo_groups.pop().unwrap_or(0);
            if let Some(prev) = self.undo_groups.last_mut() {
                *prev += last;
            }
        }
        Ok(n)
    }

    /// Every matching cell of a sheet, in row then column order (0-based).
    fn matches(&self, index: u32, search: &SheetSearch) -> Result<Vec<(u32, u32, String)>> {
        self.check_index(index)?;
        let m = self.model()?;
        let ws = m.get_model().workbook.worksheet(index).map_err(|e| anyhow!(e))?;
        let mut out = Vec::new();
        for (row, col, _) in ws.sheet_data.cells() {
            let input = m.get_cell_content(index, row, col).unwrap_or_default();
            let probe = if search.formulas { input.clone() } else { m.get_formatted_cell_value(index, row, col).unwrap_or_default() };
            if search.hits(&probe) {
                out.push(((row - 1) as u32, (col - 1) as u32, input));
            }
        }
        out.sort_by_key(|(r, c, _)| (*r, *c));
        Ok(out)
    }

    /// Matches of `search` from row `from` on (or, `backwards`, up to it): at most two rows
    /// of hits, and the total count when `count` is set (as [`FindResult`] for tables).
    pub fn find(&self, index: u32, search: &SheetSearch, from: u64, backwards: bool, count: bool) -> Result<FindResult> {
        let all = self.matches(index, search)?;
        let mut rows: Vec<FindHit> = Vec::new();
        let ordered: Vec<&(u32, u32, String)> = if backwards {
            all.iter().rev().filter(|(r, _, _)| (*r as u64) <= from).collect()
        } else {
            all.iter().filter(|(r, _, _)| (*r as u64) >= from).collect()
        };
        for (r, c, _) in ordered {
            let row = *r as u64;
            if rows.last().is_some_and(|h| h.row == row) {
                rows.last_mut().unwrap().columns.push(*c);
            } else if rows.len() == 2 {
                break;
            } else {
                rows.push(FindHit { row, columns: vec![*c] });
            }
        }
        for h in &mut rows {
            h.columns.sort_unstable();
        }
        Ok(FindResult { matches: count.then_some(all.len() as u64), hits: rows })
    }

    /// Replace `search.text` with `replacement` in every matching cell's input (formulas
    /// included when `search.formulas`), as one undo step. Returns how many cells changed.
    pub fn replace_all(&mut self, index: u32, search: &SheetSearch, replacement: &str) -> Result<usize> {
        if search.text.is_empty() {
            bail!("say what to replace");
        }
        let all = self.matches(index, search)?;
        if all.len() > MAX_BULK_CELLS {
            bail!(crate::limits::TooLarge(format!("{} cells match; at most {MAX_BULK_CELLS} can be replaced at once", all.len())));
        }
        let mut inputs = Vec::new();
        for (r, c, input) in all {
            // A formula is changed only when formulas were searched; otherwise its value is.
            if input.starts_with('=') && !search.formulas {
                continue;
            }
            let next = if search.whole {
                replacement.to_string()
            } else if search.match_case {
                input.replace(&search.text, replacement)
            } else {
                replace_any_case(&input, &search.text, replacement)
            };
            if next != input {
                inputs.push(SheetInput { row: r, column: c, value: next, base: None });
            }
        }
        let n = inputs.len();
        if n > 0 {
            self.set(index, &inputs)?;
        }
        Ok(n)
    }
}

/// `text` with every case-insensitive occurrence of `find` replaced. Lowercasing can change
/// a character's length (`İ` → `i̇`, `ẞ` → `ß`), so a match in the lowercased text is mapped
/// back through the characters it came from; one that starts or ends inside a character's
/// lowercase form is not a match. Never slices `text` off a character boundary (run-3).
fn replace_any_case(text: &str, find: &str, with: &str) -> String {
    let needle: String = find.chars().flat_map(char::to_lowercase).collect();
    if needle.is_empty() {
        return text.to_string();
    }
    // `lower`, and for each of its bytes the byte offset in `text` of the character it came from.
    let mut lower = String::with_capacity(text.len());
    let mut origin = Vec::with_capacity(text.len() + 1);
    for (i, ch) in text.char_indices() {
        let start = lower.len();
        lower.extend(ch.to_lowercase());
        origin.resize(origin.len() + (lower.len() - start), i);
    }
    origin.push(text.len());
    // A byte of `lower` that begins one character's lowercase form.
    let starts_char = |k: usize| k == 0 || k == lower.len() || origin[k - 1] != origin[k];
    let mut out = String::with_capacity(text.len());
    let (mut copied, mut at) = (0, 0);
    while let Some(p) = lower[at..].find(&needle).map(|p| p + at) {
        let end = p + needle.len();
        if starts_char(p) && starts_char(end) {
            out.push_str(&text[copied..origin[p]]);
            out.push_str(with);
            copied = origin[end];
            at = end;
        } else {
            // Inside a character's lowercase form: look again from the next character.
            at = p + lower[p..].chars().next().map_or(1, char::len_utf8);
        }
    }
    out.push_str(&text[copied..]);
    out
}

/// One key of a range sort: a column of the range (0-based within it) and its direction.
#[derive(Debug, Clone, Deserialize, Serialize, TS)]
pub struct SortBy {
    pub column: u32,
    #[serde(default)]
    #[ts(optional, as = "Option<bool>")]
    pub desc: bool,
}

impl Sheets {
    /// Sort the rows of `area` by `keys` (S.06), keeping a first header row when `header`,
    /// as one undo step. Numbers sort before text, empty cells last. Ranges holding formulas
    /// are refused: their references would point at other cells once rows move.
    pub fn sort_range(&mut self, index: u32, area: super::CellArea, keys: &[SortBy], header: bool) -> Result<()> {
        self.check_index(index)?;
        area.check("sort")?;
        if keys.is_empty() || keys.iter().any(|k| k.column >= area.columns) {
            bail!("sort by one or more columns of the range");
        }
        let m = self.model()?;
        let start = area.row + u32::from(header);
        // Each row keeps where it came from: formulas move with their row and their
        // relative references are rewritten as a copy to the new row would (S.06). Keys
        // are values: a formula sorts by what it shows.
        let mut rows: Vec<(u32, Vec<String>, Vec<String>)> = Vec::new();
        for r in start..area.row + area.rows {
            let (mut inputs, mut keys) = (Vec::new(), Vec::new());
            for c in area.column..area.column + area.columns {
                let (row, col) = (r as i32 + 1, c as i32 + 1);
                let input = m.get_cell_content(index, row, col).unwrap_or_default();
                keys.push(if input.starts_with('=') { m.get_formatted_cell_value(index, row, col).unwrap_or_default().replace(',', "") } else { input.clone() });
                inputs.push(input);
            }
            rows.push((r, inputs, keys));
        }
        #[derive(PartialEq, PartialOrd)]
        enum Key { Num(f64), Text(String), Empty }
        let key = |s: &str| if s.is_empty() { Key::Empty } else if let Ok(n) = s.parse::<f64>() { Key::Num(n) } else { Key::Text(s.to_lowercase()) };
        rows.sort_by(|(_, _, a), (_, _, b)| {
            for k in keys {
                let (x, y) = (key(&a[k.column as usize]), key(&b[k.column as usize]));
                let ord = match (&x, &y) {
                    (Key::Empty, Key::Empty) => std::cmp::Ordering::Equal,
                    (Key::Empty, _) => return std::cmp::Ordering::Greater,
                    (_, Key::Empty) => return std::cmp::Ordering::Less,
                    _ => x.partial_cmp(&y).unwrap_or(std::cmp::Ordering::Equal),
                };
                let ord = if k.desc { ord.reverse() } else { ord };
                if ord != std::cmp::Ordering::Equal {
                    return ord;
                }
            }
            std::cmp::Ordering::Equal
        });
        let mut inputs = Vec::new();
        for (i, (from, row, _)) in rows.into_iter().enumerate() {
            let to = start + i as u32;
            for (j, v) in row.into_iter().enumerate() {
                let column = area.column + j as u32;
                let value = if v.starts_with('=') && from != to { self.translate(&v, (from, column), (index, to, column))? } else { v };
                inputs.push(SheetInput { row: to, column, value, base: None });
            }
        }
        self.set(index, &inputs)
    }
}
