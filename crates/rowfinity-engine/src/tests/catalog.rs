//! Phase F baseline: catalog recognition is distinct from calculation evidence.
use super::*;
use std::collections::BTreeSet;

fn catalog() -> Value {
    serde_json::from_str(include_str!("../../data/function_catalog.json")).unwrap()
}

#[test]
fn catalog_registry_and_metadata_match_the_pinned_engine() {
    let c = catalog();
    assert_eq!(c["schema_version"], 1);
    assert_eq!(c["engine"], "ironcalc-0.9.1");
    let lock = include_str!("../../../../Cargo.lock");
    let pinned = lock.split("[[package]]").find(|p| p.contains("name = \"ironcalc_base\"\n")).unwrap();
    assert!(pinned.contains("version = \"0.9.1\"\n"), "review catalog evidence when upgrading IronCalc");
    let lang = ironcalc::base::language::get_default_language();
    let mut names = BTreeSet::new();
    let entries = c["functions"].as_array().unwrap();
    assert_eq!(entries.len(), 562, "review catalog changes explicitly");
    for f in entries {
        let name = f["name"].as_str().unwrap();
        assert!(names.insert(name), "duplicate {name}");
        assert_eq!(lang.functions.lookup(name).is_some(), f["sheet_status"] == "registered", "registry drift: {name}");
        assert_eq!(f["table_status"], "not_supported", "table support needs separate evidence");
        assert!(!f["limitations"].as_array().unwrap().is_empty());
        assert!(!f["implementation"].as_str().unwrap().is_empty());
        if f["priority"] == "P0" {
            assert!(f["signature"].as_str().unwrap().starts_with(&format!("{name}(")));
            assert!(!f["examples"].as_array().unwrap().is_empty());
            assert!(!f["evidence"].as_array().unwrap().is_empty());
        }
    }
}

#[test]
fn catalog_examples_match_independent_expectations() {
    let c = catalog();
    let mut t = TempWb::new();
    let mut inputs: Vec<SheetInput> = serde_json::from_value(c["fixture"].clone()).unwrap();
    let examples: Vec<_> = c["functions"].as_array().unwrap().iter()
        .flat_map(|f| f["examples"].as_array().unwrap().iter().map(move |e| (f["name"].as_str().unwrap(), e)))
        .collect();
    assert_eq!(examples.len(), 44);
    for (row, (_, e)) in examples.iter().enumerate() {
        inputs.push(sheet_input(row as u32, 3, e["formula"].as_str().unwrap()));
    }
    t.wb.with_sheets(|s| { s.create("Examples")?; s.set(0, &inputs) }).unwrap();
    for reopen in [false, true] {
        if reopen { t.reopen(); }
        for (row, (name, e)) in examples.iter().enumerate() {
            assert_eq!(cell_text(&t.wb, 0, row as u32, 3).as_deref(), e["expected"].as_str(), "{name}, reopened={reopen}, {}", e["formula"]);
        }
    }
}

#[test]
fn lookup_modes_errors_and_source_changes_have_golden_results() {
    let mut t = TempWb::new();
    let c = catalog();
    let mut inputs: Vec<SheetInput> = serde_json::from_value(c["fixture"].clone()).unwrap();
    let cases = [
        ("=XLOOKUP(2,A1:A4,B1:B4,\"missing\",0,-1)", "30"),
        ("=XLOOKUP(9,A1:A4,B1:B4,\"missing\")", "missing"),
        ("=XLOOKUP(9,A1:A4,B1:B4)", "#N/A"),
        ("=VLOOKUP(9,A1:B4,2,FALSE)", "#N/A"),
        ("=VLOOKUP(2,A1:B4,3,FALSE)", "#REF!"),
        ("=MATCH(9,A1:A4,0)", "#N/A"),
        ("=XLOOKUP(3,A1:A4,B1:B4,\"missing\",1)", "40"),
        ("=XLOOKUP(\"b*\",C1:C4,B1:B4,\"missing\",2)", "20"),
        ("=COUNTIF(C1:C4,\"b*\")", "2"),
        ("=IFNA(1/0,\"missing\")", "#DIV/0!"),
        ("=XLOOKUP(2,A1:A4,B1:B3)", "#VALUE!"),
        ("=SUMIF(A1:A4,2,B1:B4)", "50"),
    ];
    for (row, (formula, _)) in cases.iter().enumerate() { inputs.push(sheet_input(row as u32, 3, formula)); }
    t.wb.with_sheets(|s| { s.create("Lookup")?; s.set(0, &inputs) }).unwrap();
    for (row, (formula, expected)) in cases.iter().enumerate() {
        assert_eq!(cell_text(&t.wb, 0, row as u32, 3).as_deref(), Some(*expected), "{formula}");
    }
    t.wb.with_sheets(|s| s.set(0, &[sheet_input(2, 1, "35")])).unwrap();
    assert_eq!(cell_text(&t.wb, 0, 0, 3).as_deref(), Some("35"));
    assert_eq!(cell_text(&t.wb, 0, 11, 3).as_deref(), Some("55"));
    t.wb.with_sheets(|s| s.undo()).unwrap();
    assert_eq!(cell_text(&t.wb, 0, 0, 3).as_deref(), Some("30"));
    t.reopen();
    assert_eq!(cell_text(&t.wb, 0, 11, 3).as_deref(), Some("50"));
}

/// Desired compatibility, deliberately excluded from the passing baseline. Run this
/// explicitly when evaluating an IronCalc upgrade; remove ignore only after fixing it.
#[test]
#[ignore = "IronCalc 0.9.1 array-input XLOOKUP gap; upstream issue 1338 is related"]
fn xlookup_inline_arrays_should_find_a_present_key() {
    let mut t = TempWb::new();
    t.wb.with_sheets(|s| {
        s.create("Array lookup")?;
        s.set(0, &[sheet_input(0, 0, "=XLOOKUP(2,{1;2;2},{10;20;30})")])
    }).unwrap();
    assert_eq!(cell_text(&t.wb, 0, 0, 0).as_deref(), Some("20"));
}
