# IronCalc upstream work

Alephgrid computes sheets with IronCalc and keeps connectors, UI and the DuckDB table engine in
Alephgrid. This file triages what IronCalc lacks or gets wrong, tracks the local patch set, and
holds the focused proposals and tests to take upstream. Nothing here has been submitted yet:
submitting is an outward action that needs the maintainers' go-ahead.

**Compared against:** `ironcalc` 0.9.1, the latest release on crates.io when checked
(31 October 2026, `cargo search ironcalc`), which is also the pinned version. IronCalc's
unreleased main branch was not reviewed; do that before submitting.

## Missing functions

The catalog lists 562 names; IronCalc 0.9.1 registers 486. All 76 it does not register are
functions of other spreadsheet products without an Excel counterpart in the catalog's sources (`ARRAYFORMULA`,
`SPLIT`, `JOIN`, `REGEXMATCH`, `TO_DATE`, `COUNTUNIQUE`, …). IronCalc targets Excel
compatibility, so none is proposed upstream; they stay explicit "not registered" entries in the
help, and the Excel spellings (`TEXTSPLIT`, `TEXTJOIN`, …) cover the same needs. No missing
Excel function was found.

## Defects (proposals)

Each has a reproducer that the test suite keeps running: catalog known-gap cases fail when
the result changes (so an upgrade that fixes them is noticed), and the others below are
`tests::compat::upstream_reproducers_still_reproduce`.

| # | Reproducer | IronCalc 0.9.1 | Expected (source) | Proposal |
|---|---|---|---|---|
| 1 | dates before 1900-03-01 (catalog cases `day-c01`, `day-c02`, `month-c01`, `year-c01`) | IronCalc's own day numbering | Excel's 1900 date system, as recorded in [gaps](FORMULA_GAPS.md) | number serials below 61 as Excel does (proposal already noted in F.02) |
| 2 | `=TAKE(A1:C1,,2)` | `#CALC!` | `1,10` (Microsoft TAKE: omitted rows means all) | catalog `take-03`; treat an empty `rows` argument as "all rows" in `fn_take` |
| 3 | `=TEXTBEFORE("abc","-",,,,"none")`, `=TEXTAFTER(…)` | `#VALUE!` | `none` (Microsoft: omitted arguments take their defaults) | catalog `textbefore-03`, `textafter-03`; read `EmptyArg` as the default for instance_num, match_mode, match_end |
| 4 | `=AND(FALSE,1/0)`, `=OR(TRUE,1/0)` | stops at the deciding value | the error (Excel evaluates every argument; not yet verified against Excel output) | [gaps](FORMULA_GAPS.md#and-and-or-with-an-error-after-the-deciding-value); verify with Excel first |
| 5 | `=TEXTJOIN(",",TRUE,SORT(A1:A3))` | `#N/IMPL!` | `1,2,3` (TEXTJOIN accepts arrays) | accept `CalcResult::Array` arguments in `fn_textjoin` (and `CONCAT`) |
| 6 | `VLOOKUP(…,TRUE)` / `MATCH(…,1)` over duplicate keys | whichever duplicate the binary search reaches | Excel documents "the largest value ≤", not which duplicate | [gaps](FORMULA_GAPS.md#sorted-lookups-over-duplicate-keys); propose returning the last of the run, after checking Excel |

Each proposal is one small change with the reproducer as an IronCalc test, in the style of
`base/src/test/test_fn_*.rs`:

```rust
#[test]
fn take_with_omitted_rows_keeps_all_rows() {
    let mut model = new_empty_model();
    model._set("A1", "1");
    model._set("B1", "10");
    model._set("A2", "=TAKE(A1:B1,,1)");
    model.evaluate();
    assert_eq!(model._get_text("A2"), "1");
}
```

## Local patch set

The vendored copy (`vendor/ironcalc_base`, patch set 1, described in its
`ALEPHGRID_PATCHES.md`) changes, each with a desired-result regression in `tests::catalog` or
the F.09 tests:

| Change | Upstream status | Remove when |
|---|---|---|
| `"<=x"` / `">=x"` text criteria direction | not submitted | upstream release passes `tests::catalog` criteria cases |
| COUNTIFS / IFS dimension checks | not submitted | upstream refuses unequal ranges (`*_should_reject_unequal_ranges`) |
| TEXTJOIN `ignore_empty` with empty strings | not submitted | `textjoin_should_skip_empty_strings_when_requested` passes unpatched |
| VLOOKUP index errors (#VALUE! vs #REF!) | not submitted | `vlookup_zero_column_should_return_value_error` passes unpatched |
| XLOOKUP over arrays | issue ironcalc/IronCalc#1338 referenced, no PR | `xlookup_inline_arrays_should_find_a_present_key` passes unpatched |
| LEFT/RIGHT/MID numeric-text counts | not submitted | coercion cases pass unpatched |
| Parser rejects trailing input; token positions | not submitted | F.03 error-location tests pass unpatched |
| Array result in scalar context (debug panic) | not submitted | names tests pass in debug builds unpatched |
| `ensure_unrecorded_sheet`, `set_unrecorded_input` (F.09/F.10 link, slot and spill sheets) | addition, not submitted | upstream offers writes outside the undo history, or Alephgrid moves links out of the model |

**Upgrade procedure.** For a new IronCalc release: build against it without the patch; run
`tests::catalog` (desired-result regressions), the IronCalc suite, and the whole workspace;
drop each patch whose regression passes unpatched, re-apply the rest with their notes; review
the known gaps above (a gap that starts passing fails its test and must be relabelled); update
`engine` and `engine_patch` in the catalog. Review the patch set before the F.15 release gate.
