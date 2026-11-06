# No-code analysis

Open a workbook and choose **Analyze**: a side pane opens beside the grid, and its result and
chart dock under the grid. All data operations run in DuckDB through validated,
typed plans; the browser receives bounded results rather than downloading source tables.
The tools are available even when arbitrary SQL is disabled.

## Summaries, pivots and charts

Choose a source, zero to four group columns, and count, sum, average, minimum or maximum.
Count counts rows; other aggregates ignore NULL inputs and return NULL for an empty group.
Sum and average require numeric columns. Computed columns participate through the same query
expansion used by SQL.

Optionally select a pivot column and pick up to 24 of its values from a list of its distinct
values with counts. Missing values can be picked too; other values match the column's string
representation exactly. Unlisted
categories are excluded from pivot measures. Result headers identify each selected category.

Previews contain at most 500 rows, also subject to the normal response byte limit. A truncation
notice means more groups or rows exist. Charts show at most the first 100 groups of the first
numeric measure, with a partial-chart label when appropriate. Aggregates use the full source;
the row limit applies to their output. Bar lengths use approximate browser numbers; text labels
retain the exact values returned by the engine. Negative bars extend left of zero. Date/text
minimum and maximum results remain available as tables.

## Joins and profiling

A join uses one equality key from each source and supports inner or left joins. Key SQL types
must match exactly; convert incompatible types explicitly before joining. NULL keys do not match.
Output columns are prefixed `left_` and `right_`; joins are limited to 100 output columns.

The diagnostic table reports repeated non-NULL keys on each side and the complete output row
count. These counts are computed from key frequencies, so many-to-many multiplication is visible
without first materializing the full join. The preview still contains at most 500 rows. Join
row order is unspecified.

Profile up to 32 columns to inspect declared SQL type, total rows, NULL count, exact distinct
non-NULL values, and duplicate non-NULL occurrences beyond the first occurrence of each value.
Profiling and grouping may scan the full source; they use the normal query admission, memory,
deadline and cancellation controls.

## Saving and refresh

**Save** persists the typed plan and chart choice; **Save as new** creates a separate plan.
Opening a saved analysis reruns it on current source data. **Run** reruns on demand.
Workbook-change events mark the displayed result stale; they do not silently rerun expensive work.
Schema changes that invalidate a saved plan report an error instead of silently removing fields.
There may be up to 128 saved plans and 4 MiB of serialized saved-analysis data per workbook.

**Result as table** creates a snapshot using the plan, with a maximum of 10,000 result rows
and the normal byte limit. Larger outputs are refused without creating a partial table. The
existing materialization metadata records source revisions: the snapshot stays unchanged until
explicitly refreshed using the workbook's snapshot controls. Saving a plan does not save its
result rows. Delete removes the saved plan, not its sources or previously created snapshots.

## Find and replace

From a table, open the find bar (Mod+F) and its replace row (Mod+H, or Mod+Shift+H on macOS).
Replace works on literal, case-sensitive substrings in one editable VARCHAR column. It affects the whole table, including rows hidden by
filters. NULL cells remain NULL. Find text must be nonempty; regex and multi-column replacement
are not supported.

In the find bar's replace row, **Preview** gives the total affected-cell count and up to 50
before/after examples. **Replace all** is tied to that workbook revision: any intervening
workbook change requires a new preview.
A replacement is limited to 10,000 affected cells and 4 MiB of old/new value data and serialized
undo data. Expansion beyond that budget fails before changing data.

The most recent replacement has a durable undo record. **Undo replace** restores it
only if no workbook mutation has occurred since; this protects later edits. The browser retains
the undo revision locally, so it remains available after reloading in the same browser. Protocol
clients can retain `undo_revision` themselves. Undo history here is independent from sheet cell
undo; it is one replacement deep. Replacement, undo, revision, history and data invalidation are
transactional and use the existing mutation-id retry protection.

## Table presentation

**Format table** saves hidden columns, a frozen leading pane, per-column display formats and
conditional rules in the workbook. Formats change display, not raw values or editing input.
Current-view exports continue to honor hidden columns and retain raw values.

The formats are general, number, percent, date and timestamp, with 0–12 decimal places;
locales are en-US, en-GB, de-DE, fr-FR, hi-IN and ja-JP. Timezones are UTC, Asia/Kolkata,
America/New_York, Europe/London and Asia/Tokyo. The dialog applies its locale/precision/timezone
to all non-general formats on save; the protocol can store them per column. Exact decimal and
large-integer strings are rounded with integer arithmetic rather than conversion to a JavaScript
double. Number-valued cells use Intl formatting. DATE keeps its calendar date regardless of the
timezone; timezone-free TIMESTAMP is explicitly interpreted as UTC for display.

Freeze up to eight leading visible columns. The grid reduces the effective frozen count if the
pane would leave less than 80 px for scrolling columns. Only visible rows/columns and the small
frozen pane are fetched. At least one column must stay visible. Removed columns are pruned from
presentation on read; if schema changes leave every remaining column hidden, columns are shown.
Saved views may override hidden columns while active.

At most 16 conditional rules run per visible cell, with first-match precedence: greater, less,
equal or empty, colored red/green/amber. Numeric comparisons use browser floating-point values
within ±Number.MAX_SAFE_INTEGER; values outside that range are not compared. They are not a
substitute for exact-decimal financial rules. Rules never execute formulas or arbitrary CSS.
Presentation currently applies to tables, not IronCalc sheet cell styles. Table renames do not
migrate table-keyed presentation; re-save presentation under the new name.

## Storage and test evidence

Saved plans, presentation and the bounded replacement undo record live in `_rf_meta` (added in
storage format 5). See the phase 6 section of [TEST_MATRIX.md](TEST_MATRIX.md) for engine, server
and browser evidence.

## Reports, charts and dashboards

A **report** (`kind: "report"`) extends a summary: up to four group columns (formula columns
included, so a report can group by a looked-up value), up to eight measures (`revenue =
sum(amount)`, `orders = count()`; count, sum, average, min, max), up to four **calculated
measures** written as table formulas over the measures (`average = [@revenue]/[@orders]`,
compiled by the table-formula compiler, errors shown as blank), optional **totals** (`ROLLUP`:
a subtotal row per group level and a grand total, marked `row_kind` = `row`, `subtotal` or
`total`; subtotals follow their rows), up to eight **filters** (the view filter operators), and
an optional pivot column (one column per measure and value, at most 64). Calculated measures and
a pivot cannot be combined. Results stay bounded at 500 rows. Summaries can group by and
aggregate formula columns too.

**Drill-down.** Every summary or report row has *Show rows*: `drill` returns a view spec of the
table filtered by the row's group values (a subtotal leaves out the levels it adds up, a blank
value filters for blanks), and the grid opens it; rows are paged like any view, never
materialized.

**Charts.** A saved analysis keeps a `chart_kind`: column, bar (default), line, area, scatter or
pie, of the first measure over the groups, at most 100 groups; totals are not drawn; a list beside
the chart gives every exact value.

**Dashboards.** Saved analyses as tiles on a 12×24 grid (`save_dashboard`): tiles must fit and
must not overlap; at most 12 tiles, 6 slicers (1–24 values each) and 32 dashboards per
workbook. A **slicer** filters every report on the dashboard over its table (`column IN
values`, `(NULL)` for blanks); summaries over other tables are unaffected. Opening a dashboard
computes every tile in its own snapshot; a tile whose analysis was deleted or no longer compiles
says why instead of failing the dashboard. Dashboards are stored in `_rf_meta` (no format
change) and survive reopening.

Tests: `tests::reports` (3: measures, calculated measures, totals and subtotals, filters, pivot,
refusals; drill-down; dashboards, slicers, limits, collisions, deleted analyses, reopen), server
`every_operation_replies_with_its_declared_type`, `e2e/analysis.spec.ts` (report with totals and
a line chart, drill-down into the grid, a sliced dashboard).

## Data tools

**Validation.** A table's stored columns can carry rules (`set_validations`): one of a list
(the grid's editor offers the values as a dropdown), a number between bounds, or not blank;
at most 32 rules per table, 500 list values. Every write path keeps them: edits (typing,
pasting, appending rows by typing below the last one) are refused per value with what the rule
allows, and a SQL change that would add violations is refused and rolled back (one that keeps
them, or fixes earlier ones, goes through). Existing values are not rewritten; saving rules
reports how many rows break each one. Rules are stored in `_rf_meta` and survive reopening.
Formula columns are validated through the columns they read.

**Clean-up into a new table.** Deduplicate by one to eight key columns (first row kept, in row order), trim
spaces (optionally inner runs), split a column by a delimiter into up to 16 new columns, or
convert a column to BIGINT, DOUBLE, DATE, BOOLEAN or VARCHAR (values that do not convert become
blank). Each one is previewed first (the first 50 rows, the row count, and how many rows it
drops, changes or fails to convert) and applied as a **new table**, so the source is never
modified and undoing means deleting the new table.

**Formula inspection.** For a formula column: the columns, tables and sheet cells it reads, the
formula columns and sheet cells that read it, how it runs (row expression, lookup join, aggregate
join, position join, SQL expression), and its error cells by code.

Tests: `tests::tools` (3: rules on every write path including SQL bypass attempts and reopen;
transform previews and derived tables leaving the source unchanged; inspection), server op
coverage, `e2e/tools.spec.ts` (rule, dropdown, refused value, dedupe table, inspection).
