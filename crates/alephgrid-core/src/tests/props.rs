//! Property and state-machine tests. A table is driven by random operation sequences and
//! compared after every step with a plain `Vec` reference model. Failing cases are shrunk
//! and their seeds saved under `proptest-regressions/`; set `PROPTEST_CASES` for longer runs.

use super::*;
use proptest::prelude::*;
use serde_json::json;

/// `(row, column, value)`: `None` clears, `Some(Err)` is text that is not an integer.
type CellOp = (u64, usize, Option<Result<i32, String>>);

#[derive(Debug, Clone)]
enum Op {
    Edit(Vec<CellOp>),
    /// Edit the record at this model index (modulo length) by its record id.
    EditRecord(usize, usize, Option<Result<i32, String>>),
    Insert(u64, u64),
    Delete(u64, u64),
    Reopen,
    SortedCheck,
}

type Row = [Option<String>; 2];

/// The reference model: rows in display order with the record id each must have.
#[derive(Debug, Clone, Default)]
struct Model {
    rows: Vec<(i64, Row)>,
    next_id: i64,
}

fn value_strategy() -> impl Strategy<Value = Option<Result<i32, String>>> {
    prop_oneof![
        3 => any::<i32>().prop_map(|v| Some(Ok(v))),
        1 => Just(None),
        1 => "[a-z]{1,3}".prop_map(|s| Some(Err(s))),
    ]
}

/// `hot` concentrates inserts at the first few positions, which exhausts the gaps
/// between order keys and forces rebalancing.
fn op_strategy(hot: bool) -> impl Strategy<Value = Op> {
    let at = if hot { 0u64..3 } else { 0u64..12 };
    prop_oneof![
        5 => prop::collection::vec((0u64..14, 0usize..2, value_strategy()), 0..5).prop_map(Op::Edit),
        2 => (0usize..64, 0usize..2, value_strategy()).prop_map(|(r, c, v)| Op::EditRecord(r, c, v)),
        3 => (at, 0u64..4).prop_map(|(a, n)| Op::Insert(a, n)),
        2 => (0u64..12, 0u64..4).prop_map(|(a, n)| Op::Delete(a, n)),
        1 => Just(Op::Reopen),
        1 => Just(Op::SortedCheck),
    ]
}

/// Column 0 is INTEGER (`n`), column 1 is VARCHAR (`s`).
fn to_text(v: &Result<i32, String>) -> String {
    match v {
        Ok(i) => i.to_string(),
        Err(s) => s.clone(),
    }
}

fn expected_json(model: &Model) -> Vec<Vec<Value>> {
    model
        .rows
        .iter()
        .map(|(_, r)| {
            vec![
                r[0].as_ref().map(|s| json!(s.parse::<i64>().unwrap())).unwrap_or(Value::Null),
                r[1].as_ref().map(|s| json!(s)).unwrap_or(Value::Null),
            ]
        })
        .collect()
}

fn new_row(model: &mut Model) -> (i64, Row) {
    let id = model.next_id;
    model.next_id += 1;
    (id, [None, None])
}

/// Apply `op` to the model; false when the engine must refuse it (nothing changes then).
fn apply_model(model: &mut Model, op: &Op) -> bool {
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
            while (model.rows.len() as u64) <= max {
                let r = new_row(model);
                model.rows.push(r);
            }
            for (r, c, v) in edits {
                model.rows[*r as usize].1[*c] = v.as_ref().map(to_text);
            }
            true
        }
        Op::EditRecord(i, c, v) => {
            if model.rows.is_empty() {
                return true;
            }
            if *c == 0 && matches!(v, Some(Err(_))) {
                return false;
            }
            let n = model.rows.len();
            model.rows[*i % n].1[*c] = v.as_ref().map(to_text);
            true
        }
        Op::Insert(at, n) => {
            let at = (*at as usize).min(model.rows.len());
            for k in 0..*n as usize {
                let r = new_row(model);
                model.rows.insert(at + k, r);
            }
            true
        }
        Op::Delete(start, n) => {
            let start = (*start as usize).min(model.rows.len());
            let end = (start + *n as usize).min(model.rows.len());
            model.rows.drain(start..end);
            true
        }
        Op::Reopen | Op::SortedCheck => true,
    }
}

fn run(ops: Vec<Op>) -> Result<(), TestCaseError> {
    let mut t = TempWb::new();
    t.wb.create_table_from_query("t", "SELECT 1::INTEGER AS n, 'x'::VARCHAR AS s WHERE false").unwrap();
    let mut view = t.wb.open_view(spec("t")).unwrap().view_id;
    let mut model = Model::default();
    let mut revision = t.wb.revision().unwrap();
    for op in &ops {
        let expected_ok = apply_model(&mut model.clone(), op);
        let result = match op {
            Op::Edit(edits) => {
                let edits: Vec<CellEdit> = edits
                    .iter()
                    .map(|(r, c, v)| CellEdit {
                        row: *r,
                        column: if *c == 0 { "n" } else { "s" }.into(),
                        value: v.as_ref().map(to_text),
                        record: None, base: None })
                    .collect();
                t.wb.edit(view, &edits).map(|_| ())
            }
            Op::EditRecord(i, c, v) => {
                if model.rows.is_empty() {
                    Ok(())
                } else {
                    let (id, _) = model.rows[*i % model.rows.len()];
                    // A deliberately wrong position: the record id must win.
                    let edit = CellEdit { row: 0, column: if *c == 0 { "n" } else { "s" }.into(), value: v.as_ref().map(to_text), record: Some(id), base: None };
                    t.wb.edit(view, &[edit]).map(|_| ())
                }
            }
            Op::Insert(at, n) => t.wb.insert_rows(view, *at, *n).map(|_| ()),
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
                let mut values: Vec<(Option<i64>, i64)> =
                    model.rows.iter().map(|(id, r)| (r[0].as_ref().map(|v| v.parse().unwrap()), *id)).collect();
                // NULLS LAST, ties in display order: a stable sort of the model is the oracle.
                values.sort_by_key(|(v, _)| (v.is_none(), *v));
                let got: Vec<(Option<i64>, i64)> = {
                    let n = t.wb.view_info(sorted).unwrap().row_count;
                    let b = t.wb.fetch(sorted, 0, n).unwrap();
                    b.rows.iter().zip(&b.ids).map(|(r, id)| (r[0].as_i64(), *id)).collect()
                };
                t.wb.close_view(sorted);
                prop_assert_eq!(got, values);
                Ok(())
            }
        };
        prop_assert_eq!(result.is_ok(), expected_ok, "op {:?} result {:?}", op, result.err());
        let mutating = !matches!(op, Op::Reopen | Op::SortedCheck);
        if expected_ok {
            apply_model(&mut model, op);
        }
        // Each accepted change is exactly one revision; refused ones change nothing.
        let now = t.wb.revision().unwrap();
        // An empty edit changes nothing, so it makes no revision either (PROTOCOL.md).
        let skipped = (matches!(op, Op::EditRecord(..)) && model.rows.is_empty()) || matches!(op, Op::Edit(e) if e.is_empty());
        prop_assert_eq!(now, revision + u64::from(mutating && expected_ok && !skipped), "revision after {:?}", op);
        revision = now;
        let info = t.wb.view_info(view).unwrap();
        prop_assert_eq!(info.row_count, model.rows.len() as u64);
        prop_assert!(info.can_append);
        let block = t.wb.fetch(view, 0, 5_000).unwrap();
        prop_assert_eq!(&block.rows, &expected_json(&model), "after {:?}", op);
        let ids: Vec<i64> = model.rows.iter().map(|(id, _)| *id).collect();
        prop_assert_eq!(&block.ids, &ids, "record ids after {:?}", op);
        t.wb.verify_index("t").map_err(|e| TestCaseError::fail(format!("{e:#} after {op:?}")))?;
        // Windows agree with the full read (positions map correctly at every offset).
        for start in [1u64, 2, 5] {
            let w = t.wb.fetch(view, start, 3).unwrap();
            let s = (start as usize).min(ids.len());
            let e = (start as usize + 3).min(ids.len());
            prop_assert_eq!(&w.ids, &ids[s..e].to_vec());
        }
    }
    Ok(())
}

proptest! {
    // 48 cases per change; scheduled runs raise PROPTEST_CASES.
    #![proptest_config(ProptestConfig::with_cases(
        std::env::var("PROPTEST_CASES").ok().and_then(|v| v.parse().ok()).unwrap_or(48)
    ))]

    #[test]
    fn table_matches_reference_model(ops in prop::collection::vec(op_strategy(false), 1..25)) {
        run(ops)?;
    }

    #[test]
    fn inserts_at_one_place_rebalance_order_keys(ops in prop::collection::vec(op_strategy(true), 1..40)) {
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
