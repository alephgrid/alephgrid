---
title: File compatibility
description: What Alephgrid imports and exports, and how XLSX round-trips.
---

## Import

| Format | Becomes |
|---|---|
| CSV / TSV (auto-detected) | Table |
| Parquet | Table |
| JSON / JSONL | Table |
| gzip of the above | Table |
| XLSX | Editable sheets |

Imports are streamed and reviewed in a side pane with column types and samples before commit.
Drag and drop or pass a path on the command line.

## Export

| Source | Formats |
|---|---|
| Any table or filtered view | CSV, Parquet, JSON lines |
| Sheets | XLSX, with a report of anything XLSX cannot hold |

## XLSX round-trip

Formatting (bold, italic, underline, strikethrough, sizes, colours, borders, alignment, wrap,
number formats, merged cells) and layout (frozen rows and columns, sizes, hidden rows and
columns) are kept through XLSX import and export.

## Storage

Every workbook is one DuckDB file, sheets included, in its own folder of a plain data directory
(`--data-dir`, default `./alephgrid-data`). Nothing is uploaded anywhere. Back up with
`--backup-to` or from Administration rather than copying files of a running server; see the
[operations guide](https://github.com/alephgrid/alephgrid/blob/main/docs/OPERATIONS.md).
