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
