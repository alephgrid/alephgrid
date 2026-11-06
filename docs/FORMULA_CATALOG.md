# Sheet formulas: built-in functions and the formula editor

Sheets compute with IronCalc 0.9.1 plus a small local patch set (`alephgrid-compat-1`, see
[`vendor/ironcalc_base/ALEPHGRID_PATCHES.md`](../vendor/ironcalc_base/ALEPHGRID_PATCHES.md)).
This page covers which built-in functions are available and how they are verified, and how the
formula editor works. Formulas that read whole tables are in [TABLE_FORMULAS.md](TABLE_FORMULAS.md);
known differences from Excel are in [FORMULA_GAPS.md](FORMULA_GAPS.md).

## The function catalog

Choose **Formula help** (Shift+F3) in a workbook to search the catalog: 562 function names, with
categories, descriptions, signatures, examples, limitations and test evidence. Filters show
names the engine recognizes and, for table formulas, only supported functions. Lists show at
most 40 entries per page. Help is read-only and works with SQL turned off.

The single source is `crates/alephgrid-core/data/function_catalog.json` (schema 4). The browser
imports it directly, and engine tests check it against the pinned engine and evaluate its
examples, so there is no second hand-maintained list.

| What | Count |
|---|---|
| Names in the catalog | 562 |
| Registered by IronCalc 0.9.1 under that name | 486 (the other 76 are functions from other spreadsheet products with no Excel counterpart; their Excel equivalents, such as `TEXTSPLIT`, cover the need) |
| Reviewed signature, description and examples (44 P0 + 32 P1) | 76, offered by formula-bar autocomplete |
| Not reviewed | 486, marked "Signature not yet reviewed" and not suggested |
| Supported in table formulas | 54, each with its modes and the tests that exercise it |
| Verification cases | 351: 344 verified, 7 accepted gaps (P0 alone: 277 cases, 273 verified, 4 gaps) |

**Verification cases.** Each reviewed function has cases that pin one mode: exact and
approximate lookups, first and last duplicate matches, sorted binary search, missing values,
invalid ranges, criteria with wildcards and `~` escapes, error handling and lazy `IF` branches,
text and Unicode, signed rounding, leap days and month rollover. 156 of them form a coercion
matrix over typed inputs (a number, text `'2`, `TRUE`, an empty cell, `=NA()`, `=""`, Unicode text,
a negative decimal and wildcard strings): how ranges and direct arguments treat text, logicals,
blanks and errors. Every expected value comes from hand calculation or a documented Excel rule
(the case's `oracle`), never from the engine's output. Each case runs through the real sheet
adapter, before and after reopening a file-backed workbook. **Formula help** shows the cases
under **Mode evidence**, gaps included.

A passing case certifies that input only. Binary-search cases use sorted input with unique
keys; Unicode cases use the Basic Multilingual Plane. Not covered: locale-specific parsing, the
1904 date system, every optional-argument combination, emoji and grapheme clusters, spilled
results of P0 functions, and precision at numeric extremes. Accepted gaps keep a test that fails
if the result changes, so an engine upgrade that fixes one is noticed.

Sheet built-ins are not SQL functions: DuckDB's functions and workbook functions are separate
(see [FUNCTIONS.md](FUNCTIONS.md)).

### Updating the catalog

1. Bump `schema_version` when the data contract changes and `catalog_version` for content or
   evidence changes. Review engine upgrades against `Cargo.lock`.
2. Keep "not listed by a source" separate from "not supported" when updating source listings.
3. Add signatures, modes and examples only after review. Expected values are derived
   independently; never copy them from the evaluator.
4. Run the catalog tests (`cargo test --workspace --lib tests::catalog`). Fix real mismatches or
   mark the entry partial; never change an expected value just to match the engine.
5. After changing help or completion, run the browser tests (`e2e/catalog.spec.ts`) and keep
   workbook-function suggestions, quoted-text behaviour and accessibility intact.

## The formula editor

**Autocomplete.** In a sheet's formula bar or cell editor, typing a function name suggests the
reviewed built-ins and the workbook's published functions; Tab accepts a suggestion, and
argument help appears after the opening parenthesis. Plain text and quoted strings do not
trigger suggestions.

**Picking references.** With the formula bar at an operand position (for example after
`=SUM(`), click a cell or drag across cells to insert an A1 reference; the destination cell stays
selected. A selected reference can be replaced by picking again. Clicking another sheet's tab
shows that sheet without leaving the edit, and picked ranges are inserted as `'Sheet name'!A1:B3`;
Enter saves to the original cell and returns to its sheet, Escape (or leaving the formula bar)
discards the draft and returns. Limits: picking works in the formula bar, not the inline cell
editor; dragging is limited to the visible area (no auto-scroll); whole rows, whole columns and
table references are typed, not picked.

**Coloured references.** While the formula bar has focus, same-sheet references get coloured
dashed outlines and matching text labels, so colour is never the only cue. At most 128 ranges
are drawn, from visible grid geometry only; nothing is downloaded or evaluated for them.

**Anchors.** Press **F4** with the caret on a reference, or with reference text selected, to cycle
`A1 → $A$1 → A$1 → $A1 → A1`. Several selected references cycle independently. F4 works in both
editors; quoted strings, sheet names, function names and structured references are left alone.

**Named ranges.** **Names** (sheet toolbar) lists named ranges and defines one from the selection;
"Refers to" starts as the absolute selection, for example `='Q1 data'!$B$1:$B$3`. A name is
workbook-wide or belongs to one sheet, and on that sheet its own name wins. Names are
case-insensitive, must refer to cells or ranges, and cannot reuse a built-in function name, a
workbook function or a cell-like name (`A1`). Renaming a name rewrites the formulas that use it;
deleting it leaves `#NAME?`. Each change is one undo step and survives reopening.

**Go to** (toolbar, Mod+G or the command palette) accepts `A1`, `B2:C9`, `Sheet1!A1`,
`'Q1 data'!A1:B4` or a name, and grows the visible grid if the target is beyond it.

**Formula problems.** Before a formula is saved, the server parses it as the engine will store it
(one missing final `)` is completed). A formula that does not parse is not saved: the draft
stays, a message says what is wrong and at which character, and the caret moves there. Start
with an apostrophe to enter the text as text. Input after a complete expression (`=1 2`) is an
error.

**Error origins.** Selecting a cell that shows an error displays its message and, when it comes
from another cell, that cell with a **Go to source** button (`#DIV/0!: Divide by 0 · comes from
Calc!A1`). It costs one bounded request per selection.

Tests: engine `tests::catalog` and `sheets::editing`, `web/e2e/catalog.spec.ts`,
`web/e2e/references.spec.ts` and `web/e2e/names.spec.ts`.
