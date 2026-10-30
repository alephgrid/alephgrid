# Spreadsheet formulas over tables (F.05)

A **table formula** is a calculated column written in spreadsheet syntax, evaluated by DuckDB
over the whole table without copying rows into sheet cells:

```text
=IF([@amount] > 100, "large", "small")
=IFERROR([@revenue] / [@units], "")
=ROUND([@price] * (1 + [@tax_rate]), 2)
```

Existing calculated columns whose formula is a DuckDB SQL expression keep working unchanged;
each formula column records its language (`sql` or `sheet`). This document is the contract the
compiler implements (`crates/alephgrid-core/src/table_formula/`). Anything not listed here is
refused with an explanation and the character position, never approximated.

## Pipeline

1. **Parse** (`parse.rs`): a hand-written tokenizer and recursive-descent parser produce a
   typed syntax tree with character spans. No regular expressions turn formula text into SQL.
2. **Bind and check** (`compile.rs`): references are bound to the table's columns by name
   (case-insensitive, as DuckDB identifiers); every node gets a static type; unsupported
   functions, argument counts and type combinations are refused with their position.
3. **Compile**: each node becomes a pair of SQL expressions — its **value** and its **error**
   (`NULL`, or an error code such as `#DIV/0!`). Identifiers are quoted with `ident`, text
   with `lit`; nothing from the formula is interpolated unchecked.
4. **Evaluate lazily**: the pair is added to the table's base relation like other calculated
   columns, so reads, sorts, filters and exports see it without materializing every row in
   the browser. Only the rows a client asks for are transferred.

## Syntax

| Element | Forms | Notes |
|---|---|---|
| Numbers | `12`, `1.5`, `.5`, `2E3`, `50%` | `%` divides by 100 |
| Text | `"say ""hi"""` | Doubled quotes escape a quote |
| Logical | `TRUE`, `FALSE` | Case-insensitive |
| Errors | `#N/A`, `#DIV/0!`, `#VALUE!`, `#NUM!`, `#REF!`, `#NAME?`, `#NULL!` | |
| This row | `[@amount]`, `[@[unit price]]`, `[amount]`, `Orders[[#This Row],[amount]]` | `'` escapes `[`, `]`, `#` and `'` inside brackets |
| Whole column | `Orders[amount]`, `Orders[[amount]]` | Only where a function expects a range (lookups F.06, conditional aggregates F.08) |
| Operators | `-x` `x%` `^` `*` `/` `+` `-` `&` `=` `<>` `<` `<=` `>` `>=` | Spreadsheet precedence: negation, percent, power, multiply/divide, add/subtract, join, compare. `-2^2` is 4 |
| Calls | `NAME(arg, …)` | Allowlisted functions only |

`[amount]` without `@` means this row in a calculated column (spreadsheet implicit
intersection). Ranges (`A1:B4`), sheet cell references, names, array constants and the
intersection operator are refused in table formulas.

## Types and values

Every value has one static type; a column has one type for all rows.

| Type | From columns | SQL |
|---|---|---|
| Number | integers, decimals, floats | `DOUBLE` |
| Text | `VARCHAR` | `VARCHAR` |
| Logical | `BOOLEAN` | `BOOLEAN` |
| Date | `DATE` | `DATE` |

Other column types (timestamps, lists, structs, blobs, UUIDs …) are refused when referenced.

**Precision.** Numbers are IEEE doubles, as in sheets. `BIGINT`, `HUGEINT` and `DECIMAL`
values are converted to `DOUBLE` when used: integers beyond ±2^53 and decimals with more than
15–17 significant digits lose precision in a table formula (the source column is unchanged).
A result that is not finite (overflow) is `#NUM!`.

**Blank.** A `NULL` source value is blank. Blank is `0` in arithmetic, `""` in text, `FALSE` as
a condition, and equal to both `0` and `""` in comparisons. A formula whose whole result is
blank (`=[@note]` on an empty note) stays blank: Alephgrid keeps `NULL` distinct from `0`,
where a spreadsheet would display `0`.

**Empty text in non-text results.** `""` returned where the result is a Number, Logical or
Date (`=IFERROR([@a]/[@b], "")`) becomes blank. Any other mix of result types (`=IF(x, 1,
"one")`) is refused: a column needs one type.

## Conversions

| To | From Number | From Text | From Logical | From Date | From blank |
|---|---|---|---|---|---|
| Number | — | trimmed text parsed as a decimal number (`" 2 "`, `1e3`); otherwise `#VALUE!` | 1 / 0 | day serial (1900 system, 1900-03-01 = 61) | 0 |
| Text | shortest round-trip decimal (`0.30000000000000004`, `2`) | — | `TRUE` / `FALSE` | refused (use a date function) | `""` |
| Logical | `≠ 0` | `TRUE`/`FALSE` (any case); otherwise `#VALUE!` | — | serial `≠ 0` | `FALSE` |

Text-to-number conversion does not accept thousands separators, currency symbols, percent
signs or locale formats; sheets may. Number-to-text matches Alephgrid sheets for ordinary
magnitudes; numbers below 1e-5 or of 1e15 and above may be written differently.

## Comparison

Numbers compare numerically, text case-insensitively (by lowercase code points), logical
values `FALSE < TRUE`, dates chronologically. Across types, spreadsheet order applies:
Number < Text < Logical, so `"1" = 1` is `FALSE` and `"a" > 5` is `TRUE`. Dates compare with
numbers by serial. Blank takes the other side's zero.

## Errors

Each value carries an error code or none. Operators return the first error of their
operands, left to right, before their own (`#DIV/0!` for division by zero, `#VALUE!` for text
that is not a number, `#NUM!` for non-finite results and invalid powers). `IF` and `IFS`
evaluate only the chosen branch; `IFERROR` replaces any error, `IFNA` only `#N/A`;
`ISERROR`, `ISERR` and `ISNA` inspect errors without propagating them. SQL `NULL` never
stands for an error. Error cells read back with their code and show it in the grid.

## Functions (first increment)

| Group | Functions |
|---|---|
| Logic | `IF` (else branch required unless the result is logical), `IFS`, `IFERROR`, `IFNA`, `AND`, `OR`, `NOT` |
| Information | `ISBLANK`, `ISNUMBER`, `ISTEXT`, `ISLOGICAL`, `ISERROR`, `ISERR`, `ISNA` |
| Math | `ABS`, `ROUND`, `ROUNDUP`, `ROUNDDOWN`, `INT`, `MOD`, `SUM`, `MIN`, `MAX`, `AVERAGE` (row values only) |
| Text | `LEFT`, `RIGHT`, `MID`, `LEN`, `TRIM`, `UPPER`, `LOWER`, `SUBSTITUTE`, `CONCAT` |
| Date | `DATE`, `YEAR`, `MONTH`, `DAY`, `EDATE`, `EOMONTH` |

In `SUM`, `MIN`, `MAX`, `AVERAGE`, `AND` and `OR`, a column value is a reference: text and
blank values are skipped as a sheet skips them in referenced cells, while typed literals are
converted. Volatile functions (`TODAY`, `NOW`, `RAND`) are refused until F.10 defines a
calculation epoch.

Lookups (`XLOOKUP`, `VLOOKUP`, `XMATCH`) over whole columns are F.06; conditional aggregates
(`SUMIFS`, `COUNTIFS`, `AVERAGEIFS`) are F.08; recalculation and dependencies across tables and
sheets are F.09.

## Storage, reads and editing

Storage format 6 adds `language` (`sql` or `sheet`) to `_rf_columns`; opening an older
workbook migrates it in one transaction after the usual `.v<N>.bak` copy, and every existing
formula column stays `sql`. A Alephgrid build that only knows format 5 refuses format-6 files.

A `sheet` column adds two columns to the table's base relation: its typed value and a hidden
`_rf_e_<name>` error column (names starting with `_rf_e_` are reserved). Formulas are layered
in column order, so a formula may read earlier formula columns; it then receives their
errors too. Fetched row blocks list error cells separately (`errors: [{row, column, code}]`)
while the value stays `null`, so sorting, filters and statistics treat errors like blanks and
the grid shows the code. Renaming a column rewrites `[@old]`/`[old]` references in spreadsheet
formulas (other text is kept); deleting a column that a formula reads is refused; a formula
cannot read its own column. **Add column** offers *Spreadsheet formula* (the default) and *SQL
formula*; the formula bar edits either kind in its own language.

Not yet: exports write blank for error cells (F.13 defines XLSX/CSV error output), and
formulas reading other tables (F.06/F.08) or sheet cells (F.09).

## Row identity and order

Rows keep their record id (`_rf_id`) and persisted order key (`_rf_ord`) across edits. A
row-context formula depends only on its own row, so view sorting and filtering cannot change
its value. Order-dependent operations (first/last match in lookups) use the persisted order,
never a view's sort or DuckDB's physical order (F.06).

## Verification

`table_formula` unit tests cover parsing (precedence, escapes, spans, refusals), typing and
golden values. A differential test evaluates every formula of a corpus on random typed rows in
DuckDB and in IronCalc (the sheet engine, an independent implementation) and requires equal
values and error codes after the documented normalizations (blank results, shortest decimal
text). Differences found that are not documented above fail the test.
