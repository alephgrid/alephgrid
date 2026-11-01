---
title: Importing and exporting data
description: Supported file formats, the streamed import review, exports, and where your data lives on disk.
---

Alephgrid imports files as tables or sheets and exports them back out. Imports are streamed —
a file is never read into memory whole — and reviewed before anything is created.

## What you can import

| Format | Becomes | Notes |
|---|---|---|
| CSV / TSV | Table | Delimiter and types auto-detected |
| Parquet | Table | Read by DuckDB's native reader |
| JSON / JSONL | Table | One table per file |
| gzip | Table | Compressed CSV/TSV/JSON, decompressed on the fly |
| XLSX | Editable sheets | One sheet per worksheet, with formulas and formatting |

Two ways to get a file in:

- **Drag and drop.** Drop a file onto an open workbook, or anywhere on the hub page.
- **Command line.** Pass the file when you launch:

```bash
alephgrid flights.parquet   # opens with the file loaded
```

## The import review pane

Before an import creates anything, the file is staged and you review it in a side pane:

- **Column types and samples.** The detected columns with their types and the first rows,
  so you can see what the reader made of the file.
- **Type overrides.** Change a column's type before importing. For CSV the override is
  applied inside DuckDB's CSV reader, so text such as `007` stays exact instead of becoming
  `7`; for Parquet and JSON it is applied as a cast.
- **Progress and cancellation.** The import runs as a job that reports progress and can be
  interrupted — only that job, nothing else.
- **Errors you can fix.** A value the reader cannot parse comes back as a structured problem
  (line, column, value, suggested type), so you can change the type and retry the same
  upload instead of starting over.

An XLSX file previews the sheets it would add and lists what will not come across: charts,
pivots, macros, links, comments and unsupported functions.

## What you can export

| Format | Of what |
|---|---|
| CSV | Any table, or the current filtered view |
| Parquet | Any table, or the current filtered view |
| JSON lines | Any table, or the current filtered view |
| XLSX | Sheets, with a fidelity report |

Exporting a filtered view writes the rows you see: hidden columns stay hidden and the raw
values are kept, regardless of display formatting. Exports stream straight from a single
snapshot of the data to the file — nothing is held in memory — so exporting a
hundred-million-row table works the same as a small one.

The XLSX export comes with a report of anything XLSX cannot hold; today charts and some
styles are reported rather than exported.

The SQL workspace can also export the complete result of a query — see
[the SQL workspace](../sql-workspace/).

## Where data lives on disk

Everything is plain files in one directory, `./alephgrid-data` by default (change it with
`--data-dir` or `ALEPHGRID_DATA_DIR`):

```
alephgrid-data/
  <workbook-id>/
    workbook.json   # name, created
    data.duckdb     # tables, sheets, formulas
  .trash/<id>/      # deleted workbooks, restorable
```

Each workbook is one DuckDB file holding its tables, sheets, formulas and saved queries.
Deleting a workbook moves its directory to `.trash/`, where it stays restorable until
purged. Back up the data directory and you have backed up everything.

Details of the on-disk format are in the repository's
[architecture guide](https://github.com/alephgrid/alephgrid/blob/main/docs/ARCHITECTURE.md).

## Next

- [Analyze without SQL](../analyze/)
- [Big tables](../big-tables/)
- [Install and first workbook](../getting-started/)
