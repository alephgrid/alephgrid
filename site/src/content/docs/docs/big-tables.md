---
title: Big tables
description: How Alephgrid keeps hundreds of millions of rows responsive, and what you can do with them.
---

Open a CSV or Parquet file with hundreds of millions of rows, then scroll, sort, filter, edit
and add calculated columns in your browser.

## Why it stays fast

- Opening a workbook reads nothing into memory; the grid asks only for the rows on screen, so
  scrolling to row 900,000,000 is a range read.
- Sorts build a permutation, not a copy.
- Calculated columns (`=[fare_usd] / [distance_mi]`) are computed only for the rows you look at.
- Everything runs on a DuckDB file on your machine.

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
