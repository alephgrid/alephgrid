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
| Whole table | `Customers`, `Customers[#Data]` | Only as VLOOKUP's table; a bare name is a table, as in spreadsheets |
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

**Dates hold whole days.** A Date plus a fraction keeps the date and drops the fraction, where a
sheet would keep a time of day (`[@d] + 0.25`); a Date result before the first day (a blank
date plus a fraction below 1) is `#NUM!`.

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
`ISERROR`, `ISERR` and `ISNA` inspect errors without propagating them. `AND` and `OR` return an
error argument even after a deciding `FALSE` (`AND`) or `TRUE` (`OR`); IronCalc sheets stop at
the deciding value (see [gaps](FORMULA_GAPS.md#and-and-or-with-an-error-after-the-deciding-value)). SQL `NULL` never
stands for an error. Error cells read back with their code and show it in the grid.

## Functions (first increment)

| Group | Functions |
|---|---|
| Logic | `IF` (else branch required unless the result is logical), `IFS`, `IFERROR`, `IFNA`, `AND`, `OR`, `NOT` |
| Information | `ISBLANK`, `ISNUMBER`, `ISTEXT`, `ISLOGICAL`, `ISERROR`, `ISERR`, `ISNA` |
| Math | `ABS`, `ROUND`, `ROUNDUP`, `ROUNDDOWN`, `INT`, `MOD`, `SUM`, `MIN`, `MAX`, `AVERAGE` (row values, or whole columns), `COUNT`, `COUNTA` (whole columns) |
| Text | `LEFT`, `RIGHT`, `MID`, `LEN`, `TRIM`, `UPPER`, `LOWER`, `SUBSTITUTE`, `CONCAT` |
| Date | `DATE`, `YEAR`, `MONTH`, `DAY`, `EDATE`, `EOMONTH` |

In `SUM`, `MIN`, `MAX`, `AVERAGE`, `AND` and `OR`, a column value is a reference: text and
blank values are skipped as a sheet skips them in referenced cells, while typed literals are
converted. Volatile functions (`TODAY`, `NOW`, `RAND`) are refused until F.10 defines a
calculation epoch.

Conditional aggregates are described below; formulas reading sheet cells under
[Sheet cells](#sheet-cells-f09).

## Conditional aggregates (F.08)

```text
=SUMIFS(Orders[amount], Orders[customer_id], [@id])
=COUNTIFS(Orders[customer_id], [@id], Orders[amount], ">100")
=AVERAGEIFS(Orders[amount], Orders[channel], "w*", Orders[day], ">="&[@start])
=SUMIF(Orders[customer_id], [@id], Orders[amount])      COUNTIF, AVERAGEIF likewise
=MINIFS(Orders[amount], Orders[customer_id], [@id])     MAXIFS likewise
=SUM(Orders[amount])      AVERAGE, MIN, MAX, COUNT, COUNTA of whole columns likewise
```

All ranges are whole columns of one table (another table, or the formula's own stored
columns). The result is a Number: `SUMIFS`, `COUNTIFS`, `MINIFS` and `MAXIFS` give 0 when
nothing matches, `AVERAGEIFS` gives `#DIV/0!`. Only number and date cells are summed or averaged (text and
logical cells are skipped); an error cell in the aggregated column of a matching row makes the
result that error (the first in row order).

**Criteria** follow the sheet engine's rules exactly (differential-tested):

| Criterion | Matches |
|---|---|
| a number (or a date) | equal numbers, and text that reads as that number (`"2"`) |
| `">5"`, `"<=5"`, `"<>5"`… | numbers only for `<`/`<=`/`>`/`>=`; `<>5` matches everything except 5 |
| text, `"=text"` | text equal to it, case-insensitively; `""` also matches blank cells |
| `"<>"` | every non-blank cell (`""` included) |
| `"w*"`, `"?"`, `"a~*b"` | wildcards for `=` and `<>`: `*` any run, `?` one character, `~` escapes |
| `"<b"`, `">=b"` | text ordering (lowercase code points); numbers never match |
| `TRUE`, `"TRUE"` | logical values only |
| an error value, `"#N/A"` | that error |
| a blank criterion cell | numbers equal to 0 |
| text that is a date (`"<7/31/2023"`, `">=2024-01-01"`, `"Feb/29/2024"`) | compared as that date: `yyyy-mm-dd`, otherwise month first, English month names |

A criterion may come from the row (`[@id]`, `">"&[@limit]`; a date concatenated to an operator
uses its serial number, as in sheets). Criteria written as literals filter the source once.

**How it runs.** When every row criterion is a number, date or logical value, the criteria can
only test equality: the source is grouped once by its cells' match keys (a cell contributes its
own key and its blank equivalents) and left-joined on the row's keys, cached like a lookup.
Text criteria may carry operators or wildcards from the data, so each *distinct* criterion
value is matched once: equality ones through the grouped keys, the others by comparison
against the source, then joined back to every row. No query runs per row or per cell.
Measured at 1M orders / 100K customers (release build, `lookup_read_timings`): a page of
customers with equality `SUMIFS` and `COUNTIFS` columns takes 45–57 ms; with a text criterion
taken from each row, 347 ms (not cached).

**Whole columns.** `SUM`, `AVERAGE`, `MIN`, `MAX`, `COUNT` and `COUNTA` take whole columns of
one table (`SUM(Orders[amount])`, `MAX(Orders[a], Orders[b])`) and give the same value on every
row, computed in one scan (one aggregate relation joined to the rows). As with ranges in sheets,
text and logical cells are skipped; `COUNTA` counts every non-blank cell, `""` and errors
included; the first error in argument, then row, order is the result, except for `COUNT` and
`COUNTA`. An empty table gives 0 (`AVERAGE`: `#DIV/0!`). Whole columns and row values cannot be
mixed in one call (`SUM(Orders[a], [@b])` is refused; write `SUM(Orders[a]) + [@b]`).
Differential-tested against sheet ranges (20 formulas), with hand-checked values.

Not available yet, refused with a message: ranges from different tables, and array criteria.

## Lookups (F.06)

A formula can look up rows of another table (or of its own table's stored columns):

```text
=XLOOKUP([@customer_id], Customers[id], Customers[name], "unknown")
=XLOOKUP([@customer_id], Customers[id], Customers[name], , 0, -1)        last match
=VLOOKUP([@customer_id], Customers, 3, FALSE)
=XLOOKUP(1, (Prices[sku]=[@sku])*(Prices[region]=[@region]), Prices[price])
=XLOOKUP(TRUE, Prices[sku]=[@sku], Prices[price])
```

| Form | Meaning |
|---|---|
| `XLOOKUP(value, T[key], T[result], [if_not_found], [0], [1 or -1])` | Exact match on one column. `if_not_found` defaults to `#N/A`; search mode 1 takes the first match, -1 the last. |
| `VLOOKUP(value, T, n, FALSE)` | Exact match on `T`'s first column, returning its `n`th column in the table's column order (stored columns, then formula columns). `n` must be written in the formula; `n` < 1 is `#VALUE!` and `n` past the last column `#REF!`, as in sheets; a fractional `n` is truncated. |
| `XLOOKUP(1, (T[a]=x)*(T[b]=y)…, T[result], …)` | Several keys, written as in spreadsheets: a row matches when every comparison is true. Each comparison keeps its own type rules, so `"1"` never equals `1` and keys are never glued into text. |
| `XLOOKUP(TRUE, T[a]=x, T[result], …)` | One comparison, the same as `XLOOKUP(x, T[a], …)`. |

`T` alone, or `T[#Data]`, is the whole table. Matching follows spreadsheet exact matching:
numbers at 15 significant digits, text case-insensitively, a Date equal to its serial, values
of different types never equal, and a blank matching `0`, `""`, `FALSE` and blank cells. Rows
whose key holds an error never match. "First" and "last" mean the table's persisted row order
(`_rf_ord`), never a view's sort, so a view cannot change a lookup's result.

Not available yet, refused with a message: approximate and wildcard match modes, binary
search, `VLOOKUP` without `FALSE` (spreadsheets default to approximate match), `HLOOKUP`,
`XMATCH`, `MATCH`, `INDEX`, `LOOKUP`, column ranges such as `T[[a]:[c]]`, and computed column
numbers. Spreadsheet `VLOOKUP` treats `*`, `?` and `~` in a text value as a pattern even in
exact mode; such values give `#VALUE!` here rather than a literal match (use `XLOOKUP`, whose
exact mode compares them literally).

**How it runs.** Each lookup compiles to one grouped relation of the other table, built in a
single scan: every row contributes the keys it matches (its own value, plus the blank
equivalents), `row_number()` over the persisted order keeps one row per key, and the result is
left-joined to the formula's table on the row's key. Every row therefore gets at most one
result, and no query runs per row or per cell.

**Dependencies across tables.** Renaming a column or table rewrites `T[column]` and `T`
references in every table's formulas. A table whose formulas are referenced can only be
renamed to a name formulas can spell (letters, digits, `_`, `.`, not starting with a digit).
Deleting a column that another table's formula reads is refused, and so is deleting a table
that formulas read. Because `VLOOKUP` reads by position, deleting a column at or before the
column it returns is refused too. Tables may look each other up (orders read customers, and
customers read orders) as long as no column depends on itself; a formula that would close
such a loop is refused with the path, e.g. `circular reference: customers.label →
orders.label → customers.label`. While a lookup relation is built, a column that depends on
the column being computed shows `#REF!` there instead of looping.

A lookup into the formula's own table sees its stored columns only, not its formula columns.

**Cached lookup results.** Building a grouped relation scans the whole looked-up table, which
dominated every page read of a large table. Each grouped relation is therefore kept in the
workbook's in-memory scratch database, keyed by its SQL and by the data revisions of every
table it reads (the looked-up table, and tables its own formulas look up). Edits to the
formula's own table reuse it; an edit to a looked-up table makes the next read rebuild it.
Reads check or build it inside their own snapshot, so a page never mixes new rows with an old
lookup; the writer's transaction never uses or builds one. At most
`LOOKUP_CACHE_ROWS` (5,000,000) grouped rows are kept across a workbook's lookups, least
recently used first out; a larger lookup is evaluated uncached. The cache is not persisted.

Measured on an Apple M-series laptop (release build, `lookup_read_timings`; one XLOOKUP and
one VLOOKUP column on orders, looking up customers, about 5% of orders without a customer):

| Read | 1M orders / 100K customers, uncached | 10M orders / 1M customers, uncached | 10M / 1M, cached |
|---|---|---|---|
| Page of 100 rows (first, middle or last) | 43–47 ms | 182–189 ms | 45–58 ms |
| First page after editing one customer | 43 ms | 195 ms | 52 ms (after a one-time rebuild) |
| Rebuild both cached lookups | — | — | 120–420 ms, once per source change |
| Open a view sorted by a lookup column | 258 ms | 3.1 s | 3.5 s |

Sorting or filtering by a lookup column used to evaluate it for every row; large tables now
keep [stored formula results](#stored-formula-results-f09) for that.

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

Not yet: exports write blank for error cells (F.13 defines XLSX/CSV error output).

## Sheet cells (F.09)

A table formula can read single sheet cells, typically parameters kept in a sheet:

```text
=[@amount] * (1 + Settings!$B$1)
=[@amount] * 'FX rates'!C4
=IF(Settings!B3 = "", "none", Settings!B3)
```

The sheet name is required (`A1` alone is refused) and is quoted like in sheets when it is not
a plain name. `$` markers are kept but change nothing: a table formula is not copied between
cells. Ranges (`Settings!A1:B4`), whole columns and named ranges are refused. A cell is read
like a referenced cell: its computed value with its type (a number, text, logical or error;
dates are numbers, as in sheets), and an empty cell is blank, as an empty column value is.
Because the value has one type at a time, a cell changing from a number to text changes the
column's type.

**Snapshot reads.** Table reads never consult the live sheet model. Every committed change
that touches sheets, or the formulas that read them, also saves the value of every cell table
formulas read (`_rf_links`, storage format 7) in the same transaction. A read takes those values
from its own snapshot, so a page of rows and the cell values it was computed with always
belong to the same revision, on every connection.

**References follow their cells.** Each cell read has an anchor: a formula pointing at it in
a very hidden sheet, written outside the undo history. The sheet engine moves anchors as it
moves any formula: inserted and deleted rows and columns, cut-and-paste moves, sheet renames,
and undo or redo of those. After each change the table formulas are rewritten to match
(`Settings!$B$1` becomes `Settings!$B$2` after a row is inserted above it, `'Plan 2026'!$B$1`
after a rename), and a deleted cell becomes `#REF!`, as in sheets. As in IronCalc, undoing the
deletion does not bring the reference back. Deleting a sheet whose cells table formulas read is
refused with the columns that read it. The hidden sheet is not listed, cannot be addressed, and
is left out of XLSX exports.

**What a change reaches.** A sheet change that gives a read cell a new value changes the data
revision of the tables whose formulas read it; a new value type also changes their structure,
so their views are rebuilt. Unrelated sheet edits change no table. The server then tells every
session (the editing one included, `origin: 0`) which tables changed this way, and which tables
read them through lookups or conditional aggregates, so those refresh too. Saved query results
and live queries count the tables formulas read as sources, so a snapshot made from orders goes
stale when a customer it looked up changes.

## Sheet cells that read tables (F.09)

A sheet cell can compute over tables with the same formulas:

```text
=SUMIFS(orders[amount], orders[region], A1)
=XLOOKUP(Settings!B2, customers[id], customers[name], "unknown")
=SUM(orders[amount]) * 1.2
```

The sheet engine cannot read tables, so when it reports a name it does not know before `[`
(IronCalc's "Table not found"), Alephgrid compiles the whole formula as a table formula over a
row of nothing: whole columns, lookups, aggregates and sheet cells (on the same sheet, `A1`
needs no sheet name), but not `[@column]`, and not sheet ranges (`SUM(B1:B3)`) mixed in. A
formula that does not compile is refused with the place it goes wrong, before it is saved and
in the formula bar's check. Other formulas stay sheet formulas.

Such a *table cell* keeps its formula in `_rf_table_cells` with a result slot; the sheet engine
holds `=_rf_links!$B$<slot>`, a reference to the slot's value in the hidden link sheet, so
other sheet formulas read it like any cell (`=B1*2`). The formula bar and the grid show the
table formula instead (same-sheet cells without the sheet name). Moving, copying or undoing the
cell moves, copies or undoes that reference; slots are never reused, so redo finds the formula
again. A copy refers to the same formula: the cells it reads are not shifted. Values reach the
sheet with sheet types: numbers, text, logical values, errors, dates as serial numbers, and an
empty result as an empty cell (which reads as 0, as an empty lookup result does in sheets).

**Atomic recalculation.** Table cells are recomputed inside the transaction of every change
that could alter them: a change to a table they read (or that a table they read looks up), to
a sheet cell they read, or to their formula. A table cell's new value can change sheet cells
that table formulas read, so recalculation runs in rounds until nothing changes (bounded by the
number of table cells). If any step fails, the whole change is rolled back: tables, sheets and
values stay as they were committed. After a change that altered table cells the server tells
every session to refetch the sheets (`sheet_changed`, `sheet: null`, `origin: 0`).

**Circular references across sheets and tables** are refused, with the cells involved:
`Report!B1` reading `orders`, whose formula reads `Settings!B1`, which is `=Report!B1/1000`,
gives "circular reference: Report!B1 depends on its own value through the tables and sheet
cells its formula reads"; two table cells give their path (`Report!B2 → Report!B3 →
Report!B2`). The check follows table formulas, lookups between tables, and the sheet engine's
references, ranges, defined names and spilled arrays (at most 200,000 cells). References built
while evaluating (`INDIRECT`, `OFFSET`) cannot be followed statically; a loop through them
stops at the round limit with an error and the change is rolled back.

**Dependencies.** Renaming a table or column rewrites table cells too; deleting a table, a
column or a sheet they read is refused, naming the cells (`sheet cell Report!B1`).

**Export.** XLSX files cannot hold table formulas. A table cell is exported as its current
value, and the export returns one note per such cell ("Report!B1: the table formula … is
exported as its value "80" …"). Showing these notes in the download is F.13.

## Stored formula results (F.09)

Pages are cheap to compute live, but sorting, filtering or summarizing a view by a formula
column that reads other tables evaluates that column for every row. Tables of at least
`RESULTS_MIN_ROWS` (200,000) rows with such formulas therefore keep their formula results in the
workbook's scratch database:

* **Built from one snapshot**, stamped with the generation and data revision, in that snapshot,
  of every table they read: the table itself and every table its formulas look up or
  aggregate, transitively. Results are fresh exactly while the stamp matches the reader's
  snapshot; nothing is invalidated in place.
* **Installed atomically**, once complete, and only over older results. A view sorted or
  filtered by formula columns reads the result set its order was built from for all its pages,
  so order and values agree; when newer results are installed, it is rebuilt on its next read.
* **Recalculated in the background.** After a change commits, the server queues a bulk read job
  for each table whose results it made stale and sends `table_changed` (`origin: 0`) when the
  job ends. A newer change cancels the job still running for that table. `cancel_recalculation`
  cancels it on request and `recalculate` queues it again.
* **Visibly stale.** Meanwhile, and after a failed or cancelled build, sorted and filtered views
  keep the last committed results and say why (`stale` on view info and row blocks:
  `recalculating`, `recalculation failed: …`); the grid shows a notice with Cancel or
  Recalculate. Unsorted views read pages live, exactly. Exports and copies never write stale
  values: they read fresh results or compute live.

Sorting or filtering only by stored columns reads the table alone. Sorting by formula columns
with fresh results reads the stored results alone. Smaller tables compute everything live.
Measured at 1M orders / 100K customers (release build, `lookup_read_timings`): the first sort by
a lookup column 303 ms (it stores the results), later sorts 151 ms, a filter by a lookup column
26 ms, recalculation after a customer edit 165 ms; a sort while results are stale reads the last
results in 247 ms. 10M-row figures are F.T04.

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
text). Differences found that are not documented above fail the test. The same comparison
covers XLOOKUP, VLOOKUP and multi-key lookups against sheet ranges with duplicate keys, a blank
key, `0`, `""` and case variants. Workbook tests cover source edits, one result per row with
duplicate keys, renames, refused deletes, tables that look each other up and refused circular
references, before and after reopening.
