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
