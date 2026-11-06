use super::*;
use crate::functions::*;
fn definition(kind: FunctionKind, source: &str) -> FunctionDefinition {
    FunctionDefinition {
        name: "MARGIN".into(),
        kind,
        arguments: vec!["revenue".into(), "cost".into()],
        source: source.into(),
        documentation: "Profit margin".into(),
        dependencies: Default::default(),
        returns: None,
        volatile: false,
        tests: vec![FunctionTest { arguments: vec!["100".into(), "60".into()], expected: "0.4".into() }],
    }
}
#[test]
fn lambda_recalculates_versions_and_reopens() {
    let mut t = TempWb::new();
    let d = definition(FunctionKind::Formula, "IF(revenue=0,0,(revenue-cost)/revenue)");
    assert!(t.wb.test_function(&d).unwrap()[0].passed);
    t.wb.publish_function(d.clone()).unwrap();
    t.wb.with_sheets(|s| {
        s.set(
            0,
            &[
                SheetInput { row: 0, column: 0, value: "100".into(), base: None },
                SheetInput { row: 0, column: 1, value: "60".into(), base: None },
                SheetInput { row: 0, column: 2, value: "=MARGIN(A1,B1)".into(), base: None },
            ],
        )
    })
    .unwrap();
    assert_eq!(t.wb.sheets().fetch(0, 0, 1, 3).unwrap().rows[0][2].as_ref().unwrap().0, "0.4");
    let mut d2 = d;
    d2.source = "revenue-cost".into();
    d2.tests[0].expected = "40".into();
    t.wb.publish_function(d2).unwrap();
    assert_eq!(t.wb.functions().unwrap()[0].pinned, 1);
    t.wb.pin_functions(&std::collections::BTreeMap::from([("MARGIN".into(), 2)])).unwrap();
    assert_eq!(t.wb.sheets().fetch(0, 0, 1, 3).unwrap().rows[0][2].as_ref().unwrap().0, "40");
    t.reopen();
    assert_eq!(t.wb.sheets().fetch(0, 0, 1, 3).unwrap().rows[0][2].as_ref().unwrap().0, "40");
    t.wb.pin_functions(&std::collections::BTreeMap::from([("MARGIN".into(), 1)])).unwrap();
    assert_eq!(t.wb.sheets().fetch(0, 0, 1, 3).unwrap().rows[0][2].as_ref().unwrap().0, "0.4");
}
#[test]
fn sql_functions_are_validated_and_persisted() {
    let mut t = TempWb::new();
    let d = definition(FunctionKind::Sql, "(CAST(revenue AS DOUBLE)-CAST(cost AS DOUBLE))/CAST(revenue AS DOUBLE)");
    t.wb.publish_function(d).unwrap();
    assert_eq!(t.wb.run_query("SELECT MARGIN(100,60)", &[], 5).unwrap().rows[0][0], serde_json::json!(0.4));
    t.reopen();
    assert_eq!(t.wb.run_query("SELECT MARGIN(100,60)", &[], 5).unwrap().rows[0][0], serde_json::json!(0.4));
    for source in ["current_setting('threads')", "(SELECT 1)", "revenue); DROP TABLE _rf_meta; --", "unknown_column"] {
        let mut d = definition(FunctionKind::Sql, source);
        d.name = "UNSAFE".into();
        let before = fingerprint(&t.wb);
        assert!(t.wb.publish_function(d).is_err(), "{source}");
        assert_eq!(before, fingerprint(&t.wb));
    }
}
#[test]
fn invalid_formulas_and_failed_publish_leave_no_changes() {
    let mut t = TempWb::new();
    for source in ["MARGIN(revenue,cost)", "A1+revenue", "IF(", "MISSING(revenue)"] {
        let before = fingerprint(&t.wb);
        assert!(t.wb.publish_function(definition(FunctionKind::Formula, source)).is_err(), "{source}");
        assert_eq!(before, fingerprint(&t.wb));
    }
    let d = definition(FunctionKind::Formula, "revenue-cost");
    assert!(t.wb.publish_function(d).unwrap_err().to_string().contains("tests failed"));
}

fn unary(name: &str, kind: FunctionKind, source: &str) -> FunctionDefinition {
    FunctionDefinition {
        name: name.into(),
        kind,
        arguments: vec!["value".into()],
        source: source.into(),
        documentation: String::new(),
        tests: vec![],
        dependencies: Default::default(),
        returns: None,
        volatile: false,
    }
}

#[test]
fn ranges_nested_calls_edits_and_undo_use_engine_semantics() {
    let mut t = TempWb::new();
    t.wb.publish_function(unary("TOTALPLUS", FunctionKind::Formula, "SUM(value)+1")).unwrap();
    t.wb.publish_function(unary("TWICETOTAL", FunctionKind::Formula, "TOTALPLUS(value)*2")).unwrap();
    t.wb.with_sheets(|s| s.set(0, &[sheet_input(0, 0, "2"), sheet_input(1, 0, "3"), sheet_input(0, 1, "=TWICETOTAL(A1:A2)")])).unwrap();
    assert_eq!(cell_text(&t.wb, 0, 0, 1).as_deref(), Some("12"));
    t.wb.with_sheets(|s| s.set(0, &[sheet_input(1, 0, "5")])).unwrap();
    assert_eq!(cell_text(&t.wb, 0, 0, 1).as_deref(), Some("16"));
    t.wb.with_sheets(|s| s.undo()).unwrap();
    assert_eq!(cell_text(&t.wb, 0, 0, 1).as_deref(), Some("12"));
    t.reopen();
    assert_eq!(cell_text(&t.wb, 0, 0, 1).as_deref(), Some("12"));
}

#[test]
fn dependency_versions_upgrade_and_rollback_atomically() {
    for kind in [FunctionKind::Formula, FunctionKind::Sql] {
        let mut t = TempWb::new();
        t.wb.publish_function(unary("ADDONE", kind.clone(), "value+1")).unwrap();
        let d = unary("DOUBLEADD", kind.clone(), "ADDONE(value)*2");
        t.wb.publish_function(d.clone()).unwrap();
        t.wb.publish_function(unary("ADDONE", kind.clone(), "value+2")).unwrap();
        let before = fingerprint(&t.wb);
        assert!(t.wb.pin_functions(&std::collections::BTreeMap::from([("ADDONE".into(), 2)])).is_err());
        assert_eq!(before, fingerprint(&t.wb));
        let mut upgraded = d;
        upgraded.dependencies.insert("ADDONE".into(), 2);
        t.wb.publish_function(upgraded).unwrap();
        t.wb.pin_functions(&std::collections::BTreeMap::from([("ADDONE".into(), 2), ("DOUBLEADD".into(), 2)])).unwrap();
        let result = if kind == FunctionKind::Sql {
            scalar(&t.wb, "SELECT DOUBLEADD(3)").to_string()
        } else {
            t.wb.with_sheets(|s| s.set(0, &[sheet_input(0, 0, "=DOUBLEADD(3)")])).unwrap();
            cell_text(&t.wb, 0, 0, 0).unwrap()
        };
        assert_eq!(result, "10");
        t.reopen();
        t.wb.pin_functions(&std::collections::BTreeMap::from([("ADDONE".into(), 1), ("DOUBLEADD".into(), 1)])).unwrap();
        if kind == FunctionKind::Sql {
            assert_eq!(scalar(&t.wb, "SELECT DOUBLEADD(3)"), serde_json::json!(8));
        } else {
            assert_eq!(cell_text(&t.wb, 0, 0, 0).as_deref(), Some("8"));
        }
        let cycle = unary("ADDONE", kind, "DOUBLEADD(value)");
        assert!(t.wb.test_function(&cycle).unwrap_err().to_string().contains("recursive"));
        assert!(t.wb.publish_function(cycle).is_err());
    }
}

#[test]
fn sql_columns_refresh_and_null_decimal_date_semantics() {
    let mut t = TempWb::new();
    t.wb.publish_function(unary("UPVALUE", FunctionKind::Sql, "value+1")).unwrap();
    t.wb.create_table_from_query("numbers", "SELECT 2 AS n").unwrap();
    t.wb.add_formula_column("numbers", "next", "UPVALUE(n)").unwrap();
    let view = t.wb.open_view(spec("numbers")).unwrap().view_id;
    assert_eq!(all_rows(&t.wb, view), vec![vec![serde_json::json!(2), serde_json::json!(3)]]);
    t.wb.publish_function(unary("UPVALUE", FunctionKind::Sql, "value+2")).unwrap();
    t.wb.pin_functions(&std::collections::BTreeMap::from([("UPVALUE".into(), 2)])).unwrap();
    assert_eq!(all_rows(&t.wb, view), vec![vec![serde_json::json!(2), serde_json::json!(4)]]);
    assert_eq!(scalar(&t.wb, "SELECT UPVALUE(NULL)"), serde_json::Value::Null);
    t.wb.publish_function(unary("EXACTAMOUNT", FunctionKind::Sql, "CAST(value AS DECIMAL(18,2))+0.10::DECIMAL(18,2)")).unwrap();
    assert_eq!(scalar(&t.wb, "SELECT CAST(EXACTAMOUNT('0.20') AS VARCHAR)"), serde_json::json!("0.30"));
    t.wb.publish_function(unary("NEXTDAY", FunctionKind::Sql, "CAST(value AS DATE)+1")).unwrap();
    assert_eq!(scalar(&t.wb, "SELECT CAST(NEXTDAY('2024-02-28') AS VARCHAR)"), serde_json::json!("2024-02-29"));
}

#[test]
fn validation_conflicts_and_engine_boundaries_are_atomic() {
    let mut t = TempWb::new();
    for name in ["SUM", "abs", "A1", "rf_internal", "bad-name", ""] {
        assert!(t.wb.publish_function(unary(name, FunctionKind::Formula, "value+1")).is_err(), "{name}");
    }
    for source in ["random()", "now()", "current_date", "sum(value)", "row_number() OVER ()", "$value", "read_blob(value)"] {
        assert!(t.wb.publish_function(unary("BADSQL", FunctionKind::Sql, source)).is_err(), "{source}");
    }
    t.wb.publish_function(unary("SQLONLY", FunctionKind::Sql, "value+1")).unwrap();
    assert!(t.wb.publish_function(unary("SHEETONLY", FunctionKind::Formula, "SQLONLY(value)")).is_err());
    let mut bad = unary("DUPARGS", FunctionKind::Formula, "value");
    bad.arguments.push("VALUE".into());
    assert!(t.wb.publish_function(bad).is_err());
    assert_eq!(t.wb.functions().unwrap().len(), 1);
}

#[test]
fn failed_commit_rolls_back_registry_macros_and_sheet_model() {
    for kind in [FunctionKind::Formula, FunctionKind::Sql] {
        let mut t = TempWb::new();
        t.wb.publish_function(unary("STABLE", kind.clone(), "value+1")).unwrap();
        if kind == FunctionKind::Formula {
            t.wb.with_sheets(|s| s.set(0, &[sheet_input(0, 0, "=STABLE(3)")])).unwrap();
        }
        t.wb.publish_function(unary("STABLE", kind.clone(), "value+2")).unwrap();
        for point in ["history", "commit"] {
            let before = fingerprint(&t.wb);
            faults::fail_next(point);
            assert!(t.wb.pin_functions(&std::collections::BTreeMap::from([("STABLE".into(), 2)])).is_err());
            assert_eq!(t.wb.functions().unwrap()[0].pinned, 1);
            assert_eq!(before, fingerprint(&t.wb));
            if kind == FunctionKind::Sql {
                assert_eq!(scalar(&t.wb, "SELECT STABLE(3)"), serde_json::json!(4));
            } else {
                assert_eq!(cell_text(&t.wb, 0, 0, 0).as_deref(), Some("4"));
            }
        }
    }
}

#[test]
fn incompatible_runtime_and_missing_pins_refuse_reopen() {
    for field in ["adapter", "duckdb_version", "pinned"] {
        let mut t = TempWb::new();
        t.wb.publish_function(unary("STABLE", FunctionKind::Formula, "value+1")).unwrap();
        let mut data = serde_json::to_value(t.wb.functions().unwrap()).unwrap();
        if field == "pinned" {
            data[0][field] = serde_json::json!(99);
        } else {
            data[0]["versions"][0][field] = serde_json::json!("unsupported");
        }
        crate::storage::meta_set(&t.wb.conn, "function_registry", &data.to_string()).unwrap();
        let path = t.path();
        drop(t.wb);
        assert!(Workbook::open(&path).is_err(), "{field}");
    }
}

#[test]
fn workbook_scope_and_recreated_sheets_keep_exact_pins() {
    let mut t = TempWb::new();
    t.wb.publish_function(unary("STABLE", FunctionKind::Formula, "value+1")).unwrap();
    let other = Workbook::open_in_memory().unwrap();
    assert!(other.functions().unwrap().is_empty());
    t.wb.with_sheets(|s| s.delete(0)).unwrap();
    t.wb.with_sheets(|s| s.create("New")).unwrap();
    t.wb.with_sheets(|s| s.set(0, &[sheet_input(0, 0, "=STABLE(3)")])).unwrap();
    assert_eq!(cell_text(&t.wb, 0, 0, 0).as_deref(), Some("4"));
}

#[test]
fn formula_blanks_errors_dates_and_array_results_are_explicit() {
    let mut t = TempWb::new();
    for (name, source, input, expected) in [
        ("BLANKVALUE", "IF(ISBLANK(value),7,value)", "", "7"),
        ("SAFEVALUE", "IFERROR(1/value,99)", "0", "99"),
        ("ERRORVALUE", "1/value", "0", "#DIV/0!"),
        ("DECIMALVALUE", "ROUND(value+0.1,2)", "0.2", "0.3"),
        ("DATEVALUEYEAR", "YEAR(DATE(value,2,29))", "2024", "2024"),
        ("LITERALVALUE", "value", "=1+2", "=1+2"),
    ] {
        let mut d = unary(name, FunctionKind::Formula, source);
        d.tests.push(FunctionTest { arguments: vec![input.into()], expected: expected.into() });
        let got = t.wb.test_function(&d).unwrap();
        assert!(got[0].passed, "{name}: {got:?}");
        t.wb.publish_function(d).unwrap();
    }
    t.wb.publish_function(unary("PAIRVALUE", FunctionKind::Formula, "{1,2}")).unwrap();
    t.wb.with_sheets(|s| s.set(0, &[sheet_input(0, 0, "=PAIRVALUE(0)"), sheet_input(0, 1, "occupied")])).unwrap();
    // Capture actual pinned engine behavior explicitly, without promising Excel spill support.
    let text = cell_text(&t.wb, 0, 0, 0).unwrap();
    assert_eq!(text, "#SPILL!");
    t.wb.with_sheets(|s| s.set(0, &[sheet_input(0, 1, "")])).unwrap();
    assert_eq!(cell_text(&t.wb, 0, 0, 0).as_deref(), Some("1"));
    assert_eq!(cell_text(&t.wb, 0, 0, 1).as_deref(), Some("2"));
}

#[test]
fn xlsx_roundtrip_preserves_formula_but_not_registry() {
    let mut t = TempWb::new();
    t.wb.publish_function(unary("STABLE", FunctionKind::Formula, "value+1")).unwrap();
    t.wb.with_sheets(|s| s.set(0, &[sheet_input(0, 0, "=STABLE(3)")])).unwrap();
    let path = t.dir.path().join("functions.xlsx");
    t.wb.sheets().export_xlsx(&path).unwrap();
    let mut other = Workbook::open_in_memory().unwrap();
    other.import_any(&path, None).unwrap();
    assert_eq!(cell_text(&other, 0, 0, 0).as_deref(), Some("4"));
    assert!(other.functions().unwrap().is_empty(), "XLSX has no Alephgrid version registry");
    assert!(
        other.publish_function(unary("STABLE", FunctionKind::Formula, "value+2")).is_err(),
        "imported name is not silently overwritten"
    );
}

#[test]
fn storage_v3_migrates_before_function_publication() {
    let mut t = TempWb::new();
    crate::storage::meta_set(&t.wb.conn, "format", "3").unwrap();
    t.reopen();
    assert_eq!(t.wb.open_report().migrated_from, Some(3));
    t.wb.publish_function(unary("STABLE", FunctionKind::Sql, "value+1")).unwrap();
    t.reopen();
    assert_eq!(scalar(&t.wb, "SELECT STABLE(3)"), serde_json::json!(4));
}

#[test]
fn oversized_definitions_and_expanding_dependency_graphs_are_refused() {
    let mut t = TempWb::new();
    let mut oversized = unary("TOOBIG", FunctionKind::Formula, "value");
    oversized.source = " ".repeat(4097);
    assert!(t.wb.publish_function(oversized).is_err());
    let mut tests = unary("TOOMANY", FunctionKind::Formula, "value");
    tests.tests = vec![FunctionTest { arguments: vec!["1".into()], expected: "1".into() }; 33];
    assert!(t.wb.publish_function(tests).is_err());
    let mut missing = unary("MISSINGDEP", FunctionKind::Formula, "value");
    missing.dependencies.insert("UNKNOWN".into(), 1);
    assert!(t.wb.publish_function(missing).is_err());
    t.wb.publish_function(unary("EXPANDZERO", FunctionKind::Formula, "value+1")).unwrap();
    let mut previous = "EXPANDZERO".to_string();
    let mut refused = false;
    for suffix in ["ONE", "TWO", "THREE", "FOUR", "FIVE"] {
        let name = format!("EXPAND{suffix}");
        let source = vec![format!("{previous}(value)"); 16].join("+");
        let before = fingerprint(&t.wb);
        match t.wb.publish_function(unary(&name, FunctionKind::Formula, &source)) {
            Ok(_) => previous = name,
            Err(e) => {
                assert!(e.to_string().contains("too large"), "{e}");
                assert_eq!(before, fingerprint(&t.wb));
                refused = true;
                break;
            }
        }
    }
    assert!(refused, "expression expansion is bounded before evaluation");
}

// ---- U.06c: typed test inputs and expected values -------------------------------------
// Expected values follow docs/FUNCTIONS.md: a blank cell, empty text and an error value are
// different inputs; an entered "" is a blank cell and an entered NULL binds SQL NULL.

fn typed(name: &str, kind: FunctionKind, source: &str, cases: Vec<(TestValue, TestValue)>) -> FunctionDefinition {
    let mut d = unary(name, kind, source);
    d.tests = cases.into_iter().map(|(a, e)| FunctionTest { arguments: vec![a], expected: e }).collect();
    d
}

#[test]
fn formula_tests_tell_blank_cells_empty_text_and_errors_apart() {
    let t = TempWb::new();
    let source = r#"IFERROR(IF(ISBLANK(value),"blank",IF(ISTEXT(value),"text:"&value,IF(ISNUMBER(value),"number",value&""))),"error")"#;
    let d = typed("WHATIS", FunctionKind::Formula, source, vec![
        (TestValue::blank(), "blank".into()),
        (TestValue::text(""), "text:".into()),
        (TestValue::text("100"), "text:100".into()),
        ("100".into(), "number".into()),
        ("".into(), "blank".into()),
        (TestValue::error("#DIV/0!"), "error".into()),
        (TestValue::text("=1+2"), "text:=1+2".into()),
        (TestValue::text("say \"hi\""), "text:say \"hi\"".into()),
    ]);
    let got = t.wb.test_function(&d).unwrap();
    assert_eq!(got.iter().map(|r| r.actual.as_str()).collect::<Vec<_>>(), ["blank", "text:", "text:100", "number", "blank", "error", "text:=1+2", "text:say \"hi\""]);
    assert!(got.iter().all(|r| r.passed && r.kind == TestValueKind::Text), "{got:?}");
    // The exact error reaches the function.
    let na = typed("NAVALUE", FunctionKind::Formula, "value", vec![(TestValue::error("#N/A"), TestValue::error("#N/A")), (TestValue::error("#VALUE!"), TestValue::error("#VALUE!"))]);
    let got = t.wb.test_function(&na).unwrap();
    assert!(got.iter().all(|r| r.passed), "{got:?}");
    assert_eq!(got[1].kind, TestValueKind::Error);
    // IFNA is in the documented built-in set and tells #N/A from other errors.
    let ifna = typed("NAONLY", FunctionKind::Formula, r#"IFNA(value,"na")"#, vec![(TestValue::error("#N/A"), "na".into()), (TestValue::error("#VALUE!"), TestValue::error("#VALUE!"))]);
    let got = t.wb.test_function(&ifna).unwrap();
    assert!(got.iter().all(|r| r.passed), "{got:?}");
    // Only known error values.
    let bad = typed("BADERROR", FunctionKind::Formula, "value", vec![(TestValue::error("1+1"), "2".into())]);
    assert!(t.wb.test_function(&bad).unwrap_err().to_string().contains("not an error value"));
}

#[test]
fn typed_expected_values_compare_the_kind_of_value() {
    let t = TempWb::new();
    let d = typed("RATIO", FunctionKind::Formula, "1/value", vec![
        ("0".into(), TestValue::error("#DIV/0!")),
        ("0".into(), TestValue::text("#DIV/0!")),
        ("0".into(), "#DIV/0!".into()),
        ("4".into(), "0.25".into()),
        ("4".into(), TestValue::text("0.25")),
    ]);
    let got = t.wb.test_function(&d).unwrap();
    assert_eq!(got.iter().map(|r| r.passed).collect::<Vec<_>>(), [true, false, true, true, false]);
    assert_eq!(got[3].kind, TestValueKind::Number);
    let empty = typed("EMPTYTEXT", FunctionKind::Formula, r#"value&"""#, vec![(TestValue::text(""), TestValue::text("")), (TestValue::text(""), TestValue::blank())]);
    let got = t.wb.test_function(&empty).unwrap();
    assert_eq!(got.iter().map(|r| r.passed).collect::<Vec<_>>(), [true, false]);
}

#[test]
fn sql_tests_tell_null_text_and_the_null_marker_apart() {
    let t = TempWb::new();
    let d = typed("SQLWHAT", FunctionKind::Sql, "CASE WHEN value IS NULL THEN 'null' ELSE 'text:' || value END", vec![
        (TestValue::blank(), "null".into()),
        (TestValue::text("NULL"), "text:NULL".into()),
        ("NULL".into(), "null".into()),
        (TestValue::text(""), "text:".into()),
    ]);
    let got = t.wb.test_function(&d).unwrap();
    assert!(got.iter().all(|r| r.passed && r.kind == TestValueKind::Text), "{got:?}");
    let blank = typed("SQLBLANK", FunctionKind::Sql, "CASE WHEN value = 'x' THEN NULL ELSE value END", vec![("x".into(), TestValue::blank()), ("x".into(), TestValue::text("NULL")), ("x".into(), "NULL".into())]);
    let got = t.wb.test_function(&blank).unwrap();
    assert_eq!(got.iter().map(|r| r.passed).collect::<Vec<_>>(), [true, false, true]);
    assert_eq!(got[0].kind, TestValueKind::Blank);
    let error = typed("SQLERROR", FunctionKind::Sql, "value", vec![(TestValue::error("#N/A"), "x".into())]);
    assert!(t.wb.test_function(&error).unwrap_err().to_string().contains("error values"));
}

#[test]
fn stored_text_tests_still_load_and_typed_tests_round_trip() {
    // A definition written before typed values: every value is text.
    let old = r#"{"name":"OLD","kind":"formula","arguments":["a"],"source":"a","documentation":"","tests":[{"arguments":[""],"expected":"0"}]}"#;
    let d: FunctionDefinition = serde_json::from_str(old).unwrap();
    assert_eq!(d.tests[0].arguments[0], TestValue::Entered(String::new()));
    let mut t = TempWb::new();
    let typed = typed("KEEPS", FunctionKind::Formula, "IF(ISBLANK(value),1,2)", vec![(TestValue::blank(), "1".into()), (TestValue::text(""), "2".into())]);
    t.wb.publish_function(typed).unwrap();
    t.reopen();
    let stored = &t.wb.functions().unwrap()[0].versions[0].definition.tests;
    assert_eq!(stored[0].arguments[0], TestValue::blank());
    assert_eq!(stored[1].arguments[0], TestValue::text(""));
    assert_eq!(serde_json::to_value(&stored[1]).unwrap(), serde_json::json!({ "arguments": [{ "type": "text", "value": "" }], "expected": "2" }));
}

#[test]
fn cancelled_tests_stop() {
    let t = TempWb::new();
    let d = typed("STOPS", FunctionKind::Formula, "value", vec![("1".into(), "1".into())]);
    let cancel = std::sync::atomic::AtomicBool::new(true);
    let err = t.wb.reader().unwrap().test_function(&d, &cancel).unwrap_err();
    assert!(err.to_string().contains("cancelled"), "{err}");
}

// ---- U.06e: what activating a version affects -----------------------------------------

#[test]
fn usage_lists_cells_columns_and_dependent_functions() {
    let mut t = TempWb::new();
    t.wb.publish_function(unary("BUMP", FunctionKind::Formula, "value+1")).unwrap();
    t.wb.publish_function(unary("TWICEBUMP", FunctionKind::Formula, "BUMP(value)*2")).unwrap();
    t.wb.publish_function(unary("SQLBUMP", FunctionKind::Sql, "value + 1")).unwrap();
    t.wb.with_sheets(|s| {
        s.set(0, &[
            sheet_input(1, 1, "=BUMP(A1)"),
            sheet_input(4, 2, "=SUM(1, bump (2))"),
            sheet_input(5, 2, "=\"BUMP(1)\""),
            sheet_input(6, 2, "=BUMPY(1)"),
            sheet_input(7, 2, "BUMP(1)"),
        ])
    })
    .unwrap();
    let table = t.wb.create_demo_table(5).unwrap().name;
    t.wb.add_formula_column(&table, "bumped", "SQLBUMP(fare_usd)").unwrap();
    let u = t.wb.function_usage("bump").unwrap();
    assert_eq!(u.name, "BUMP");
    assert_eq!(u.cell_count, 2);
    assert_eq!(u.cells.iter().map(|c| c.cell.as_str()).collect::<Vec<_>>(), ["B2", "C5"]);
    assert!(u.cells[0].formula.eq_ignore_ascii_case("=BUMP(A1)"), "{:?}", u.cells[0]);
    assert!(u.columns.is_empty());
    assert_eq!(u.dependents.len(), 1);
    assert_eq!((u.dependents[0].name.as_str(), u.dependents[0].version, u.dependents[0].uses), ("TWICEBUMP", 1, 1));
    let s = t.wb.reader().unwrap().function_usage("SQLBUMP").unwrap();
    assert_eq!(s.columns.len(), 1);
    assert_eq!((s.columns[0].table.as_str(), s.columns[0].column.as_str()), (table.as_str(), "bumped"));
    assert!(t.wb.function_usage("NOPE").is_err());
    // No Python runtime here, and nothing turned off.
    assert!(u.runtime.is_none() && u.python_unavailable.is_none());
}
