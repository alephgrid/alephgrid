//! F.12: validation (including bypass attempts), transforms as derived tables, and formula
//! inspection. Expected values by hand from the rows below.
use super::*;
use crate::tools::*;

fn people() -> TempWb {
    let mut t = TempWb::new();
    t.wb.create_table_from_query("people", "SELECT * FROM (VALUES (1, ' Ann  Lee ', 'gold', '42', 'a;b'), (2, 'Bo', 'silver', 'x', 'c'), (3, 'Bo', 'bronze', '7', 'd;e;f'), (4, 'Cy', NULL, NULL, NULL)) v(id, name, tier, age, tags)").unwrap();
    t
}

#[test]
fn validation_rules_hold_on_every_write_path() {
    let mut t = people();
    let report = t.wb.set_validations("people", vec![
        Validation { column: "tier".into(), rule: Rule::OneOf { values: vec!["gold".into(), "silver".into()] } },
        Validation { column: "id".into(), rule: Rule::Between { min: Some(1.0), max: Some(100.0) } },
        Validation { column: "name".into(), rule: Rule::Required },
    ]).unwrap();
    // bronze breaks the list; ids and names are fine.
    assert_eq!(report.violations, [1, 0, 0]);
    let v = t.wb.open_view(spec("people")).unwrap();
    assert!(t.wb.edit(v.view_id, &[edit(0, "tier", "platinum")]).unwrap_err().to_string().contains("one of \"gold\", \"silver\""));
    assert!(t.wb.edit(v.view_id, &[edit(0, "id", "500")]).unwrap_err().to_string().contains("≤ 100"));
    assert!(t.wb.edit(v.view_id, &[CellEdit { row: 0, column: "name".into(), value: None, record: None, base: None }]).unwrap_err().to_string().contains("not blank"));
    // Appending a row by typing below the last one is an edit too.
    assert!(t.wb.edit(v.view_id, &[edit(4, "id", "0")]).is_err());
    t.wb.edit(v.view_id, &[edit(0, "tier", "silver")]).unwrap();
    // SQL changes cannot bypass the rules; one that keeps them is fine.
    let before = fingerprint(&t.wb);
    let e = t.wb.sql_change("UPDATE people SET tier = 'tin' WHERE id = 2", &[]).unwrap_err().to_string();
    assert!(e.contains("breaks the validation of people.\"tier\""), "{e}");
    assert_eq!(fingerprint(&t.wb), before);
    t.wb.sql_change("UPDATE people SET tier = 'gold' WHERE id = 2", &[]).unwrap();
    // Rules survive reopening.
    t.reopen();
    assert_eq!(t.wb.validations("people").unwrap().len(), 3);
}

#[test]
fn transforms_preview_and_create_derived_tables() {
    let mut t = people();
    let cells = |p: &TransformPreview, c: usize| p.sample.rows.iter().map(|r| r[c].as_str().unwrap_or("-").to_string()).collect::<Vec<_>>();
    // Deduplicate by name: the second Bo goes.
    let dedupe = Transform::Dedupe { keys: vec!["name".into()] };
    let p = t.wb.preview_transform("people", &dedupe).unwrap();
    assert_eq!((p.rows, p.affected), (3, 1));
    let trim = Transform::Trim { column: "name".into(), inner: true };
    let p = t.wb.preview_transform("people", &trim).unwrap();
    assert_eq!((cells(&p, 1)[0].as_str(), p.affected), ("Ann Lee", 1));
    let split = Transform::Split { column: "tags".into(), delimiter: ";".into(), parts: 2 };
    let p = t.wb.preview_transform("people", &split).unwrap();
    assert_eq!(p.sample.columns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["id", "name", "tier", "age", "tags", "tags_1", "tags_2"]);
    assert_eq!((cells(&p, 6), p.affected), (vec!["b".to_string(), "-".into(), "e".into(), "-".into()], 1), "d;e;f has more than 2 parts");
    // Convert: "x" does not read as a number.
    let convert = Transform::Convert { column: "age".into(), to: "bigint".into() };
    let p = t.wb.preview_transform("people", &convert).unwrap();
    assert_eq!(p.affected, 1);
    // Applying creates a table; the source is unchanged.
    let before = scalar(&t.wb, "SELECT count(*) FROM people");
    let info = t.wb.apply_transform("people", &dedupe, "people_unique").unwrap();
    assert_eq!(info.row_count, 3);
    assert_eq!(scalar(&t.wb, "SELECT count(*) FROM people"), before);
    let info = t.wb.apply_transform("people", &convert, "people_typed").unwrap();
    assert_eq!(info.row_count, 4);
    assert_eq!(t.wb.columns("people_typed").unwrap().iter().find(|c| c.name == "age").unwrap().sql_type, "BIGINT");
    assert!(t.wb.preview_transform("people", &Transform::Split { column: "tags".into(), delimiter: "".into(), parts: 2 }).is_err());
}

#[test]
fn formula_inspection_says_what_a_formula_reads_and_who_reads_it() {
    let mut t = people();
    t.wb.create_table_from_query("tiers", "SELECT * FROM (VALUES ('gold', 0.2), ('silver', 0.1)) v(tier, rate)").unwrap();
    t.wb.add_formula_column_as("people", "rate", "=XLOOKUP([@tier],tiers[tier],tiers[rate])", FormulaLanguage::Sheet).unwrap();
    t.wb.add_formula_column_as("people", "pct", "=[@rate]*100", FormulaLanguage::Sheet).unwrap();
    let i = t.wb.inspect_formula("people", "rate").unwrap();
    assert_eq!(i.reads_columns, ["tier"]);
    assert_eq!(i.reads_tables, ["tiers[tier]", "tiers[rate]"]);
    assert_eq!(i.plan, ["lookup join"]);
    assert_eq!(i.read_by, ["people.pct"]);
    // bronze and the blank tier have no rate.
    assert_eq!(i.errors, [("#N/A".to_string(), 2)]);
    assert_eq!(t.wb.inspect_formula("people", "pct").unwrap().plan, ["row expression"]);
    assert!(t.wb.inspect_formula("people", "tier").is_err());
}
