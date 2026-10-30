use super::*;
use serde_json::json;
fn fixture() -> TempWb {
    let mut t = TempWb::new();
    t.wb.create_table_from_query("sales","SELECT * FROM (VALUES (1,'north',10.00::DECIMAL(12,2)),(1,'north',20.00),(2,'south',30.00),(3,NULL,NULL),(NULL,'south',5.00)) t(id,region,amount)").unwrap();
    t.wb.create_table_from_query("people", "SELECT * FROM (VALUES (1,'Ada'),(1,'Grace'),(2,'Lin'),(NULL,'Null person')) t(id,name)")
        .unwrap();
    t
}
fn summary() -> AnalysisSpec {
    AnalysisSpec::Summary {
        table: "sales".into(),
        groups: vec!["region".into()],
        aggregate: Aggregate::Sum,
        value: Some("amount".into()),
        pivot: None,
    }
}
#[test]
fn summary_pivot_profile_and_join_have_independent_oracles() {
    let t = fixture();
    let r = t.wb.analyze(&summary()).unwrap();
    assert_eq!(
        r.result.rows,
        vec![vec![json!("north"), json!("30.00")], vec![json!("south"), json!("35.00")], vec![json!(null), json!(null)]]
    );
    let pivot = AnalysisSpec::Summary {
        table: "sales".into(),
        groups: vec![],
        aggregate: Aggregate::Count,
        value: None,
        pivot: Some(Pivot { column: "region".into(), values: vec![Some("north".into()), Some("south".into()), None] }),
    };
    assert_eq!(t.wb.analyze(&pivot).unwrap().result.rows, vec![vec![json!(2), json!(2), json!(1)]]);
    let profile = AnalysisSpec::Profile { table: "sales".into(), columns: vec!["id".into()] };
    assert_eq!(
        t.wb.analyze(&profile).unwrap().result.rows,
        vec![vec![json!("id"), json!("INTEGER"), json!(5), json!(1), json!(3), json!(1)]]
    );
    for (join, count) in [(JoinKind::Inner, 5), (JoinKind::Left, 7)] {
        let spec = AnalysisSpec::Join { left: "sales".into(), right: "people".into(), left_key: "id".into(), right_key: "id".into(), join };
        let r = t.wb.analyze(&spec).unwrap();
        assert_eq!(r.result.rows.len(), count);
        assert_eq!(r.diagnostics.unwrap().rows[0], vec![json!(1), json!(1), json!(count)]);
    }
}
#[test]
fn saved_analysis_refreshes_and_snapshot_is_bounded() {
    let mut t = fixture();
    let a = t.wb.save_analysis(None, "By region", summary(), true).unwrap();
    t.reopen();
    assert_eq!(t.wb.analyses().unwrap()[0].id, a.id);
    t.wb.sql_change("UPDATE sales SET amount=40 WHERE id=2", &[]).unwrap();
    let r = t.wb.analyze(&t.wb.analyses().unwrap()[0].spec).unwrap();
    assert_eq!(r.result.rows[1][1], json!("45.00"));
    t.wb.materialize_analysis("summary", &a.spec).unwrap();
    assert_eq!(scalar(&t.wb, "SELECT count(*) FROM summary"), json!(3));
    t.wb.create_table_from_query("big", "SELECT i FROM range(11000) t(i)").unwrap();
    let spec =
        AnalysisSpec::Summary { table: "big".into(), groups: vec!["i".into()], aggregate: Aggregate::Count, value: None, pivot: None };
    let r = t.wb.analyze(&spec).unwrap().result;
    assert_eq!(r.rows.len(), OUTPUT_ROWS);
    assert!(r.truncated);
    assert!(t.wb.materialize_analysis("too_big", &spec).is_err());
    assert!(!t.wb.list_tables().unwrap().iter().any(|t| t.name == "too_big"));
    t.wb.delete_analysis(&a.id).unwrap();
    assert!(t.wb.analyses().unwrap().is_empty());
}
#[test]
fn replace_preview_apply_undo_restart_and_conflicts() {
    let mut t = fixture();
    let spec = ReplaceSpec { table: "sales".into(), column: "region".into(), find: "north".into(), replacement: "east'\n".into() };
    let p = t.wb.replace_preview(&spec).unwrap();
    assert_eq!(p.matches, 2);
    assert_eq!(p.sample.rows[0], vec![json!("north"), json!("east'\n")]);
    let applied = t.wb.replace_apply(&spec, p.sample.revision).unwrap();
    t.reopen();
    assert_eq!(scalar(&t.wb, "SELECT count(*) FROM sales WHERE region='north'"), json!(0));
    t.wb.replace_undo(applied.undo_revision).unwrap();
    assert_eq!(scalar(&t.wb, "SELECT count(*) FROM sales WHERE region='north'"), json!(2));
    assert!(t.wb.replace_apply(&spec, p.sample.revision).is_err());
    let p = t.wb.replace_preview(&spec).unwrap();
    let applied = t.wb.replace_apply(&spec, p.sample.revision).unwrap();
    t.wb.sql_change("UPDATE sales SET region='new' WHERE id=2", &[]).unwrap();
    assert!(t.wb.replace_undo(applied.undo_revision).is_err());
    assert_eq!(scalar(&t.wb, "SELECT region FROM sales WHERE id=2"), json!("new"));
}
#[test]
fn validation_and_faults_leave_state_unchanged() {
    let mut t = fixture();
    let invalid = AnalysisSpec::Join {
        left: "sales".into(),
        right: "people".into(),
        left_key: "amount".into(),
        right_key: "id".into(),
        join: JoinKind::Inner,
    };
    assert!(t.wb.analyze(&invalid).is_err());
    let internal = AnalysisSpec::Profile { table: "_rf_meta".into(), columns: vec!["value".into()] };
    assert!(t.wb.analyze(&internal).is_err());
    let malicious = AnalysisSpec::Summary {
        table: "sales; DROP TABLE sales".into(),
        groups: vec![],
        aggregate: Aggregate::Count,
        value: None,
        pivot: None,
    };
    assert!(t.wb.analyze(&malicious).is_err());
    let spec = ReplaceSpec { table: "sales".into(), column: "region".into(), find: "north".into(), replacement: "east".into() };
    for point in ["history", "commit"] {
        let p = t.wb.replace_preview(&spec).unwrap();
        let before = fingerprint(&t.wb);
        faults::fail_next(point);
        assert!(t.wb.replace_apply(&spec, p.sample.revision).is_err());
        assert_eq!(before, fingerprint(&t.wb));
    }
}
#[test]
fn presentation_persists_and_bounds_are_enforced() {
    let mut t = fixture();
    let p = Presentation {
        hidden: vec!["id".into()],
        frozen: 1,
        formats: std::collections::BTreeMap::from([(
            "amount".into(),
            DisplayFormat { kind: DisplayKind::Number, decimals: 2, locale: "de-DE".into(), timezone: "UTC".into() },
        )]),
        rules: vec![ColorRule { column: "amount".into(), condition: Condition::Greater, value: 20.0, color: "green".into() }],
    };
    t.wb.save_presentation("sales", p).unwrap();
    t.reopen();
    assert_eq!(t.wb.presentation("sales").unwrap().frozen, 1);
    let mut p = t.wb.presentation("sales").unwrap();
    p.frozen = 9;
    assert!(t.wb.save_presentation("sales", p).is_err());
    let mut p = t.wb.presentation("sales").unwrap();
    p.rules[0].color = "url(hostile)".into();
    assert!(t.wb.save_presentation("sales", p).is_err());
}

#[test]
fn empty_sources_skew_and_quoted_identifiers() {
    let mut t = fixture();
    t.wb.rename_table("sales", "l").unwrap();
    t.wb.rename_table("people", "r").unwrap();
    let spec =
        AnalysisSpec::Join { left: "l".into(), right: "r".into(), left_key: "id".into(), right_key: "id".into(), join: JoinKind::Left };
    assert_eq!(t.wb.analyze(&spec).unwrap().result.rows.len(), 7);
    t.wb.create_table_from_query("odd name", "SELECT 'x' AS \"a\"\"b\", 3 AS n WHERE false").unwrap();
    let empty = AnalysisSpec::Summary { table: "odd_name".into(), groups: vec![], aggregate: Aggregate::Count, value: None, pivot: None };
    assert_eq!(t.wb.analyze(&empty).unwrap().result.rows, vec![vec![json!(0)]]);
    let p = AnalysisSpec::Profile { table: "odd_name".into(), columns: vec!["a\"b".into()] };
    assert_eq!(t.wb.analyze(&p).unwrap().result.rows[0][2], json!(0));
    t.wb.create_table_from_query("skew", "SELECT 1 AS id FROM range(1000)").unwrap();
    let join = AnalysisSpec::Join {
        left: "skew".into(),
        right: "skew".into(),
        left_key: "id".into(),
        right_key: "id".into(),
        join: JoinKind::Inner,
    };
    let result = t.wb.analyze(&join).unwrap();
    assert_eq!(result.diagnostics.unwrap().rows[0][2], json!(1000000));
    assert_eq!(result.result.rows.len(), 500);
    assert!(result.result.truncated);
}
#[test]
fn replacement_limits_and_undo_failure_are_atomic() {
    let mut t = fixture();
    t.wb.create_table_from_query("many", "SELECT 'aaa' AS s FROM range(10001)").unwrap();
    let s = ReplaceSpec { table: "many".into(), column: "s".into(), find: "a".into(), replacement: "x".into() };
    let before = fingerprint(&t.wb);
    assert!(t.wb.replace_apply(&s, t.wb.revision().unwrap()).is_err());
    assert_eq!(before, fingerprint(&t.wb));
    t.wb.create_table_from_query("expansion", "SELECT repeat('a',2000) AS s").unwrap();
    let s = ReplaceSpec { table: "expansion".into(), column: "s".into(), find: "a".into(), replacement: "x".repeat(4096) };
    assert!(t.wb.replace_apply(&s, t.wb.revision().unwrap()).unwrap_err().to_string().contains("4 MiB"));
    let s = ReplaceSpec { table: "sales".into(), column: "region".into(), find: "north".into(), replacement: "east".into() };
    let applied = t.wb.replace_apply(&s, t.wb.revision().unwrap()).unwrap();
    let before = fingerprint(&t.wb);
    faults::fail_next("commit");
    assert!(t.wb.replace_undo(applied.undo_revision).is_err());
    assert_eq!(before, fingerprint(&t.wb));
    t.wb.replace_undo(applied.undo_revision).unwrap();
}
#[test]
fn schema_drift_is_explicit_and_presentation_keeps_a_visible_column() {
    let mut t = fixture();
    let a = t.wb.save_analysis(None, "Summary", summary(), true).unwrap();
    let b = t.wb.save_analysis(None, "Second", summary(), false).unwrap();
    t.wb.delete_analysis(&b.id).unwrap();
    let c = t.wb.save_analysis(None, "Third", summary(), false).unwrap();
    assert_ne!(b.id, c.id);
    t.wb.save_presentation("sales", Presentation { hidden: vec!["id".into(), "region".into()], ..Default::default() }).unwrap();
    t.wb.drop_column("sales", "amount").unwrap();
    assert!(t.wb.presentation("sales").unwrap().hidden.is_empty());
    assert!(t.wb.analyze(&a.spec).unwrap_err().to_string().contains("unknown column"));
}
