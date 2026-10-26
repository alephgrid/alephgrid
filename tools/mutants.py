"""Targeted mutation check for Rowfinity's critical engine safeguards.

Run from the repository root: `python3 tools/mutants.py`. Each mutant disables one
safeguard; the relevant engine tests must fail ("KILLED"). Sources are restored after
each run. See docs/TEST_STRATEGY.md.
"""
import pathlib, subprocess, sys
W = pathlib.Path("crates/rowfinity-engine/src/workbook.rs")
S = pathlib.Path("crates/rowfinity-engine/src/sheets.rs")
mutants = [
  ("edit: skip exact-input check", W, "wire::check_exact_input(col, value).map_err(|e| anyhow!(e))?;", "let _ = wire::check_exact_input(col, value);", "tests::precision"),
  ("edit: empty batch not short-circuited", W, "        if edits.is_empty() {\n            return Ok(EditOutcome { table, cells_changed: 0, rows_appended: 0 });\n        }\n", "", "tests::integrity"),
  ("edit: last write wins removed", W, "Some(&i) => cells[i].2 = value,", "Some(&_i) => cells.push((e.row, e.column.as_str(), value)),", "tests::integrity"),
  ("with_sheets: no reload on failure", W, "        if result.is_err() {\n            match self.load_sheets() {", "        if false {\n            match self.load_sheets() {", "tests::sheets"),
  ("sheets: no coordinate validation", S, "            if i.row >= MAX_ROWS || i.column >= MAX_COLUMNS {", "            if false {", "tests::sheets"),
  ("sheets: undo one entry only", S, "        for _ in 0..n {\n            m.undo()", "        for _ in 0..n.min(1) {\n            m.undo()", "tests::sheets"),
  ("drop_column: ignore dependents", W, "        if !users.is_empty() {\n            bail!(", "        if false && !users.is_empty() {\n            bail!(", "tests::formulas"),
  ("rename: no formula rewrite", W, "            if let Some(updated) = formula::rename_reference(&self.conn, f, name, new_name)? {", "            if let Some(updated) = None::<String> {", "tests::formulas"),
  ("expand_query: skip WITH merge", W, "        match crate::sql::leading_with(sql) {", "        match None::<usize> {", "tests::sql"),
  ("typed_columns: no BIGINT inference", S, '                "BIGINT"\n            } else if all', '                "DOUBLE"\n            } else if all', "tests::sheets"),
]
results = []
for name, path, old, new, filt in mutants:
    src = path.read_text()
    if old not in src:
        results.append((name, "MUTATION NOT APPLIED")); continue
    path.write_text(src.replace(old, new, 1))
    try:
        r = subprocess.run(["cargo", "test", "-q", "-p", "rowfinity-engine", "--lib", filt], capture_output=True, text=True)
        out = r.stdout + r.stderr
        if "error[" in out or "could not compile" in out:
            results.append((name, "did not compile"))
        else:
            results.append((name, "KILLED" if r.returncode != 0 else "SURVIVED"))
    finally:
        path.write_text(src)
for n, r in results:
    print(f"{r:22} {n}")
sys.exit(0 if all(r == "KILLED" for _, r in results) else 1)
