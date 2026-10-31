//! F.09: stored formula results for sorting and filtering by formula columns of large tables.
//! Expected orders and values are worked out by hand from the rows below.
use super::*;
use serde_json::json;

/// customers: (1, Cy), (2, Ann), (3, Bo); orders (id, cid): (10, 1), (11, 2), (12, 3),
/// (13, 9), (14, 2), (15, 1), with `who` = the customer's name or "none". Results are stored
/// for tables of any size here.
fn shop() -> TempWb {
    let mut t = TempWb::new();
    t.wb.shared().set_results_min_rows(1);
    t.wb.create_table_from_query("customers", "SELECT * FROM (VALUES (1, 'Cy'), (2, 'Ann'), (3, 'Bo')) v(id, name)").unwrap();
    t.wb.create_table_from_query("orders", "SELECT * FROM (VALUES (10, 1), (11, 2), (12, 3), (13, 9), (14, 2), (15, 1)) v(id, cid)").unwrap();
    t.wb.add_formula_column_as("orders", "who", "=XLOOKUP([@cid],customers[id],customers[name],\"none\")", FormulaLanguage::Sheet).unwrap();
    t
}

fn by_who(desc: bool) -> ViewSpec {
    ViewSpec { table: "orders".into(), sort: vec![SortKey { column: "who".into(), desc }], ..Default::default() }
}

/// (id, who) of every row of the view, in view order, and the view's staleness.
fn read(wb: &Workbook, view: u64) -> (Vec<String>, Option<String>) {
    let b = wb.fetch(view, 0, 100).unwrap();
    (b.rows.iter().map(|r| format!("{}:{}", r[0], r[2].as_str().unwrap_or("-"))).collect(), b.stale)
}

#[test]
fn sorting_and_filtering_by_a_lookup_column_reads_stored_results() {
    let mut t = shop();
    assert_eq!(t.wb.shared().results_builds(), 0);
    let v = t.wb.open_view(by_who(false)).unwrap();
    // Ann (11, 14), Bo (12), Cy (10, 15), none (13); ties keep table order.
    assert_eq!(read(&t.wb, v.view_id), (["11:Ann", "14:Ann", "12:Bo", "10:Cy", "15:Cy", "13:none"].map(String::from).to_vec(), None));
    assert_eq!(t.wb.shared().results_builds(), 1, "the first sort built them");
    let d = t.wb.open_view(by_who(true)).unwrap();
    assert_eq!(read(&t.wb, d.view_id).0[0], "13:none");
    let f = t.wb.open_view(ViewSpec { table: "orders".into(), filters: vec![Filter { column: "who".into(), op: FilterOp::Eq, value: json!("Cy") }], ..Default::default() }).unwrap();
    assert_eq!(read(&t.wb, f.view_id).0, ["10:Cy", "15:Cy"]);
    let s = t.wb.stats(f.view_id, "id", 0, 2).unwrap();
    assert_eq!(s.count, 2);
    // Sorting by a stored column never needs them.
    let i = t.wb.open_view(ViewSpec { table: "orders".into(), sort: vec![SortKey { column: "id".into(), desc: true }], ..Default::default() }).unwrap();
    assert_eq!(read(&t.wb, i.view_id).0[0], "15:Cy");
    assert_eq!(t.wb.shared().results_builds(), 1, "later sorts and filters reused them");
    // Small tables compute live.
    t.wb.shared().set_results_min_rows(1_000);
    let e = t.wb.open_view(by_who(true)).unwrap();
    assert_eq!(read(&t.wb, e.view_id).0[0], "13:none");
    assert_eq!(t.wb.shared().results_builds(), 1);
}

#[test]
fn stale_results_stay_visible_until_new_ones_replace_them_atomically() {
    let mut t = shop();
    let v = t.wb.open_view(by_who(false)).unwrap();
    assert_eq!(read(&t.wb, v.view_id).0[0], "11:Ann");
    // Ann becomes Zed: the stored results no longer match, and say so.
    let c = t.wb.open_view(spec("customers")).unwrap();
    t.wb.edit(c.view_id, &[edit(1, "name", "Zed")]).unwrap();
    assert_eq!(t.wb.stale_results().unwrap(), ["orders"]);
    let ticket = t.wb.shared().results_queued("orders");
    let s = t.wb.open_view(by_who(false)).unwrap();
    assert_eq!(s.stale.as_deref(), Some("recalculating"));
    // The view keeps the order and the values it was built from, flagged.
    assert_eq!(read(&t.wb, s.view_id), (["11:Ann", "14:Ann", "12:Bo", "10:Cy", "15:Cy", "13:none"].map(String::from).to_vec(), Some("recalculating".into())));
    // Unsorted pages are read live, exactly.
    let plain = t.wb.open_view(spec("orders")).unwrap();
    let b = t.wb.fetch(plain.view_id, 0, 2).unwrap();
    assert_eq!((b.rows[1][2].clone(), b.stale), (json!("Zed"), None));
    // Exports never write stale values.
    let out = t.dir.path().join("orders.csv");
    t.wb.export_view(s.view_id, &out, ExportFormat::Csv).unwrap();
    assert!(std::fs::read_to_string(&out).unwrap().contains("11,2,Zed"));

    // New results are installed in one step; views built on the old ones rebuild. (Text
    // sorts by code point: "Zed" before "none".)
    let reader = t.wb.reader().unwrap();
    assert!(reader.recalculate("orders", Some(ticket)).unwrap());
    assert!(t.wb.stale_results().unwrap().is_empty());
    assert_eq!(read(&t.wb, s.view_id), (["12:Bo", "10:Cy", "15:Cy", "11:Zed", "14:Zed", "13:none"].map(String::from).to_vec(), None));
    assert!(!reader.recalculate("orders", None).unwrap(), "already fresh");
    assert_eq!(t.wb.shared().results_builds(), 2);
}

#[test]
fn a_failed_recalculation_keeps_the_last_results_and_says_why() {
    let mut t = shop();
    let v = t.wb.open_view(by_who(false)).unwrap();
    let c = t.wb.open_view(spec("customers")).unwrap();
    t.wb.edit(c.view_id, &[edit(1, "name", "Zed")]).unwrap();
    let ticket = t.wb.shared().results_queued("orders");
    crate::faults::fail_next("recalculate");
    assert!(t.wb.db().recalculate("orders", Some(ticket)).is_err());
    assert!(matches!(t.wb.shared().results_activity("orders"), ResultsActivity::Failed(_)));
    let s = t.wb.open_view(by_who(false)).unwrap();
    let why = s.stale.clone().unwrap();
    assert!(why.starts_with("recalculation failed: "), "{why}");
    assert_eq!(read(&t.wb, s.view_id).0[0], "11:Ann", "the last committed results");
    assert_eq!(read(&t.wb, v.view_id).1.as_deref(), Some(why.as_str()));
    // A build superseded by a newer one does not report its failure.
    let old = t.wb.shared().results_queued("orders");
    let _new = t.wb.shared().results_queued("orders");
    crate::faults::fail_next("recalculate");
    assert!(t.wb.db().recalculate("orders", Some(old)).is_err());
    assert_eq!(t.wb.shared().results_activity("orders"), ResultsActivity::Recalculating);
    // The next build succeeds.
    assert!(t.wb.recalculate("orders").unwrap());
    assert_eq!(t.wb.shared().results_activity("orders"), ResultsActivity::Idle);
    assert_eq!(read(&t.wb, s.view_id), (["12:Bo", "10:Cy", "15:Cy", "11:Zed", "14:Zed", "13:none"].map(String::from).to_vec(), None));
}

#[test]
fn an_interrupted_recalculation_is_cancelled_and_installs_nothing() {
    let mut t = TempWb::new();
    t.wb.shared().set_results_min_rows(1);
    t.wb.create_table_from_query("customers", "SELECT i AS id, 'c' || i AS name FROM range(200000) t(i)").unwrap();
    t.wb.create_table_from_query("orders", "SELECT i AS id, (i * 7919) % 200000 AS cid FROM range(2000000) t(i)").unwrap();
    t.wb.add_formula_column_as("orders", "who", "=XLOOKUP([@cid],customers[id],customers[name],\"none\")", FormulaLanguage::Sheet).unwrap();
    t.wb.recalculate("orders").unwrap();
    let c = t.wb.open_view(spec("customers")).unwrap();
    t.wb.edit(c.view_id, &[edit(0, "name", "first")]).unwrap();
    let before = t.wb.shared().results_builds();
    let reader = t.wb.reader().unwrap();
    let interrupt = reader.interrupt_handle();
    let ticket = t.wb.shared().results_queued("orders");
    let job = std::thread::spawn(move || reader.recalculate("orders", Some(ticket)).map_err(|e| e.to_string()));
    while !job.is_finished() {
        interrupt.interrupt();
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    let out = job.join().unwrap();
    assert!(out.is_err(), "the build was interrupted: {out:?}");
    assert_eq!(t.wb.shared().results_activity("orders"), ResultsActivity::Failed("cancelled".into()));
    assert_eq!(t.wb.shared().results_builds(), before, "nothing installed");
    assert_eq!(t.wb.stale_results().unwrap(), ["orders"]);
}

/// Lookup tables built in a snapshot are shared once it commits, and a snapshot checks that a
/// cached table is visible to it before using it: a build that fails part-way leaves other
/// connections reading correctly, and so does a table cached after a snapshot began. (The
/// race this guards against, a cancelled recalculation while views read the same lookup, is
/// exercised end to end by `e2e/tableformulas.spec.ts`.)
#[test]
fn cached_lookups_are_shared_only_when_other_connections_can_see_them() {
    let mut t = shop();
    t.wb.shared().set_results_min_rows(1_000);
    let reader = t.wb.reader().unwrap();
    let v = reader.open_view(spec("orders")).unwrap();
    let who = |b: &RowBlock| b.rows.iter().map(|r| r[2].as_str().unwrap_or("-").to_string()).collect::<Vec<_>>();
    // The writer's snapshot builds the lookup and is then rolled back.
    let db = t.wb.db();
    let failed = db.snapshot(|| -> anyhow::Result<()> {
        let base = db.base_select("orders")?;
        let n: i64 = db.conn.query_row(&format!("SELECT count(*) FROM ({base})"), [], |r| r.get(0))?;
        assert_eq!(n, 6);
        anyhow::bail!("cancelled")
    });
    assert!(failed.is_err());
    assert_eq!(who(&reader.fetch(v.view_id, 0, 10, 0, None).unwrap()), ["Cy", "Ann", "Bo", "none", "Ann", "Cy"]);
    // A table the reader caches after the writer's snapshot began.
    let c = t.wb.open_view(spec("customers")).unwrap();
    t.wb.edit(c.view_id, &[edit(0, "name", "Di")]).unwrap();
    let db = t.wb.db();
    db.snapshot(|| {
        let _ = db.conn.query_row("SELECT 1", [], |r| r.get::<_, i64>(0))?;
        assert_eq!(who(&reader.fetch(v.view_id, 0, 10, 0, None)?)[0], "Di");
        let base = db.base_select("orders")?;
        let first: String = db.conn.query_row(&format!("SELECT who FROM ({base}) ORDER BY _rf_ord LIMIT 1"), [], |r| r.get(0))?;
        assert_eq!(first, "Di");
        Ok(())
    }).unwrap();
}
