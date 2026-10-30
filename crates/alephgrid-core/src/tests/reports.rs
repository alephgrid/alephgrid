//! F.11: pivot reports, drill-down, charts and dashboards. Expected values by hand from:
//! orders (id, region, cid, amount) = (1, West, 1, 100), (2, East, 2, 50), (3, West, 2, 25),
//! (4, East, 1, 75), (5, West, blank, 10); customers 1 Gold, 2 Silver; `tier` looks the
//! customer up ("none" without one).
use super::*;
use crate::reports::*;
use serde_json::json;

fn shop() -> TempWb {
    let mut t = TempWb::new();
    t.wb.create_table_from_query("customers", "SELECT * FROM (VALUES (1, 'Gold'), (2, 'Silver')) v(id, tier)").unwrap();
    t.wb.create_table_from_query("orders", "SELECT * FROM (VALUES (1, 'West', 1, 100), (2, 'East', 2, 50), (3, 'West', 2, 25), (4, 'East', 1, 75), (5, 'West', NULL, 10)) v(id, region, cid, amount)").unwrap();
    t.wb.add_formula_column_as("orders", "tier", "=XLOOKUP([@cid],customers[id],customers[tier],\"none\")", FormulaLanguage::Sheet).unwrap();
    t
}

fn report(filters: Vec<Filter>, totals: bool) -> AnalysisSpec {
    AnalysisSpec::Report {
        table: "orders".into(),
        groups: vec!["region".into(), "tier".into()],
        measures: vec![
            Measure { aggregate: Aggregate::Sum, value: Some("amount".into()), name: "revenue".into() },
            Measure { aggregate: Aggregate::Count, value: None, name: "orders".into() },
        ],
        calculated: vec![Calculated { name: "average".into(), formula: "=[@revenue]/[@orders]".into() }],
        totals,
        filters,
        pivot: None,
    }
}

fn rows(r: &AnalysisResult) -> Vec<String> {
    r.result.rows.iter().map(|row| row.iter().map(|v| match v { Value::String(s) => s.clone(), v => v.to_string() }).collect::<Vec<_>>().join("|")).collect()
}

#[test]
fn reports_have_several_measures_calculated_measures_and_totals() {
    let t = shop();
    let r = t.wb.analyze(&report(vec![], true)).unwrap();
    assert_eq!(r.result.columns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["region", "tier", "revenue", "orders", "average", "row_kind"]);
    // Detail rows, then each region's subtotal, then the grand total; tiers by code point.
    assert_eq!(rows(&r), [
        "East|Gold|75.0|1.0|75.0|row", "East|Silver|50.0|1.0|50.0|row", "East|null|125.0|2.0|62.5|subtotal",
        "West|Gold|100.0|1.0|100.0|row", "West|Silver|25.0|1.0|25.0|row", "West|none|10.0|1.0|10.0|row", "West|null|135.0|3.0|45.0|subtotal",
        "null|null|260.0|5.0|52.0|total",
    ]);
    // Filters narrow the rows first.
    let west = Filter { column: "region".into(), op: FilterOp::Eq, value: json!("West") };
    let r = t.wb.analyze(&report(vec![west], true)).unwrap();
    assert_eq!(rows(&r).last().unwrap(), "null|null|135.0|3.0|45.0|total");
    // Without totals: detail rows only.
    assert_eq!(t.wb.analyze(&report(vec![], false)).unwrap().result.rows.len(), 5);
    // A pivot: one column per measure and value.
    let pivot = AnalysisSpec::Report {
        table: "orders".into(), groups: vec!["region".into()],
        measures: vec![Measure { aggregate: Aggregate::Sum, value: Some("amount".into()), name: "revenue".into() }],
        calculated: vec![], totals: true, filters: vec![],
        pivot: Some(Pivot { column: "tier".into(), values: vec![Some("Gold".into()), Some("Silver".into())] }),
    };
    let r = t.wb.analyze(&pivot).unwrap();
    assert_eq!(r.result.columns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["region", "revenue: Gold", "revenue: Silver", "row_kind"]);
    assert_eq!(rows(&r), ["East|75|50|row", "West|100|25|row", "null|175|75|total"]);
    // Refused with reasons.
    let bad = |spec: AnalysisSpec| t.wb.analyze(&spec).unwrap_err().to_string();
    let mut dup = report(vec![], true);
    if let AnalysisSpec::Report { calculated, .. } = &mut dup { calculated.push(Calculated { name: "revenue".into(), formula: "=1".into() }); }
    assert!(bad(dup).contains("distinct"));
    let mut wrong = report(vec![], true);
    if let AnalysisSpec::Report { calculated, .. } = &mut wrong { calculated[0].formula = "=[@nope]*2".into(); }
    assert!(bad(wrong).contains("calculated measure \"average\""));
}

#[test]
fn drilling_into_a_row_opens_the_rows_behind_it() {
    let mut t = shop();
    let spec = report(vec![], true);
    let r = t.wb.analyze(&spec).unwrap();
    let ids = |wb: &mut Workbook, view: ViewSpec| {
        let v = wb.open_view(view).unwrap();
        all_rows(wb, v.view_id).iter().map(|r| r[0].to_string()).collect::<Vec<_>>()
    };
    let west_silver = crate::reports::drill(&spec, &r.result.rows[4]).unwrap();
    assert_eq!(ids(&mut t.wb, west_silver), ["3"]);
    let west = crate::reports::drill(&spec, &r.result.rows[6]).unwrap();
    assert_eq!(ids(&mut t.wb, west), ["1", "3", "5"]);
    let total = crate::reports::drill(&spec, &r.result.rows[7]).unwrap();
    assert_eq!(ids(&mut t.wb, total).len(), 5);
}

#[test]
fn dashboards_place_tiles_share_slicers_and_persist() {
    let mut t = shop();
    let a = t.wb.save_analysis_as(None, "Revenue", report(vec![], true), true, ChartKind::Line).unwrap();
    let b = t.wb.save_analysis(None, "By region", AnalysisSpec::Summary { table: "orders".into(), groups: vec!["tier".into()], aggregate: Aggregate::Count, value: None, pivot: None }, true).unwrap();
    assert_eq!(t.wb.analyses().unwrap()[0].chart_kind, ChartKind::Line);
    let tile = |id: &str, x, y| Tile { analysis: id.into(), x, y, w: 6, h: 8 };
    let east = Slicer { table: "orders".into(), column: "region".into(), values: vec![Some("East".into())] };
    let d = t.wb.save_dashboard(None, "Sales", vec![tile(&a.id, 0, 0), tile(&b.id, 6, 0)], vec![east]).unwrap();
    let out = t.wb.dashboard_results(&d.id).unwrap();
    // The slicer applies to the report; the summary counts tiers over every region (it is
    // grouped by the looked-up tier, a formula column).
    assert_eq!(rows(out.tiles[0].result.as_ref().unwrap()).last().unwrap(), "null|null|125.0|2.0|62.5|total");
    assert_eq!(rows(out.tiles[1].result.as_ref().unwrap()), ["Gold|2", "Silver|2", "none|1"]);
    // Limits and collisions.
    let e = t.wb.save_dashboard(None, "x", vec![tile(&a.id, 0, 0), tile(&b.id, 3, 4)], vec![]).unwrap_err().to_string();
    assert!(e.contains("overlap"), "{e}");
    let e = t.wb.save_dashboard(None, "x", vec![tile(&a.id, 8, 0)], vec![]).unwrap_err().to_string();
    assert!(e.contains("does not fit"), "{e}");
    assert!(t.wb.save_dashboard(None, "x", vec![tile("a999", 0, 0)], vec![]).unwrap_err().to_string().contains("unknown analysis"));
    // A deleted analysis leaves its tile saying so; reopening keeps everything.
    t.wb.delete_analysis(&b.id).unwrap();
    t.reopen();
    let out = t.wb.dashboard_results(&d.id).unwrap();
    assert_eq!(out.tiles[1].error.as_deref(), Some("this analysis was deleted"));
    assert_eq!(t.wb.dashboards().unwrap()[0].slicers.len(), 1);
    t.wb.delete_dashboard(&d.id).unwrap();
    assert!(t.wb.dashboards().unwrap().is_empty());
}
