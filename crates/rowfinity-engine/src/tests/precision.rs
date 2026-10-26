//! 1.05: exact integers and decimals across fetch, edit, SQL results, stats and export.
//!
//! Wire contract: integers within ±(2^53 − 1) are JSON numbers; larger integers and every
//! DECIMAL value are JSON strings holding the exact decimal text. DOUBLE stays a number.

use super::*;
use serde_json::json;

const SAFE: i64 = (1 << 53) - 1;

fn exact_table(t: &mut TempWb) -> u64 {
    t.wb
        .create_table_from_query(
            "exact",
            "SELECT * FROM (VALUES
               (1, 42::BIGINT, 0::HUGEINT, 0::UBIGINT, 1.10::DECIMAL(10,2), 0.1::DOUBLE),
               (2, 9007199254740993::BIGINT, 170141183460469231731687303715884105727::HUGEINT,
                   18446744073709551615::UBIGINT, 12345678.90::DECIMAL(10,2), 1e300::DOUBLE),
               (3, '-9223372036854775808'::BIGINT, '-170141183460469231731687303715884105728'::HUGEINT,
                   9007199254740991::UBIGINT, -0.01::DECIMAL(10,2), -2.5::DOUBLE),
               (4, 9007199254740991::BIGINT, NULL, NULL, NULL, NULL)
             ) v(k, big, huge, ubig, money, dbl)",
        )
        .unwrap();
    t.wb.open_view(spec("exact")).unwrap().view_id
}

#[test]
fn fetch_returns_exact_values() {
    let mut t = TempWb::new();
    let v = exact_table(&mut t);
    let kinds: Vec<_> = t.wb.view_info(v).unwrap().columns.iter().map(|c| c.kind).collect();
    assert_eq!(kinds, [Kind::Int, Kind::Int, Kind::Int, Kind::Int, Kind::Number, Kind::Number]);
    let rows = all_rows(&t.wb, v);
    assert_eq!(rows[0], vec![json!(1), json!(42), json!(0), json!(0), json!("1.10"), json!(0.1)]);
    assert_eq!(
        rows[1],
        vec![
            json!(2),
            json!("9007199254740993"),
            json!("170141183460469231731687303715884105727"),
            json!("18446744073709551615"),
            json!("12345678.90"),
            json!(1e300)
        ]
    );
    assert_eq!(rows[2][1], json!("-9223372036854775808"));
    assert_eq!(rows[2][2], json!("-170141183460469231731687303715884105728"));
    assert_eq!(rows[2][3], json!(SAFE), "2^53 - 1 is still a safe JSON number");
    assert_eq!(rows[2][4], json!("-0.01"));
    assert_eq!(rows[3][1], json!(SAFE));
    assert_eq!(rows[3][2], Value::Null);
}

#[test]
fn edits_store_exact_values_and_reject_lossy_input() {
    let mut t = TempWb::new();
    let v = exact_table(&mut t);
    t.wb.edit(v, &[edit(0, "big", "9007199254740993"), edit(0, "money", "-12345678.99"), edit(0, "huge", "-1")]).unwrap();
    let rows = all_rows(&t.wb, v);
    assert_eq!(rows[0][1], json!("9007199254740993"));
    assert_eq!(rows[0][4], json!("-12345678.99"));
    assert_eq!(rows[0][2], json!(-1));
    t.wb.edit(v, &[edit(0, "money", "1.230"), edit(0, "k", "7.0"), edit(1, "k", " 8 ")]).unwrap();
    let rows = all_rows(&t.wb, v);
    assert_eq!((rows[0][4].clone(), rows[0][0].clone(), rows[1][0].clone()), (json!("1.23"), json!(7), json!(8)));

    let before = all_rows(&t.wb, v);
    for (column, bad) in [
        ("k", "1.5"),                   // would round to 2
        ("money", "1.234"),             // would round to 1.23
        ("money", "123456789.00"),      // overflows DECIMAL(10,2)
        ("big", "9223372036854775808"), // overflows BIGINT
        ("ubig", "-1"),
        ("k", "1e3"),
        ("money", "abc"),
    ] {
        let err = t.wb.edit(v, &[edit(1, "dbl", "3"), edit(0, column, bad)]).unwrap_err();
        assert!(err.to_string().contains("not a valid"), "{column} = {bad}: {err}");
        assert_eq!(all_rows(&t.wb, v), before, "{column} = {bad} must not change anything");
    }
}

#[test]
fn small_integer_type_boundaries() {
    let mut t = TempWb::new();
    t.wb.create_table_from_query("small", "SELECT 0::TINYINT AS t, 0::UTINYINT AS u, 0::SMALLINT AS s, 0::INTEGER AS i").unwrap();
    let v = t.wb.open_view(spec("small")).unwrap().view_id;
    t.wb.edit(v, &[edit(0, "t", "-128"), edit(0, "u", "255"), edit(0, "s", "32767"), edit(0, "i", "-2147483648")]).unwrap();
    assert_eq!(all_rows(&t.wb, v)[0], vec![json!(-128), json!(255), json!(32767), json!(-2147483648i64)]);
    for (c, bad) in [("t", "128"), ("u", "256"), ("u", "-1"), ("s", "32768"), ("i", "2147483648")] {
        assert!(t.wb.edit(v, &[edit(0, c, bad)]).is_err(), "{c} = {bad}");
    }
}

#[test]
fn dates_and_timestamps() {
    let mut t = TempWb::new();
    t.wb
        .create_table_from_query(
            "d",
            "SELECT DATE '2024-02-29' AS day, TIMESTAMP '2024-03-10 02:30:00' AS ts, TIMESTAMPTZ '2024-01-01 00:00:00+00' AS tz",
        )
        .unwrap();
    let v = t.wb.open_view(spec("d")).unwrap().view_id;
    let row = all_rows(&t.wb, v)[0].clone();
    assert_eq!(row[0], json!("2024-02-29"));
    assert_eq!(row[1], json!("2024-03-10 02:30:00"));
    assert_eq!(row[2], json!("2024-01-01 00:00:00+00"), "TIMESTAMPTZ is shown in UTC");
    assert!(t.wb.edit(v, &[edit(0, "day", "2023-02-29")]).is_err(), "not a leap year");
    t.wb.edit(v, &[edit(0, "day", "1900-01-01"), edit(0, "tz", "2024-06-01 12:00:00+02")]).unwrap();
    let row = all_rows(&t.wb, v)[0].clone();
    assert_eq!(row[0], json!("1900-01-01"));
    assert_eq!(row[2], json!("2024-06-01 10:00:00+00"));
}

#[test]
fn query_results_and_stats_are_exact() {
    let mut t = TempWb::new();
    let v = exact_table(&mut t);
    let p = t.wb.preview_query("SELECT big, money, big + 1 AS next FROM exact ORDER BY k", 10).unwrap();
    assert_eq!(p.rows[1], vec![json!("9007199254740993"), json!("12345678.90"), json!("9007199254740994")]);
    assert_eq!(p.columns[1].kind, Kind::Number);
    // Sum of a DECIMAL column is exact; sum of large BIGINTs does not go through f64.
    let s = t.wb.stats(v, "money", 0, 4).unwrap();
    assert_eq!(s.sum, Some(json!("12345679.99")));
    assert_eq!(s.min, Some(json!("-0.01")));
    assert_eq!(s.max, Some(json!("12345678.90")));
    let s = t.wb.stats(v, "big", 0, 2).unwrap();
    assert_eq!(s.sum, Some(json!("9007199254741035")));
    assert_eq!((s.rows, s.count), (2, 2));
}

#[test]
fn exports_are_exact() {
    let mut t = TempWb::new();
    exact_table(&mut t);
    let csv = t.dir.path().join("exact.csv");
    t.wb.export_table("exact", &csv, ExportFormat::Csv).unwrap();
    let text = std::fs::read_to_string(&csv).unwrap();
    assert!(text.contains("9007199254740993"), "{text}");
    assert!(text.contains("170141183460469231731687303715884105727"));
    assert!(text.contains("12345678.90"));
    let parquet = t.dir.path().join("exact.parquet");
    t.wb.export_table("exact", &parquet, ExportFormat::Parquet).unwrap();
    let back = t.wb.import_file(&parquet, Some("back")).unwrap();
    assert_eq!(back.row_count, 4);
    let differences = scalar(
        &t.wb,
        "SELECT count(*) FROM (SELECT * EXCLUDE (_rf_id) FROM exact EXCEPT SELECT * EXCLUDE (_rf_id) FROM back)",
    );
    assert_eq!(differences, json!(0), "parquet round trip preserves every value and type");
    let json_path = t.dir.path().join("exact.jsonl");
    t.wb.export_table("exact", &json_path, ExportFormat::Json).unwrap();
    assert!(std::fs::read_to_string(&json_path).unwrap().contains("9007199254740993"));
}
