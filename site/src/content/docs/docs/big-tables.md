---
title: Big tables
description: How Alephgrid keeps tables with millions of rows responsive, what was measured, and what you can do with them.
---

Open a large CSV or Parquet file, then scroll, sort, filter, edit and add calculated columns in
your browser. The table stays in DuckDB on disk; the grid streams only the rows you look at.

## Why it stays fast

- Opening a workbook reads nothing into memory; the grid asks only for the rows on screen, so
  jumping deep into a table is a range read, not a load.
- Sorts build a permutation, not a copy.
- Calculated columns (`=[fare_usd] / [distance_mi]`) are computed only for the rows you look at.
- Everything runs on a DuckDB file on your machine.

## Measured

On a 20-million-row, 9-column table, with a debug build on an 8-core laptop with 8 GB of RAM:

| Operation | Time |
| --- | --- |
| Reopen the workbook after a restart | 3 ms |
| Fetch 100 rows at row 0, 10M or 19.99M | 3–11 ms |
| Filter `carrier = DL AND origin = SEA` (125k matches) | 34 ms |
| Sort all 20M rows by a float column | 0.95 s |
| Sum, average, min and max of a column over all rows | 28 ms |
| `GROUP BY carrier` in the SQL workspace | 39–57 ms |
| Edit 1 cell / paste 1,000 cells | 3 ms / 43 ms |

Reproduce with `alephgrid --demo-rows 20000000` and
[`docs/bench.mjs`](https://github.com/alephgrid/alephgrid/blob/main/docs/bench.mjs). Release-build
benchmarks over varied data shapes will be published with v0.1.0.

## Working with tables

- Typed edits, insert and delete rows; add, rename and delete columns.
- Multi-column sort and filters over the whole table; header value filters with counts; saved
  views.
- The **Analyze** pane: summaries, pivots, joins, charts and column profiles without SQL;
  deduplicate, split, trim and convert types with previews.
- Live sum, average and count of the current selection.

## Spreadsheet formulas over big tables

`=SUMIFS(orders[amount], orders[region], A2)` in a sheet cell runs as one query over the whole
table. See the repository's
[table formulas guide](https://github.com/alephgrid/alephgrid/blob/main/docs/TABLE_FORMULAS.md).

## SQL when you want it

Filters, pivots, joins and charts work without SQL. When you want it, the SQL workspace docks
under the grid with a schema browser, autocomplete, `$parameters`, cancellable previews, and
saved queries with versions. It reads the workbook's own tables only, and a server can turn it
off entirely with `--disable-sql`.

## Compute

Each open workbook gets its own share of memory and threads, sleeps when idle and wakes when
opened. Admins can grant larger sizes with `--profile`. Details:
[compute guide](https://github.com/alephgrid/alephgrid/blob/main/docs/COMPUTE.md).
