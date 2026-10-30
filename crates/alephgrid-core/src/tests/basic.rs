use super::*;
use serde_json::json;

fn demo(rows: u64) -> (Workbook, String) {
    let mut wb = Workbook::open_in_memory().unwrap();
    let t = wb.create_demo_table(rows).unwrap();
    (wb, t.name)
}

fn spec(table: &str) -> ViewSpec {
    ViewSpec { table: table.into(), ..Default::default() }
}

#[test]
fn identity_view_reads_windows() {
    let (mut wb, t) = demo(10_000);
    let v = wb.open_view(spec(&t)).unwrap();
    assert_eq!(v.row_count, 10_000);
    assert!(v.can_append);
    let names: Vec<_> = v.columns.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names[0], "flight_date");
    let block = wb.fetch(v.view_id, 9_990, 100).unwrap();
    assert_eq!(block.rows.len(), 10);
    assert_eq!(block.rows[0].len(), v.columns.len());
}

#[test]
fn sort_and_filter() {
    let (mut wb, t) = demo(5_000);
    let mut s = spec(&t);
    s.filters.push(Filter { column: "carrier".into(), op: FilterOp::Eq, value: json!("DL") });
    s.sort.push(SortKey { column: "fare_usd".into(), desc: true });
    let v = wb.open_view(s).unwrap();
    assert!(v.row_count > 0 && v.row_count < 5_000);
    assert!(!v.can_append);
    let carrier = v.columns.iter().position(|c| c.name == "carrier").unwrap();
    let fare = v.columns.iter().position(|c| c.name == "fare_usd").unwrap();
    let block = wb.fetch(v.view_id, 0, 50).unwrap();
    let fares: Vec<f64> = block.rows.iter().map(|r| r[fare].as_f64().unwrap()).collect();
    assert!(fares.windows(2).all(|w| w[0] >= w[1]));
    assert!(block.rows.iter().all(|r| r[carrier] == json!("DL")));
}

#[test]
fn edit_append_and_type_errors() {
    let (mut wb, t) = demo(100);
    let v = wb.open_view(spec(&t)).unwrap();
    let out = wb
        .edit(
            v.view_id,
            &[
                CellEdit { row: 3, column: "carrier".into(), value: Some("ZZ".into()), record: None },
                CellEdit { row: 101, column: "distance_mi".into(), value: Some("42".into()), record: None },
            ],
        )
        .unwrap();
    assert_eq!(out.rows_appended, 2);
    let info = wb.view_info(v.view_id).unwrap();
    assert_eq!(info.row_count, 102);
    let carrier = info.columns.iter().position(|c| c.name == "carrier").unwrap();
    assert_eq!(wb.fetch(v.view_id, 3, 1).unwrap().rows[0][carrier], json!("ZZ"));

    let err = wb
        .edit(v.view_id, &[CellEdit { row: 0, column: "distance_mi".into(), value: Some("far".into()), record: None }])
        .unwrap_err();
    assert!(err.to_string().contains("not a valid"), "{err}");
}

#[test]
fn formula_columns_are_lazy_and_sortable() {
    let (mut wb, t) = demo(1_000);
    wb.add_formula_column(&t, "fare_per_mile", "=[fare_usd] / [distance_mi]").unwrap();
    wb.add_formula_column(&t, "fare_x2", "fare_per_mile * 2").unwrap();
    assert!(wb.add_formula_column(&t, "broken", "no_such_column + 1").is_err());

    let mut s = spec(&t);
    s.sort.push(SortKey { column: "fare_x2".into(), desc: false });
    let v = wb.open_view(s).unwrap();
    let last = v.columns.len() - 1;
    assert_eq!(v.columns[last].name, "fare_x2");
    let block = wb.fetch(v.view_id, 0, 20).unwrap();
    let values: Vec<f64> = block.rows.iter().map(|r| r[last].as_f64().unwrap()).collect();
    assert!(values.windows(2).all(|w| w[0] <= w[1]));

    let err = wb.edit(v.view_id, &[CellEdit { row: 0, column: "fare_x2".into(), value: Some("1".into()), record: None }]);
    assert!(err.is_err());
}

#[test]
fn insert_and_delete_keep_ids_dense() {
    let (mut wb, t) = demo(50);
    let v = wb.open_view(spec(&t)).unwrap();
    wb.insert_rows(v.view_id, 10, 5).unwrap();
    assert_eq!(wb.view_info(v.view_id).unwrap().row_count, 55);
    assert!(wb.fetch(v.view_id, 10, 1).unwrap().rows[0].iter().all(|x| x.is_null()));
    wb.delete_rows(v.view_id, 0, 20).unwrap();
    let info = wb.view_info(v.view_id).unwrap();
    assert_eq!(info.row_count, 35);
    assert!(info.can_append, "ids should be dense again after delete");
}

#[test]
fn stats_and_query() {
    let (mut wb, t) = demo(1_000);
    let v = wb.open_view(spec(&t)).unwrap();
    let s = wb.stats(v.view_id, "distance_mi", 0, 1_000).unwrap();
    assert_eq!(s.count, 1_000);
    assert!(s.sum.unwrap().as_i64().unwrap() > 0);
    let p = wb.preview_query(&format!("SELECT carrier, count(*) n FROM {t} GROUP BY 1 ORDER BY 2 DESC"), 3).unwrap();
    assert_eq!(p.rows.len(), 3);
    assert!(p.truncated);
    let created = wb.create_table_from_query("by_carrier", &format!("SELECT carrier, avg(fare_usd) FROM {t} GROUP BY 1")).unwrap();
    assert_eq!(created.row_count, 8);
}

#[test]
fn import_and_export_roundtrip() {
    let dir = std::env::temp_dir().join(format!("alephgrid-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let csv = dir.join("people.csv");
    std::fs::write(&csv, "name,age\nAda,36\nLinus,54\n").unwrap();
    let mut wb = Workbook::open_in_memory().unwrap();
    let t = wb.import_file(&csv, None).unwrap();
    assert_eq!((t.name.as_str(), t.row_count, t.column_count), ("people", 2, 2));
    let out = dir.join("out.parquet");
    wb.export_table("people", &out, ExportFormat::Parquet).unwrap();
    let back = wb.import_file(&out, Some("people")).unwrap();
    assert_eq!(back.name, "people_2");
    assert_eq!(back.row_count, 2);
    std::fs::remove_dir_all(&dir).ok();
}

fn input(row: u32, column: u32, value: &str) -> SheetInput {
    SheetInput { row, column, value: value.into() }
}

#[test]
fn sheets_formulas_undo_and_persistence() {
    let dir = std::env::temp_dir().join(format!("alephgrid-sheets-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("wb.duckdb");
    {
        let mut wb = Workbook::open(&db).unwrap();
        let s = wb.with_sheets(|s| s.create("Budget")).unwrap();
        assert_eq!((s.index, s.name.as_str()), (0, "Budget"));
        wb.with_sheets(|s| s.set(0, &[input(0, 0, "2"), input(1, 0, "3"), input(2, 0, "=SUM(A1:A2)*10")])).unwrap();
        let block = wb.sheets().fetch(0, 0, 3, 2).unwrap();
        let total = block.rows[2][0].as_ref().unwrap();
        assert_eq!(total.0, "50");
        assert!(matches!(total.1, crate::sheets::Align::Right));
        assert_eq!(total.2.as_deref(), Some("=SUM(A1:A2)*10"));

        wb.with_sheets(|s| s.set(0, &[input(0, 0, "7")])).unwrap();
        assert_eq!(wb.sheets().fetch(0, 2, 1, 1).unwrap().rows[0][0].as_ref().unwrap().0, "100");
        assert!(wb.with_sheets(|s| s.undo()).unwrap());
        assert_eq!(wb.sheets().fetch(0, 2, 1, 1).unwrap().rows[0][0].as_ref().unwrap().0, "50");

        let second = wb.with_sheets(|s| s.create("Budget")).unwrap();
        assert_eq!(second.name, "Budget 2", "names are made unique");
        wb.with_sheets(|s| s.set(1, &[input(0, 0, "=Budget!A3+1")])).unwrap();
        assert_eq!(wb.sheets().fetch(1, 0, 1, 1).unwrap().rows[0][0].as_ref().unwrap().0, "51");
    }
    let wb = Workbook::open(&db).unwrap();
    let list = wb.sheets().list();
    assert_eq!(list.len(), 2);
    assert_eq!((list[0].rows, list[0].columns), (3, 1));
    assert_eq!(wb.sheets().fetch(1, 0, 1, 1).unwrap().rows[0][0].as_ref().unwrap().0, "51");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn xlsx_roundtrip_and_bridges() {
    let dir = std::env::temp_dir().join(format!("alephgrid-xlsx-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (mut wb, t) = demo(500);
    let sheet = wb.create_sheet_from_query("By carrier", &format!("SELECT carrier, count(*) AS n FROM {t} GROUP BY 1 ORDER BY 1"), 1000).unwrap().sheet;
    assert_eq!((sheet.rows, sheet.columns), (9, 2), "header + 8 carriers");
    wb.with_sheets(|s| s.set(sheet.index, &[input(9, 0, "Total"), input(9, 1, "=SUM(B2:B9)")])).unwrap();

    let xlsx = dir.join("out.xlsx");
    wb.sheets().export_xlsx(&xlsx).unwrap();
    let mut other = Workbook::open_in_memory().unwrap();
    match other.import_any(&xlsx, None).unwrap() {
        Imported::Sheets { sheets, warnings } => {
            assert_eq!(sheets[0].name, "By carrier");
            assert!(warnings.is_empty(), "{warnings:?}");
        }
        _ => panic!("xlsx should import as sheets"),
    }
    let total = other.sheets().fetch(0, 9, 1, 2).unwrap();
    assert_eq!(total.rows[0][1].as_ref().unwrap().0, "500");

    let table = other.sheet_to_table(0, "carriers").unwrap();
    assert_eq!((table.row_count, table.column_count), (9, 2));
    let cols = other.columns("carriers").unwrap();
    assert_eq!(cols[1].kind, Kind::Int, "types are sniffed from sheet values");
    std::fs::remove_dir_all(&dir).ok();
}
