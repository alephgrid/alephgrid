# P0 formula compatibility gaps

Pinned engine: IronCalc 0.9.1. Local reproductions: 28 October 2026.
These cases block complete F.02 certification. Expected results are retained in the
versioned catalog; six explicitly ignored desired-result regressions are separate
from passing coverage. No dependency patch, upstream issue or upstream PR is included.

Use the catalog fixture: A1:A4 = 1, 2, 2, 4; B1:B4 = 10, 20, 30, 40;
C1:C4 = alpha, beta, beta, gamma. All cases were reproduced through Rowfinity's
actual sheet adapter. The five newly discovered cases also fail after reopening.

| Case ID | Formula | Expected | Observed | Next action |
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
describes ignoring empty cells. Rowfinity's desired contract additionally skips explicit
empty-string arguments with `ignore_empty=TRUE`; this scalar case needs direct Excel/Sheets
comparison during upstream triage rather than treating the empty-cell documentation alone
as proof of scalar parity. The expected result remains an explicit product contract.

## Reproduce and resolve

```bash
CARGO_INCREMENTAL=0 cargo test -p rowfinity-engine tests::catalog -j 1
# Deliberately fails until the six engine gaps are resolved:
CARGO_INCREMENTAL=0 cargo test -p rowfinity-engine tests::catalog -j 1 -- --ignored
```

1. Reproduce each minimal case on current upstream as well as the pinned version.
2. Check existing issues, then prepare a focused engine regression and proposed fix.
3. Adopt a reviewed pinned release/commit; avoid a formula-text rewrite in Rowfinity.
4. Re-run the desired-result tests, source-change/undo/reopen tests and browser tests.
5. Remove the corresponding ignore and change its manifest status only after the fix
   passes. Update the asserted known-gap inventory and record the dependency version.

This document is a triage package, not a claim that current upstream has the same defects.
