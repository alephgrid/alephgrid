---
title: Spreadsheets and formulas
description: Excel-syntax formulas, keyboard-first editing, formatting, charts and XLSX round-trip.
---

Sheets in Alephgrid use Excel syntax and an Excel-compatible engine
([IronCalc](https://github.com/ironcalc/IronCalc)), so formulas behave the way you expect.

## Formulas

- Excel functions including `XLOOKUP`, `SUMIFS`, dynamic arrays, `LET` and `LAMBDA`.
- Cross-sheet references (`=Budget!B4*2`) and named ranges.
- Formula autocomplete and argument help.
- Table reads from sheet cells: `=XLOOKUP(A2, customers[id], customers[region])` runs as one
  query over the whole table.
- Define your own reusable functions as sheet formulas, SQL expressions or sandboxed Python,
  with tests, published versions, pinning and rollback.

## Editing

Keyboard first: arrows, Shift and Mod selection, F2, Mod+A (the filled block, then the sheet),
copy, cut, paste with formulas translated, paste special (values, formulas, formats, transpose),
fill down and right, and a fill handle that continues series (1, 2 → 3, 4; Week 1 → Week 2) or
repeats patterns. The complete list lives in the repository's
[keyboard reference](https://github.com/alephgrid/alephgrid/blob/main/docs/KEYBOARD.md).

## Formatting and layout

Bold, italic, underline, strikethrough, sizes, colours, borders, alignment, wrap, number
formats and merged cells; frozen rows and columns, row and column sizes, hide and unhide, and
zoom 50–200%. All of it is kept through XLSX import and export.

## Sheet data tools

Find and replace (one sheet or all), sort and filter a range, colour rules and scales,
allowed-value lists, charts (column, bar, line, area, scatter, pie) previewed before they are
saved, and comments with replies.

## What is and isn't shipped

The full inventory, including what is partial or not planned, is the repository's
[feature inventory](https://github.com/alephgrid/alephgrid/blob/main/docs/SPREADSHEET_FEATURES.md).
