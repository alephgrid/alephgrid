# P0 formula compatibility gaps

Pinned engine: IronCalc 0.9.1 with the local `rowfinity-compat-1` patch (29 October 2026).
The six original desired-result regressions now pass and run without ignores.
See [patch provenance and removal conditions](../vendor/ironcalc_base/ROWFINITY_PATCHES.md).
The table below preserves the original unpatched observations. This resolves those
specific defects, not complete P0 certification or all Excel compatibility.

Use the catalog fixture: A1:A4 = 1, 2, 2, 4; B1:B4 = 10, 20, 30, 40;
C1:C4 = alpha, beta, beta, gamma. All cases were reproduced through Rowfinity's
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
describes ignoring empty cells. Rowfinity's desired contract additionally skips explicit
empty-string arguments with `ignore_empty=TRUE`; this scalar case needs direct Excel/Sheets
comparison during upstream triage rather than treating the empty-cell documentation alone
as proof of scalar parity. The expected result remains an explicit product contract.

## Reproduce and follow up

```bash
CARGO_INCREMENTAL=0 cargo test --workspace --lib tests::catalog -j 1
```

All 121 catalog cases are enabled, alongside dedicated regressions and a 23-case
boundary suite checked before and after reopening. The added XLOOKUP support covers
one-dimensional inline lookup arrays and equally shaped inline/range results;
multi-column result spills and range-lookup/inline-result combinations remain outside
this patch. Binary searches retain IronCalc's sorted-input precondition.

Before removing the local patch, reproduce on current upstream, check existing issues,
and prepare focused upstream tests/proposals. Adopt an upstream release only after
these regressions pass. No upstream issue or PR was submitted by this change.
TEXTJOIN scalar empty-string behavior remains a Rowfinity contract pending independent
Excel/Sheets parity evidence. Broader coercion/argument-mode certification remains open.
