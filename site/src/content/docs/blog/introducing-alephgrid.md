---
title: "Introducing Alephgrid: room for every row"
date: 2026-11-01
authors: alephgrid
excerpt: An open-source spreadsheet for data that doesn't fit in a spreadsheet — big tables streamed from DuckDB, real Excel formulas, SQL when you want it, running entirely on your own machine.
---

Spreadsheets are the most successful data tool ever made, and they stop working the moment your
data gets big. Databases handle the big data and stop working the moment you want to *touch* it —
format a column, write `=SUMIFS(...)`, drag a fill handle.

Alephgrid is our attempt to remove that trade-off. It's an open-source (Apache-2.0) spreadsheet
that opens large CSV and Parquet files, keeps them scrollable and editable in your browser
without loading them into memory, and puts an ordinary, Excel-compatible spreadsheet right next to them —
with formulas that can read the big tables directly.

## How it works

One Rust binary with the UI embedded. Every workbook is a DuckDB file on your machine. Opening a
workbook reads nothing into memory: the grid requests only the rows on screen, sorts build a
permutation instead of a copy, and calculated columns compute only what you look at.

Sheets run on [IronCalc](https://github.com/ironcalc/IronCalc), an Excel-compatible engine, so
`XLOOKUP`, `SUMIFS`, dynamic arrays, `LET` and `LAMBDA` behave the way you expect. And a sheet
formula like `=SUMIFS(orders[amount], orders[region], A2)` compiles to one query over the whole
table — not one lookup per row.

## What's in the first release

v0.1.0 is being prepared now. It ships sheets with formatting, charts, comments and XLSX
round-trip; big tables with sort, filters, calculated columns and the Analyze pane; reusable
functions in sheet-formula, SQL or sandboxed Python form; an optional SQL workspace; teams with
spaces, roles, SSO, quotas, audit and backups; and read-only
[MCP access](https://github.com/alephgrid/alephgrid/blob/main/docs/MCP.md) so AI agents can query
a workbook with a content-hashed receipt for every result.

There are no seats, tiers or paywalls. Export, sign-in, teams — everything is free.

## Try it today

Until binaries ship, you can [build from source](../../docs/getting-started/) in a few minutes
(plus one DuckDB compile):

```bash
git clone https://github.com/alephgrid/alephgrid && cd alephgrid
(cd web && npm install && npm run build)
cargo build --release
./target/release/alephgrid --demo-rows 10000000
```

Follow the [releases page](../../releases/) for v0.1.0, and if something breaks, an issue with a
reproducible file is the most valuable thing you can send us.
