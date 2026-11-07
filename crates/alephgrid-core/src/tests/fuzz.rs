//! Bounded fuzz targets (9.14): the table-formula parser and the sheet formula checker, CSV
//! import, and cells pasted from other apps. Each target is a proptest strategy run in
//! chunks until `FUZZ_CASES` cases (default 64) or `FUZZ_SECONDS` seconds (default 30) are
//! used up, whichever comes first, and prints how many cases it ran. Failing inputs are
//! shrunk and saved under `proptest-regressions/tests/fuzz.txt` (the corpus of past
//! failures, replayed first on every later run; CI keeps the folder as an artifact).
//! `PROPTEST_RNG_SEED` replays one run. The scheduled CI tier raises both bounds.
//!
//! Oracles are independent of the code under test: the generator knows the values it wrote
//! into the CSV and the cells it pasted, and where every cell reference must move to.

use super::*;
use crate::table_formula::{self, Reference};
use proptest::prelude::*;
use proptest::test_runner::{Config, TestCaseError, TestRunner};
use serde_json::json;
use std::time::{Duration, Instant};

fn env_number(name: &str, default: u64) -> u64 {
    std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

/// Run `test` on values from `strategy` within the case and time bounds.
fn fuzz<S: Strategy>(name: &str, strategy: S, test: impl Fn(S::Value) -> Result<(), TestCaseError>)
where
    S::Value: std::fmt::Debug,
{
    let cases = env_number("FUZZ_CASES", 64).max(1);
    let budget = Duration::from_secs(env_number("FUZZ_SECONDS", 30));
    let started = Instant::now();
    let mut done = 0;
    while done < cases && started.elapsed() < budget {
        let chunk = (cases - done).min(32) as u32;
        let config = Config { cases: chunk, source_file: Some(file!()), ..Config::default() };
        if let Err(e) = TestRunner::new(config).run(&strategy, &test) {
            panic!("fuzz target {name} failed after {done} earlier cases: {e}");
        }
        done += chunk as u64;
    }
    eprintln!("fuzz {name}: {done} cases in {:.1}s", started.elapsed().as_secs_f64());
}

// ------------------------------------------------------------------ formulas

/// Text built from the characters formulas are made of, so the parser gets past its first
/// token far more often than with arbitrary Unicode.
const FORMULA_CHARS: &str = "=?[A-Za-z0-9$!:'\"(),;+*/^&<>%#@. _\\[\\]{}-]{0,48}";

/// A cell reference as `(sheet in lower case, row, column, (column fixed, row fixed))`.
type CellPosition = (String, i64, i64, (bool, bool));

fn cell_refs(formula: &str) -> Result<Vec<CellPosition>, TestCaseError> {
    let refs = table_formula::references(formula).map_err(|e| TestCaseError::fail(format!("{formula:?} no longer parses: {e}")))?;
    Ok(refs
        .into_iter()
        .filter_map(|r| match r {
            Reference::Cell { cell, fixed, .. } => Some((cell.sheet.to_lowercase(), cell.row as i64, cell.column as i64, fixed)),
            _ => None,
        })
        .collect())
}

/// A sheet cell reference well inside the grid (rows from 10, columns from C), so a shift by
/// at most 5 rows and 2 columns never pushes it off; each part may carry a `$`.
fn cell_ref() -> impl Strategy<Value = String> {
    (prop::sample::select(vec!["Data", "Plan", "'Q1 plan'"]), any::<bool>(), 3u32..60, any::<bool>(), 10u32..5000)
        .prop_map(|(sheet, fc, column, fr, row)| format!("{sheet}!{}{}{}{row}", if fc { "$" } else { "" }, table_formula::column_letters(column), if fr { "$" } else { "" }))
}

/// Well-formed formulas from the grammar in docs/TABLE_FORMULAS.md.
fn formula_expr() -> impl Strategy<Value = String> {
    let leaf = prop_oneof![
        (0u32..100_000).prop_map(|n| n.to_string()),
        (0u32..1000, 1u32..100).prop_map(|(a, b)| format!("{a}.{b}")),
        "[a-z ]{0,6}".prop_map(|s| format!("\"{s}\"")),
        prop::sample::select(vec!["TRUE", "FALSE", "[a]", "[b]", "[@a]", "[amount due]", "Orders[total]"]).prop_map(String::from),
        cell_ref(),
    ];
    leaf.prop_recursive(4, 24, 3, |inner| {
        prop_oneof![
            (inner.clone(), prop::sample::select(vec!["+", "-", "*", "/", "^", "&", "=", "<>", "<", ">", "<=", ">="]), inner.clone())
                .prop_map(|(a, op, b)| format!("{a}{op}{b}")),
            inner.clone().prop_map(|a| format!("-{a}")),
            inner.clone().prop_map(|a| format!("({a})")),
            (prop::sample::select(vec!["SUM", "IF", "ROUND", "MAX", "MIN", "AND", "CONCAT", "LEN"]), prop::collection::vec(inner, 1..4))
                .prop_map(|(f, args)| format!("{f}({})", args.join(","))),
        ]
    })
    .prop_map(|e| format!("={e}"))
}

/// The table-formula parser and the functions built on it return a value or an error for
/// any input; they never panic or hang.
#[test]
fn table_formula_functions_are_total_on_any_text() {
    let columns = vec![("a".to_string(), "DOUBLE".to_string()), ("b".to_string(), "VARCHAR".to_string())];
    let input = prop_oneof![FORMULA_CHARS.prop_map(String::from), "\\PC{0,64}".prop_map(String::from), formula_expr()];
    fuzz("table_formula_any_text", input, |f| {
        let parsed = table_formula::parse(&f).is_ok();
        // references() and the rewriting functions agree with the parser on what parses.
        prop_assert_eq!(table_formula::references(&f).is_ok(), parsed, "{:?}", f);
        prop_assert_eq!(table_formula::shift_cells(&f, 3, -1).is_ok(), parsed, "{:?}", f);
        let _ = table_formula::compile(&f, &columns, "_rf_fz_");
        let _ = table_formula::rename_column(&f, "a", "new name");
        let _ = table_formula::qualify_cells(&f, "Data");
        let _ = table_formula::unqualify_cells(&f, "Data");
        Ok(())
    });
}

/// Every formula from the grammar parses, and shifting it moves each relative part of each
/// cell reference by exactly the shift (a `$` keeps its part); shifting back restores the
/// references.
#[test]
fn generated_formulas_parse_and_shift_exactly() {
    fuzz("table_formula_shift", (formula_expr(), -5i64..=5, -2i64..=2), |(f, rows, columns)| {
        prop_assert!(table_formula::parse(&f).is_ok(), "the grammar produced {:?}, which does not parse: {:?}", f, table_formula::parse(&f).err());
        let before = cell_refs(&f)?;
        let shifted = table_formula::shift_cells(&f, rows, columns).unwrap().unwrap_or_else(|| f.clone());
        let expected: Vec<_> = before
            .iter()
            .map(|(sheet, row, column, fixed)| (sheet.clone(), if fixed.1 { *row } else { row + rows }, if fixed.0 { *column } else { column + columns }, *fixed))
            .collect();
        prop_assert_eq!(&cell_refs(&shifted)?, &expected, "{:?} shifted by ({}, {}) gave {:?}", f, rows, columns, shifted);
        let back = table_formula::shift_cells(&shifted, -rows, -columns).unwrap().unwrap_or_else(|| shifted.clone());
        prop_assert_eq!(cell_refs(&back)?, before);
        Ok(())
    });
}

/// The sheet formula checker (IronCalc's parser, as used when a cell is typed) answers for
/// any text. Found by this target on 7 Nov 2026: `=gaaAaAA$` (a seven-letter column)
/// overflowed in IronCalc's `column_to_number`; fixed in the vendored copy.
#[test]
fn sheet_formula_check_is_total_on_any_text() {
    let mut wb = Workbook::open_in_memory().unwrap();
    wb.with_sheets(|s| s.create("Data").map(|_| ())).unwrap();
    for f in ["=gaaAaAA$", "=GAAAAAA1", "=ZZZZZZZZZZZZ1+1", "=A1:ZZZZZZZZ2"] {
        assert!(wb.sheets().check_formula(0, 0, 0, f).is_ok(), "{f:?}");
    }
    let input = prop_oneof![FORMULA_CHARS.prop_map(String::from), "=\\PC{0,48}".prop_map(String::from), formula_expr()];
    fuzz("sheet_formula_check", (input, 0u32..50, 0u32..20), |(f, row, column)| {
        let s = wb.sheets();
        prop_assert!(s.check_formula(0, row, column, &f).is_ok(), "{:?}", f);
        Ok(())
    });
}

// ------------------------------------------------------------------ CSV import

fn csv_quote(v: &str) -> String {
    format!("\"{}\"", v.replace('"', "\"\""))
}

/// A CSV file written with standard quoting imports exactly the values written, in order:
/// delimiters, quotes, newlines, formula-like text and non-ASCII inside quoted values are
/// data. The first column is a row number so the header is recognisable; the others are
/// imported as text. Found by this target on 7 Nov 2026: with a tab and a `;` only inside
/// quoted values, DuckDB's sniffer took `;` as the delimiter; fixed in
/// `import::csv_delimiter`.
#[test]
fn quoted_csv_imports_exactly_what_was_written() {
    let value = "[a-zA-Z0-9 ,;\t\"'\n=+@.%é漢😀-]{1,12}";
    let table = (1usize..5).prop_flat_map(move |columns| prop::collection::vec(prop::collection::vec(value, columns), 1..20));
    let check = |rows: Vec<Vec<String>>| -> Result<(), TestCaseError> {
        let columns = rows[0].len();
        let dir = tempfile::tempdir().unwrap();
        let mut text = String::from("n");
        for c in 1..=columns {
            text.push_str(&format!(",c{c}"));
        }
        text.push('\n');
        for (i, row) in rows.iter().enumerate() {
            text.push_str(&(i + 1).to_string());
            for v in row {
                text.push(',');
                text.push_str(&csv_quote(v));
            }
            text.push('\n');
        }
        let path = dir.path().join("fuzz.csv");
        std::fs::write(&path, &text).unwrap();
        let mut types = vec![ColumnType { name: "n".into(), sql_type: "BIGINT".into() }];
        types.extend((1..=columns).map(|c| ColumnType { name: format!("c{c}"), sql_type: "VARCHAR".into() }));
        let mut wb = Workbook::open_in_memory().unwrap();
        let t = wb.import_file_with(&path, Some("fuzz"), &types).map_err(|e| TestCaseError::fail(format!("import failed: {e:#}\n{text}")))?;
        prop_assert_eq!(t.row_count, rows.len() as u64, "{}", text);
        let expected: Vec<Vec<Value>> = rows
            .iter()
            .enumerate()
            .map(|(i, row)| std::iter::once(json!(i + 1)).chain(row.iter().map(|v| json!(v))).collect())
            .collect();
        let v = wb.open_view(spec("fuzz")).unwrap().view_id;
        prop_assert_eq!(all_rows(&wb, v), expected, "{}", text);
        Ok(())
    };
    let strings = |rows: &[&[&str]]| rows.iter().map(|r| r.iter().map(|v| v.to_string()).collect()).collect::<Vec<Vec<String>>>();
    for fixed in [strings(&[&[" ", "\t"], &["\t;", "a"]]), strings(&[&["a;b", "c\td"], &["e|f", ";"]]), strings(&[&["'"], &[",A"]])] {
        check(fixed).unwrap();
    }
    fuzz("csv_round_trip", table, check);
}

/// Any bytes named `.csv` either import (and the table holds the rows reported) or fail
/// with a message, leaving no table behind; preview never creates anything.
#[test]
fn any_bytes_import_or_fail_cleanly() {
    let bytes = prop_oneof![
        prop::collection::vec(any::<u8>(), 0..256),
        "[a-z0-9,;\"\n\r\t ]{0,200}".prop_map(|s| s.into_bytes()),
    ];
    fuzz("csv_any_bytes", bytes, |data| {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("any.csv");
        std::fs::write(&path, &data).unwrap();
        let mut wb = Workbook::open_in_memory().unwrap();
        let _ = wb.preview_file(&path, &[]);
        prop_assert!(wb.list_tables().unwrap().is_empty(), "a preview created a table");
        match wb.import_file(&path, Some("anything")) {
            Ok(t) => {
                prop_assert_eq!(scalar(&wb, "SELECT count(*) FROM anything"), json!(t.row_count));
            }
            Err(e) => {
                let problem = crate::import::explain_error(&format!("{e:#}"));
                prop_assert!(!problem.message.trim().is_empty());
                prop_assert!(wb.list_tables().unwrap().is_empty(), "a failed import left a table");
            }
        }
        Ok(())
    });
}

// ------------------------------------------------------------------ pasted cells

fn pasted_cell() -> impl Strategy<Value = PastedCell> {
    ("[a-zA-Z0-9 =+@'\"\t.,%$:()-]{0,10}", any::<bool>()).prop_map(|(value, text)| PastedCell { value, text, style: None, formula: None })
}

/// Cells pasted from another app (F.04b): text cells and anything starting with `=` keep
/// exactly the pasted characters, a typed value's leading apostrophe is a quote prefix, and
/// no pasted value becomes a formula (no input starts with `=`); missing cells of a ragged
/// row clear the target; one undo step empties it again. Found by this target on 7 Nov
/// 2026: the text `+""` stayed the formula `=+""` (its result is text, which passed the
/// text check); fixed in `Sheets::set_values`.
#[test]
fn pasted_cells_keep_text_and_clear_and_undo() {
    let grid = prop::collection::vec(prop::collection::vec(pasted_cell(), 1..4), 1..4);
    let check = |(cells, row, column): (Vec<Vec<PastedCell>>, u32, u32)| -> Result<(), TestCaseError> {
        let mut wb = Workbook::open_in_memory().unwrap();
        wb.with_sheets(|s| s.create("Paste").map(|_| ())).unwrap();
        let width = cells.iter().map(|r| r.len()).max().unwrap() as u32;
        let area = wb.with_sheets(|s| s.paste_cells(0, (row, column), &cells, PasteMode::All)).map_err(|e| TestCaseError::fail(format!("{e:#}")))?;
        prop_assert_eq!(area, CellArea { row, column, rows: cells.len() as u32, columns: width });
        let s = wb.sheets();
        for (dr, line) in cells.iter().enumerate() {
            for dc in 0..width as usize {
                let d = s.cell_detail(0, row + dr as u32, column + dc as u32).unwrap();
                prop_assert!(!d.input.starts_with('='), "pasted {:?} became the formula {:?}", line.get(dc), d.input);
                match line.get(dc) {
                    None => prop_assert!(d.input.is_empty() && d.display.is_empty(), "a missing cell was not cleared: {:?}", d),
                    Some(p) if p.value.is_empty() => prop_assert!(d.display.is_empty(), "{:?}", d),
                    Some(p) if p.text || p.value.starts_with('=') => prop_assert_eq!(&d.display, &p.value, "pasted {:?}", p),
                    // A typed value: the apostrophe is a quote prefix, the rest is the text.
                    Some(p) if p.value.starts_with('\'') => prop_assert_eq!(&d.display, &p.value[1..], "pasted {:?}", p),
                    Some(_) => {} // numbers, dates and so on are read as typed
                }
            }
        }
        drop(s);
        prop_assert!(wb.with_sheets(|s| s.undo()).unwrap(), "one undo step");
        let s = wb.sheets();
        for r in 0..cells.len() as u32 {
            for c in 0..width {
                prop_assert!(s.cell_detail(0, row + r, column + c).unwrap().display.is_empty());
            }
        }
        Ok(())
    };
    let cell = |value: &str, text: bool| PastedCell { value: value.into(), text, style: None, formula: None };
    check((vec![vec![cell("+\"\"", true)]], 0, 0)).unwrap();
    check((vec![vec![cell("-\"a\"&\"b\"", true), cell("'=A1", false), cell("'", false)]], 1, 1)).unwrap();
    fuzz("paste_cells", (grid, 0u32..5, 0u32..5), check);
}

/// "Formulas are never made from pasted text" (`Sheets::paste_cells`): a value pasted as a
/// typed value (not text) that starts with `+`, `-` or `@` must not become a formula either.
/// Found by this target on 7 Nov 2026 (`-"` and `+A1` were stored as formulas); fixed in
/// `Sheets::set_values`. Found again the same day: `'=` (a quote prefix, then `=`) showed
/// the input `=`, because replacing the target's formats also dropped the quote prefix,
/// which IronCalc keeps in the style; fixed in `Sheets::paste_cells`. A typed `'=A1` is
/// the text `=A1`, and its input keeps the apostrophe so editing it again keeps it text.
#[test]
fn pasted_values_never_become_formulas() {
    let mut wb = Workbook::open_in_memory().unwrap();
    wb.with_sheets(|s| s.create("Paste").map(|_| ())).unwrap();
    for value in ["-\"", "+A1", "-A1", "@SUM(1)", "'=", "'=A1"] {
        wb.with_sheets(|s| s.paste_cells(0, (0, 0), &[vec![PastedCell { value: value.into(), text: false, style: None, formula: None }]], PasteMode::All)).unwrap();
        let d = wb.sheets().cell_detail(0, 0, 0).unwrap();
        assert!(!d.input.starts_with('='), "{value:?} became the formula {:?}", d.input);
    }
    for (value, display, input) in [("'=", "=", "'="), ("'=A1", "=A1", "'=A1"), ("'00123", "00123", "'00123")] {
        wb.with_sheets(|s| s.paste_cells(0, (0, 0), &[vec![PastedCell { value: value.into(), text: false, style: None, formula: None }]], PasteMode::All)).unwrap();
        let d = wb.sheets().cell_detail(0, 0, 0).unwrap();
        assert_eq!((d.display.as_str(), d.input.as_str()), (display, input), "pasted {value:?}");
    }
    fuzz("paste_values_not_formulas", pasted_cell(), |p| {
        let mut wb = Workbook::open_in_memory().unwrap();
        wb.with_sheets(|s| s.create("Paste").map(|_| ())).unwrap();
        wb.with_sheets(|s| s.paste_cells(0, (0, 0), &[vec![p.clone()]], PasteMode::All)).map_err(|e| TestCaseError::fail(format!("{e:#}")))?;
        let d = wb.sheets().cell_detail(0, 0, 0).unwrap();
        prop_assert!(p.value.starts_with('=') || !d.input.starts_with('='), "{:?} became the formula {:?}", p.value, d.input);
        Ok(())
    });
}

/// Text pasted as text keeps every character, including a leading apostrophe (`'90s`).
/// Found by the paste target on 7 Nov 2026 (the apostrophe was read as a quote prefix and
/// dropped); fixed in `Sheets::set_values`, which also copies query results into sheets.
#[test]
fn pasted_text_keeps_a_leading_apostrophe() {
    let mut wb = Workbook::open_in_memory().unwrap();
    wb.with_sheets(|s| s.create("Paste").map(|_| ())).unwrap();
    for value in ["'", "'90s", "''quoted'"] {
        wb.with_sheets(|s| s.paste_cells(0, (0, 0), &[vec![PastedCell { value: value.into(), text: true, style: None, formula: None }]], PasteMode::All)).unwrap();
        assert_eq!(wb.sheets().cell_detail(0, 0, 0).unwrap().display, value);
    }
    let text = "'[a-zA-Z0-9 '=+-]{0,8}".prop_map(|value| PastedCell { value, text: true, style: None, formula: None });
    fuzz("paste_leading_apostrophe", text, |p| {
        let mut wb = Workbook::open_in_memory().unwrap();
        wb.with_sheets(|s| s.create("Paste").map(|_| ())).unwrap();
        wb.with_sheets(|s| s.paste_cells(0, (0, 0), &[vec![p.clone()]], PasteMode::All)).map_err(|e| TestCaseError::fail(format!("{e:#}")))?;
        prop_assert_eq!(wb.sheets().cell_detail(0, 0, 0).unwrap().display, p.value);
        Ok(())
    });
}
