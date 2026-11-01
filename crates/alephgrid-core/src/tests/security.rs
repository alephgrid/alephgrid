//! SEC.T01 (engine side): regression tests from security audit run-1's local validation
//! plans. A workbook file is untrusted input: nothing stored in it may reach files, URLs
//! or extensions, and stored formulas and functions are checked again when read back.

use super::*;
use crate::functions::*;
use crate::storage::Lockdown;

/// Edit a closed workbook file directly, as someone with the file (or a crafted backup) can.
fn tamper(path: &std::path::Path, sql: &str) {
    let conn = duckdb::Connection::open(path).unwrap();
    conn.execute_batch(sql).unwrap();
    conn.execute_batch("CHECKPOINT;").unwrap();
}

#[test]
fn a_crafted_file_cannot_read_files_when_opened_locked_down() {
    let mut t = TempWb::new();
    let canary = t.dir.path().join("canary.txt");
    std::fs::write(&canary, "SECRET-CANARY").unwrap();
    let path = t.path();
    t.wb = Workbook::open_in_memory().unwrap();
    // The file's sheets "table" becomes a view over a local file.
    tamper(&path, &format!("DROP TABLE _rf_sheets; CREATE VIEW _rf_sheets AS SELECT content::BLOB AS data FROM read_text({});", crate::sql::lit(&canary.to_string_lossy())));
    // Control: without the lockdown the view does read the file.
    let plain = duckdb::Connection::open(&path).unwrap();
    let text: Vec<u8> = plain.query_row("SELECT data FROM _rf_sheets", [], |r| r.get(0)).unwrap();
    assert_eq!(text, b"SECRET-CANARY");
    drop(plain);
    // Opened as restore (no directories) and as the server (its scratch directories) open
    // it: the read is refused, and SQL cannot re-enable access.
    let scratch = t.dir.path().join("scratch");
    std::fs::create_dir_all(&scratch).unwrap();
    for lock in [Lockdown::default(), Lockdown { allowed: vec![scratch.clone()], settings: vec![] }] {
        let wb = Workbook::open_locked(&path, &lock).unwrap();
        assert!(wb.is_locked_down().unwrap());
        let err = wb.raw_query("SELECT data FROM _rf_sheets").unwrap_err().to_string();
        assert!(err.to_lowercase().contains("disabled") || err.to_lowercase().contains("permission"), "{err}");
        assert!(wb.raw_query("SET enable_external_access = true").is_err());
    }
}

#[test]
fn locked_down_open_still_reads_and_writes_allowed_directories() {
    let t = TempWb::new();
    let path = t.path();
    let scratch = t.dir.path().join("scratch");
    std::fs::create_dir_all(&scratch).unwrap();
    drop(t.wb);
    let lock = Lockdown { allowed: vec![scratch.clone()], settings: vec![("threads".into(), "2".into())] };
    let mut wb = Workbook::open_locked(&path, &lock).unwrap();
    assert_eq!(wb.setting("threads").unwrap(), "2");
    wb.create_table_from_query("t", "SELECT 1 AS x").unwrap();
    let out = scratch.join("out.csv");
    wb.raw_query(&format!("COPY t TO {} (FORMAT csv)", crate::sql::lit(&out.to_string_lossy()))).unwrap();
    assert!(out.exists());
    let outside = t.dir.path().join("outside.csv");
    assert!(wb.raw_query(&format!("COPY t TO {} (FORMAT csv)", crate::sql::lit(&outside.to_string_lossy()))).is_err());
    assert!(!outside.exists());
}

#[test]
fn a_stored_formula_that_fails_validation_reads_as_null() {
    let mut t = TempWb::new();
    t.wb.create_table_from_query("t", "SELECT 1 AS x").unwrap();
    t.wb.add_formula_column("t", "f", "x + 1").unwrap();
    let v = t.wb.open_view(spec("t")).unwrap().view_id;
    assert_eq!(all_rows(&t.wb, v), vec![vec![serde_json::json!(1), serde_json::json!(2)]]);
    let path = t.path();
    t.wb = Workbook::open_in_memory().unwrap();
    // A subquery and a settings read: both refused when written, so both refused when read back.
    for bad in ["(SELECT 42)", "current_setting('threads')"] {
        tamper(&path, &format!("UPDATE _rf_columns SET formula = {} WHERE name = 'f';", crate::sql::lit(bad)));
        let mut wb = Workbook::open(&path).unwrap();
        let v = wb.open_view(spec("t")).unwrap().view_id;
        assert_eq!(all_rows(&wb, v), vec![vec![serde_json::json!(1), serde_json::Value::Null]], "{bad}");
    }
}

#[test]
fn a_tampered_sql_function_is_refused_before_it_becomes_a_macro() {
    let mut t = TempWb::new();
    let d = FunctionDefinition {
        name: "TWICE".into(),
        kind: FunctionKind::Sql,
        arguments: vec!["value".into()],
        source: "value * 2".into(),
        documentation: String::new(),
        dependencies: Default::default(),
        returns: None,
        volatile: false,
        tests: vec![FunctionTest { arguments: vec!["2".into()], expected: "4".into() }],
    };
    t.wb.publish_function(d).unwrap();
    let path = t.path();
    t.wb = Workbook::open_in_memory().unwrap();
    tamper(&path, "UPDATE _rf_meta SET value = replace(value, 'value * 2', '1); CREATE TABLE pwned AS SELECT 1; SELECT (1') WHERE key = 'function_registry';");
    let mut wb = match Workbook::open(&path) {
        Ok(wb) => wb,
        Err(_) => return, // refusing to open is also safe
    };
    let pins = [("TWICE".to_string(), 1u32)].into_iter().collect();
    assert!(wb.pin_functions(&pins).is_err());
    let n = wb.raw_query("SELECT count(*) FROM duckdb_tables() WHERE table_name = 'pwned'").unwrap();
    assert_eq!(n[0][0], duckdb::types::Value::BigInt(0));
}


/// SEC.07: a formula over a huge declared range, with the used area stretched by a value
/// in the far corner, is refused within the evaluation budget instead of running unbounded.
#[test]
fn a_formula_over_a_huge_range_is_refused_and_taken_back() {
    let mut t = TempWb::new();
    t.wb.with_sheets(|s| s.create("S")).unwrap();
    t.wb.with_sheets(|s| s.set(0, &[sheet_input(0, 25, "1"), sheet_input(1_048_575, 16_383, "1"), sheet_input(5, 5, "keep")])).unwrap();
    let before = fingerprint(&t.wb);
    let started = std::time::Instant::now();
    for f in ["=SUM(B2:XFD1048576)", "=SUM(A:XFD)", "=ROWS(MMULT(B2:XFD1048576,B2:XFD1048576))", "=COUNTA(B2:XFD1048576)"] {
        let one = std::time::Instant::now();
        let err = t.wb.with_sheets(|s| s.set(0, &[sheet_input(0, 0, f), sheet_input(5, 5, "changed")])).unwrap_err();
        eprintln!("{f}: refused in {:?}", one.elapsed());
        assert!(err.downcast_ref::<crate::limits::TooLarge>().is_some(), "{f}: {err:#}");
        assert_eq!(cell_text(&t.wb, 0, 0, 0), None, "{f}: the edit is taken back");
        assert_eq!(cell_text(&t.wb, 0, 5, 5).as_deref(), Some("keep"), "{f}: the whole edit");
    }
    assert!(started.elapsed() < std::time::Duration::from_secs(60), "bounded: {:?}", started.elapsed());
    assert_eq!(fingerprint(&t.wb), before, "nothing changed, nothing entered the history");
    // Ordinary whole-column formulas still work.
    t.wb.with_sheets(|s| s.set(0, &[sheet_input(0, 0, "=SUM(Z:Z)")])).unwrap();
    assert_eq!(cell_text(&t.wb, 0, 0, 0).as_deref(), Some("1"));
}

/// SEC.07: a cancel that arrives while an evaluation runs stops it and takes the edit back;
/// a cancel left over from before does not stop the next one.
#[test]
fn a_cancelled_evaluation_takes_the_edit_back() {
    let mut s = crate::sheets::Sheets::from_bytes(None).unwrap();
    s.create("S").unwrap();
    let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    s.set_cancel(flag.clone());
    let data: Vec<SheetInput> = (0..2000).map(|r| sheet_input(r, 0, &r.to_string())).collect();
    s.set(0, &data).unwrap();
    // A stale cancel (nothing was running) does not stop the next edit.
    flag.store(true, std::sync::atomic::Ordering::SeqCst);
    s.set(0, &[sheet_input(0, 2, "=A1*2")]).unwrap();
    // A cancel during a long evaluation stops it.
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (f, d) = (flag.clone(), done.clone());
    let setter = std::thread::spawn(move || {
        while !d.load(std::sync::atomic::Ordering::SeqCst) {
            f.store(true, std::sync::atomic::Ordering::SeqCst);
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    });
    let heavy: Vec<SheetInput> = (0..3000).map(|r| sheet_input(r, 1, "=SUM($A$1:$A$2000)")).collect();
    let err = s.set(0, &heavy);
    done.store(true, std::sync::atomic::Ordering::SeqCst);
    setter.join().unwrap();
    let err = err.unwrap_err();
    assert!(format!("{err:#}").contains("cancelled"), "{err:#}");
    let v = s.fetch(0, 0, 1, 2).unwrap().rows[0].get(1).cloned().flatten();
    assert!(v.is_none(), "the edit was taken back: {v:?}");
}

/// SEC.07: whole-column functions walk only the used area, so a far-corner value cannot
/// make them read every cell; COUNTBLANK still counts what lies beyond.
#[test]
fn whole_column_functions_walk_only_the_used_area() {
    let mut t = TempWb::new();
    t.wb.with_sheets(|s| s.create("S")).unwrap();
    t.wb.with_sheets(|s| s.set(0, &[sheet_input(0, 1, "2"), sheet_input(1, 1, "x"), sheet_input(2, 1, "3")])).unwrap();
    let started = std::time::Instant::now();
    t.wb.with_sheets(|s| {
        s.set(0, &[sheet_input(0, 0, "=COUNTA(B:CV)"), sheet_input(1, 0, "=COUNTBLANK(B1:C4)"), sheet_input(2, 0, "=COUNT(B:B)"), sheet_input(3, 0, "=AVERAGE(B:B)")])
    })
    .unwrap();
    assert_eq!(cell_text(&t.wb, 0, 0, 0).as_deref(), Some("3"));
    assert_eq!(cell_text(&t.wb, 0, 1, 0).as_deref(), Some("5"));
    assert_eq!(cell_text(&t.wb, 0, 2, 0).as_deref(), Some("2"));
    assert_eq!(cell_text(&t.wb, 0, 3, 0).as_deref(), Some("2.5"));
    assert!(started.elapsed() < std::time::Duration::from_secs(20), "{:?}", started.elapsed());
    // A far-corner value stretches the used area: the walk would be ~100M cells, so the
    // edit is refused within the budget instead.
    let err = t.wb.with_sheets(|s| s.set(0, &[sheet_input(1_048_575, 200, "1"), sheet_input(4, 0, "=COUNTA(B:CV)")])).unwrap_err();
    assert!(err.downcast_ref::<crate::limits::TooLarge>().is_some(), "{err:#}");
}

/// SEC.08: an .xlsx whose parts expand far beyond its size is refused before it is parsed.
#[test]
fn an_xlsx_that_expands_too_far_is_refused_before_parsing() {
    use std::io::Write;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bomb.xlsx");
    {
        let mut z = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
        let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        z.start_file("[Content_Types].xml", opts).unwrap();
        z.write_all(b"<Types/>").unwrap();
        z.start_file("xl/worksheets/sheet1.xml", opts).unwrap();
        let chunk = vec![b' '; 1 << 20];
        for _ in 0..24 {
            z.write_all(&chunk).unwrap();
        }
        z.finish().unwrap();
    }
    assert!(std::fs::metadata(&path).unwrap().len() < 1 << 20, "the file itself is small");
    let err = crate::sheets::Sheets::preview_xlsx(&path).unwrap_err();
    assert!(err.downcast_ref::<crate::limits::TooLarge>().is_some(), "{err:#}");
    assert!(format!("{err:#}").contains("expands to"), "{err:#}");
    let mut t = TempWb::new();
    let err = t.wb.with_sheets(|s| s.import_xlsx(&path).map(|_| ())).unwrap_err();
    assert!(err.downcast_ref::<crate::limits::TooLarge>().is_some(), "{err:#}");
}

/// SEC.09: both formula parsers bound nesting, operator count and length, so a hostile
/// formula is an error, not a stack overflow. Run on a 2 MiB stack, as server threads have.
#[test]
fn deeply_nested_formulas_are_errors_not_stack_overflows() {
    std::thread::Builder::new()
        .stack_size(2 << 20)
        .spawn(|| {
            let deep = |n: usize| format!("={}1{}", "(".repeat(n), ")".repeat(n));
            let calls = |n: usize| format!("={}1{}", "ABS(".repeat(n), ")".repeat(n));
            let minus = |n: usize| format!("={}1", "-".repeat(n));
            let chain = |n: usize| format!("=1{}", "+1".repeat(n));
            // Table formulas (spreadsheet syntax over tables).
            use crate::table_formula::parse;
            assert!(parse(&deep(50)).is_ok());
            assert!(parse(&chain(500)).is_ok());
            for bad in [deep(150), deep(100_000), calls(150), calls(50_000), minus(5_000), minus(200_000), chain(5_000)] {
                let e = parse(&bad).unwrap_err();
                assert!(e.message.contains("nests") || e.message.contains("operators") || e.message.contains("longer"), "{}", e.message);
            }
            assert!(parse(&format!("=\"{}\"", "x".repeat(9000))).unwrap_err().message.contains("longer"));

            // Sheet cells (the vendored engine's parser), through an edit.
            let mut s = crate::sheets::Sheets::from_bytes(None).unwrap();
            s.create("S").unwrap();
            s.set(0, &[sheet_input(0, 0, &deep(50))]).unwrap();
            for bad in [deep(150), deep(100_000), calls(50_000), minus(200_000), chain(5_000)] {
                // Refused or stored as an error: either way the process survives and the cell
                // is not a number.
                let _ = s.set(0, &[sheet_input(1, 0, &bad)]);
                let v = s.fetch(0, 1, 1, 1).unwrap().rows[0].first().cloned().flatten().map(|c| c.0);
                assert_ne!(v.as_deref(), Some("1"), "{}", &bad[..20]);
            }
        })
        .unwrap()
        .join()
        .expect("no stack overflow");
}

/// SEC.12: the sheet undo history keeps a bounded number of steps, the engine keeps nothing
/// for a remote copy, and the latest steps still undo.
#[test]
fn sheet_history_and_change_queue_stay_bounded() {
    let mut s = crate::sheets::Sheets::from_bytes(None).unwrap();
    s.create("S").unwrap();
    for i in 0..(crate::limits::SHEET_UNDO_STEPS + 50) {
        s.set(0, &[sheet_input(0, 0, &i.to_string())]).unwrap();
    }
    assert!(s.engine_undo_len() <= crate::limits::SHEET_UNDO_STEPS + 1, "{}", s.engine_undo_len());
    assert_eq!(s.queued_changes(), 0);
    assert!(s.undo().unwrap());
    let v = s.fetch(0, 0, 1, 1).unwrap().rows[0][0].clone().map(|c| c.0);
    assert_eq!(v.as_deref(), Some((crate::limits::SHEET_UNDO_STEPS + 48).to_string().as_str()));
    // A big paste counts by its cells.
    let big: Vec<SheetInput> = (0..(crate::limits::SHEET_UNDO_ENTRIES as u32 / 4)).map(|r| sheet_input(r, 1, "1")).collect();
    for _ in 0..5 {
        s.set(0, &big).unwrap();
    }
    assert!(s.engine_undo_len() <= crate::limits::SHEET_UNDO_ENTRIES + big.len(), "{}", s.engine_undo_len());
}

/// SEC.13: a CSV export is faithful by default; the CSV for spreadsheets prefixes text that
/// a spreadsheet would run as a formula, and leaves numbers alone. Text copied into a sheet
/// stays text even when it looks like a formula whose result is text.
#[test]
fn csv_for_spreadsheets_neutralizes_formula_text_and_sheets_keep_it_text() {
    let mut t = TempWb::new();
    let csv = t.dir.path().join("in.csv");
    std::fs::write(&csv, "s,n\n=1+1,-5\n+x,1\n-y,2\n@z,3\n\"\tw\",4\nplain,5\n=\"a\"&\"b\",6\n").unwrap();
    t.wb.import_file_with(&csv, Some("t"), &[crate::import::ColumnType { name: "s".into(), sql_type: "VARCHAR".into() }]).unwrap();
    let v = t.wb.open_view(spec("t")).unwrap().view_id;
    let read = |p: &std::path::Path| parse_rows(&std::fs::read_to_string(p).unwrap());
    let faithful = t.dir.path().join("faithful.csv");
    t.wb.export_view(v, &faithful, ExportFormat::Csv).unwrap();
    let rows = read(&faithful);
    assert_eq!(rows[1], vec!["=1+1", "-5"]);
    assert_eq!(rows[5][0], "\tw");
    let safe = t.dir.path().join("safe.csv");
    t.wb.export_view(v, &safe, ExportFormat::CsvExcel).unwrap();
    let rows = read(&safe);
    let firsts: Vec<&str> = rows[1..].iter().map(|r| r[0].as_str()).collect();
    assert_eq!(firsts, vec!["'=1+1", "'+x", "'-y", "'@z", "'\tw", "plain", "'=\"a\"&\"b\""]);
    assert_eq!(rows[1][1], "-5", "numbers are unchanged");
    // The same through a query export.
    let q = t.dir.path().join("q.csv");
    t.wb.export_query("SELECT s, n FROM t ORDER BY n", &[], &q, ExportFormat::CsvExcel).unwrap();
    assert_eq!(read(&q)[1], vec!["'=1+1", "-5"]);

    // Into a sheet: formula-looking text, including a text-valued formula, stays text.
    t.wb.sheet_from_view(v, "S", 100).unwrap();
    let sheets = t.wb.sheets().list();
    let idx = sheets.iter().find(|s| s.name == "S").unwrap().index;
    let s = t.wb.sheets();
    let cells = s.fetch(idx, 0, 8, 1).unwrap();
    let col: Vec<Option<String>> = cells.rows.iter().map(|r| r[0].clone().map(|c| c.0)).collect();
    assert!(col.contains(&Some("=\"a\"&\"b\"".to_string())), "{col:?}");
    assert!(col.contains(&Some("=1+1".to_string())), "{col:?}");
    assert!(!col.contains(&Some("ab".to_string())), "not evaluated: {col:?}");
}

fn parse_rows(text: &str) -> Vec<Vec<String>> {
    let mut r = csv_reader(text);
    std::mem::take(&mut r)
}

/// A small CSV reader for the tests (quotes and doubled quotes).
fn csv_reader(text: &str) -> Vec<Vec<String>> {
    let mut rows = vec![];
    let mut row = vec![];
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match (quoted, c) {
            (true, '"') if chars.peek() == Some(&'"') => {
                field.push('"');
                chars.next();
            }
            (true, '"') => quoted = false,
            (true, c) => field.push(c),
            (false, '"') => quoted = true,
            (false, ',') => row.push(std::mem::take(&mut field)),
            (false, '\n') => {
                row.push(std::mem::take(&mut field));
                rows.push(std::mem::take(&mut row));
            }
            (false, c) => field.push(c),
        }
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        rows.push(row);
    }
    rows
}
