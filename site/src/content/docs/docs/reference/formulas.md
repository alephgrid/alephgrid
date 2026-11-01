---
title: Formula compatibility
description: What Excel functions and behaviors Alephgrid supports, what is verified, what is partial, and the known gaps.
---

Alephgrid's sheet engine is [IronCalc](https://github.com/ironcalc/IronCalc) (pinned at 0.9.1
with a small local compatibility patch). Full Excel parity is a long-term target, not a launch
claim: this page says exactly what is verified today, what is partial, and what is a known
gap. The in-app **Formula help** (Shift+F3) searches the same inventory, with availability
filters and per-function evidence.

## The catalog

Compatibility is tracked in one versioned catalog of 562 function names, compared against
competitor listings from official sources:

- **486 names are registered** in the engine under the exact catalog name; 76 are not.
- **44 priority (P0) functions** have reviewed signatures, descriptions and hand-calculated
  examples, checked through the actual sheet adapter before and after reopening a workbook.
  These 44 are what the formula bar offers as suggestions.
- **277 verification cases** cover those 44 functions: 273 verified, 4 accepted gaps. The
  cases span exact and approximate lookups, duplicate matches, conditional-aggregate
  criteria, error handling and lazy `IF` branches, text and Unicode cases, signed rounding,
  and leap days. Expected values are derived independently (the documented Excel rule),
  never copied from the engine's own output.
- The **518 entries without a reviewed signature** are explicitly marked "Not reviewed" in
  the help and are excluded from autocomplete. A registered name is not a parity claim.

A passing case certifies that input only. Not yet covered: locale-specific parsing, the 1904
date system, all optional-argument combinations, emoji and grapheme clusters, array and spill
results of P0 functions, and numerical precision at extremes.

## Table formulas

52 functions compile in spreadsheet [table formulas](../../table-formulas/) over DuckDB
tables, each with recorded modes (for example, exact-match `XLOOKUP`; whole columns for
`SUMIFS`) and the differential or golden tests that exercise it. The catalog's table status
refers to spreadsheet syntax over table references, not similarly named SQL functions.

## Fixed by the local patch

Six differences from the documented Excel rule were found and fixed in the local
`alephgrid-compat-1` patch; all their regressions now pass:

| Formula | Excel rule | Unpatched engine |
|---|---|---|
| `=SUMIFS(B1:B4,A1:A3,2)` | `#VALUE!` (dimensions must match) | summed anyway |
| `=COUNTIFS(A1:A4,2,B1:B3,">0")` | `#VALUE!` | counted anyway |
| `=AVERAGEIFS(B1:B4,A1:A3,2)` | `#VALUE!` | averaged anyway |
| `=VLOOKUP(2,A1:B4,0,FALSE)` | `#VALUE!` (index below 1) | `#REF!` |
| `=TEXTJOIN("-",TRUE,"a","","b")` | `a-b` | `a--b` |
| `=XLOOKUP(2,{1;2;2},{10;20;30})` | `20` | `#N/A` |

The `TEXTJOIN` scalar empty-string behavior is an Alephgrid contract pending independent
Excel comparison. The `XLOOKUP` fix covers one-dimensional inline lookup arrays with equally
shaped results; multi-column result spills remain outside the patch. The patch has not yet
been sent upstream.

## Known gaps

These differences are accepted and guarded by tests in both directions: a gap that starts
matching Excel fails the suite until it is reviewed and relabelled verified.

### Dates before 1900-03-01

IronCalc numbers serial 1 as 1899-12-31 and rejects serial 0; Excel's 1900 system numbers
serial 1 as 1900-01-01 and counts the fictitious 1900-02-29. Serials below 61 therefore
differ; from serial 61 (1900-03-01) the systems agree. The practical cases:

| Formula | Excel | Alephgrid |
|---|---|---|
| `=DAY(0)` | `0` | `#NUM!` |
| `=DAY(2)` | `2` | `1` |
| `=MONTH(0)` | `1` | `#NUM!` |
| `=YEAR(<empty cell>)` | `1900` | `#NUM!` |

Matching Excel needs one change to the engine's date numbering (dates, parsing, formatting,
financial functions and XLSX together), which is tracked as upstream work. Only raw serials
0–60 and dates in January–February 1900 differ.

### AND and OR with an error after the deciding value

In table formulas, `AND` and `OR` evaluate every argument and return the first error even
after a deciding `FALSE` or `TRUE`; the sheet engine stops at the deciding value, so
`=AND(FALSE, 1/0)` is `FALSE` in a sheet and `#DIV/0!` in a table formula. Which one Excel
does has not been verified here yet.

### Sorted lookups over duplicate keys

`VLOOKUP(…, TRUE)` and `MATCH` with modes 1 or -1 over table columns return the last row of a
run of equal keys. The sheet engine's binary search returns whichever equal element it meets,
and Excel's documented rule does not say which duplicate. The table behavior is Alephgrid's
documented choice, not verified against Excel.

### INDEX with row 0

`INDEX(range, 0)` in a sheet spills the whole column. A table formula cell holds one value,
so table `INDEX` with row 0 gives `#VALUE!`. Accepted difference, not an engine defect.

:::note
The full records, reproducers and upstream triage plan live in the repository:
[formula catalog](https://github.com/alephgrid/alephgrid/blob/main/docs/FORMULA_CATALOG.md)
and [compatibility gaps](https://github.com/alephgrid/alephgrid/blob/main/docs/FORMULA_GAPS.md).
:::
