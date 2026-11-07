"""Targeted mutation check for Alephgrid's critical engine safeguards.

Run from the repository root: `python3 tools/mutants.py [NAME ...]` (all mutants, or those
whose name contains one of the NAMEs). Each mutant disables one safeguard; the relevant
engine tests must fail ("KILLED"). A search text must occur exactly once, so the mutant
lands where intended. Sources are restored after each run. See docs/TEST_STRATEGY.md.
"""
import pathlib, subprocess, sys
W = pathlib.Path("crates/alephgrid-core/src/workbook.rs")
S = pathlib.Path("crates/alephgrid-core/src/sheets.rs")
R = pathlib.Path("crates/alephgrid-core/src/reads.rs")
mutants = [
  ("edit: skip exact-input check", W, "crate::wire::check_exact_input(col, value).map_err(|e| anyhow!(e))?;", "let _ = crate::wire::check_exact_input(col, value);", "tests::precision"),
  ("edit: empty batch not short-circuited", W, "        if edits.is_empty() {\n            // Nothing to write", "        if false {\n            // Nothing to write", "tests::integrity"),
  ("edit: last write wins removed", W, "Some(&i) => cells[i].2 = value,", "Some(&_i) => cells.push((t, e.column.as_str(), value)),", "tests::integrity"),
  ("with_sheets: no reload on failure", W, "                if self.sheet_epoch != epoch || panicked {\n                    match self.load_sheets() {", "                if false {\n                    match self.load_sheets() {", "tests::sheets"),
  ("sheets: no coordinate validation", S, "            if i.row >= MAX_ROWS || i.column >= MAX_COLUMNS {", "            if false {", "tests::sheets"),
  ("sheets: undo one entry only", S, "        for _ in 0..n {\n            m.undo()", "        for _ in 0..n.min(1) {\n            m.undo()", "tests::sheets"),
  ("drop_column: ignore dependents", W, "        if !users.is_empty() {\n            bail!(\"{name:?} is used by formula column", "        if false && !users.is_empty() {\n            bail!(\"{name:?} is used by formula column", "tests::formulas"),
  ("rename: no formula rewrite", W, "FormulaLanguage::Sql => formula::rename_reference(&self.conn, f, name, new_name)?,", "FormulaLanguage::Sql => None,", "tests::formulas"),
  ("expand_query: skip WITH merge", R, "        match crate::sql::leading_with(sql) {", "        match None::<usize> {", "tests::sql"),
  ("typed_columns: no BIGINT inference", S, '                "BIGINT"\n            } else if all', '                "DOUBLE"\n            } else if all', "tests::sheets"),
]
results = []
chosen = sys.argv[1:]
# A sixth element marks a known survivor: reported, but not a failure. Each one names the
# quarantined test (docs/TEST_STRATEGY.md) whose fix will kill it.
known = {m[0]: m[5] for m in mutants if len(m) > 5}
for name, path, old, new, filt, *_ in mutants:
    if chosen and not any(c in name for c in chosen):
        continue
    src = path.read_text()
    if src.count(old) != 1:
        results.append((name, "MUTATION NOT APPLIED" if old not in src else "SEARCH TEXT NOT UNIQUE")); continue
    path.write_text(src.replace(old, new, 1))
    try:
        r = subprocess.run(["cargo", "test", "-q", "-p", "alephgrid-core", "--lib", filt], capture_output=True, text=True)
        out = r.stdout + r.stderr
        if "error[" in out or "could not compile" in out:
            results.append((name, "did not compile"))
        else:
            results.append((name, "KILLED" if r.returncode != 0 else "SURVIVED"))
    finally:
        path.write_text(src)
for n, r in results:
    print(f"{r:22} {n}" + (f"  (known: {known[n]})" if r == "SURVIVED" and n in known else ""))
sys.exit(0 if all(r == "KILLED" or (r == "SURVIVED" and n in known) for n, r in results) else 1)
