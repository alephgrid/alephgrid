# Alephgrid compatibility patch set 1

Base: crates.io ironcalc_base 0.9.1, copied from the locally cached published crate.
License: MIT OR Apache-2.0; original license files and third-party notices retained.
Owner: Alephgrid maintainers. Status: local patch, not an upstream-approved release.

Changes:
- Text criteria `"<=x"` and `">=x"` (COUNTIF/SUMIF/AVERAGEIF and the *IFS forms) compare in
  the right direction; upstream counted text after `x` for `<=` and before it for `>=`.
- COUNTIFS checks both range dimensions against the first criteria range.
- Shared IFS aggregation checks criteria dimensions against the result range (SUMIFS,
  AVERAGEIFS, MINIFS and MAXIFS).
- TEXTJOIN with ignore_empty skips scalar and range empty strings. This is Alephgrid's
  explicit contract; it is not a claim of independently measured Excel parity.
- VLOOKUP distinguishes nonpositive indexes (#VALUE!) from out-of-bounds indexes (#REF!).
- XLOOKUP accepts array-valued lookup vectors with same-shape array/range results,
  reusing the existing search algorithms. Existing range lookup behavior is retained.
- LEFT, RIGHT and MID convert numeric text and logical count/position arguments, as
  their other numeric arguments already did (`self.text_number`). The text argument is
  still checked first, so its error wins. Added 30 October 2026 (coercion matrix).
- The parser rejects input left after a complete expression (`=1 2`, `=1+2)`,
  `SIN(x)(3)`) instead of silently dropping it; such cells keep their text and show
  #ERROR!. A missing final `)` is still completed. Unexpected-token errors report where
  the token starts instead of position 0. Two upstream parser tests that pinned the old
  behavior are updated (`test_parser_bad_formula_3`, `sin_is_not_immediately_invocable`).
  Added 30 October 2026 (F.03 error locations).

- A scalar formula whose result becomes a multi-cell array (a name redefined as a range
  after the formula was entered) shows #VALUE! "Array result in scalar context" in debug
  builds too; an upstream `debug_assert!` panicked there. Alephgrid re-enters such cells
  after a name change so they spill as if typed now. Added 30 October 2026.

- `UserModel::ensure_unrecorded_sheet` and `UserModel::set_unrecorded_input` create a very
  hidden sheet and write its cells without entering the undo history. Alephgrid keeps anchors
  for the sheet cells table formulas read there (F.09), so the engine moves them with their
  cells while undoing a user's change never undoes that bookkeeping. Additions only; no
  existing behavior changes. Added 30 October 2026 (F.09).

- External functions (patch set 2, phase 8): `Model::set_external_functions` /
  `UserModel::set_external_functions` register a host hook (`src/external.rs`). A call of a
  name the hook claims (not a built-in, not bound by LET) evaluates its arguments (ranges and
  arrays as rows of values, whole rows and columns trimmed to the used area, at most 100,000
  cells) and returns what the hook answers. Without a hook nothing changes. Alephgrid answers
  from its cache of Python results and says "Calculating" (`#CALC!`) for the rest. Maintenance:
  one new module and about 130 lines in `model.rs` (`call_external`, `external_value` and the
  check in `NamedFunctionKind`) to carry to each IronCalc upgrade; an upstream
  custom-function API would replace it. Added 31 October 2026.

Upstream test suite (`cargo test --lib` in a copy of this directory with the repository's
rust-toolchain.toml): original 0.9.1 passes 2765 (8 ignored); patched passes 2765 after the
two test updates above (rerun 30 October 2026 with the F.09 additions: 2765 pass).

Regression evidence lives in alephgrid-core tests::catalog. Keep the upstream
source diff small; do not format or change unrelated dependency code.
Related XLOOKUP issue: https://github.com/ironcalc/IronCalc/issues/1338
No issue/PR has been submitted and current upstream has not yet been reproduced.

Removal: adopt a pinned upstream release/commit only when all desired-result and
source-edit/undo/reopen regressions pass without this patch, review the upstream
source diff, then remove this directory and the root Cargo patch declaration.
Review this patch before each dependency upgrade and before the F.15 release gate.

Original crates.io archive checksum recorded in the pre-patch lockfile:
`ecf55ea346dcda5e0f0034d8300dc2d40ffbcb64e7c4db23bfa1d8a0cf11c738`.
This identifies the original archive, not the patched directory.

## Security limits (SEC.07, SEC.09, SEC.12; 1 November 2026)

- `evaluation.rs`, `model.rs`: an evaluation reads at most `visit_budget` cells
  (`DEFAULT_VISIT_BUDGET`, set by Alephgrid through `set_evaluation_limits`), a range may
  declare at most `MAX_RANGE_CELLS` and become an array of at most `MAX_ARRAY_CELLS`; a
  `cancel` flag stops a running evaluation; `evaluation_stopped()` says why. `clip_to_used_area`
  clips every range, not only whole rows and columns, and the COUNT/COUNTA/COUNTBLANK/AVERAGE/
  AVERAGEA/MIN/MAX/AND/OR/XOR/CONCAT/MULTINOMIAL walkers clip through `clip_range`
  (COUNTBLANK adds the skipped area as blanks).
- `expressions/parser/mod.rs`: at most `MAX_FORMULA_CHARS` characters, `MAX_NESTING` levels
  and `MAX_OPERATORS` operators per formula; beyond, a parse error node.
- `user_model/common.rs`, `user_model/history.rs`: `set_queue_changes(false)` keeps no queue
  for remote models; `forget_oldest_undo`, `undo_len`, `clear_history` let the host bound
  the history.

Regression evidence: alephgrid-core `tests::security`. These are not upstream behaviour.

## Scientific format sign (S.02; 1 November 2026)

- `formatter/format.rs`: a value from 1 to 10 formatted with `0.00E+00` showed `E-00`; the
  exponent is negative only below 1. Regression: alephgrid-core
  `tests::sheetformat::number_formats_change_the_display_not_the_value`.

## LAMBDA calls are bounded (security run-2; 2 November 2026)

- `functions/logical/lambda.rs`, `evaluation.rs`: `call_lambda` and `call_lambda_with_values`
  go through `enter_lambda`, which refuses a call (`#CALC!`) past `MAX_LAMBDA_DEPTH` (1000)
  calls inside one another or past the evaluation's stack limit measured from the outermost
  call, and charges one visit (`Evaluation::spend`) so the budget, deadline and cancel stop
  long chains. Upstream, a lambda that calls itself (`=LAMBDA(g,g(g))(LAMBDA(g,g(g)))`, or a
  defined name whose LAMBDA calls its own name) recursed until the thread's stack
  overflowed, which ends the process. Regression: alephgrid-core
  `tests::security::lambda_recursion_is_an_error_not_a_stack_overflow`.

## Cut and paste onto another sheet (F.04d; 7 November 2026)

- `user_model/clipboard.rs`: a cut reads the moved formulas on the sheet they come from (the
  source sheet), not the sheet they are pasted on; copies are unchanged.
- `expressions/parser/move_formula.rs`: `MoveContext::moved_to` names the sheet a cut area lands
  on when it is another sheet. References into the area then point there (unqualified in a
  formula on that sheet, qualified elsewhere); references from moved formulas to cells left
  behind already named their sheet. `None` keeps the upstream same-sheet behaviour.
- `cut_paste.rs`, `model.rs`: formulas elsewhere and defined names that refer to the moved
  cells follow them to the other sheet (also when the cell address stays the same, e.g. A1 to
  A1); conditional formats stay on their sheet on a cross-sheet cut (a range cannot span two
  sheets). Upstream, a cut to another sheet left references in moved formulas pointing at the
  target sheet's cells and moved outside references within the source sheet.
  The upstream `test_move_formula.rs` contexts gain `moved_to: None`.

Regression: alephgrid-core `sheets::editing::tests::cut_to_another_sheet_moves_cells_and_references`
(fails without this patch: `=C1+A1` moved from Data to Other read `=C1+D5`, the wrong C1).
The upstream test suite was not rerun for this patch (it needs a copy outside the workspace and
its own build); rerun it with the next review.

## Long column names do not overflow (fuzz finding; 7 November 2026)

- `expressions/utils/mod.rs`: `column_to_number` accumulates with checked arithmetic and
  returns its existing "Column is not valid." error when the letters overflow `i32`.
  Upstream, a reference-like token with seven or more letters (`=gaaAaAA$`, read as column
  `GAAAAAA`) overflowed `column_number * 26`, which panics in debug builds (the bounded fuzz
  target `sheet_formula_check_is_total_on_any_text` found it) and wraps silently in release
  builds. Upstream status: not reported yet. Remove when upstream checks the overflow.
  Regression: alephgrid-core `tests::fuzz::sheet_formula_check_is_total_on_any_text` (fixed
  cases; panicked without this patch). The upstream test suite was not rerun for this patch.
