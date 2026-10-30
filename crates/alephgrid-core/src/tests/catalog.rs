//! Phase F baseline: catalog recognition is distinct from calculation evidence.
use super::*;
use std::collections::BTreeSet;

fn catalog() -> Value {
    serde_json::from_str(include_str!("../../data/function_catalog.json")).unwrap()
}

/// The table-formula differential and golden tests, by name, with the formula corpora
/// they use appended to their bodies.
fn table_tests() -> Vec<(String, String)> {
    let src = include_str!("../table_formula/tests.rs");
    let corpus = |name: &str| src.split(&format!("const {name}: &[&str] = &[")).nth(1).and_then(|r| r.split("\n];").next()).unwrap_or("").to_string();
    let mut out = Vec::new();
    for part in src.split("#[test]\nfn ").skip(1) {
        let name = part.split('(').next().unwrap().to_string();
        let mut body = part.split("\n}\n").next().unwrap().to_string();
        for c in ["CORPUS"] {
            if body.contains(c) { body.push_str(&corpus(c)); }
        }
        out.push((name, body));
    }
    out
}

/// Whether the table-formula compiler accepts the function at all (it may still refuse a
/// mode or argument): anything but its refusal of the name.
fn compiles_in_tables(name: &str) -> bool {
    let cols = [("n".to_string(), "DOUBLE".to_string())];
    match crate::table_formula::compile(&format!("={name}([@n])"), &cols, "_rf_p_") {
        Ok(_) => true,
        Err(e) => !(e.message.contains("is not available in table formulas") || e.message.contains("is not available for tables") || e.message.contains("not available yet") || e.message.contains("changes on every calculation")),
    }
}

#[test]
fn catalog_registry_and_metadata_match_the_pinned_engine() {
    let c = catalog();
    assert_eq!(c["schema_version"], 4);
    assert_eq!(c["engine"], "ironcalc-0.9.1");
    assert_eq!(c["engine_patch"], "alephgrid-compat-1");
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
        if f["table_status"] == "supported" {
            // Each cited test exists and exercises the function in a table formula.
            let evidence = f["table_evidence"].as_array().unwrap();
            assert!(!evidence.is_empty() && !f["table_modes"].as_str().unwrap().is_empty(), "{name}");
            for t in evidence {
                let t = t.as_str().unwrap();
                assert!(table_tests().iter().any(|(n, body)| n == t && body.contains(&format!("{name}("))), "{name}: {t}");
            }
        } else {
            assert_eq!(f["table_status"], "not_supported", "{name}");
        }
        assert_eq!(f["table_status"] == "supported", compiles_in_tables(name), "table status drift: {name}");
        assert!(!f["limitations"].as_array().unwrap().is_empty());
        assert!(!f["implementation"].as_str().unwrap().is_empty());
        // P0 entries are all reviewed; reviewed P1 entries (F.10) meet the same bar.
        if f["priority"] == "P0" || !f["signature"].is_null() {
            assert!(f["signature"].as_str().unwrap().starts_with(&format!("{name}(")));
            assert!(!f["examples"].as_array().unwrap().is_empty());
            assert!(!f["evidence"].as_array().unwrap().is_empty());
            assert!(f["verification_cases"].as_array().unwrap().len() >= 2, "mode evidence: {name}");
            // Every documented argument rule names cases of this function that exercise it.
            let sig = f["signature"].as_str().unwrap();
            let params: Vec<&str> = sig[sig.find('(').unwrap() + 1..sig.rfind(')').unwrap()]
                .split(',').map(|p| p.trim_matches(|c: char| c == ' ' || c == '[' || c == ']' || c == '…'))
                .filter(|p| !p.is_empty()).collect();
            let args = f["arguments"].as_array().unwrap();
            assert_eq!(args.iter().map(|a| a["name"].as_str().unwrap()).collect::<Vec<_>>(), params, "{name}");
            let ids: BTreeSet<_> = f["verification_cases"].as_array().unwrap().iter().map(|c| c["id"].as_str().unwrap()).collect();
            for a in args {
                assert!(!a["rule"].as_str().unwrap().is_empty());
                let cited = a["cases"].as_array().unwrap();
                assert!(!cited.is_empty(), "{name} {}", a["name"]);
                assert!(cited.iter().all(|c| ids.contains(c.as_str().unwrap())), "{name} {}", a["name"]);
            }
        } else {
            assert!(f["signature"].is_null() && f.get("arguments").is_none(), "unreviewed entries stay explicit: {name}");
        }
    }
}

/// Independent, hand-calculated cases are stored alongside the help metadata so
/// unsupported behavior cannot silently acquire a verified badge in the UI.
#[test]
fn certification_cases_match_independent_expectations() {
    let c = catalog();
    let mut t = TempWb::new();
    let mut inputs: Vec<SheetInput> = serde_json::from_value(c["fixture"].clone()).unwrap();
    let cases: Vec<_> = c["functions"].as_array().unwrap().iter()
        .flat_map(|f| f["verification_cases"].as_array().unwrap()).collect();
    assert_eq!(cases.len(), 351, "review certification case changes explicitly");
    assert_eq!(cases.iter().filter(|c| c["group"] == "coercion").count(), 156);
    assert_eq!(cases.iter().filter(|c| c["status"] == "verified").count(), 344);
    let gaps: BTreeSet<_> = cases.iter().filter(|c| c["status"] == "known_gap")
        .map(|c| c["id"].as_str().unwrap()).collect();
    assert_eq!(gaps, BTreeSet::from(["day-c01", "day-c02", "month-c01", "year-c01", "take-03", "textafter-03", "textbefore-03"]),
        "changing the known-gap set requires review of the desired-result regressions");
    let mut ids = BTreeSet::new();
    for (row, case) in cases.iter().enumerate() {
        assert!(ids.insert(case["id"].as_str().unwrap()));
        assert!(!case["mode"].as_str().unwrap().is_empty());
        assert!(matches!(case["status"].as_str(), Some("verified" | "known_gap")));
        assert!(matches!(case["group"].as_str(), Some("modes" | "coercion")));
        assert!(!case["oracle"].as_str().unwrap().is_empty(), "every expected value names its source");
        if case["status"] == "known_gap" {
            assert!(!case["note"].as_str().unwrap().is_empty());
        }
        if case["spill"] != true {
            inputs.push(sheet_input(row as u32, 3, case["formula"].as_str().unwrap()));
        }
    }
    // Array results (F.10) spill from column U, 12 rows apart, and are read cell by cell.
    let spills: Vec<(usize, &&Value)> = cases.iter().enumerate().filter(|(_, c)| c["spill"] == true).collect();
    inputs.extend(spills.iter().enumerate().map(|(k, (_, c))| sheet_input(k as u32 * 12, 20, c["formula"].as_str().unwrap())));
    t.wb.with_sheets(|s| { s.create("Certification")?; s.set(0, &inputs) }).unwrap();
    let mut failures = Vec::new();
    for reopen in [false, true] {
        if reopen { t.reopen(); }
        for (row, case) in cases.iter().enumerate() {
            if case["spill"] == true {
                continue;
            }
            let actual = cell_text(&t.wb, 0, row as u32, 3).expect("formula cell must survive, even with empty display text");
            if case["status"] == "known_gap" {
                // A gap that starts passing must be relabelled (with review), not left as a gap.
                if Some(actual.as_str()) == case["expected"].as_str() {
                    failures.push(format!("{} now matches its expected result; review and mark it verified", case["id"]));
                }
                continue;
            }
            if Some(actual.as_str()) != case["expected"].as_str() {
                failures.push(format!("{} {}: expected {}, received {actual:?}, reopened={reopen}", case["id"], case["formula"], case["expected"]));
            }
        }
    }
    for reopen in [false, true] {
        if reopen { t.reopen(); }
        for (k, (_, case)) in spills.iter().enumerate() {
            let want: Vec<Vec<&str>> = case["expected"].as_str().unwrap().split(';').map(|r| r.split(',').collect()).collect();
            let (rows, cols) = (want.len() as u32, want[0].len() as u32);
            let block = t.wb.sheets().fetch(0, k as u32 * 12, rows + 1, cols + 21).unwrap();
            let got: Vec<Vec<String>> = (0..=rows).map(|r| (0..=cols).map(|c| {
                block.rows.get(r as usize).and_then(|row| row.get(20 + c as usize)).cloned().flatten().map(|x| x.0).unwrap_or_default()
            }).collect()).collect();
            let inside: Vec<Vec<String>> = got[..rows as usize].iter().map(|r| r[..cols as usize].to_vec()).collect();
            let outside_empty = got[rows as usize].iter().all(|v| v.is_empty()) && got.iter().all(|r| r[cols as usize].is_empty());
            let matches = inside == want.iter().map(|r| r.iter().map(|v| v.to_string()).collect::<Vec<_>>()).collect::<Vec<_>>() && outside_empty;
            if case["status"] == "known_gap" {
                if matches {
                    failures.push(format!("{} now matches its expected result; review and mark it verified", case["id"]));
                }
                continue;
            }
            if !matches {
                failures.push(format!("{} {}: expected spill {}, received {got:?}, reopened={reopen}", case["id"], case["formula"], case["expected"]));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// The coercion cases depend on these cell types; check them with type predicates
/// so a changed input convention cannot silently change what the matrix certifies.
#[test]
fn coercion_fixture_has_the_documented_types() {
    let c = catalog();
    let mut inputs: Vec<SheetInput> = serde_json::from_value(c["fixture"].clone()).unwrap();
    let checks = [
        ("=ISNUMBER(F1)", "TRUE"), ("=ISTEXT(F2)", "TRUE"), ("=F2", "2"), ("=ISLOGICAL(F3)", "TRUE"),
        ("=ISBLANK(F4)", "TRUE"), ("=ISNA(F5)", "TRUE"), ("=ISBLANK(F6)", "FALSE"), ("=ISTEXT(F6)", "TRUE"),
        ("=LEN(F6)", "0"), ("=F7", "Ärger"), ("=F8", "-3.5"), ("=G1", "a*b"), ("=G2", "a?b"), ("=G3", "axb"),
    ];
    for (row, (formula, _)) in checks.iter().enumerate() { inputs.push(sheet_input(row as u32, 3, formula)); }
    let mut t = TempWb::new();
    t.wb.with_sheets(|s| { s.create("Fixture")?; s.set(0, &inputs) }).unwrap();
    for reopen in [false, true] {
        if reopen { t.reopen(); }
        for (row, (formula, expected)) in checks.iter().enumerate() {
            assert_eq!(cell_text(&t.wb, 0, row as u32, 3).as_deref(), Some(*expected), "{formula}, reopened={reopen}");
        }
    }
}

fn assert_compatibility_regression(id: &str) {
    let c = catalog();
    let case = c["functions"].as_array().unwrap().iter()
        .flat_map(|f| f["verification_cases"].as_array().unwrap())
        .find(|c| c["id"] == id).unwrap();
    let mut inputs: Vec<SheetInput> = serde_json::from_value(c["fixture"].clone()).unwrap();
    inputs.push(sheet_input(0, 3, case["formula"].as_str().unwrap()));
    let mut t = TempWb::new();
    t.wb.with_sheets(|s| { s.create("Gap")?; s.set(0, &inputs) }).unwrap();
    assert_eq!(cell_text(&t.wb, 0, 0, 3).unwrap_or_default(), case["expected"].as_str().unwrap(), "{id}: {}", case["formula"]);
}

macro_rules! compatibility_regression_test {
    ($name:ident, $id:literal) => {
        #[test]
        fn $name() { assert_compatibility_regression($id); }
    };
}

compatibility_regression_test!(averageifs_should_reject_unequal_ranges, "averageifs-03");
compatibility_regression_test!(countifs_should_reject_unequal_ranges, "countifs-02");
compatibility_regression_test!(sumifs_should_reject_unequal_ranges, "sumifs-03");
compatibility_regression_test!(textjoin_should_skip_empty_strings_when_requested, "textjoin-01");
compatibility_regression_test!(vlookup_zero_column_should_return_value_error, "vlookup-06");

#[test]
fn catalog_examples_match_independent_expectations() {
    let c = catalog();
    let mut t = TempWb::new();
    let mut inputs: Vec<SheetInput> = serde_json::from_value(c["fixture"].clone()).unwrap();
    let examples: Vec<_> = c["functions"].as_array().unwrap().iter()
        .flat_map(|f| f["examples"].as_array().unwrap().iter().map(move |e| (f["name"].as_str().unwrap(), e)))
        .collect();
    assert_eq!(examples.len(), 76, "44 P0 and 32 reviewed P1 (F.10) examples");
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

/// Regression for the locally patched inline-array lookup path.
#[test]
fn xlookup_inline_arrays_should_find_a_present_key() {
    let mut t = TempWb::new();
    t.wb.with_sheets(|s| {
        s.create("Array lookup")?;
        s.set(0, &[sheet_input(0, 0, "=XLOOKUP(2,{1;2;2},{10;20;30})")])
    }).unwrap();
    assert_eq!(cell_text(&t.wb, 0, 0, 0).as_deref(), Some("20"));
}

#[test]
fn patched_formula_boundaries_survive_reopen() {
    let cases = [
        ("=SUMIFS(A1:A3,B1:C3,1)", "#VALUE!"),
        ("=AVERAGEIFS(A1:A3,B1:C3,1)", "#VALUE!"),
        ("=COUNTIFS(A1:A3,1,B1:C3,1)", "#VALUE!"),
        ("=SUMIFS(B1:B4,A1:A4,2)", "50"),
        ("=AVERAGEIFS(B1:B4,A1:A4,2)", "25"),
        ("=COUNTIFS(A1:A4,2,B1:B4,\">0\")", "2"),
        ("=TEXTJOIN(\"-\",FALSE,\"a\",\"\",\"b\")", "a--b"),
        ("=TEXTJOIN(\"-\",TRUE,\"\",\"\")", ""),
        ("=VLOOKUP(2,A1:B4,-1,FALSE)", "#VALUE!"),
        ("=VLOOKUP(2,A1:B4,3,FALSE)", "#REF!"),
        ("=XLOOKUP(2,{1,2,2},{10,20,30})", "20"),
        ("=XLOOKUP(2,{1;2;2},{10;20;30},\"none\",0,-1)", "30"),
        ("=XLOOKUP(9,{1;2;2},{10;20;30},\"none\")", "none"),
        ("=XLOOKUP(9,{1;2;2},{10;20;30})", "#N/A"),
        ("=XLOOKUP(3,{1;2;4},{10;20;40},\"none\",-1)", "20"),
        ("=XLOOKUP(3,{1;2;4},{10;20;40},\"none\",1)", "40"),
        ("=XLOOKUP(2,{1;2;4},{10;20;40},\"none\",0,2)", "20"),
        ("=XLOOKUP(2,{4;2;1},{40;20;10},\"none\",0,-2)", "20"),
        ("=XLOOKUP(3,{1;2;4},{10;20;40},\"none\",0,2)", "none"),
        ("=XLOOKUP(2,{1;2;4},{10;20})", "#VALUE!"),
        ("=XLOOKUP(2,{1;2;4},{10,20,40})", "#VALUE!"),
        ("=XLOOKUP(2,{1;2;2;4},B1:B4)", "20"),
        ("=XLOOKUP(\"b*\",{\"alpha\";\"beta\"},{10;20},\"none\",2)", "20"),
    ];
    let mut t = TempWb::new();
    let mut inputs: Vec<SheetInput> = serde_json::from_value(catalog()["fixture"].clone()).unwrap();
    for (row, (formula, _)) in cases.iter().enumerate() {
        inputs.push(sheet_input(row as u32, 3, formula));
    }
    t.wb.with_sheets(|s| { s.create("Boundaries")?; s.set(0, &inputs) }).unwrap();
    for reopen in [false, true] {
        if reopen { t.reopen(); }
        for (row, (formula, expected)) in cases.iter().enumerate() {
            assert_eq!(cell_text(&t.wb, 0, row as u32, 3).as_deref(), Some(*expected), "{formula}, reopened={reopen}");
        }
    }
}
