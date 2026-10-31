# P0 formula compatibility gaps

Pinned engine: IronCalc 0.9.1 with the local `alephgrid-compat-1` patch (29 October 2026).
The six original desired-result regressions now pass and run without ignores. The coercion
matrix (30 October 2026) found seven more differences: one is patched (LEFT/RIGHT/MID numeric
text counts), two were wrong expectations corrected from observed Excel output (IFERROR/IFNA
of an empty cell), and four are accepted date-system gaps (below).
See [patch provenance and removal conditions](../vendor/ironcalc_base/ALEPHGRID_PATCHES.md).
The table below preserves the original unpatched observations. This resolves those
specific defects, not complete P0 certification or all Excel compatibility.

Use the catalog fixture: A1:A4 = 1, 2, 2, 4; B1:B4 = 10, 20, 30, 40;
C1:C4 = alpha, beta, beta, gamma. All cases were reproduced through Alephgrid's
actual sheet adapter. The original five newly discovered cases also failed after reopening; the patched catalog is checked before and after reopening.

| Case ID | Formula | Expected | Original unpatched result | Patch scope |
|---|---|---|---|---|
| averageifs-03 | `=AVERAGEIFS(B1:B4,A1:A3,2)` | `#VALUE!` | `25` | Validate dimensions before evaluation upstream |
| countifs-02 | `=COUNTIFS(A1:A4,2,B1:B3,">0")` | `#VALUE!` | `2` | Validate every criteria range upstream |
| sumifs-03 | `=SUMIFS(B1:B4,A1:A3,2)` | `#VALUE!` | `50` | Validate sum/criteria dimensions upstream |
| textjoin-01 | `=TEXTJOIN("-",TRUE,"a","","b")` | `a-b` | `a--b` | Review scalar empty-string handling and range parity upstream |
| vlookup-06 | `=VLOOKUP(2,A1:B4,0,FALSE)` | `#VALUE!` | `#REF!` | Separate nonpositive index from out-of-range index errors upstream |
| xlookup-10 | `=XLOOKUP(2,{1;2;2},{10;20;30})` | `20` | `#N/A` | Review inline-array evaluation; related upstream issue 1338 |

## Contract evidence and limits

Microsoft documents matching dimensions for
[SUMIFS](https://support.microsoft.com/en-us/excel/how-to-correct-a-value-error-in-the-sumif-sumifs-function),
[COUNTIFS](https://support.microsoft.com/en-gb/excel/functions/countifs-function) and
[AVERAGEIFS](https://support.microsoft.com/en-us/excel/functions/averageifs-function).
The desired `#VALUE!` outcomes enforce rejection rather than silent partial aggregation.
Microsoft also documents the
[VLOOKUP nonpositive-index error](https://support.microsoft.com/en-us/excel/how-to-correct-a-value-error-in-the-vlookup-function).

The [TEXTJOIN reference](https://support.microsoft.com/en-gb/excel/functions/textjoin-function)
describes ignoring empty cells. Alephgrid's desired contract additionally skips explicit
empty-string arguments with `ignore_empty=TRUE`; this scalar case needs direct Excel/Sheets
comparison during upstream triage rather than treating the empty-cell documentation alone
as proof of scalar parity. The expected result remains an explicit product contract.

## Reproduce and follow up

```bash
CARGO_INCREMENTAL=0 cargo test --workspace --lib tests::catalog -j 1
```

All 277 catalog cases run (273 verified, 4 accepted gaps checked to still differ), alongside
dedicated regressions and a 23-case boundary suite checked before and after reopening. The added XLOOKUP support covers
one-dimensional inline lookup arrays and equally shaped inline/range results;
multi-column result spills and range-lookup/inline-result combinations remain outside
this patch. Binary searches retain IronCalc's sorted-input precondition.

Before removing the local patch, reproduce on current upstream, check existing issues,
and prepare focused upstream tests/proposals. Adopt an upstream release only after
these regressions pass. No upstream issue or PR was submitted by this change.
TEXTJOIN scalar empty-string behavior remains a Alephgrid contract pending independent
Excel/Sheets parity evidence. Broader coercion/argument-mode certification remains open.

## Coercion matrix gaps (30 October 2026)

The 156 coercion cases use typed inputs in F1:G8 (number, text `'2`, TRUE, empty, `=NA()`,
`=""`, Unicode text, negative decimal, and three wildcard strings). Expected values cite the
Microsoft Excel rule for each case (`oracle` in the catalog), not this engine's output.
150 passed at first; the six differences and their resolution:

| Case ID | Formula | Expected | IronCalc 0.9.1 + patch | Decision |
|---|---|---|---|---|
| day-c01 | `=DAY(0)` | `0` | `#NUM!` | Accepted gap: date numbering |
| day-c02 | `=DAY(F1)` (serial 2) | `2` | `1` | Accepted gap: date numbering |
| month-c01 | `=MONTH(0)` | `1` | `#NUM!` | Accepted gap: date numbering |
| year-c01 | `=YEAR(F4)` (empty → 0) | `1900` | `#NUM!` | Accepted gap: date numbering |
| iferror-c02 | `=IFERROR(F4,"x")` | `0` (was: empty text) | `0` | Expectation corrected; verified |
| ifna-c02 | `=IFNA(F4,"x")` | `0` (was: empty text) | `0` | Expectation corrected; verified |

**Dates before 1900-03-01 (accepted).** IronCalc numbers serial 1 as 1899-12-31 and rejects
serial 0. Excel's 1900 system numbers serial 1 as 1900-01-01 and counts the fictitious
1900-02-29, so serials below 61 differ; from serial 61 (1900-03-01) the systems agree
(`day-c03`, `month-c03`). IronCalc is consistent with itself: `DATE(1900,1,1)` is serial 2 and
`DAY` reads it back as 1. A patch limited to DAY/MONTH/YEAR was tried and rejected: it made
`DAY(DATE(1900,1,1))` return 2 and broke six upstream tests that pin the current numbering on
purpose. Matching Excel needs one change to IronCalc's date numbering (DATE, DATEVALUE,
parsing, formatting, financial functions and XLSX), which is upstream work tracked in F.14.
Practical effect: only raw serials 0–60 and dates in January–February 1900 differ; the most
likely case is `YEAR` of an empty cell, which is `#NUM!` here and 1900 in Excel.

**IFERROR/IFNA of an empty cell (corrected).** Microsoft's function reference says an empty
value is treated as empty text, but observed Excel (and OpenOffice) output is 0, the same as a
direct reference to the empty cell. This was recorded by Microsoft in OASIS OpenFormula issue
[OFFICE-3468](https://issues.oasis-open.org/browse/OFFICE-3468), and ODF 1.2 removed the
empty-string rule to match. The expected value is now 0 and both cases are verified; IronCalc
already returns 0, and no engine change was made.

The test guards these gaps in both directions: a gap that starts matching its expected
value fails the suite until it is reviewed and relabelled `verified`, and the gap set is
listed explicitly. No ignored tests are used for them.

## AND and OR with an error after the deciding value

Found by the F.09 sheet-cell comparison (`table_formula::tests::sheet_cells_agree_with_the_sheet_engine`).
`=AND(FALSE, P!A5)` where `P!A5` is `=1/0`: IronCalc returns `FALSE`, because it stops at the
first `FALSE` (`logical_nary`, `short_circuit_value`); table formulas return `#DIV/0!`, because
they evaluate every argument and return the first error. `OR` stops at the first `TRUE` the same
way. Which one Excel does has not been verified here; Excel is generally described as not
short-circuiting `AND`/`OR`, which would make IronCalc the one to change. Until it is checked
against Excel (F.14), the difference is documented in [table formulas](TABLE_FORMULAS.md#errors)
and is the only `AND`/`OR` case the comparison accepts.

## Sorted lookups over duplicate keys

`VLOOKUP(…, TRUE)`, `MATCH(…, 1)` and `MATCH(…, -1)` over table columns return the **last** row
of a run of equal keys (the last row whose key is at most, or at least, the value). The sheet
engine's binary search (IronCalc `binary_search_on_array`) returns whichever equal element its
midpoints reach, so the two engines can return different rows of the run. Excel's documented
rule ("the largest value that is less than or equal to") does not say which duplicate; the
choice of the last row is Alephgrid's and is not verified against Excel. Differential tests use
sorted keys without duplicates; table tests check the documented choice by hand. Proposed for
F.14: pin the sheet behaviour upstream (last duplicate) and verify against Excel output.

## INDEX with row 0

`INDEX(range, 0)` in a sheet returns the whole column, which spills. A table formula cell holds
one value, so table `INDEX` with row 0 (or a fraction below 1) gives `#VALUE!`, as for negative
rows. Accepted difference; not an engine defect.
