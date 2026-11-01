---
title: Analyze without SQL
description: Summaries, pivots, joins, charts, column profiles and clean-up tools that need no SQL, with previews before anything changes.
---

Open a workbook and choose **Analyze**. Every operation runs in DuckDB through a validated,
typed plan; your browser receives bounded results rather than downloading source tables. The
tools work even on a server where SQL is disabled.

## Summaries and pivots

Choose a source, up to four group columns, and a measure: count, sum, average, minimum or
maximum. Count counts rows; the other aggregates ignore NULL inputs. Formula columns can be
grouped and measured like any other column.

A **report** extends a summary with up to eight measures (`revenue = sum(amount)`,
`orders = count()`), up to four calculated measures written as formulas over the measures
(`average = [@revenue]/[@orders]`), optional subtotals per group level plus a grand total,
and up to eight filters.

Optionally pick a pivot column and list up to 24 explicit categories, one per line — `(NULL)`
selects missing values. Unlisted categories are excluded, and the result headers name each
category. Calculated measures and a pivot cannot be combined.

Previews contain at most 500 rows; a truncation notice tells you more groups exist. The
aggregates themselves always use the full source — the limit applies only to the output.

**Drill-down.** Every summary or report row has *Show rows*: it opens the source table in the
grid, filtered to that row's group values, paged like any view.

## Charts

A saved analysis keeps a chart: bars (the default), a line, or points of the first measure
over the groups, at most the first 100 groups, with a label when the chart is partial. A list
beside the chart gives every exact value, so the bars are never your only copy of a number.

Saved analyses can be laid out as tiles on a **dashboard**, with slicers that filter every
report on it over its table. A workbook holds up to 32 dashboards of up to 12 tiles and
6 slicers each.

## Joins

A join takes one equality key from each source and is inner or left. Key SQL types must match
exactly — convert incompatible types first — and NULL keys do not match. Output columns are
prefixed `left_` and `right_`, up to 100 of them.

Before you materialize anything, a diagnostic table reports repeated non-NULL keys on each
side and the complete output row count, computed from key frequencies — so many-to-many
blow-ups are visible without running the full join. The preview stays within 500 rows.

## Column profiles

Profile up to 32 columns at once: declared SQL type, total rows, NULL count, exact distinct
non-NULL values, and duplicate occurrences. Profiling and grouping may scan the full source;
**Cancel run** stops the active request at any time.

## Clean-up tools, previewed first

Each tool shows a preview — the first 50 rows, the row count, and how many rows it would
drop, change or fail to convert — and then applies as a **new table**. The source is never
modified, so undoing means deleting the new table.

| Tool | What it does |
|---|---|
| Deduplicate | By key columns; the first row is kept, in row order |
| Trim | Removes surrounding spaces, optionally inner runs too |
| Split | One column by a delimiter into up to 16 new columns |
| Convert | To BIGINT, DOUBLE, DATE, BOOLEAN or VARCHAR; values that do not convert become blank |

Tables can also carry validation rules (one of a list, a number between bounds, not blank)
that every write path enforces — typing, pasting, and even SQL changes that would add
violations are refused.

## Saving and refresh

**Save analysis** persists the plan and chart choice; opening it reruns it on current data,
and workbook changes mark a displayed result stale rather than silently rerunning expensive
work. **Create result table** snapshots the result (up to 10,000 rows) as a table that stays
unchanged until you explicitly refresh it.

## Live selection statistics

Select any cells and the bar under the grid shows the live sum, average and count of the
selection — no menu, no formula.

The full specification, including every limit, is in the repository's
[analysis guide](https://github.com/alephgrid/alephgrid/blob/main/docs/ANALYSIS.md).

## Next

- [The SQL workspace](../sql-workspace/) — when you do want to write the query yourself
- [Big tables](../big-tables/)
- [Importing and exporting data](../importing-data/)
