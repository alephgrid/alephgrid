# P0 formula compatibility gaps

Pinned engine: IronCalc 0.9.1 with the local `alephgrid-compat-1` patch (29 October 2026).
The six original desired-result regressions now pass and run without ignores. The coercion
matrix (30 October 2026) found seven more differences: one is patched (LEFT/RIGHT/MID numeric
text counts), six remain explicit known gaps (below).
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

All 277 catalog cases run (271 verified, 6 known gaps checked to still differ), alongside
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
150 pass; these six are labelled `known_gap`:

| Case ID | Formula | Expected | IronCalc 0.9.1 + patch | Decision |
|---|---|---|---|---|
| day-c01 | `=DAY(0)` | `0` | `#NUM!` | Date system, not patched |
| day-c02 | `=DAY(F1)` (serial 2) | `2` | `1` | Date system, not patched |
| month-c01 | `=MONTH(0)` | `1` | `#NUM!` | Date system, not patched |
| year-c01 | `=YEAR(F4)` (empty → 0) | `1900` | `#NUM!` | Date system, not patched |
| iferror-c02 | `=IFERROR(F4,"x")` | empty text | `0` | Confirm against Excel output first |
| ifna-c02 | `=IFNA(F4,"x")` | empty text | `0` | Confirm against Excel output first |

IronCalc numbers serial 1 as 1899-12-31 and rejects serial 0. Excel's 1900 system numbers
serial 1 as 1900-01-01 and counts the fictitious 1900-02-29, so serials below 61 differ;
from serial 61 (1900-03-01) the systems agree (`day-c03`, `month-c03`). Changing this would
also change date formatting, parsing and XLSX round trips, so it needs upstream discussion
rather than a local patch. For IFERROR/IFNA, Microsoft's reference says an empty value is
treated as empty text; IronCalc returns 0, as for `=F4`. The expected value stays as
documented until independent Excel output confirms or contradicts it.

The test guards these gaps in both directions: a gap that starts matching its expected
value fails the suite until it is reviewed and relabelled `verified`, and the gap set is
listed explicitly. No ignored tests are used for them.
