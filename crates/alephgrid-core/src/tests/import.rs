//! 2.05: import preview, type overrides, actionable errors and progress.

use super::*;
use serde_json::json;

fn write(dir: &tempfile::TempDir, name: &str, text: &str) -> std::path::PathBuf {
    let p = dir.path().join(name);
    std::fs::write(&p, text).unwrap();
    p
}

fn types(pairs: &[(&str, &str)]) -> Vec<ColumnType> {
    pairs.iter().map(|(n, t)| ColumnType { name: n.to_string(), sql_type: t.to_string() }).collect()
}

#[test]
fn csv_preview_shows_detected_types_and_first_rows() {
    let dir = tempfile::tempdir().unwrap();
    let mut csv = String::from("id,zip,city,joined\n");
    for i in 0..50 {
        csv.push_str(&format!("{i},0{:04},City {i},2024-01-{:02}\n", 1000 + i, 1 + i % 28));
    }
    let path = write(&dir, "people.csv", &csv);
    let wb = Workbook::open_in_memory().unwrap();
    let p = wb.preview_file(&path, &[]).unwrap();
    assert_eq!((p.format.as_str(), p.suggested_name.as_str()), ("CSV", "people"));
    let cols: Vec<(&str, &str)> = p.columns.iter().map(|c| (c.name.as_str(), c.sql_type.as_str())).collect();
    // DuckDB's sniffer keeps values with leading zeros as text.
    assert_eq!(cols, [("id", "BIGINT"), ("zip", "VARCHAR"), ("city", "VARCHAR"), ("joined", "DATE")]);
    assert_eq!(p.rows.len(), 20, "only the first rows");
    assert_eq!(p.rows[0], vec![json!(0), json!("01000"), json!("City 0"), json!("2024-01-01")]);
    assert!(wb.list_tables().unwrap().is_empty(), "a preview creates nothing");

    // Overrides show how the data would convert, next to what was detected.
    let p = wb.preview_file(&path, &types(&[("zip", "BIGINT"), ("id", "VARCHAR")])).unwrap();
    assert_eq!((p.rows[0][0].clone(), p.rows[0][1].clone()), (json!("0"), json!(1000)));
    assert_eq!((p.columns[1].sql_type.as_str(), p.columns[1].detected.as_str()), ("BIGINT", "VARCHAR"));
}

#[test]
fn overrides_apply_on_import_and_keep_text_exact() {
    let dir = tempfile::tempdir().unwrap();
    let path = write(&dir, "codes.csv", "code,amount\n007,1.50\n042,2.25\n");
    let mut wb = Workbook::open_in_memory().unwrap();
    let t = wb.import_file_with(&path, Some("codes"), &types(&[("code", "VARCHAR"), ("amount", "DECIMAL(10,2)")])).unwrap();
    assert_eq!(t.row_count, 2);
    let cols = wb.columns("codes").unwrap();
    assert_eq!((cols[0].sql_type.as_str(), cols[1].sql_type.as_str()), ("VARCHAR", "DECIMAL(10,2)"));
    let v = wb.open_view(spec("codes")).unwrap().view_id;
    assert_eq!(all_rows(&wb, v), vec![vec![json!("007"), json!("1.50")], vec![json!("042"), json!("2.25")]]);
}

#[test]
fn a_late_bad_value_explains_itself_and_the_suggested_fix_works() {
    let dir = tempfile::tempdir().unwrap();
    let mut csv = String::from("n,label\n");
    // Beyond the 100,000-row type-detection sample.
    for i in 0..120_000 {
        csv.push_str(&format!("{i},x{i}\n"));
    }
    csv.push_str("oops,last\n");
    let path = write(&dir, "late.csv", &csv);
    let mut wb = Workbook::open_in_memory().unwrap();
    let err = wb.import_file_with(&path, None, &[]).unwrap_err();
    let problem = err.downcast_ref::<ImportProblem>().expect("a structured problem");
    assert_eq!(problem.line, Some(120_002), "header is line 1");
    assert_eq!(problem.column.as_deref(), Some("n"));
    assert_eq!(problem.value.as_deref(), Some("oops"));
    assert!(problem.message.contains("Import \"n\" as VARCHAR"), "{}", problem.message);
    assert!(wb.list_tables().unwrap().is_empty(), "nothing was created");

    let fix = types(&[(problem.column.as_deref().unwrap(), problem.suggested_type.as_deref().unwrap())]);
    let t = wb.import_file_with(&path, None, &fix).unwrap();
    assert_eq!(t.row_count, 120_001);
    assert_eq!(scalar(&wb, "SELECT n FROM late WHERE label = 'last'"), json!("oops"));
    assert_eq!(scalar(&wb, "SELECT n FROM late WHERE label = 'x119999'"), json!("119999"));
}

#[test]
fn invalid_overrides_are_rejected_before_reading() {
    let dir = tempfile::tempdir().unwrap();
    let path = write(&dir, "t.csv", "a\n1\n");
    let mut wb = Workbook::open_in_memory().unwrap();
    assert!(wb.import_file_with(&path, None, &types(&[("a", "VARCHAR); DROP TABLE x; --")])).is_err());
    assert!(wb.import_file_with(&path, None, &types(&[("missing", "VARCHAR")])).unwrap_err().to_string().contains("no column"));
    assert!(wb.preview_file(&path, &types(&[("a", "MAP(INT, INT)")])).is_err());
    assert!(wb.list_tables().unwrap().is_empty());
}

#[test]
fn parquet_and_json_overrides_cast_after_reading() {
    let dir = tempfile::tempdir().unwrap();
    let mut wb = Workbook::open_in_memory().unwrap();
    wb.create_table_from_query("src", "SELECT i AS n, (i * 1.5)::DOUBLE AS x FROM range(5) r(i)").unwrap();
    let parquet = dir.path().join("src.parquet");
    wb.export_table("src", &parquet, ExportFormat::Parquet).unwrap();
    let json_path = dir.path().join("src.jsonl");
    wb.export_table("src", &json_path, ExportFormat::Json).unwrap();
    for (path, name) in [(&parquet, "from_parquet"), (&json_path, "from_json")] {
        let p = wb.preview_file(path, &types(&[("n", "VARCHAR")])).unwrap();
        assert_eq!(p.rows[3][0], json!("3"), "{name}");
        wb.import_file_with(path, Some(name), &types(&[("n", "VARCHAR"), ("x", "DECIMAL(10,1)")])).unwrap();
        let cols = wb.columns(name).unwrap();
        assert_eq!((cols[0].sql_type.as_str(), cols[1].sql_type.as_str()), ("VARCHAR", "DECIMAL(10,1)"), "{name}");
        assert_eq!(scalar(&wb, &format!("SELECT sum(x)::VARCHAR FROM {name}")), json!("15.0"));
    }
}

#[test]
fn xlsx_preview_lists_sheets_without_importing() {
    let dir = tempfile::tempdir().unwrap();
    let mut src = Workbook::open_in_memory().unwrap();
    src.with_sheets(|s| {
        let a = s.create("One")?.index;
        s.set(a, &[sheet_input(0, 0, "=NOPE(1)")])?;
        s.create("Two").map(|_| ())
    })
    .unwrap();
    let path = dir.path().join("two.xlsx");
    src.sheets().export_xlsx(&path).unwrap();
    let wb = Workbook::open_in_memory().unwrap();
    match wb.preview_any(&path, &[]).unwrap() {
        UploadPreview::Sheets { sheets, warnings } => {
            assert_eq!(sheets.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["One", "Two"]);
            assert!(warnings[0].contains("NOPE"), "{warnings:?}");
        }
        _ => panic!("sheets preview expected"),
    }
    assert!(wb.sheets().list().is_empty(), "previewing adds no sheets");
}

#[test]
fn long_imports_report_progress() {
    let dir = tempfile::tempdir().unwrap();
    let mut csv = String::with_capacity(40_000_000);
    csv.push_str("a,b,c\n");
    for i in 0..2_000_000u64 {
        csv.push_str(&format!("{i},{},{}\n", i * 7 % 1000, i % 13));
    }
    let path = write(&dir, "big.csv", &csv);
    let mut wb = Workbook::open_in_memory().unwrap();
    let handle = wb.progress_handle();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop2 = stop.clone();
    let poller = std::thread::spawn(move || {
        let mut seen = Vec::new();
        while !stop2.load(std::sync::atomic::Ordering::SeqCst) {
            if let Some(pct) = handle.query_progress().percentage() {
                seen.push(pct);
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        seen
    });
    wb.import_file_with(&path, None, &[]).unwrap();
    stop.store(true, std::sync::atomic::Ordering::SeqCst);
    let seen = poller.join().unwrap();
    assert!(!seen.is_empty(), "progress was reported during the import");
    assert!(seen.iter().all(|p| (0.0..=100.0).contains(p)), "{seen:?}");
}
