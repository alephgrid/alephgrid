# Spreadsheet feature inventory

F.04e: what a spreadsheet user can do in Alephgrid sheets and tables today, and what is not
there yet. Full Excel or Google Sheets compatibility is a long-term goal, not a claim. Each
row points at the task that tracks it. **Shipped** means a test exercises it (named in the
task); **Partial** names what is missing; **Planned** and **Not planned** are not available.

## Selection, clipboard and editing

| Feature | State | Notes and task |
| --- | --- | --- |
| Type over a cell, F2 / double-click to edit, Enter / Tab / Escape | Shipped | F.03, [KEYBOARD.md](KEYBOARD.md) |
| Arrows, Shift+arrows, Mod+arrows, Home/End, PageUp/PageDown | Shipped | Grid keys |
| Select whole rows / columns (Shift+Space / Ctrl+Space), the corner selects all | Shipped | |
| Mod+A: the filled block first, then the whole sheet | Shipped | F.04a, `e2e/fillhandle.spec.ts` |
| Copy / cut / paste inside a workbook, formulas translated, cut moves cells and their references | Shipped | F.04, `e2e/commands.spec.ts` |
| Copied-range outline (until Escape) | Shipped | F.04b |
| Paste text from other apps (tab-separated) | Partial | Pastes as values; formulas and formats from other apps are not kept |
| Paste special: all / formulas / values / formats, transpose | Shipped | F.04d, a menu at the active cell |
| Fill down / right (Mod+D / Mod+R) | Shipped | F.04 |
| Fill handle drag: series, counting text, repeated patterns, any direction | Shipped | F.04c, one undo step |
| Copy, fill or drag a formula that reads a table (`=XLOOKUP(A2, customers[id], …)`) | Shipped | References shift like any formula; `$` parts stay. Pasted onto another sheet, its cell references keep naming the original sheet. A table formula that spills into several cells cannot be copied yet (refused with a message) |
| Fill a date or weekday series by name (Mon → Tue, Jan → Feb) | Planned | Dates stored as numbers continue as numbers |
| Move cells to another sheet | Planned | Refused with an explanation |
| Undo / redo (own last step) | Shipped | [COLLABORATION.md](COLLABORATION.md) |

## Formulas

| Feature | State | Notes and task |
| --- | --- | --- |
| Built-in functions (pinned engine) | Shipped | [FORMULA_CATALOG.md](FORMULA_CATALOG.md), gaps in [FORMULA_GAPS.md](FORMULA_GAPS.md) |
| Autocomplete, argument help, F4 anchors, coloured references | Shipped | F.03, [the formula editor](FORMULA_CATALOG.md#the-formula-editor) |
| Reference picking by click / drag | Partial | Same sheet only |
| Named ranges | Shipped | F.03 |
| Dynamic arrays, LET / LAMBDA | Shipped | F.10 |
| Table formulas in spreadsheet syntax (XLOOKUP, SUMIFS, …) at table scale | Shipped | F.05–F.09, [TABLE_FORMULAS.md](TABLE_FORMULAS.md) |
| Custom functions (formula, SQL, Python) | Shipped | [FUNCTIONS.md](FUNCTIONS.md), [PYTHON_FUNCTIONS.md](PYTHON_FUNCTIONS.md) |

## Formatting and layout

| Feature | State | Notes and task |
| --- | --- | --- |
| Bold / italic / underline / strikethrough, font size and colour, fill, borders, alignment, wrap | Shipped | S.01 |
| Number formats (number, currency, percent, date, time, scientific, text, custom), decimals | Shipped | S.02 |
| Freeze rows and columns, hide / unhide, sizes, auto-fit | Shipped | S.05 |
| Merged cells | Shipped | S.05 |
| Conditional formatting (value rules, colour scales) and validation lists | Shipped | S.10 |
| Format painter | Planned | |
| Zoom 50–200 % | Shipped | S.08 |

## Data

| Feature | State | Notes and task |
| --- | --- | --- |
| Sort and filter a range (one undo step) | Shipped | S.06 |
| Find and replace, this sheet or all sheets | Shipped | S.04 |
| Table header filters with value counts | Shipped | U.16 |
| Deduplicate, split, trim, type conversion with previews | Shipped | F.12 |
| Pivots with several measures, totals, drill-down | Shipped | F.11 (Analyze pane, U.15) |
| Charts: column, bar, line, area, scatter, pie | Shipped | S.09 / U.18 |
| Chart and rule tools open as drafts and save only on Create / Apply | Planned | U.04b |
| Comments with replies, resolve | Shipped | S.11 |

## Sheets, files and collaboration

| Feature | State | Notes and task |
| --- | --- | --- |
| Tabs: add, rename, duplicate, reorder, colour, hide | Shipped | S.07 |
| XLSX import / export with a compatibility report | Shipped | F.13; charts and some styles are reported, not exported |
| CSV, Parquet import | Shipped | |
| Live collaboration, history, revision restore | Shipped | Phase 7 |
| Printing and page setup | Not planned | Export to XLSX and print from there |
| Macros / VBA | Not planned | Custom functions cover the calculation use |
| Pivot tables inside the grid (Excel layout) | Not planned for v1 | Pivots live in the Analyze pane |
