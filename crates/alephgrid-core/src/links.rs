//! Sheet cells read by table formulas (F.09): keeping formulas, anchors and saved values in
//! step with the sheets, inside the transaction of the change that moved them.
//!
//! Table reads never look at the live sheet model: they read each cell's value from
//! `_rf_links`, in their own snapshot, so a page of rows and the cell values it was computed
//! from always belong to the same committed revision.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use anyhow::{anyhow, bail, Result};

use crate::reads::Db;
use crate::sheets::{Anchor, SlotCell, LINK_SHEET};
use crate::storage::{self, Link};
use crate::table_formula::{references, rewrite_cells, CellRef, CellValue, Reference};
use crate::workbook::Workbook;

/// The sheet cells a table formula reads, as written.
pub(crate) fn cells_read(formula: &str) -> Vec<CellRef> {
    references(formula).map(|refs| refs.into_iter().filter_map(|r| match r { Reference::Cell { cell, .. } => Some(cell), _ => None }).collect()).unwrap_or_default()
}

/// The tables a formula reads whole columns of (lowercase).
pub(crate) fn tables_read(formula: &str) -> Vec<String> {
    references(formula)
        .map(|refs| refs.into_iter().filter_map(|r| match r { Reference::Column { table, .. } | Reference::Table { table, .. } => Some(table.to_lowercase()), _ => None }).collect())
        .unwrap_or_default()
}

/// What one pass over the anchors changed.
#[derive(Default)]
struct Delta {
    /// Linked cells whose saved value changed.
    cells: Vec<CellRef>,
    /// Tables (lowercase) whose formulas read those cells or were rewritten.
    tables: BTreeSet<String>,
    /// Table cells whose formulas were rewritten.
    slots: BTreeSet<u32>,
}

/// `None` when `input` is not a table formula: not a formula, or one the sheet engine reads.
/// Otherwise the formula as stored (bare cells qualified by their sheet, no `=`) when it
/// compiles as a table cell's formula, reading sheet cells from the live sheets (entering
/// it links them), or what is wrong with it.
pub(crate) fn table_cell_problem(db: &Db, sheet: u32, row: u32, column: u32, input: &str) -> Result<Option<std::result::Result<String, crate::sheets::FormulaProblem>>> {
    let trimmed = input.trim_start();
    if !trimmed.starts_with('=') {
        return Ok(None);
    }
    let (problem, names) = {
        let sheets = db.shared.sheets.lock().unwrap_or_else(|e| e.into_inner());
        (sheets.check_formula(sheet, row, column, input)?, sheets.names())
    };
    // The sheet engine reports names it does not know as tables only after `name[`; a table
    // named alone (`SORT(orders)`) reads as an unknown name there, unless a name defines it.
    let unknown_table = problem.as_ref().is_some_and(|p| p.message.starts_with("Table not found"));
    let names_table = problem.is_none() && crate::table_formula::references(trimmed).is_ok_and(|refs| refs.iter().any(|r| match r {
        Reference::Table { table, .. } => !names.iter().any(|n| n.name.eq_ignore_ascii_case(table)) && db.table_exists(table).unwrap_or(false),
        _ => false,
    }));
    if !unknown_table && !names_table {
        return Ok(None);
    }
    let lead = input.chars().count() - trimmed.chars().count();
    let home = db.shared.sheets.lock().unwrap_or_else(|e| e.into_inner()).info(sheet)?.name;
    let problem = |message: String, at: usize| crate::sheets::FormulaProblem { message, position: (lead + at) as u32 };
    let (qualified, back) = match crate::table_formula::qualify_cells(trimmed, &home) {
        Ok(q) => q,
        Err(e) => return Ok(Some(Err(problem(e.message, e.span.start)))),
    };
    Ok(Some(match db.table_cell_value(&qualified, true)? {
        Ok(_) => Ok(qualified.trim_start_matches('=').to_string()),
        Err(e) => Err(problem(e.message, back(e.span.start))),
    }))
}

/// The table-formula type a cell value gets; a change of type changes the column's type.
fn kind(v: &CellValue) -> u8 {
    match v {
        CellValue::Text(_) => 1,
        CellValue::Bool(_) => 2,
        CellValue::Blank | CellValue::Number(_) | CellValue::Error(_) => 0,
        CellValue::Array(_) => 3,
    }
}

impl Workbook {
    /// Bring links and table cells up to date, inside the caller's transaction (see
    /// [`Self::sync_anchors`] and the module documentation). Table cells whose inputs
    /// changed are recomputed, in rounds, until no value changes: a table cell's value can
    /// change sheet cells that table formulas read, which can change other table cells.
    /// Circular references between sheets and tables are refused before anything is
    /// computed.
    pub(crate) fn sync_links(&mut self) -> Result<()> {
        // Most workbooks have no links: one small query, before anything is parsed.
        let candidates: i64 = self.conn.query_row(
            "SELECT (SELECT count(*) FROM _rf_links) + (SELECT count(*) FROM _rf_table_cells)
                  + (SELECT count(*) FROM _rf_columns WHERE language = 'sheet' AND contains(formula, '!'))",
            [],
            |r| r.get(0),
        )?;
        if candidates == 0 {
            return Ok(());
        }
        let mut cells = storage::table_cells(&self.conn)?;
        let placed = if cells.is_empty() { Vec::new() } else { self.sheets().slot_cells() };
        let in_use: BTreeSet<u32> = placed.iter().map(|p| p.slot).filter(|s| cells.iter().any(|c| c.slot == *s)).collect();
        let delta = self.sync_anchors(&mut cells, &in_use)?;
        if cells.is_empty() {
            return Ok(());
        }
        self.sheets().set_table_cells(cells.iter().map(|c| (c.slot, c.formula.clone())).collect());
        if in_use.is_empty() {
            return Ok(());
        }
        let readers = self.db().table_readers()?;
        self.check_cross_cycles(&cells, &placed, &in_use, &readers)?;
        let affected = |changed: BTreeSet<String>| -> BTreeSet<String> {
            let mut out = changed.clone();
            let mut stack: Vec<String> = changed.into_iter().collect();
            while let Some(t) = stack.pop() {
                for r in readers.get(&t).into_iter().flatten() {
                    if out.insert(r.clone()) {
                        stack.push(r.clone());
                    }
                }
            }
            out
        };
        let changing: BTreeSet<String> = storage::changing_tables(&self.conn)?.into_iter().map(|t| t.to_lowercase()).collect();
        let mut dirty = affected(changing.difference(&self.settled).cloned().chain(delta.tables.iter().cloned()).collect());
        let mut changed_cells = delta.cells;
        let mut rewritten = delta.slots;
        let mut wrote = !rewritten.is_empty();
        let rounds = in_use.len() + 2;
        for round in 0.. {
            let mut writes: Vec<(u32, CellValue)> = Vec::new();
            for c in cells.iter_mut().filter(|c| in_use.contains(&c.slot)) {
                let stale = c.value.is_none()
                    || rewritten.contains(&c.slot)
                    || tables_read(&c.formula).iter().any(|t| dirty.contains(t))
                    || cells_read(&c.formula).iter().any(|r| changed_cells.iter().any(|x| x.same(r)));
                if !stale {
                    continue;
                }
                // A formula that no longer compiles (what it read was renamed or deleted
                // outside Alephgrid, or an undo brought back an old formula) shows #REF!.
                crate::faults::check("table_cell")?;
                let value = self.db().table_cell_value(&c.formula, false)?.unwrap_or_else(|_| CellValue::Error("#REF!".into()));
                if c.value.as_ref() != Some(&value) {
                    storage::set_table_cell_value(&self.conn, c.slot, &value)?;
                    c.value = Some(value.clone());
                    writes.push((c.slot, value));
                }
            }
            if writes.is_empty() {
                break;
            }
            if round + 1 >= rounds {
                bail!("the values of sheet cells and the tables they read kept changing; check for a circular reference through INDIRECT or OFFSET");
            }
            self.sheets().set_slot_values(&writes)?;
            wrote = true;
            let next = self.sync_anchors(&mut cells, &in_use)?;
            dirty = affected(next.tables);
            changed_cells = next.cells;
            rewritten = next.slots;
        }
        self.settled = storage::changing_tables(&self.conn)?.into_iter().map(|t| t.to_lowercase()).collect();
        if wrote {
            self.sheet_epoch += 1;
            self.derived_sheets = true;
            self.save_sheets()?;
        }
        Ok(())
    }

    /// Refuse circular references between table cells: a table cell reads tables (and the
    /// tables those read), whose formulas read sheet cells, which may depend on table cells
    /// through sheet formulas; the cells a table cell reads directly count too.
    fn check_cross_cycles(&self, cells: &[storage::TableCell], placed: &[SlotCell], in_use: &BTreeSet<u32>, readers: &HashMap<String, Vec<String>>) -> Result<()> {
        // Tables each table reads, from who reads whom.
        let mut reads: HashMap<String, Vec<String>> = HashMap::new();
        for (read, by) in readers {
            for r in by {
                reads.entry(r.clone()).or_default().push(read.clone());
            }
        }
        let mut table_cells: HashMap<String, Vec<CellRef>> = HashMap::new();
        for (t, _, f) in self.all_sheet_formulas()? {
            table_cells.entry(t.to_lowercase()).or_default().extend(cells_read(&f));
        }
        let mut edges: BTreeMap<u32, BTreeSet<u32>> = BTreeMap::new();
        for c in cells.iter().filter(|c| in_use.contains(&c.slot)) {
            let mut tables: BTreeSet<String> = tables_read(&c.formula).into_iter().collect();
            let mut stack: Vec<String> = tables.iter().cloned().collect();
            while let Some(t) = stack.pop() {
                for r in reads.get(&t).into_iter().flatten() {
                    if tables.insert(r.clone()) {
                        stack.push(r.clone());
                    }
                }
            }
            let mut feed = cells_read(&c.formula);
            for t in &tables {
                feed.extend(table_cells.get(t).cloned().unwrap_or_default());
            }
            let feeds = self.sheets().slots_feeding(&feed).map_err(|e| anyhow!("{e}"))?;
            edges.insert(c.slot, feeds.into_iter().filter(|s| in_use.contains(s)).collect());
        }
        let name = |slot: u32| placed.iter().find(|p| p.slot == slot).map(|p| p.cell.text((false, false))).unwrap_or_else(|| format!("table cell {slot}"));
        // Depth-first search; a slot met again on the current path closes a cycle.
        fn visit(s: u32, edges: &BTreeMap<u32, BTreeSet<u32>>, path: &mut Vec<u32>, done: &mut BTreeSet<u32>) -> Option<Vec<u32>> {
            if let Some(at) = path.iter().position(|p| *p == s) {
                let mut cycle = path[at..].to_vec();
                cycle.push(s);
                return Some(cycle);
            }
            if !done.insert(s) {
                return None;
            }
            path.push(s);
            for n in edges.get(&s).into_iter().flatten() {
                if let Some(c) = visit(*n, edges, path, done) {
                    return Some(c);
                }
            }
            path.pop();
            None
        }
        let mut done = BTreeSet::new();
        for s in edges.keys() {
            if let Some(cycle) = visit(*s, &edges, &mut Vec::new(), &mut done) {
                if cycle.len() == 2 {
                    bail!("circular reference: {} depends on its own value through the tables and sheet cells its formula reads", name(cycle[0]));
                }
                bail!("circular reference: {} (through the tables and sheet cells their formulas read)", cycle.into_iter().map(name).collect::<Vec<_>>().join(" → "));
            }
        }
        Ok(())
    }

    /// Enter sheet cells. A formula naming a table (`=SUMIFS(orders[amount], …)`), which the
    /// sheet engine cannot read, becomes a table cell: its formula is checked and stored with
    /// a new result slot, and the sheet engine holds a reference to that slot.
    pub fn set_sheet_cells(&mut self, sheet: u32, inputs: &[crate::sheets::SheetInput]) -> Result<()> {
        let mut inputs = inputs.to_vec();
        let mut new: Vec<(usize, String)> = Vec::new();
        for (k, i) in inputs.iter().enumerate() {
            if let Some(formula) = self.table_formula_of(sheet, i.row, i.column, &i.value)? {
                new.push((k, formula));
            }
        }
        let spills: Vec<bool> = new.iter().map(|(_, f)| self.db().table_cell_spills(f)).collect();
        self.mutating("sheets", None, |wb| {
            for ((k, formula), spills) in new.iter().zip(spills) {
                let slot = storage::add_table_cell(&wb.conn, formula)?;
                if spills {
                    // The reference must find its sheet when the sheet engine reads it.
                    wb.sheet_epoch += 1;
                    wb.sheets().ensure_spill_sheet(slot)?;
                }
                inputs[*k].value = crate::sheets::slot_input(slot, spills);
            }
            wb.with_sheets(|s| s.set(sheet, &inputs))
        })
    }

    /// The table formula of an input, without its `=`, when it is one (see
    /// [`Self::set_sheet_cells`]); an error says what is wrong with it.
    fn table_formula_of(&self, sheet: u32, row: u32, column: u32, input: &str) -> Result<Option<String>> {
        let at = format!("{}{}", crate::sheets::column_name(column), row + 1);
        if crate::sheets::slot_of(input.trim()).is_some() {
            bail!("{at}: {LINK_SHEET} is reserved for Alephgrid's own use");
        }
        match self.table_cell_problem(sheet, row, column, input)? {
            None => Ok(None),
            Some(Ok(formula)) => Ok(Some(formula)),
            Some(Err(p)) => bail!("{at}: {} (at character {})", p.message, p.position + 1),
        }
    }

    /// `None` when `input` is not a table formula; otherwise whether it can be computed.
    pub(crate) fn table_cell_problem(&self, sheet: u32, row: u32, column: u32, input: &str) -> Result<Option<std::result::Result<String, crate::sheets::FormulaProblem>>> {
        table_cell_problem(&self.db(), sheet, row, column, input)
    }

    /// Bring the anchors and saved values of linked cells up to date, inside the caller's
    /// transaction:
    ///
    /// 1. follow anchors the sheet engine moved (row/column inserts and deletes, moves, sheet
    ///    renames, undo), rewriting the formulas that read those cells; a deleted cell
    ///    becomes `#REF!`;
    /// 2. keep exactly one anchor per cell that some formula reads;
    /// 3. save each cell's current value, and mark the tables whose inputs changed: a new
    ///    value changes their data revision, a new value type or a rewritten formula their
    ///    structure too.
    ///
    /// `slots` are the table cells in use; the cells their formulas read are linked too, and
    /// their formulas follow moved cells like table formulas do (all of `cells` are rewritten,
    /// so undoing their removal finds them pointing at the right cells).
    fn sync_anchors(&mut self, cells: &mut [storage::TableCell], slots: &BTreeSet<u32>) -> Result<Delta> {
        let mut delta = Delta::default();
        let old = storage::links(&self.conn)?;
        let mut formulas = self.all_sheet_formulas()?;
        let slot_reads = |cells: &[storage::TableCell]| -> Vec<CellRef> {
            cells.iter().filter(|c| slots.contains(&c.slot)).flat_map(|c| cells_read(&c.formula)).collect()
        };
        if old.is_empty() && formulas.iter().all(|(_, _, f)| cells_read(f).is_empty()) && slot_reads(cells).is_empty() {
            return Ok(delta);
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
            for c in cells.iter_mut() {
                let map = |r: &CellRef| moved.iter().find(|(from, _)| from.same(r)).map(|(_, to)| to.clone());
                if let Some(updated) = rewrite_cells(&c.formula, map).map_err(|e| anyhow!("formula error: {e}"))? {
                    storage::set_table_cell_formula(&self.conn, c.slot, &updated)?;
                    c.formula = updated;
                    delta.slots.insert(c.slot);
                }
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
            for c in slot_reads(cells) {
                let Some((_, sheet)) = sheets.visible_sheet(&c.sheet) else { continue };
                let c = CellRef { sheet, ..c };
                if !wanted.iter().any(|(w, _)| w.same(&c)) {
                    wanted.push((c, BTreeSet::new()));
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
                if link.is_none_or(|l| l.value != value) {
                    delta.cells.push(cell.clone());
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
        delta.tables = data.union(&structure).map(|t| t.to_lowercase()).collect();
        Ok(delta)
    }

    /// Table cells on the sheets now, with their formulas, in sheet order.
    pub(crate) fn table_cells_in_use(&self) -> Result<Vec<(CellRef, String)>> {
        let cells = storage::table_cells(&self.conn)?;
        if cells.is_empty() {
            return Ok(Vec::new());
        }
        let placed = self.sheets().slot_cells();
        Ok(placed.into_iter().filter_map(|p| cells.iter().find(|c| c.slot == p.slot).map(|c| (p.cell, c.formula.clone()))).collect())
    }

    /// Rewrite every table cell's formula with `f` (`Ok(None)`: unchanged), in the caller's
    /// transaction. Cells no longer on a sheet are rewritten too, for undo.
    pub(crate) fn rewrite_table_cells(&self, f: impl Fn(&str) -> Result<Option<String>>) -> Result<()> {
        for c in storage::table_cells(&self.conn)? {
            if let Some(updated) = f(&c.formula)? {
                storage::set_table_cell_formula(&self.conn, c.slot, &updated)?;
            }
        }
        Ok(())
    }

    /// Delete a sheet, refusing while table formulas read its cells.
    pub fn delete_sheet(&mut self, index: u32) -> Result<()> {
        let name = self.sheets().info(index)?.name.to_lowercase();
        let mut users = Vec::new();
        for (t, column, f) in self.all_sheet_formulas()? {
            if cells_read(&f).iter().any(|c| c.sheet.to_lowercase() == name) {
                users.push(format!("formula column {t}.{column:?}"));
            }
        }
        for (place, f) in self.table_cells_in_use()? {
            if cells_read(&f).iter().any(|c| c.sheet.to_lowercase() == name) && place.sheet.to_lowercase() != name {
                users.push(format!("sheet cell {}", place.text((false, false))));
            }
        }
        if !users.is_empty() {
            bail!("this sheet is read by {}; change or delete {} first", users.join(", "), if users.len() == 1 { "it" } else { "them" });
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
