//! Property and state-machine tests. A table is driven by random operation sequences and
//! compared after every step with a plain `Vec` reference model. Failing cases are shrunk
//! and their seeds saved under `proptest-regressions/`; set `PROPTEST_CASES` for longer runs.

use super::*;
use proptest::prelude::*;
use serde_json::json;

#[derive(Debug, Clone)]
enum Op {
    /// Edit `(row, column)`; `None` clears, `Some(Err)` is an invalid integer.
    Edit(Vec<(u64, usize, Option<Result<i32, String>>)>),
    Insert(u64, u64),
    Delete(u64, u64),
    Reopen,
    SortedCheck,
}

type Row = [Option<String>; 2];

fn value_strategy() -> impl Strategy<Value = Option<Result<i32, String>>> {
    prop_oneof![
        3 => any::<i32>().prop_map(|v| Some(Ok(v))),
        1 => Just(None),
        1 => "[a-z]{1,3}".prop_map(|s| Some(Err(s))),
    ]
}

fn op_strategy() -> impl Strategy<Value = Op> {
    prop_oneof![
        6 => prop::collection::vec((0u64..14, 0usize..2, value_strategy()), 0..5).prop_map(Op::Edit),
        2 => (0u64..12, 0u64..4).prop_map(|(a, n)| Op::Insert(a, n)),
        2 => (0u64..12, 0u64..4).prop_map(|(a, n)| Op::Delete(a, n)),
        1 => Just(Op::Reopen),
        1 => Just(Op::SortedCheck),
    ]
}

/// Column 0 is INTEGER (`n`), column 1 is VARCHAR (`s`).
fn to_text(column: usize, v: &Result<i32, String>) -> String {
    match (column, v) {
        (_, Ok(i)) => i.to_string(),
        (_, Err(s)) => s.clone(),
    }
}

fn expected_json(model: &[Row]) -> Vec<Vec<Value>> {
    model
        .iter()
        .map(|r| {
            vec![
                r[0].as_ref().map(|s| json!(s.parse::<i64>().unwrap())).unwrap_or(Value::Null),
                r[1].as_ref().map(|s| json!(s)).unwrap_or(Value::Null),
            ]
        })
        .collect()
}

fn apply_model(model: &mut Vec<Row>, op: &Op) -> bool {
    match op {
        Op::Edit(edits) => {
            if edits.is_empty() {
                return true;
            }
            // Only the last value written to a cell counts.
            let last = |i: usize| !edits[i + 1..].iter().any(|e| e.0 == edits[i].0 && e.1 == edits[i].1);
            let invalid = (0..edits.len()).any(|i| last(i) && edits[i].1 == 0 && matches!(edits[i].2, Some(Err(_))));
            if invalid {
                return false;
            }
            let max = edits.iter().map(|e| e.0).max().unwrap();
            while (model.len() as u64) <= max {
                model.push([None, None]);
            }
            for (r, c, v) in edits {
                model[*r as usize][*c] = v.as_ref().map(|v| to_text(*c, v));
            }
            true
        }
        Op::Insert(at, n) => {
            let at = (*at as usize).min(model.len());
            for _ in 0..*n {
                model.insert(at, [None, None]);
            }
            true
        }
        Op::Delete(start, n) => {
            let start = (*start as usize).min(model.len());
            let end = (start + *n as usize).min(model.len());
            model.drain(start..end);
            true
        }
        Op::Reopen | Op::SortedCheck => true,
    }
}

fn run(ops: Vec<Op>) -> Result<(), TestCaseError> {
    let mut t = TempWb::new();
    t.wb.create_table_from_query("t", "SELECT 1::INTEGER AS n, 'x'::VARCHAR AS s WHERE false").unwrap();
    let mut view = t.wb.open_view(spec("t")).unwrap().view_id;
    let mut model: Vec<Row> = Vec::new();
    for op in &ops {
        let expected_ok = apply_model(&mut model.clone(), op);
        let result = match op {
            Op::Edit(edits) => {
                let edits: Vec<CellEdit> = edits
                    .iter()
                    .map(|(r, c, v)| CellEdit {
                        row: *r,
                        column: if *c == 0 { "n" } else { "s" }.into(),
                        value: v.as_ref().map(|v| to_text(*c, v)),
                    })
                    .collect();
                t.wb.edit(view, &edits).map(|_| ())
            }
            Op::Insert(at, n) => t.wb.insert_rows(view, (*at).min(model.len() as u64), *n).map(|_| ()),
            Op::Delete(start, n) => t.wb.delete_rows(view, *start, *n).map(|_| ()),
            Op::Reopen => {
                t.reopen();
                view = t.wb.open_view(spec("t")).unwrap().view_id;
                Ok(())
            }
            Op::SortedCheck => {
                let mut s = spec("t");
                s.sort.push(SortKey { column: "n".into(), desc: false });
                let sorted = t.wb.open_view(s).unwrap().view_id;
                let mut values: Vec<Option<i64>> = model.iter().map(|r| r[0].as_ref().map(|v| v.parse().unwrap())).collect();
                // NULLS LAST, ties in record order: a stable sort of the model is the oracle.
                values.sort_by_key(|v| (v.is_none(), *v));
                let got: Vec<Option<i64>> = all_rows(&t.wb, sorted).iter().map(|r| r[0].as_i64()).collect();
                t.wb.close_view(sorted);
                prop_assert_eq!(got, values);
                Ok(())
            }
        };
        prop_assert_eq!(result.is_ok(), expected_ok, "op {:?} result {:?}", op, result.err());
        if expected_ok {
            apply_model(&mut model, op);
        }
        let info = t.wb.view_info(view).unwrap();
        prop_assert_eq!(info.row_count, model.len() as u64);
        prop_assert!(info.can_append, "row ids must stay dense");
        prop_assert_eq!(all_rows(&t.wb, view), expected_json(&model), "after {:?}", op);
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 48, ..ProptestConfig::default() })]

    #[test]
    fn table_matches_reference_model(ops in prop::collection::vec(op_strategy(), 1..25)) {
        run(ops)?;
    }

    #[test]
    fn formula_translation_never_panics_and_keeps_literals(s in "\\PC{0,40}") {
        let out = crate::sql::formula_to_sql(&s);
        // A formula without brackets or a leading '=' is passed through unchanged.
        if !s.contains('[') && !s.trim().starts_with('=') {
            prop_assert_eq!(out, s.trim());
        }
    }

    #[test]
    fn sanitized_table_names_are_safe(s in "\\PC{0,40}") {
        let name = crate::sql::sanitize_table_name(&s);
        prop_assert!(!name.is_empty());
        prop_assert!(!name.starts_with('_'));
        prop_assert!(name.chars().all(|c| c.is_alphanumeric() || c == '_'));
    }
}
