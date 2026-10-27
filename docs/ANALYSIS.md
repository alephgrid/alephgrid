# No-code analysis

Open a workbook and choose **Analyze**. All data operations run in DuckDB through validated,
typed plans; the browser receives bounded results rather than downloading source tables.
The tools are available even when arbitrary SQL is disabled. Python, connectors and shared-space
authorization are separate later phases.

## Summaries, pivots and charts

Choose a source, zero to four group columns, and count, sum, average, minimum or maximum.
Count counts rows; other aggregates ignore NULL inputs and return NULL for an empty group.
Sum and average require numeric columns. Computed columns participate through the same query
expansion used by SQL.

Optionally select a pivot column and enter up to 24 explicit categories, one per line. `(NULL)`
selects missing values; other entries match the column's string representation exactly. Unlisted
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
deadline and cancellation controls. **Cancel run** cancels the active analysis request.

## Saving and refresh

**Save analysis** persists the typed plan and chart choice; **Save as new** creates a separate
plan. Opening a saved analysis reruns it on current source data. **Run / refresh** reruns on demand.
Workbook-change events mark the displayed result stale; they do not silently rerun expensive work.
Schema changes that invalidate a saved plan report an error instead of silently removing fields.
There may be up to 128 saved plans and 4 MiB of serialized saved-analysis data per workbook.

**Create result table** creates a snapshot using the plan, with a maximum of 10,000 result rows
and the normal byte limit. Larger outputs are refused without creating a partial table. The
existing materialization metadata records source revisions: the snapshot stays unchanged until
explicitly refreshed using the workbook's snapshot controls. Saving a plan does not save its
result rows. Delete removes the saved plan, not its sources or previously created snapshots.

## Find and replace

From a table, choose **Find / replace**. The initial release replaces literal, case-sensitive
substrings in one editable VARCHAR column. It affects the whole table, including rows hidden by
filters. NULL cells remain NULL. Find text must be nonempty; regex and multi-column replacement
are not supported.

**Preview replacement** gives the total affected-cell count and up to 50 before/after examples.
Apply is tied to that workbook revision: any intervening workbook change requires a new preview.
A replacement is limited to 10,000 affected cells and 4 MiB of old/new value data and serialized
undo data. Expansion beyond that budget fails before changing data.

The most recent replacement has a durable undo record. **Undo last replacement** restores it
only if no workbook mutation has occurred since; this protects later edits. The browser retains
the undo revision locally, so it remains available after reloading in the same browser. Protocol
clients can retain `undo_revision` themselves. Undo history here is independent from sheet cell
undo; it is one replacement deep. Replacement, undo, revision, history and data invalidation are
transactional and use the existing mutation-id retry protection.

## Table presentation

**Format table** saves hidden columns, a frozen leading pane, per-column display formats and
conditional rules in the workbook. Formats change display, not raw values or editing input.
Current-view exports continue to honor hidden columns and retain raw values.

The initial format set is general, number, percent, date and timestamp, with 0–12 decimal places;
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

Storage format 5 adds saved plans, presentation and the bounded replacement undo record in
`_rf_meta`. Existing databases migrate transactionally with a pre-migration backup. Older binaries
refuse format 5 rather than editing data without understanding its new metadata.

See the phase 6 section of [TEST_MATRIX.md](TEST_MATRIX.md) for engine, server and browser evidence.
