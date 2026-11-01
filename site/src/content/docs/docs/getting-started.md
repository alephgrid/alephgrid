---
title: Install and first workbook
description: Build Alephgrid from source, launch it, import a file, write a formula and export.
---

Alephgrid is pre-release: the ready installation path today is building from source. Native
binaries, package managers and published container images arrive with v0.1.0 — the
[releases page](../../releases/) tracks exactly what is ready.

## Build from source

Requires **Rust 1.88+** and **Node 20+**.

```bash
git clone https://github.com/alephgrid/alephgrid && cd alephgrid
(cd web && npm install && npm run build)   # build the UI
cargo build --release
./target/release/alephgrid
```

The first build compiles DuckDB from source, which takes several minutes.

## Launch

```bash
./target/release/alephgrid flights.parquet   # opens http://127.0.0.1:7878
./target/release/alephgrid --demo-rows 10000000   # or 10M generated rows
```

On your own machine there is no sign-in and the server listens on `127.0.0.1` only. Data lives
in `./alephgrid-data` (change with `--data-dir`).

## Import a file

Drag a CSV, TSV, Parquet, JSON/JSONL or gzip file onto the window, or pass it on the command
line. The import is streamed and reviewed in a side pane with column types and samples before it
becomes a table. XLSX files import as editable sheets.

## Write a formula

Sheets use Excel syntax: `=SUM(B2:B9)`, `=XLOOKUP(...)`, `=Budget!B4*2`. A sheet formula can
read a whole table by name:

```
=SUMIFS(orders[amount], orders[region], A2)
```

That runs as one query over the whole table — not one lookup per row — however many rows the
table has.

## Save and export

Everything persists automatically in the workbook's DuckDB file. Export any table or filtered
view as CSV, Parquet or JSON lines, and sheets as XLSX (with a report of anything XLSX cannot
hold).

## Next

- [Spreadsheets and formulas](../spreadsheets/)
- [Big tables](../big-tables/)
- [Self-hosting](../self-hosting/)
