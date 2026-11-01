---
title: Table formulas
description: Spreadsheet-syntax formulas over whole tables, evaluated by DuckDB as single queries — syntax, supported functions, lookups, aggregates and limits.
---

A table formula is a calculated column written in spreadsheet syntax and evaluated by DuckDB
over the whole table, without copying rows into sheet cells:

```text
=IF([@amount] > 100, "large", "small")
=IFERROR([@revenue] / [@units], "")
=ROUND([@price] * (1 + [@tax_rate]), 2)
```

Sheet cells can compute over tables with the same formulas — `=SUMIFS(orders[amount],
orders[region], A1)` in a sheet cell runs as one query over the whole table, not one lookup
per row. Each formula column records its language (`sql` or `sheet`); existing SQL calculated
columns keep working unchanged. Anything the compiler does not support is refused with an
explanation and the character position, never approximated.

## Syntax

| Element | Forms | Notes |
|---|---|---|
| Numbers | `12`, `1.5`, `.5`, `2E3`, `50%` | `%` divides by 100 |
| Text | `"say ""hi"""` | Doubled quotes escape a quote |
| Logical | `TRUE`, `FALSE` | Case-insensitive |
| Errors | `#N/A`, `#DIV/0!`, `#VALUE!`, `#NUM!`, `#REF!`, `#NAME?`, `#NULL!` | |
| This row | `[@amount]`, `[@[unit price]]`, `[amount]` | `'` escapes `[`, `]`, `#`, `'` inside brackets |
| Whole column | `Orders[amount]` | Where a function expects a range (lookups, aggregates) |
| Whole table | `Customers` | Only as VLOOKUP's table |
| Operators | `-x` `x%` `^` `*` `/` `+` `-` `&` `=` `<>` `<` `<=` `>` `>=` | Spreadsheet precedence: `-2^2` is 4 |

Ranges (`A1:B4`), bare sheet cell references, named ranges, array constants and the
intersection operator are refused in table formulas. Every value has one static type
(Number, Text, Logical or Date), and a column has one type for all rows — `=IF(x, 1, "one")`
is refused. Blank (`NULL`) stays blank in a blank result, where a sheet would display `0`.

## Supported functions

| Group | Functions |
|---|---|
| Logic | `IF`, `IFS`, `IFERROR`, `IFNA`, `AND`, `OR`, `NOT` |
| Information | `ISBLANK`, `ISNUMBER`, `ISTEXT`, `ISLOGICAL`, `ISERROR`, `ISERR`, `ISNA` |
| Math | `ABS`, `ROUND`, `ROUNDUP`, `ROUNDDOWN`, `INT`, `MOD`, `SUM`, `MIN`, `MAX`, `AVERAGE`, `COUNT`, `COUNTA` |
| Text | `LEFT`, `RIGHT`, `MID`, `LEN`, `TRIM`, `UPPER`, `LOWER`, `SUBSTITUTE`, `CONCAT` |
| Date | `DATE`, `YEAR`, `MONTH`, `DAY`, `EDATE`, `EOMONTH` |
| Conditional aggregates | `SUMIF(S)`, `COUNTIF(S)`, `AVERAGEIF(S)`, `MINIFS`, `MAXIFS` |
| Lookups | `XLOOKUP`, `VLOOKUP`, `INDEX`, `MATCH`, `XMATCH` |

Volatile functions (`TODAY`, `NOW`, `RAND`, `RANDBETWEEN`, `RANDARRAY`) are refused in table
formulas: results are cached and recomputed only when their inputs change, so a value that
changes on its own would be silently stale. Ordinary sheet cells keep the sheet engine's
volatile behaviour.

## Lookups and aggregates across tables

A formula can look up rows of another table, or aggregate its columns:

```text
=XLOOKUP([@customer_id], Customers[id], Customers[name], "unknown")
=VLOOKUP([@customer_id], Customers, 3, FALSE)
=XLOOKUP(1, (Prices[sku]=[@sku])*(Prices[region]=[@region]), Prices[price])
=SUMIFS(Orders[amount], Orders[customer_id], [@id])
=COUNTIFS(Orders[customer_id], [@id], Orders[amount], ">100")
=SUM(Orders[amount])
```

Each lookup or aggregate compiles to one grouped relation built in a single scan and joined
back to the rows — no query runs per row or per cell, and only the rows a client asks for are
transferred. Nearest-match (`match_mode` -1 or 1), wildcard (`match_mode` 2) and sorted
(`VLOOKUP(…, TRUE)`, `MATCH`) modes follow the sheet engine and are differential-tested against
it. Criteria follow sheet rules exactly, wildcards included (`"w*"`, `"?"`, `~` escapes).

"First" and "last" match mean the table's persisted row order, never a view's sort, so sorting
a view cannot change a lookup's result. Grouped relations are cached in memory and rebuilt when
a looked-up table changes; measured at 1M orders / 100K customers, a page with lookup and
`SUMIFS` columns reads in well under 100 ms.

Refused with a message: `HLOOKUP`, `LOOKUP`, binary-search modes, column ranges such as
`T[[a]:[c]]`, computed column numbers, ranges from different tables in one aggregate, array
criteria, and mixing whole columns with row values in one call (`SUM(Orders[a], [@b])` —
write `SUM(Orders[a]) + [@b]`).

## Sheet cells as parameters

A table formula can read single sheet cells, typically parameters kept in a settings sheet:

```text
=[@amount] * (1 + Settings!$B$1)
=IF(Settings!B3 = "", "none", Settings!B3)
```

The sheet name is required; ranges, whole sheet columns and named ranges are refused.
References follow their cells: inserting rows, cut-and-paste moves and sheet renames rewrite
the formula to match, and a deleted cell becomes `#REF!`, as in sheets. A sheet change that
gives a read cell a new value refreshes every table that reads it, on every connection.

## Spilling table results into sheets

A sheet cell can spill rows of a table as a dynamic array:

```text
=SORT(UNIQUE(orders[region]))
=FILTER(orders[amount], orders[region]="West", "none")
=TAKE(SORT(orders, 3, -1), 10)
```

`FILTER`, `SORT`, `UNIQUE` and `TAKE` over whole columns of one table compile to one ordered
query. Spills are capped at 10,000 cells; a larger result is refused when entered, pointing
at the route for big results (sort and filter the table's view, and create a table from it).

## Copying, renaming and references

A table formula is one formula for the whole column, so it is not copied between cells and
its references never shift: `$` markers in sheet-cell references are kept but change nothing.
In ordinary sheet cells, copy and paste translate references as you expect; a copied *table
cell* (a sheet cell computing over tables) refers to the same formula, so the cells it reads
are not shifted.

Renaming a column or table rewrites every formula that references it, in every table. Deleting
a column, table or sheet that formulas read is refused, naming what reads it. Circular
references across tables and sheets are refused with the path involved. Formula columns are
layered in column order, so a formula may read earlier formula columns but never its own.

## Limits and caveats

- Numbers are IEEE doubles, as in sheets: `BIGINT`, `HUGEINT` and `DECIMAL` values convert
  to `DOUBLE` when used, so integers beyond ±2^53 and more than 15–17 significant digits
  lose precision in the formula (the source column is unchanged).
- Dates hold whole days: a Date plus a fraction keeps the date and drops the fraction,
  where a sheet would keep a time of day.
- Columns of other types (timestamps, lists, structs, blobs, UUIDs) are refused when
  referenced.
- Text-to-number conversion does not accept thousands separators, currency symbols or
  locale formats; sheets may.
- `AND` and `OR` evaluate every argument, so an error after the deciding value is returned,
  where the sheet engine stops at the deciding value. See
  [formula compatibility](../reference/formulas/) for this and other known differences.
- XLSX files cannot hold table formulas: a table cell is exported as its current value, and
  the download lists one note per such cell before anything is written.

:::note
Every supported formula is differential-tested against the sheet engine
([IronCalc](https://github.com/ironcalc/IronCalc)) on the same data; differences that are not
documented fail the test. The full contract is the repository's
[table formula specification](https://github.com/alephgrid/alephgrid/blob/main/docs/TABLE_FORMULAS.md).
:::
