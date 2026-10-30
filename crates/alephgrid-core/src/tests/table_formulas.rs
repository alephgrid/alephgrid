//! F.05: spreadsheet-syntax calculated columns stored in a workbook and read through views.
use super::*;

fn setup() -> TempWb {
    let mut t = TempWb::new();
    t.wb.create_table_from_query("orders", "SELECT * FROM (VALUES (1, 100.0, 4, 'Ann'), (2, 50.0, 0, NULL), (3, NULL, 2, 'Bo')) v(id, amount, units, buyer)").unwrap();
    t
}

fn block(wb: &mut Workbook) -> RowBlock {
    let v = wb.open_view(spec("orders")).unwrap();
    wb.fetch(v.view_id, 0, 10).unwrap()
}

#[test]
fn sheet_formula_columns_compute_errors_and_survive_reopen() {
    let mut t = setup();
    t.wb.add_formula_column_as("orders", "unit_price", "=[@amount]/[@units]", FormulaLanguage::Sheet).unwrap();
    t.wb.add_formula_column_as("orders", "label", "=IF([@unit_price]>20,\"high\",\"low\")&\" \"&[@buyer]", FormulaLanguage::Sheet).unwrap();
    t.wb.add_formula_column_as("orders", "safe", "=IFERROR([@unit_price],\"\")", FormulaLanguage::Sheet).unwrap();
    let cols = t.wb.columns("orders").unwrap();
    let names: Vec<&str> = cols.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["id", "amount", "units", "buyer", "unit_price", "label", "safe"], "error columns stay hidden");
    assert_eq!(cols[4].formula_language, Some(FormulaLanguage::Sheet));
    assert_eq!(cols[4].sql_type, "DOUBLE");
    assert_eq!(cols[5].sql_type, "VARCHAR");
    for reopen in [false, true] {
        if reopen { t.reopen(); }
        let b = block(&mut t.wb);
        let row = |r: usize| b.rows[r].iter().map(|v| v.to_string()).collect::<Vec<_>>().join("|");
        assert_eq!(row(0), "1|\"100.0\"|4|\"Ann\"|25.0|\"high Ann\"|25.0");
        // Row 2: 50/0 is #DIV/0!; the label depends on it and shows the same error.
        assert_eq!(row(1), "2|\"50.0\"|0|null|null|null|null");
        // Row 3: blank amount is 0, so 0/2 = 0.
        assert_eq!(row(2), "3|null|2|\"Bo\"|0.0|\"low Bo\"|0.0");
        let marks: Vec<(u32, u32, &str)> = b.errors.iter().map(|e| (e.row, e.column, e.code.as_str())).collect();
        assert_eq!(marks, [(1, 4, "#DIV/0!"), (1, 5, "#DIV/0!")], "IFERROR(…, \"\") leaves a blank, not an error");
    }
}

#[test]
fn dependencies_follow_renames_and_block_deletes() {
    let mut t = setup();
    t.wb.add_formula_column_as("orders", "total", "=[@amount]*[units]+[@[amount]]", FormulaLanguage::Sheet).unwrap();
    t.wb.rename_column("orders", "amount", "net amount").unwrap();
    let total = t.wb.columns("orders").unwrap().into_iter().find(|c| c.name == "total").unwrap();
    assert_eq!(total.formula.as_deref(), Some("=[@[net amount]]*[units]+[@[net amount]]"));
    let err = t.wb.drop_column("orders", "units").unwrap_err().to_string();
    assert!(err.contains("total"), "{err}");
    // Rejected formulas change nothing.
    let err = t.wb.add_formula_column_as("orders", "bad", "=[@missing]+1", FormulaLanguage::Sheet).unwrap_err().to_string();
    assert!(err.contains("no column named") && err.contains("character"), "{err}");
    assert!(t.wb.update_formula("orders", "total", "=[@total]+1").unwrap_err().to_string().contains("no column named"), "a formula cannot read itself");
    t.wb.update_formula("orders", "total", "=ROUND([@[net amount]]/3, 1)").unwrap();
    let b = block(&mut t.wb);
    assert_eq!(b.rows[0][4].to_string(), "33.3");
    // Reserved names cannot be created.
    assert!(t.wb.add_column("orders", "_rf_e_x", "INTEGER").is_err());
}

// ---------------------------------------------------------------- F.06 lookups across tables

/// Customers has a duplicate id (1: "Ann" then "Ann B") and a blank id; orders look them up.
fn lookups() -> TempWb {
    let mut t = TempWb::new();
    t.wb.create_table_from_query("customers", "SELECT * FROM (VALUES (1, 'Ann', 'gold'), (2, 'Bo', 'silver'), (1, 'Ann B', 'bronze'), (NULL, 'Nobody', 'none')) v(id, name, tier)").unwrap();
    t.wb.create_table_from_query("orders", "SELECT * FROM (VALUES (10, 1, 'bronze'), (11, 2, 'gold'), (12, 3, 'gold'), (13, NULL, 'none')) v(id, cid, want)").unwrap();
    for (name, f) in [
        ("customer", "=XLOOKUP([@cid],customers[id],customers[name],\"unknown\")"),
        ("last", "=XLOOKUP([@cid],customers[id],customers[name],\"unknown\",0,-1)"),
        ("tier", "=VLOOKUP([@cid],customers,3,FALSE)"),
        ("both", "=XLOOKUP(1,(customers[id]=[@cid])*(customers[tier]=[@want]),customers[name],\"none\")"),
    ] {
        t.wb.add_formula_column_as("orders", name, f, FormulaLanguage::Sheet).unwrap();
    }
    t
}

fn table_block(wb: &mut Workbook, table: &str) -> RowBlock {
    let v = wb.open_view(spec(table)).unwrap();
    wb.fetch(v.view_id, 0, 100).unwrap()
}

/// Rendered rows: values as JSON text, error cells as their code.
fn rendered(b: &RowBlock) -> Vec<String> {
    b.rows.iter().enumerate().map(|(r, row)| {
        row.iter().enumerate().map(|(c, v)| {
            b.errors.iter().find(|e| e.row as usize == r && e.column as usize == c).map(|e| e.code.clone()).unwrap_or_else(|| v.to_string())
        }).collect::<Vec<_>>().join("|")
    }).collect()
}

#[test]
fn lookups_give_one_result_per_row_and_follow_source_edits() {
    let mut t = lookups();
    let expect = |first: &str| vec![
        format!("10|1|\"bronze\"|\"{first}\"|\"Ann B\"|\"gold\"|\"Ann B\""),
        "11|2|\"gold\"|\"Bo\"|\"Bo\"|\"silver\"|\"none\"".to_string(),
        "12|3|\"gold\"|\"unknown\"|\"unknown\"|#N/A|\"none\"".to_string(),
        // A blank key finds the blank id; the pair (blank, "none") matches too.
        "13|null|\"none\"|\"Nobody\"|\"Nobody\"|\"none\"|\"Nobody\"".to_string(),
    ];
    assert_eq!(rendered(&table_block(&mut t.wb, "orders")), expect("Ann"), "duplicates give the first or last match, one row per order");
    // Editing the source changes the lookups that read it, and nothing else.
    let v = t.wb.open_view(spec("customers")).unwrap();
    t.wb.edit(v.view_id, &[CellEdit { row: 0, column: "name".into(), value: Some("Anna".into()), record: None }]).unwrap();
    assert_eq!(rendered(&table_block(&mut t.wb, "orders")), expect("Anna"));
    t.reopen();
    assert_eq!(rendered(&table_block(&mut t.wb, "orders")), expect("Anna"));
}

#[test]
fn renames_rewrite_lookups_in_other_tables_and_deletes_are_refused() {
    let mut t = lookups();
    let before = rendered(&table_block(&mut t.wb, "orders"));
    let formula = |wb: &Workbook, name: &str| wb.columns("orders").unwrap().into_iter().find(|c| c.name == name).unwrap().formula.unwrap();
    t.wb.rename_column("customers", "name", "full name").unwrap();
    assert_eq!(formula(&t.wb, "customer"), "=XLOOKUP([@cid],customers[id],customers[full name],\"unknown\")");
    assert_eq!(rendered(&table_block(&mut t.wb, "orders")), before);
    assert_eq!(t.wb.rename_table("customers", "clients").unwrap(), "clients");
    assert_eq!(formula(&t.wb, "tier"), "=VLOOKUP([@cid],clients,3,FALSE)");
    assert_eq!(formula(&t.wb, "both"), "=XLOOKUP(1,(clients[id]=[@cid])*(clients[tier]=[@want]),clients[full name],\"none\")");
    assert_eq!(rendered(&table_block(&mut t.wb, "orders")), before);
    // A name that formulas cannot spell is refused while formulas refer to the table.
    assert!(t.wb.rename_table("clients", "2024").unwrap_err().to_string().contains("letters, digits"));
    let err = t.wb.drop_column("clients", "full name").unwrap_err().to_string();
    assert!(err.contains("orders.\"customer\"") && err.contains("orders.\"last\""), "{err}");
    // VLOOKUP reads by column number: deleting a column at or before the one it returns would
    // shift its result; deleting one after it is fine.
    let err = t.wb.drop_column("clients", "tier").unwrap_err().to_string();
    assert!(err.contains("orders.\"tier\""), "{err}");
    t.wb.add_column("clients", "extra", "INTEGER").unwrap();
    t.wb.drop_column("clients", "extra").unwrap();
    assert!(t.wb.drop_table("clients").unwrap_err().to_string().contains("orders.\"customer\""));
    t.reopen();
    assert_eq!(rendered(&table_block(&mut t.wb, "orders")), before);
}

#[test]
fn tables_may_look_each_other_up_but_circular_references_are_refused() {
    let mut t = lookups();
    // Orders read customers and customers read orders, through different columns:
    // customers.first_label → orders.label → customers.label is a chain, not a cycle.
    t.wb.add_formula_column_as("customers", "label", "=[@name]&\"!\"", FormulaLanguage::Sheet).unwrap();
    t.wb.add_formula_column_as("orders", "label", "=XLOOKUP([@cid],customers[id],customers[label],\"-\")", FormulaLanguage::Sheet).unwrap();
    t.wb.add_formula_column_as("customers", "first_order", "=XLOOKUP([@id],orders[cid],orders[id],0)", FormulaLanguage::Sheet).unwrap();
    t.wb.add_formula_column_as("customers", "first_label", "=XLOOKUP([@id],orders[cid],orders[label],\"-\")", FormulaLanguage::Sheet).unwrap();
    for reopen in [false, true] {
        if reopen { t.reopen(); }
        let orders: Vec<String> = rendered(&table_block(&mut t.wb, "orders")).iter().map(|r| r.rsplit('|').next().unwrap().to_string()).collect();
        assert_eq!(orders, ["\"Ann!\"", "\"Bo!\"", "\"-\"", "\"Nobody!\""]);
        let customers: Vec<String> = rendered(&table_block(&mut t.wb, "customers")).iter().map(|r| r.split('|').skip(3).collect::<Vec<_>>().join("|")).collect();
        assert_eq!(customers, ["\"Ann!\"|10.0|\"Ann!\"", "\"Bo!\"|11.0|\"Bo!\"", "\"Ann B!\"|10.0|\"Ann!\"", "\"Nobody!\"|13.0|\"Nobody!\""]);
    }
    // Making customers.label read orders.label closes the loop.
    let err = t.wb.update_formula("customers", "label", "=XLOOKUP([@id],orders[cid],orders[label],\"-\")").unwrap_err().to_string();
    assert!(err.contains("circular reference: customers.label → orders.label → customers.label"), "{err}");
    let err = t.wb.add_formula_column_as("orders", "loop", "=XLOOKUP([@cid],customers[id],customers[first_label],\"-\")", FormulaLanguage::Sheet);
    assert!(err.is_ok(), "reading a column that reads this table is not circular by itself: {err:?}");
}

/// Lookup read timings on large tables (F.06/F.T04). Opt-in, release build:
/// `ALEPHGRID_BENCH_ROWS=1000000 cargo test --release -p alephgrid-core --lib lookup_read_timings -- --ignored --nocapture`
#[test]
#[ignore]
fn lookup_read_timings() {
    let rows: u64 = std::env::var("ALEPHGRID_BENCH_ROWS").ok().and_then(|v| v.parse().ok()).unwrap_or(1_000_000);
    let customers = (rows / 10).max(1);
    let mut t = TempWb::new();
    let time = |label: &str, f: &mut dyn FnMut()| {
        let start = std::time::Instant::now();
        f();
        eprintln!("{label:<44} {:>9.1} ms", start.elapsed().as_secs_f64() * 1000.0);
    };
    t.wb.create_table_from_query("customers", &format!(
        "SELECT i AS id, 'customer ' || i AS name, (i % 5)::INTEGER AS tier FROM range({customers}) r(i)"
    )).unwrap();
    t.wb.create_table_from_query("orders", &format!(
        "SELECT i AS id, (hash(i) % {})::BIGINT AS cid, (i % 1000) * 1.5 AS amount FROM range({rows}) r(i)", customers + customers / 20
    )).unwrap();
    eprintln!("{rows} orders, {customers} customers (about 5% of orders have no customer)");
    time("add XLOOKUP column (compile + check)", &mut || {
        t.wb.add_formula_column_as("orders", "customer", "=XLOOKUP([@cid],customers[id],customers[name],\"none\")", FormulaLanguage::Sheet).unwrap();
    });
    time("add VLOOKUP column", &mut || {
        t.wb.add_formula_column_as("orders", "tier", "=VLOOKUP([@cid],customers,3,FALSE)", FormulaLanguage::Sheet).unwrap();
    });
    let mut view = 0;
    time("open view", &mut || { view = t.wb.open_view(spec("orders")).unwrap().view_id; });
    for (label, at) in [("first page (100 rows)", 0), ("first page again", 0), ("middle page", rows / 2), ("last page", rows.saturating_sub(100))] {
        time(label, &mut || { let b = t.wb.fetch(view, at, 100).unwrap(); assert_eq!(b.rows.len(), 100.min(rows as usize)); });
    }
    let mut sorted = spec("orders");
    sorted.sort = vec![SortKey { column: "customer".into(), desc: false }];
    time("open view sorted by the lookup column", &mut || { view = t.wb.open_view(sorted.clone()).unwrap().view_id; });
    time("first page of the sorted view", &mut || { t.wb.fetch(view, 0, 100).unwrap(); });
    // F.09: the first sort stored the formula results (tables of 200,000 rows or more).
    let mut desc = sorted.clone();
    desc.sort[0].desc = true;
    time("sort by the lookup column again (stored)", &mut || { view = t.wb.open_view(desc.clone()).unwrap().view_id; });
    let mut filtered = spec("orders");
    filtered.filters = vec![Filter { column: "tier".into(), op: FilterOp::Eq, value: serde_json::json!(3) }];
    time("filter by the lookup column (stored)", &mut || { view = t.wb.open_view(filtered.clone()).unwrap().view_id; });
    time("first page of the filtered view", &mut || { t.wb.fetch(view, 0, 100).unwrap(); });
    let cv = t.wb.open_view(spec("customers")).unwrap().view_id;
    time("edit one customer name", &mut || {
        t.wb.edit(cv, &[CellEdit { row: 0, column: "name".into(), value: Some("renamed".into()), record: None }]).unwrap();
    });
    t.wb.shared().results_queued("orders");
    time("sort while results are stale (last results)", &mut || { view = t.wb.open_view(sorted.clone()).unwrap().view_id; });
    time("recalculate stored results", &mut || { t.wb.recalculate("orders").unwrap(); });
    time("sort with the new results", &mut || { view = t.wb.open_view(desc.clone()).unwrap().view_id; });
    let mut view = 0;
    time("open view after the source edit", &mut || { view = t.wb.open_view(spec("orders")).unwrap().view_id; });
    time("first page after the source edit", &mut || { t.wb.fetch(view, 0, 100).unwrap(); });
    // F.07: nearest-match lookups (one ASOF join per kind of key, no per-row work).
    time("add nearest-smaller XLOOKUP column", &mut || {
        t.wb.add_formula_column_as("orders", "near", "=XLOOKUP([@cid]+0.5,customers[id],customers[name],,-1)", FormulaLanguage::Sheet).unwrap();
    });
    let mut nview = 0;
    time("open view with the nearest-match column", &mut || { nview = t.wb.open_view(spec("orders")).unwrap().view_id; });
    time("first page with the nearest-match column", &mut || { t.wb.fetch(nview, 0, 100).unwrap(); });
    time("middle page with the nearest-match column", &mut || { t.wb.fetch(nview, rows / 2, 100).unwrap(); });
    t.wb.drop_column("orders", "near").unwrap();
    // Conditional aggregates the other way: each customer's order total and count.
    time("add SUMIFS and COUNTIFS columns on customers", &mut || {
        t.wb.add_formula_column_as("customers", "total", "=SUMIFS(orders[amount],orders[cid],[@id])", FormulaLanguage::Sheet).unwrap();
        t.wb.add_formula_column_as("customers", "n", "=COUNTIFS(orders[cid],[@id],orders[amount],\">100\")", FormulaLanguage::Sheet).unwrap();
    });
    let mut cview = 0;
    time("open customers view", &mut || { cview = t.wb.open_view(spec("customers")).unwrap().view_id; });
    for label in ["customers first page", "customers first page again"] {
        time(label, &mut || { t.wb.fetch(cview, 0, 100).unwrap(); });
    }
    time("add a text-criteria COUNTIFS (per distinct criterion)", &mut || {
        t.wb.add_formula_column_as("customers", "named", "=COUNTIFS(orders[customer],[@name])", FormulaLanguage::Sheet).unwrap();
    });
    let mut cview = 0;
    time("open customers view again", &mut || { cview = t.wb.open_view(spec("customers")).unwrap().view_id; });
    time("customers first page with the text criterion", &mut || { t.wb.fetch(cview, 0, 100).unwrap(); });
}


#[test]
fn cached_lookups_are_rebuilt_only_when_their_source_changes() {
    let mut t = lookups();
    let cached = |wb: &Workbook| wb.shared.lookups.lock().unwrap().tables();
    let before = rendered(&table_block(&mut t.wb, "orders"));
    let first = cached(&t.wb);
    assert_eq!(first.len(), 4, "one grouped relation per lookup formula: {first:?}");
    // Reading again reuses them.
    assert_eq!(rendered(&table_block(&mut t.wb, "orders")), before);
    assert_eq!(cached(&t.wb), first);
    // Editing the table that holds the formulas does not touch the looked-up table's results.
    let v = t.wb.open_view(spec("orders")).unwrap();
    t.wb.edit(v.view_id, &[CellEdit { row: 1, column: "want".into(), value: Some("silver".into()), record: None }]).unwrap();
    let after = rendered(&table_block(&mut t.wb, "orders"));
    assert_eq!(after[1], "11|2|\"silver\"|\"Bo\"|\"Bo\"|\"silver\"|\"Bo\"", "the multi-key lookup sees the new key");
    assert_eq!(cached(&t.wb), first);
    // Editing the looked-up table rebuilds them, with the new data.
    let v = t.wb.open_view(spec("customers")).unwrap();
    t.wb.edit(v.view_id, &[CellEdit { row: 1, column: "name".into(), value: Some("Bea".into()), record: None }]).unwrap();
    let after = rendered(&table_block(&mut t.wb, "orders"));
    assert_eq!(after[1], "11|2|\"silver\"|\"Bea\"|\"Bea\"|\"silver\"|\"Bea\"");
    let rebuilt = cached(&t.wb);
    assert_eq!(rebuilt.len(), 4);
    assert!(rebuilt.iter().all(|n| !first.contains(n)), "{first:?} → {rebuilt:?}");
    // A reopened workbook starts with an empty cache and computes the same results.
    t.reopen();
    assert!(t.wb.shared.lookups.lock().unwrap().is_empty());
    assert_eq!(rendered(&table_block(&mut t.wb, "orders")), after);
}

#[test]
fn the_lookup_cache_drops_the_least_recently_used_beyond_its_budget() {
    let t = TempWb::new();
    let half = crate::limits::LOOKUP_CACHE_ROWS / 2;
    let mut cache = crate::catalog::LookupCache::default();
    let table = || Some(std::sync::Arc::new(t.wb.shared.new_scratch("lk")));
    let revs = |r: u64| vec![("customers".to_string(), r)];
    cache.put("a", revs(1), table(), half);
    cache.put("b", revs(1), table(), half);
    assert_eq!(cache.len(), 2);
    assert!(cache.get("a", &revs(1)).is_some(), "a is now the most recently used");
    assert!(cache.get("a", &revs(2)).is_none(), "another data revision is a miss");
    cache.put("c", revs(1), table(), half);
    assert_eq!(cache.len(), 2);
    assert!(cache.get("b", &revs(1)).is_none(), "b was the least recently used");
    assert!(cache.get("a", &revs(1)).is_some() && cache.get("c", &revs(1)).is_some());
    // A result too large to keep is remembered as such, without rows.
    cache.put("huge", revs(1), None, crate::limits::LOOKUP_CACHE_ROWS * 2);
    assert!(matches!(cache.get("huge", &revs(1)), Some(None)));
    assert!(cache.rows() <= crate::limits::LOOKUP_CACHE_ROWS);
}

#[test]
fn conditional_aggregates_across_tables_follow_edits_and_renames() {
    let mut t = TempWb::new();
    t.wb.create_table_from_query("customers", "SELECT * FROM (VALUES (1, 'Ann'), (2, 'Bo'), (3, 'Cy')) v(id, name)").unwrap();
    t.wb.create_table_from_query("orders", "SELECT * FROM (VALUES (10, 1, 100.0, 'web'), (11, 1, 50.0, 'shop'), (12, 2, 30.0, 'web')) v(id, cid, amount, channel)").unwrap();
    for (name, f) in [
        ("total", "=SUMIFS(orders[amount],orders[cid],[@id])"),
        ("orders", "=COUNTIFS(orders[cid],[@id])"),
        ("average", "=AVERAGEIFS(orders[amount],orders[cid],[@id])"),
        ("web", "=SUMIFS(orders[amount],orders[cid],[@id],orders[channel],\"w*\")"),
        ("by_name", "=COUNTIFS(orders[channel],\"<>\"&[@name])"),
    ] {
        t.wb.add_formula_column_as("customers", name, f, FormulaLanguage::Sheet).unwrap();
    }
    // Orders read a total that customers compute from orders: a chain, not a cycle.
    t.wb.add_formula_column_as("orders", "share", "=[@amount]/XLOOKUP([@cid],customers[id],customers[total])", FormulaLanguage::Sheet).unwrap();
    let customers = |wb: &mut Workbook| rendered(&table_block(wb, "customers")).iter().map(|r| r.split('|').skip(2).collect::<Vec<_>>().join("|")).collect::<Vec<_>>();
    assert_eq!(customers(&mut t.wb), ["150.0|2.0|75.0|100.0|3.0", "30.0|1.0|30.0|30.0|3.0", "0.0|0.0|#DIV/0!|0.0|3.0"]);
    let shares: Vec<String> = rendered(&table_block(&mut t.wb, "orders")).iter().map(|r| r.rsplit('|').next().unwrap().to_string()).collect();
    assert_eq!(shares, ["0.6666666666666666", "0.3333333333333333", "1.0"]);
    // An edit to orders moves the totals.
    let v = t.wb.open_view(spec("orders")).unwrap();
    t.wb.edit(v.view_id, &[CellEdit { row: 2, column: "cid".into(), value: Some("3".into()), record: None }]).unwrap();
    assert_eq!(customers(&mut t.wb), ["150.0|2.0|75.0|100.0|3.0", "0.0|0.0|#DIV/0!|0.0|3.0", "30.0|1.0|30.0|30.0|3.0"]);
    // Renames follow; a delete that a formula needs is refused.
    t.wb.rename_column("orders", "amount", "net").unwrap();
    let total = t.wb.columns("customers").unwrap().into_iter().find(|c| c.name == "total").unwrap().formula.unwrap();
    assert_eq!(total, "=SUMIFS(orders[net],orders[cid],[@id])");
    assert!(t.wb.drop_column("orders", "channel").unwrap_err().to_string().contains("customers.\"web\""));
    t.reopen();
    assert_eq!(customers(&mut t.wb), ["150.0|2.0|75.0|100.0|3.0", "0.0|0.0|#DIV/0!|0.0|3.0", "30.0|1.0|30.0|30.0|3.0"]);
}

/// F.07 in a workbook: tiered rates by nearest match, a sorted VLOOKUP that stops answering
/// when its keys are no longer sorted, INDEX/MATCH, and dependencies. By hand from the rows:
/// tiers (min, rate) = (0, 0), (100, 0.05), (500, 0.1), (1000, 0.15); amounts 50, 100,
/// 750, 2000 and blank.
#[test]
fn nearest_sorted_and_positional_lookups_in_a_workbook() {
    let mut t = TempWb::new();
    t.wb.create_table_from_query("tiers", "SELECT * FROM (VALUES (0, 0.0), (100, 0.05), (500, 0.1), (1000, 0.15)) v(min, rate)").unwrap();
    t.wb.create_table_from_query("orders", "SELECT * FROM (VALUES (1, 50), (2, 100), (3, 750), (4, 2000), (5, NULL)) v(id, amount)").unwrap();
    for (name, f) in [
        ("rate", "=XLOOKUP([@amount],tiers[min],tiers[rate],,-1)"),
        ("band", "=VLOOKUP([@amount],tiers,2)"),
        ("pos", "=MATCH([@amount],tiers[min])"),
        ("next", "=INDEX(tiers,MATCH([@amount],tiers[min])+1,2)"),
        ("above", "=XLOOKUP([@amount]+1,tiers[min],tiers[min],-1,1)"),
    ] {
        t.wb.add_formula_column_as("orders", name, f, FormulaLanguage::Sheet).unwrap();
    }
    let rendered = |wb: &mut Workbook| rendered(&table_block(wb, "orders"));
    // A blank amount: XLOOKUP's exact match of a blank is the 0 key, VLOOKUP reads it as 0,
    // MATCH finds nothing.
    assert_eq!(rendered(&mut t.wb), [
        "1|50|0.0|0.0|1.0|0.05|100.0",
        "2|100|0.05|0.05|2.0|0.1|500.0",
        "3|750|0.1|0.1|3.0|0.15|1000.0",
        "4|2000|0.15|0.15|4.0|#REF!|-1.0",
        "5|null|0.0|0.0|#N/A|#N/A|100.0",
    ].map(String::from));
    // Unsorted keys: the sorted lookups say #N/A; the nearest match does not need sorting.
    let v = t.wb.open_view(spec("tiers")).unwrap();
    t.wb.edit(v.view_id, &[CellEdit { row: 1, column: "min".into(), value: Some("2000".into()), record: None }]).unwrap();
    assert_eq!(rendered(&mut t.wb)[2], "3|750|0.1|#N/A|#N/A|#N/A|1000.0");
    // Deleting a column the lookups read is refused, INDEX's column number included.
    let e = t.wb.drop_column("tiers", "rate").unwrap_err().to_string();
    assert!(e.contains("orders.\"rate\"") && e.contains("orders.\"next\""), "{e}");
    // After reopening, with the unsorted mins 0, 2000, 500, 1000: 51 is nearest 500 above.
    t.reopen();
    assert_eq!(rendered(&mut t.wb)[0], "1|50|0.0|#N/A|#N/A|#N/A|500.0");
}

/// F.T02: first and last matches follow the table's persisted order, so inserting a row
/// above changes which duplicate a lookup returns; an edit to the looked-up table refreshes
/// the cached relation instead of serving a stale one. By hand from the rows below.
#[test]
fn lookups_follow_reordered_sources_and_reject_stale_caches() {
    let mut t = TempWb::new();
    t.wb.create_table_from_query("codes", "SELECT * FROM (VALUES ('a', 'first'), ('a', 'second')) v(k, label)").unwrap();
    t.wb.create_table_from_query("uses", "SELECT * FROM (VALUES (1, 'a')) v(id, k)").unwrap();
    t.wb.add_formula_column_as("uses", "first", "=XLOOKUP([@k],codes[k],codes[label])", FormulaLanguage::Sheet).unwrap();
    t.wb.add_formula_column_as("uses", "last", "=XLOOKUP([@k],codes[k],codes[label],,0,-1)", FormulaLanguage::Sheet).unwrap();
    assert_eq!(rendered(&table_block(&mut t.wb, "uses")), ["1|\"a\"|\"first\"|\"second\""]);
    // A new duplicate inserted at the top becomes the first match.
    let v = t.wb.open_view(spec("codes")).unwrap();
    t.wb.insert_rows(v.view_id, 0, 1).unwrap();
    t.wb.edit(v.view_id, &[edit(0, "k", "a"), edit(0, "label", "zeroth")]).unwrap();
    assert_eq!(rendered(&table_block(&mut t.wb, "uses")), ["1|\"a\"|\"zeroth\"|\"second\""]);
    // Deleting the last duplicate: the cached relation is rebuilt, not reused.
    t.wb.delete_rows(v.view_id, 2, 1).unwrap();
    assert_eq!(rendered(&table_block(&mut t.wb, "uses")), ["1|\"a\"|\"zeroth\"|\"first\""]);
}
