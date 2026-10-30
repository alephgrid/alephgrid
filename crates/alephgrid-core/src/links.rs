//! Sheet cells read by table formulas (F.09): keeping formulas, anchors and saved values in
//! step with the sheets, inside the transaction of the change that moved them.
//!
//! Table reads never look at the live sheet model: they read each cell's value from
//! `_rf_links`, in their own snapshot, so a page of rows and the cell values it was computed
//! from always belong to the same committed revision.

use std::collections::{BTreeSet, HashMap};

use anyhow::{anyhow, bail, Result};

use crate::sheets::Anchor;
use crate::storage::{self, Link};
use crate::table_formula::{references, rewrite_cells, CellRef, CellValue, Reference};
use crate::workbook::Workbook;

/// The sheet cells a table formula reads, as written.
pub(crate) fn cells_read(formula: &str) -> Vec<CellRef> {
    references(formula).map(|refs| refs.into_iter().filter_map(|r| match r { Reference::Cell { cell, .. } => Some(cell), _ => None }).collect()).unwrap_or_default()
}

/// The table-formula type a cell value gets; a change of type changes the column's type.
fn kind(v: &CellValue) -> u8 {
    match v {
        CellValue::Text(_) => 1,
        CellValue::Bool(_) => 2,
        CellValue::Blank | CellValue::Number(_) | CellValue::Error(_) => 0,
    }
}

impl Workbook {
    /// Bring the links up to date, inside the caller's transaction:
    ///
    /// 1. follow anchors the sheet engine moved (row/column inserts and deletes, moves, sheet
    ///    renames, undo), rewriting the formulas that read those cells; a deleted cell
    ///    becomes `#REF!`;
    /// 2. keep exactly one anchor per cell that some formula reads;
    /// 3. save each cell's current value, and mark the tables whose inputs changed: a new
    ///    value changes their data revision, a new value type or a rewritten formula their
    ///    structure too.
    pub(crate) fn sync_links(&mut self) -> Result<()> {
        // Most workbooks have no links: two small queries, before anything is parsed.
        let candidates: i64 = self.conn.query_row(
            "SELECT (SELECT count(*) FROM _rf_links) + (SELECT count(*) FROM _rf_columns WHERE language = 'sheet' AND contains(formula, '!'))",
            [],
            |r| r.get(0),
        )?;
        if candidates == 0 {
            return Ok(());
        }
        let old = storage::links(&self.conn)?;
        let mut formulas = self.all_sheet_formulas()?;
        if old.is_empty() && formulas.iter().all(|(_, _, f)| cells_read(f).is_empty()) {
            return Ok(());
        }
        // 1. Where each anchor points now.
        let mut moved: Vec<(CellRef, Option<CellRef>)> = Vec::new();
        let mut missing: BTreeSet<u32> = BTreeSet::new();
        {
            let sheets = self.sheets();
            for l in &old {
                let now = match sheets.anchor(l.anchor) {
                    Anchor::At(c) => sheets.visible_sheet(&c.sheet).map(|(_, sheet)| CellRef { sheet, ..c }),
                    Anchor::Gone => None,
                    Anchor::Missing => {
                        missing.insert(l.anchor);
                        sheets.visible_sheet(&l.cell.sheet).map(|(_, sheet)| CellRef { sheet, ..l.cell.clone() })
                    }
                };
                if now.as_ref() != Some(&l.cell) {
                    moved.push((l.cell.clone(), now));
                }
            }
        }
        let mut structure: BTreeSet<String> = BTreeSet::new();
        if !moved.is_empty() {
            for (t, name, f) in &formulas {
                let map = |c: &CellRef| moved.iter().find(|(from, _)| from.same(c)).map(|(_, to)| to.clone());
                if let Some(updated) = rewrite_cells(f, map).map_err(|e| anyhow!("formula error: {e}"))? {
                    self.conn.execute("UPDATE _rf_columns SET formula = ? WHERE table_name = ? AND name = ?", duckdb::params![updated, t, name])?;
                    structure.insert(t.clone());
                }
            }
            if !structure.is_empty() {
                formulas = self.all_sheet_formulas()?;
            }
        }
        // 2. One anchor per cell read on a visible sheet, keeping existing anchors in place.
        let mut wanted: Vec<(CellRef, BTreeSet<String>)> = Vec::new();
        {
            let sheets = self.sheets();
            for (t, _, f) in &formulas {
                for c in cells_read(f) {
                    let Some((_, sheet)) = sheets.visible_sheet(&c.sheet) else { continue };
                    let c = CellRef { sheet, ..c };
                    match wanted.iter_mut().find(|(w, _)| w.same(&c)) {
                        Some((_, readers)) => { readers.insert(t.clone()); }
                        None => wanted.push((c, BTreeSet::from([t.clone()]))),
                    }
                }
            }
        }
        let now_at = |l: &Link| -> Option<CellRef> {
            match moved.iter().find(|(from, _)| from == &l.cell) { Some((_, to)) => to.clone(), None => Some(l.cell.clone()) }
        };
        let mut used: BTreeSet<u32> = BTreeSet::new();
        let mut assigned: Vec<(u32, CellRef, Option<&Link>)> = Vec::new();
        for (w, _) in &wanted {
            if let Some(l) = old.iter().find(|l| !used.contains(&l.anchor) && now_at(l).is_some_and(|c| c.same(w))) {
                used.insert(l.anchor);
                assigned.push((l.anchor, w.clone(), Some(l)));
            }
        }
        let mut anchors: Vec<(u32, Option<CellRef>)> = Vec::new();
        let mut next = 1u32;
        for (w, _) in &wanted {
            if assigned.iter().any(|(_, c, _)| c.same(w)) {
                continue;
            }
            while used.contains(&next) {
                next += 1;
            }
            used.insert(next);
            assigned.push((next, w.clone(), None));
            anchors.push((next, Some(w.clone())));
        }
        for (anchor, cell, link) in &assigned {
            if link.is_some() && missing.contains(anchor) {
                anchors.push((*anchor, Some(cell.clone())));
            }
        }
        for l in &old {
            if !used.contains(&l.anchor) {
                anchors.push((l.anchor, None));
            }
        }
        // A new anchor may reuse the row of one being cleared: clear first.
        anchors.sort_by_key(|(_, c)| c.is_some());
        // 3. Current values, and what they change.
        let mut links: Vec<Link> = Vec::new();
        let mut data: BTreeSet<String> = BTreeSet::new();
        {
            let sheets = self.sheets();
            for (anchor, cell, link) in &assigned {
                let value = sheets.cell_value(cell).unwrap_or(CellValue::Blank);
                let readers = wanted.iter().find(|(w, _)| w.same(cell)).map(|(_, r)| r.clone()).unwrap_or_default();
                match link {
                    Some(l) if l.value == value => {}
                    Some(l) if kind(&l.value) == kind(&value) => data.extend(readers),
                    _ => structure.extend(readers),
                }
                links.push(Link { anchor: *anchor, cell: cell.clone(), value });
            }
        }
        links.sort_by_key(|l| l.anchor);
        if links != old {
            storage::set_links(&self.conn, &links)?;
        }
        for t in &structure {
            storage::update_table(&self.conn, t, None, None)?;
        }
        for t in &data {
            self.linked.entry(t.clone()).or_insert(false);
        }
        for t in &structure {
            self.linked.insert(t.clone(), true);
        }
        for t in data.difference(&structure) {
            storage::touch_table(&self.conn, t)?;
        }
        if !anchors.is_empty() {
            self.sheet_epoch += 1;
            self.sheets().set_anchors(&anchors)?;
            self.save_sheets()?;
        }
        Ok(())
    }

    /// Delete a sheet, refusing while table formulas read its cells.
    pub fn delete_sheet(&mut self, index: u32) -> Result<()> {
        let name = self.sheets().info(index)?.name.to_lowercase();
        let mut users = Vec::new();
        for (t, column, f) in self.all_sheet_formulas()? {
            if cells_read(&f).iter().any(|c| c.sheet.to_lowercase() == name) {
                users.push(format!("{t}.{column:?}"));
            }
        }
        if !users.is_empty() {
            bail!("this sheet is read by formula column{} {}; change or delete {} first",
                if users.len() == 1 { "" } else { "s" }, users.join(", "), if users.len() == 1 { "it" } else { "them" });
        }
        self.with_sheets(|s| s.delete(index))
    }

    /// Tables whose values changed in revision `revision` (the last mutation) without being
    /// what the change was about: tables whose formulas read sheet cells that changed, and
    /// tables whose formulas read, directly or through other tables, any table the revision
    /// changed. Each comes with whether its columns changed too (reopen views, not only
    /// refetch rows).
    pub fn changed_tables(&self, revision: u64) -> Result<Vec<(String, bool)>> {
        let direct: Vec<String> = storage::data_revisions(&self.conn)?.into_iter().filter(|(_, r)| *r == revision).map(|(t, _)| t).collect();
        if direct.is_empty() {
            return Ok(Vec::new());
        }
        let readers = self.db().table_readers()?;
        let names: HashMap<String, String> = storage::user_tables(&self.conn)?.into_iter().map(|t| (t.to_lowercase(), t)).collect();
        let mut out: std::collections::BTreeMap<String, bool> = self.linked.clone();
        let mut seen: BTreeSet<String> = direct.iter().cloned().collect();
        let mut stack: Vec<String> = direct;
        while let Some(t) = stack.pop() {
            for r in readers.get(&t.to_lowercase()).into_iter().flatten() {
                if let Some(actual) = names.get(r) {
                    if seen.insert(actual.clone()) {
                        out.entry(actual.clone()).or_insert(false);
                        stack.push(actual.clone());
                    }
                }
            }
        }
        Ok(out.into_iter().collect())
    }
}
