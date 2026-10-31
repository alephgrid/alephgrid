//! 4.01–4.06 in the engine: schema, query correctness against hand-computed results, bound
//! parameters, bounded previews, complete exports, saved queries with versions, materialized
//! results with source revisions and refresh, data revisions, bounded copies into sheets.

use super::*;
use crate::sqlpolicy::{BadParameter, ParamType};
use crate::storage;
use serde_json::json;

fn p(name: &str, ty: ParamType, value: Option<&str>) -> QueryParam {
    QueryParam { name: name.into(), ty, value: value.map(String::from) }
}

/// Orders with duplicates, NULLs, Unicode, exact decimals and a formula column.
/// Rows: (id, region, amount, qty, day, note)
type Order = (i64, &'static str, &'static str, i64, &'static str, Option<&'static str>);
const ORDERS: &[Order] = &[
    (1, "north", "10.25", 2, "2024-02-28", Some("first")),
    (2, "south", "0.10", 1, "2024-02-29", None),
    (3, "north", "10.25", 2, "2024-02-28", Some("first")),
    (4, "東京", "99999999999999.99", 3, "2024-03-01", Some("großer Auftrag")),
    (5, "south", "0.20", 5, "2023-12-31", Some("O'Brien; DROP TABLE orders; --")),
    (6, "west", "7.00", 0, "2024-01-01", None),
];

fn setup() -> TempWb {
    let mut t = TempWb::new();
    let values: Vec<String> = ORDERS
        .iter()
        .map(|(id, region, amount, qty, day, note)| {
            format!(
                "({id}, '{region}', {amount}::DECIMAL(18,2), {qty}, DATE '{day}', {})",
                note.map(|n| format!("'{}'", n.replace('\'', "''"))).unwrap_or("NULL".into())
            )
        })
        .collect();
    t.wb.create_table_from_query(
        "orders",
        &format!("SELECT * FROM (VALUES {}) v(id, region, amount, qty, day, note)", values.join(", ")),
    )
    .unwrap();
    t.wb.add_formula_column("orders", "line_total", "[amount] * [qty]").unwrap();
    t.wb.create_table_from_query("regions", "SELECT * FROM (VALUES ('north', 'N'), ('south', 'S'), ('east', 'E')) v(region, code)").unwrap();
    t
}

fn rows(t: &TempWb, sql: &str, params: &[QueryParam]) -> Vec<Vec<serde_json::Value>> {
    t.wb.run_query(sql, params, 5_000).unwrap_or_else(|e| panic!("{sql}: {e:#}")).rows
}

// ------------------------------------------------------------------ 4.01

#[test]
fn schema_lists_user_tables_with_columns_and_nothing_internal() {
    let mut t = setup();
    t.wb.create_table_from_query("Prix_日本", "SELECT 1 AS \"a \"\"b\"\"\", 2 AS \"select\", 3 AS \"MiXed\"").unwrap();
    let schema = t.wb.schema().unwrap();
    let names: Vec<&str> = schema.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["Prix_日本", "orders", "regions"]);
    let orders = schema.iter().find(|s| s.name == "orders").unwrap();
    assert_eq!(orders.row_count, ORDERS.len() as u64);
    let cols: Vec<(&str, &str)> = orders.columns.iter().map(|c| (c.name.as_str(), c.sql_type.as_str())).collect();
    assert_eq!(
        cols,
        [("id", "INTEGER"), ("region", "VARCHAR"), ("amount", "DECIMAL(18,2)"), ("qty", "INTEGER"), ("day", "DATE"), ("note", "VARCHAR"), ("line_total", "DECIMAL(18,2)")]
    );
    assert_eq!(orders.columns.last().unwrap().formula.as_deref(), Some("[amount] * [qty]"));
    assert!(schema.iter().flat_map(|s| s.columns.iter()).all(|c| !c.name.starts_with("_rf_")));
    // Schema agrees with the per-view listing, and quoted identifiers select those columns.
    let v = t.wb.open_view(spec("Prix_日本")).unwrap();
    let prix = schema.iter().find(|s| s.name == "Prix_日本").unwrap();
    assert_eq!(prix.columns.iter().map(|c| &c.name).collect::<Vec<_>>(), v.columns.iter().map(|c| &c.name).collect::<Vec<_>>());
    assert_eq!(rows(&t, "SELECT \"a \"\"b\"\"\", \"select\", \"MiXed\" FROM \"prix_日本\"", &[]), vec![vec![json!(1), json!(2), json!(3)]]);
}

// ------------------------------------------------------------------ query correctness

#[test]
fn queries_match_hand_computed_results() {
    let t = setup();
    // Empty result and one row.
    assert!(rows(&t, "SELECT * FROM orders WHERE id > 100", &[]).is_empty());
    assert_eq!(rows(&t, "SELECT id FROM orders WHERE region = '東京'", &[]), vec![vec![json!(4)]]);
    // Duplicates are kept, DISTINCT removes them.
    assert_eq!(rows(&t, "SELECT count(*) FROM orders WHERE note = 'first'", &[]), vec![vec![json!(2)]]);
    assert_eq!(rows(&t, "SELECT count(*) FROM (SELECT DISTINCT region, amount, qty, day, note FROM orders)", &[]), vec![vec![json!(5)]]);
    // NULLs: count(col) skips them, NULL = NULL is not true, IS NULL finds them.
    assert_eq!(rows(&t, "SELECT count(*), count(note), count(*) FILTER (WHERE note IS NULL) FROM orders", &[]), vec![vec![json!(6), json!(4), json!(2)]]);
    assert_eq!(rows(&t, "SELECT count(*) FROM orders WHERE note = NULL", &[]), vec![vec![json!(0)]]);

    // Join + aggregate + calculated column, against totals computed here in Rust from ORDERS
    // (scaled integers: cents), independent of DuckDB.
    let mut by_region: std::collections::BTreeMap<&str, i128> = Default::default();
    for (_, region, amount, qty, _, _) in ORDERS {
        let cents: i128 = amount.replace('.', "").parse().unwrap();
        *by_region.entry(region).or_default() += cents * *qty as i128;
    }
    let cents = |c: i128| format!("{}.{:02}", c / 100, c % 100);
    let expected: Vec<Vec<serde_json::Value>> = ["north", "south"]
        .iter()
        .map(|r| vec![json!(r), json!(if *r == "north" { "N" } else { "S" }), json!(cents(by_region[r]))])
        .collect();
    for sql in [
        // plain join on the formula column
        "SELECT o.region, r.code, sum(o.line_total) AS total FROM orders o JOIN regions r USING (region) GROUP BY ALL ORDER BY 1",
        // the same through a user CTE and a calculated column written inline
        "WITH t AS (SELECT region, amount * qty AS lt FROM orders) SELECT t.region, r.code, sum(t.lt) FROM t JOIN regions r ON r.region = t.region GROUP BY ALL ORDER BY 1",
        // nested subquery and a semi-join
        "SELECT region, (SELECT code FROM regions r WHERE r.region = o.region), sum(line_total) FROM orders o WHERE region IN (SELECT region FROM regions) GROUP BY region ORDER BY region",
    ] {
        assert_eq!(rows(&t, sql, &[]), expected, "{sql}");
    }
    // Left join keeps unmatched rows with NULLs; regions without orders give NULL sums.
    assert_eq!(
        rows(&t, "SELECT r.region, sum(o.qty) FROM regions r LEFT JOIN orders o USING (region) GROUP BY ALL ORDER BY 1", &[]),
        vec![vec![json!("east"), json!(null)], vec![json!("north"), json!(4)], vec![json!("south"), json!(6)]]
    );
    // Window functions and a calculated column in the select list.
    assert_eq!(
        rows(&t, "SELECT id, qty * 10 AS q10, sum(qty) OVER (ORDER BY id) AS running FROM orders ORDER BY id LIMIT 3", &[]),
        vec![vec![json!(1), json!(20), json!(2)], vec![json!(2), json!(10), json!(3)], vec![json!(3), json!(20), json!(5)]]
    );
    // Unusual identifiers survive as result column names.
    let r = t.wb.run_query("SELECT 1 AS \"weird \"\"name\"\"\", 2 AS \"日本\", 3 AS \"_rf_id\"", &[], 10).unwrap();
    assert_eq!(r.columns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["weird \"name\"", "日本"], "internal names stay hidden");
}

#[test]
fn exact_numbers_and_dates() {
    let t = setup();
    let r = rows(
        &t,
        "SELECT 9007199254740993::BIGINT, 170141183460469231731687303715884105727::HUGEINT, 0.1::DECIMAL(38,37),
                sum(amount), max(amount) * 2, avg(qty)::DOUBLE FROM orders",
        &[],
    );
    assert_eq!(
        r,
        vec![vec![
            json!("9007199254740993"),
            json!("170141183460469231731687303715884105727"),
            json!("0.1000000000000000000000000000000000000"),
            json!("100000000000027.79"),
            json!("199999999999999.98"),
            json!(13.0 / 6.0)
        ]]
    );
    // Dates: leap day arithmetic, ordering, NULL dates; TIMESTAMPTZ is shown in UTC.
    assert_eq!(
        rows(&t, "SELECT DATE '2024-02-28' + 1, DATE '2023-02-28' + 1, max(day), min(day), TIMESTAMPTZ '2024-01-01 01:00:00+01' FROM orders", &[]),
        vec![vec![json!("2024-02-29"), json!("2023-03-01"), json!("2024-03-01"), json!("2023-12-31"), json!("2024-01-01 00:00:00+00")]]
    );
}

// ------------------------------------------------------------------ 4.02

#[test]
fn parameters_are_bound_not_spliced() {
    let t = setup();
    let hostile = "O'Brien; DROP TABLE orders; --";
    // The hostile note is found exactly by equality; it is a value, never SQL.
    assert_eq!(rows(&t, "SELECT id FROM orders WHERE note = $n", &[p("n", ParamType::Text, Some(hostile))]), vec![vec![json!(5)]]);
    for (value, expected) in [
        ("first", 2),
        ("FIRST", 0),
        ("f%", 0),       // no wildcard meaning with =
        ("' OR '1'='1", 0),
        ("großer Auftrag", 1),
        ("", 0),
    ] {
        assert_eq!(rows(&t, "SELECT count(*) FROM orders WHERE note = $n", &[p("n", ParamType::Text, Some(value))]), vec![vec![json!(expected)]], "{value:?}");
    }
    // LIKE sees the value as a pattern (a value, still not SQL).
    assert_eq!(rows(&t, "SELECT count(*) FROM orders WHERE note LIKE $n", &[p("n", ParamType::Text, Some("f%"))]), vec![vec![json!(2)]]);
    // NULL binds NULL: = matches nothing, IS NOT DISTINCT FROM matches the NULL notes.
    let null = p("n", ParamType::Text, None);
    assert_eq!(rows(&t, "SELECT count(*) FROM orders WHERE note = $n", std::slice::from_ref(&null)), vec![vec![json!(0)]]);
    assert_eq!(rows(&t, "SELECT count(*) FROM orders WHERE note IS NOT DISTINCT FROM $n", &[null]), vec![vec![json!(2)]]);
    // Exact numbers, dates, booleans; a parameter used twice.
    assert_eq!(
        rows(
            &t,
            "SELECT $big, $big + 1, $dec, $d + 1, $ts, $b, count(*) FILTER (WHERE amount = $dec) FROM orders",
            &[
                p("big", ParamType::Integer, Some("9007199254740993")),
                p("dec", ParamType::Decimal, Some("10.25")),
                p("d", ParamType::Date, Some("2024-02-28")),
                p("ts", ParamType::Timestamp, Some("2024-01-02 03:04:05.000006")),
                p("b", ParamType::Boolean, Some("false")),
            ]
        ),
        vec![vec![json!("9007199254740993"), json!("9007199254740994"), json!("10.25"), json!("2024-02-29"), json!("2024-01-02 03:04:05.000006"), json!(false), json!(2)]]
    );
    assert_eq!(
        rows(&t, "SELECT count(*) FROM orders WHERE amount > $x", &[p("x", ParamType::Decimal, Some("99999999999999.98"))]),
        vec![vec![json!(1)]],
        "an exact decimal parameter compares exactly"
    );
}

#[test]
fn parameter_errors_name_the_parameter() {
    let t = setup();
    let err = |sql: &str, params: &[QueryParam]| format!("{:#}", t.wb.run_query(sql, params, 10).unwrap_err());
    let missing = t.wb.run_query("SELECT * FROM orders WHERE region = $region", &[], 10).unwrap_err();
    assert!(missing.downcast_ref::<BadParameter>().is_some());
    assert!(format!("{missing:#}").contains("$region has no value"), "{missing:#}");
    assert!(err("SELECT 1", &[p("x", ParamType::Text, Some("1"))]).contains("does not use the parameter $x"));
    assert!(err("SELECT $n", &[p("n", ParamType::Integer, Some("12abc"))]).contains("$n: \"12abc\" is not an integer"));
    assert!(err("SELECT $d", &[p("d", ParamType::Date, Some("2024-02-30"))]).contains("is not a date"));
    // A value that parses but does not fit the column it is compared with: DuckDB's error.
    let e = err("SELECT * FROM orders WHERE id = $v", &[p("v", ParamType::Text, Some("1 OR 1=1"))]);
    assert!(e.contains("Could not convert string '1 OR 1=1'"), "{e}");
    // Errors point into the user's own text, even with formula-column CTEs added in front.
    let e = err("SELECT line_total,\n       nonexistent\nFROM orders", &[]);
    assert!(e.contains("nonexistent") && e.contains("LINE 2:"), "{e}");
}

// ------------------------------------------------------------------ 4.03

#[test]
fn previews_are_bounded_and_say_what_they_read() {
    let mut t = setup();
    t.wb.create_table_from_query("big", "SELECT i FROM range(20000) r(i)").unwrap();
    for (n, limit, truncated) in [(0u64, 10usize, false), (10, 10, false), (11, 10, true), (20000, 5000, true)] {
        let r = t.wb.run_query(&format!("SELECT i FROM big WHERE i < {n} ORDER BY i"), &[], limit).unwrap();
        assert_eq!(r.rows.len() as u64, n.min(limit as u64), "{n}/{limit}");
        assert_eq!(r.truncated, truncated, "{n}/{limit}");
        assert_eq!(r.row_limit, limit as u64);
        if let Some(last) = r.rows.last() {
            assert_eq!(last[0], json!(n.min(limit as u64) - 1));
        }
    }
    // The preview never holds more than the protocol limit, whatever is asked.
    let r = t.wb.run_query("SELECT i FROM big", &[], 1_000_000).unwrap();
    assert_eq!((r.rows.len(), r.truncated, r.row_limit), (limits::MAX_QUERY_ROWS, true, limits::MAX_QUERY_ROWS as u64));
    // Timing, revision and sources with their data revisions.
    let r = t.wb.run_query("SELECT * FROM orders JOIN regions USING (region)", &[], 10).unwrap();
    assert_eq!(r.revision, t.wb.revision().unwrap());
    let data = storage::data_revisions(&t.wb.conn).unwrap();
    assert_eq!(
        r.sources,
        vec![
            SourceRevision { table: "orders".into(), data_rev: data["orders"] },
            SourceRevision { table: "regions".into(), data_rev: data["regions"] }
        ]
    );
    assert!(r.elapsed_ms < 60_000);
}

#[test]
fn exports_hold_every_row_of_one_snapshot() {
    let mut t = setup();
    let n: i64 = 100_000;
    t.wb.create_table_from_query("big", &format!("SELECT i, i::VARCHAR || '''x' AS s, (i * 2)::HUGEINT * 4611686018427387904 AS h FROM range({n}) r(i)")).unwrap();
    for format in [ExportFormat::Csv, ExportFormat::Parquet, ExportFormat::Json] {
        let path = t.dir.path().join(format!("out.{}", format.extension()));
        let (written, revision) = t.wb.export_query("SELECT * FROM big WHERE i >= $lo", &[p("lo", ParamType::Integer, Some("0"))], &path, format).unwrap();
        assert_eq!(written, n as u64);
        assert_eq!(revision, t.wb.revision().unwrap());
        // Read back with DuckDB's readers (unrestricted in this test) and compare with closed forms.
        let reader = match format {
            ExportFormat::Csv | ExportFormat::CsvExcel => "read_csv",
            ExportFormat::Parquet => "read_parquet",
            ExportFormat::Json => "read_json",
        };
        // JSON numbers beyond a double are left out of the check for the JSON reader, whose
        // type inference is not the oracle here; CSV and Parquet carry them exactly.
        let json_out = format == ExportFormat::Json;
        let h = if json_out { "'-'" } else { "sum(h::HUGEINT)::VARCHAR" };
        let back = t
            .wb
            .raw_query(&format!(
                "SELECT count(*), sum(i)::VARCHAR, {h}, count(*) FILTER (WHERE s = i::VARCHAR || '''x') FROM {reader}('{}')",
                path.display()
            ))
            .unwrap();
        let sum_i = n * (n - 1) / 2;
        let sum_h = if json_out { "-".to_string() } else { ((sum_i as i128) * 2 * 4_611_686_018_427_387_904i128).to_string() };
        assert_eq!(
            format!("{:?}", back[0]),
            format!("{:?}", vec![
                duckdb::types::Value::BigInt(n),
                duckdb::types::Value::Text(sum_i.to_string()),
                duckdb::types::Value::Text(sum_h),
                duckdb::types::Value::BigInt(n)
            ]),
            "{format:?}"
        );
    }
    // An export that fails part-way reports the error (the server removes the file).
    let path = t.dir.path().join("broken.csv");
    let err = t.wb.export_query("SELECT CASE WHEN i = 50000 THEN error('boom') ELSE i END FROM big", &[], &path, ExportFormat::Csv).unwrap_err();
    assert!(format!("{err:#}").contains("boom"), "{err:#}");
}

// ------------------------------------------------------------------ 4.04

#[test]
fn saved_queries_keep_immutable_versions_across_reopen() {
    let mut t = setup();
    let sql = "SELECT id FROM orders WHERE region = $r";
    let params = vec![p("r", ParamType::Text, Some("north"))];
    let before = t.wb.revision().unwrap();
    let q = t.wb.save_query(None, "By region", QueryKind::Query, sql, &params).unwrap();
    assert_eq!((q.version, q.kind, q.name.as_str()), (1, QueryKind::Query, "By region"));
    assert_eq!(t.wb.revision().unwrap(), before + 1, "one revision per save");
    // Same SQL and parameters: no new version (but the change is still one revision).
    let same = t.wb.save_query(Some(&q.id), "By region", QueryKind::Query, sql, &params).unwrap();
    assert_eq!(same.version, 1);
    // Changed SQL, then changed parameter value, then kind and name only.
    assert_eq!(t.wb.save_query(Some(&q.id), "By region", QueryKind::Query, "SELECT id, qty FROM orders WHERE region = $r", &params).unwrap().version, 2);
    let south = vec![p("r", ParamType::Text, Some("south"))];
    assert_eq!(t.wb.save_query(Some(&q.id), "By region", QueryKind::Query, "SELECT id, qty FROM orders WHERE region = $r", &south).unwrap().version, 3);
    let live = t.wb.save_query(Some(&q.id), "Region (live)", QueryKind::Live, "SELECT id, qty FROM orders WHERE region = $r", &south).unwrap();
    assert_eq!((live.version, live.kind), (3, QueryKind::Live));
    assert_eq!(t.wb.rename_query(&q.id, "Region rows").unwrap().version, 3, "renaming is not a new version");
    // Name clashes (case-insensitive), empty names, unknown ids, policy refusals: nothing saved.
    t.wb.save_query(None, "Other", QueryKind::Query, "SELECT 1", &[]).unwrap();
    let fp = fingerprint(&t.wb);
    assert!(t.wb.save_query(None, "region ROWS", QueryKind::Query, "SELECT 2", &[]).unwrap_err().to_string().contains("already exists"));
    assert!(t.wb.rename_query(&q.id, "OTHER").is_err());
    assert!(t.wb.save_query(None, "  ", QueryKind::Query, "SELECT 2", &[]).is_err());
    assert!(t.wb.save_query(Some("nope"), "x", QueryKind::Query, "SELECT 2", &[]).is_err());
    assert!(t.wb.save_query(None, "evil", QueryKind::Query, "SELECT * FROM read_csv('/etc/passwd')", &[]).is_err());
    assert!(t.wb.save_query(None, "evil2", QueryKind::Query, "SELECT 1; DROP TABLE orders", &[]).is_err());
    assert!(t.wb.save_query(None, "params", QueryKind::Query, "SELECT $a", &[]).is_err(), "parameters must match placeholders");
    assert_eq!(fingerprint(&t.wb), fp, "refused saves change nothing");

    let versions = t.wb.query_versions(&q.id).unwrap();
    assert_eq!(versions.iter().map(|v| v.version).collect::<Vec<_>>(), [1, 2, 3]);
    assert_eq!(versions[0].sql, sql);
    assert_eq!(versions[0].params, params);
    assert_eq!(versions[2].params, south);
    assert!(versions.windows(2).all(|w| w[0].revision < w[1].revision), "each version records the revision that saved it");

    t.reopen();
    let after: Vec<SavedQuery> = t.wb.saved_queries().unwrap();
    assert_eq!(after.iter().map(|q| (q.name.as_str(), q.version, q.kind)).collect::<Vec<_>>(), [("Other", 1, QueryKind::Query), ("Region rows", 3, QueryKind::Live)]);
    assert_eq!(t.wb.query_versions(&q.id).unwrap().len(), 3, "versions survive reopening");
    // Running a saved version gives that version's result.
    let v1 = &t.wb.query_versions(&q.id).unwrap()[0];
    assert_eq!(t.wb.run_query(&v1.sql, &v1.params, 10).unwrap().rows, vec![vec![json!(1)], vec![json!(3)]]);
    t.wb.delete_query(&q.id).unwrap();
    assert!(t.wb.query_versions(&q.id).is_err());
    assert_eq!(t.wb.raw_query(&format!("SELECT count(*) FROM _rf_query_versions WHERE query_id = '{}'", q.id)).unwrap()[0][0], duckdb::types::Value::BigInt(0));
}

// ------------------------------------------------------------------ 4.05 / 4.06

#[test]
fn materialized_tables_record_sources_and_refresh_all_or_nothing() {
    let mut t = setup();
    let q = t.wb.save_query(None, "Totals", QueryKind::Query, "SELECT region, sum(line_total) AS total FROM orders WHERE qty >= $min GROUP BY ALL ORDER BY 1", &[p("min", ParamType::Integer, Some("1"))]).unwrap();
    let info = t.wb.create_table_from_query_with("totals", &q.sql, &q.params, Some((&q.id, q.version))).unwrap();
    assert_eq!(info.row_count, 3);
    let created_at = t.wb.revision().unwrap();
    let mat = |t: &TempWb, table: &str| t.wb.materialized().unwrap().into_iter().find(|m| m.table == table);
    // The setup tables were made from queries too (of literal VALUES: no sources).
    let all: Vec<String> = t.wb.materialized().unwrap().into_iter().map(|m| m.table).collect();
    assert_eq!(all, ["orders", "regions", "totals"]);
    assert!(mat(&t, "orders").unwrap().sources.is_empty());
    let m = mat(&t, "totals").unwrap();
    assert_eq!((m.table.as_str(), m.query.as_deref(), m.query_version, m.revision), ("totals", Some(q.id.as_str()), Some(1), created_at));
    assert_eq!(m.sources.iter().map(|s| s.table.as_str()).collect::<Vec<_>>(), ["orders"]);
    assert!(!m.stale && !m.edited);
    let total = |t: &TempWb, region: &str| rows(t, &format!("SELECT total FROM totals WHERE region = '{region}'"), &[])[0][0].clone();
    assert_eq!(total(&t, "north"), json!("41.00"));

    // A source edit makes it stale; the snapshot keeps its old values until refreshed.
    let v = t.wb.open_view(spec("orders")).unwrap();
    t.wb.edit(v.view_id, &[edit(0, "qty", "4")]).unwrap();
    let m = mat(&t, "totals").unwrap();
    assert!(m.stale);
    assert_eq!(m.sources[0].current_rev, Some(t.wb.revision().unwrap()));
    assert_eq!(total(&t, "north"), json!("41.00"), "a snapshot does not follow its source");
    let tv0 = t.wb.open_view(spec("totals")).unwrap().view_id;
    let old_ids: Vec<i64> = t.wb.fetch(tv0, 0, 10).unwrap().ids;

    // Injected failure during refresh: nothing changes.
    let fp = fingerprint(&t.wb);
    faults::fail_next("refresh_materialized");
    assert!(t.wb.refresh_materialized("totals").is_err());
    assert_eq!(fingerprint(&t.wb), fp);

    let refreshed = t.wb.refresh_materialized("totals").unwrap();
    assert!(!refreshed.stale && !refreshed.edited);
    assert_eq!(refreshed.revision, t.wb.revision().unwrap());
    assert_eq!(total(&t, "north"), json!("61.50"), "41.00 + 2 × 10.25");
    let tv = t.wb.open_view(spec("totals")).unwrap();
    let new_ids = t.wb.fetch(tv.view_id, 0, 10).unwrap().ids;
    assert_eq!(new_ids.len(), 3);
    assert!(new_ids.iter().all(|id| !old_ids.contains(id)), "record ids are never reused");
    t.wb.verify_index("totals").unwrap(); // consistent (errors otherwise)

    // Editing the materialized table by hand is flagged; a refresh replaces those edits.
    t.wb.edit(tv.view_id, &[edit(0, "total", "1.00")]).unwrap();
    assert!(mat(&t, "totals").unwrap().edited);
    t.wb.refresh_materialized("totals").unwrap();
    assert_eq!(total(&t, "north"), json!("61.50"));

    // The result's columns change: refused, table untouched, still stale.
    t.wb.add_column("orders", "extra", "INTEGER").unwrap();
    t.wb.save_query(Some(&q.id), "Totals", QueryKind::Query, &q.sql, &q.params).unwrap();
    t.wb.create_table_from_query_with("wide", "SELECT * FROM regions", &[], None).unwrap();
    t.wb.add_column("regions", "more", "INTEGER").unwrap();
    let fp = fingerprint(&t.wb);
    let err = t.wb.refresh_materialized("wide").unwrap_err();
    assert!(err.downcast_ref::<crate::queries::SchemaChanged>().is_some(), "{err:#}");
    assert_eq!(fingerprint(&t.wb), fp);

    // Renaming keeps the record; a renamed or dropped source cannot be refreshed.
    t.wb.rename_table("totals", "region_totals").unwrap();
    assert!(t.wb.materialized().unwrap().iter().any(|m| m.table == "region_totals"));
    t.wb.rename_table("regions", "areas").unwrap();
    let wide = mat(&t, "wide").unwrap();
    assert!(wide.stale && wide.sources[0].current_rev.is_none(), "a missing source is stale");
    let fp = fingerprint(&t.wb);
    let err = t.wb.refresh_materialized("wide").unwrap_err();
    assert!(format!("{err:#}").contains("no table named \"regions\""), "{err:#}");
    assert_eq!(fingerprint(&t.wb), fp);

    // Everything survives reopening; dropping the table drops its record.
    t.reopen();
    let names: Vec<String> = t.wb.materialized().unwrap().into_iter().map(|m| m.table).collect();
    assert_eq!(names, ["areas", "orders", "region_totals", "wide"]);
    t.wb.drop_table("wide").unwrap();
    assert!(mat(&t, "wide").is_none(), "dropping the table drops its record");
    t.wb.create_demo_table(10).unwrap();
    assert!(t.wb.refresh_materialized("flights").unwrap_err().to_string().contains("not made from a query"));
}

#[test]
fn every_table_change_advances_its_data_revision_and_nothing_else_does() {
    let mut t = setup();
    let rev_of = |t: &TempWb, table: &str| storage::data_revisions(&t.wb.conn).unwrap()[table];
    let v = t.wb.open_view(spec("orders")).unwrap();
    let check = |t: &mut TempWb, what: &str, f: &dyn Fn(&mut Workbook)| {
        let regions = rev_of(t, "regions");
        f(&mut t.wb);
        assert_eq!(rev_of(t, "orders"), t.wb.revision().unwrap(), "{what} stamps orders with its revision");
        assert_eq!(rev_of(t, "regions"), regions, "{what} leaves other tables alone");
    };
    check(&mut t, "edit", &|wb| { wb.edit(v.view_id, &[edit(0, "qty", "9")]).unwrap(); });
    check(&mut t, "append", &|wb| { wb.edit(v.view_id, &[edit(6, "qty", "1")]).unwrap(); });
    check(&mut t, "insert_rows", &|wb| { wb.insert_rows(v.view_id, 1, 2).unwrap(); });
    check(&mut t, "delete_rows", &|wb| { wb.delete_rows(v.view_id, 1, 2).unwrap(); });
    check(&mut t, "add_column", &|wb| wb.add_column("orders", "c", "INTEGER").unwrap());
    check(&mut t, "add_formula_column", &|wb| wb.add_formula_column("orders", "f", "[qty] + 1").unwrap());
    check(&mut t, "update_formula", &|wb| wb.update_formula("orders", "f", "[qty] + 2").unwrap());
    check(&mut t, "rename_column", &|wb| wb.rename_column("orders", "c", "d").unwrap());
    check(&mut t, "drop_column", &|wb| wb.drop_column("orders", "d").unwrap());
    check(&mut t, "sql insert", &|wb| { wb.sql_change("INSERT INTO orders (id) VALUES (99)", &[]).unwrap(); });
    check(&mut t, "sql update", &|wb| { wb.sql_change("UPDATE orders SET qty = qty + 1 WHERE id = 99", &[]).unwrap(); });
    check(&mut t, "sql delete", &|wb| { wb.sql_change("DELETE FROM orders WHERE id = 99", &[]).unwrap(); });
    // Saving a query or a view, renaming a table and failed changes leave it.
    let before = rev_of(&t, "orders");
    t.wb.save_query(None, "q", QueryKind::Query, "SELECT 1", &[]).unwrap();
    t.wb.save_view(None, "v", spec("orders"), Default::default()).unwrap();
    assert!(t.wb.edit(v.view_id, &[edit(0, "qty", "not a number")]).is_err());
    t.wb.rename_table("orders", "orders2").unwrap();
    assert_eq!(rev_of(&t, "orders2"), before);
}

#[test]
fn sheet_copies_never_drop_rows_silently() {
    let mut t = setup();
    let limit = Workbook::sheet_copy_limit(2, 100);
    assert_eq!(limit, 100);
    for (n, allow, ok, truncated) in [(99u64, false, true, false), (100, false, true, false), (101, false, false, true), (101, true, true, true)] {
        let fp = fingerprint(&t.wb);
        let sql = "SELECT i, 9007199254740993 + i AS big FROM range($n) r(i)";
        let out = t.wb.create_sheet_from_query_with(&format!("S{n}{allow}"), sql, &[p("n", ParamType::Integer, Some(&n.to_string()))], 100, allow);
        match out {
            Ok(copy) => {
                assert!(ok, "{n} {allow}");
                assert_eq!(copy.rows_copied, n.min(100));
                assert_eq!(copy.truncated, truncated);
                assert_eq!(copy.row_limit, 100);
                // Integers a double cannot hold are kept as text with every digit.
                assert_eq!(cell_text(&t.wb, copy.sheet.index, 1, 1).as_deref(), Some("9007199254740993"));
                assert_eq!(cell_text(&t.wb, copy.sheet.index, n.min(100) as u32, 0).as_deref(), Some((n.min(100) - 1).to_string().as_str()));
            }
            Err(e) => {
                assert!(!ok, "{n} {allow}: {e:#}");
                assert!(e.downcast_ref::<limits::TooLarge>().is_some(), "{e:#}");
                assert!(format!("{e:#}").contains("more than 100 rows"), "{e:#}");
                assert_eq!(fingerprint(&t.wb), fp, "a refused copy creates nothing");
            }
        }
    }
}
