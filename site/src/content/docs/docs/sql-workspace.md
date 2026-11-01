---
title: The SQL workspace
description: A docked SQL console with a schema browser, parameters, saved and live queries, and a strict boundary around the workbook's own data.
---

Alephgrid never requires SQL — filters, pivots, joins and charts all work from the
[Analyze pane](../analyze/). When you want it, the SQL workspace docks under the grid. It
stays closed until you open it from **Tools → SQL workspace** or the toolbar; drag its top
edge to resize, maximize it over the grid, close it with its button or Escape.

## Writing queries

- **Schema browser.** The workbook's tables and columns, right next to the editor.
- **Autocomplete** for table and column names as you type.
- **`$parameters`.** Name a parameter in the query and fill it in before running:

```sql
SELECT carrier, count(*) AS flights
FROM flights
WHERE origin = $origin          -- bound as a value
GROUP BY carrier
ORDER BY flights DESC;
```

Parameters are bound through the prepared-statement API with an exact type — an integer, a
decimal, a date — and never become SQL text.

- **Cancellable previews.** A preview reads at most 5,000 rows from one snapshot and reports
  whether it was truncated, the time taken, and the data revision it read. A running query
  can be cancelled at any time.

## Saved queries, live queries, tables

Three related things, kept distinct:

| Kind | Behaviour |
|---|---|
| Saved query | Text plus parameters, with immutable versions |
| Live query | Re-run on current data whenever opened; marked stale when a source table changes |
| Table from a query | A snapshot with provenance, refreshed only on request |

A table created from a query records the SQL, parameters, sources and the revisions it was
computed at, and is listed as stale or edited when those move. A refresh is all or nothing;
if the query's result columns have changed, it fails with `schema_changed` and changes
nothing. A query result can also open as a sheet — summarize 100M rows, choose *Open in
sheet*, then model on top with ordinary formulas.

## Exports

The workspace exports the complete result of a query — not only the preview — as CSV,
Parquet or JSON lines. The result streams from one snapshot straight to the file, so size is
bounded by disk, not memory. See [importing and exporting data](../importing-data/).

## What SQL can touch

A query reads the workbook's own tables only. The statement is checked against DuckDB's own
parse tree before it runs: no internal `_rf_*` tables, no other catalogs or schemas, no
system views, and no files or URLs. The workbook's DuckDB instance has external access
switched off while open, so SQL cannot reach the filesystem or the network even in
principle.

Changes are allowed in a constrained form: `INSERT INTO`, `UPDATE … SET … WHERE` and
`DELETE FROM … WHERE` are validated the same way and applied like ordinary edits, with one
revision and history entry. Everything else that would change data or schema is refused.

## Turning SQL off

A server can disable user-written SQL entirely:

```bash
alephgrid --disable-sql         # or ALEPHGRID_DISABLE_SQL
```

With it set, every operation that runs user-written SQL is refused: the console, saved
queries, tables from queries, SQL formula columns and SQL functions. The
[Analyze pane](../analyze/) keeps working, because its plans are typed and validated rather
than free-form SQL.

The policy mechanics are documented in the repository's
[architecture guide](https://github.com/alephgrid/alephgrid/blob/main/docs/ARCHITECTURE.md).

## Next

- [Analyze without SQL](../analyze/)
- [Big tables](../big-tables/)
- [Self-hosting](../self-hosting/)
