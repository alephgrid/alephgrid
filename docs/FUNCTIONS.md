# Workbook functions

Workbook functions are reusable calculations written as a sheet formula, a SQL scalar expression or
[Python](PYTHON_FUNCTIONS.md). Open a workbook and choose **Functions**: a side panel opens beside
the grid with **Code**, **Tests**, **Docs** and **Versions** tabs. Definitions belong to that
workbook. Space libraries share saved queries and views, not functions ([TEAMS.md](TEAMS.md)).

## Create and call

1. Choose **New**, set **Language** to **Formula**, name `MARGIN`, and arguments `revenue, cost`.
2. Enter `IF(revenue=0,0,(revenue-cost)/revenue)` and describe its purpose on the **Docs** tab.
3. On the **Tests** tab, **Add test** with inputs `100` and `60` and expected result `0.4`. (Tests
   can also be edited as JSON under Advanced: `[{"arguments":["100","60"],"expected":"0.4"}]`.)
4. Choose **Run tests** (or Mod+Enter in the panel), then **Publish version**. Failed expected values
   prevent publication. Opening the panel or an example never runs code. While tests run, **Cancel**
   stops them on the server (a Python worker is killed, a SQL test interrupted); the panel says
   **Cancelled** only once the server has stopped, and a cancelled run leaves the draft a draft.
5. In a sheet enter `=MARGIN(B2,C2)`. Cells and ranges are passed to IronCalc's named LAMBDA evaluator, with no source substitution.

The formula bar suggests active functions: type `=MAR`, press Tab to insert `MARGIN(`, and see its signature, documentation and an example made from its first test (`Example: =MARGIN(100, 60) → 0.4`). Sheet tabs suggest formula functions; table tabs suggest SQL functions. Suggestions refresh when the formula bar receives focus. Calling a function never opens the panel.

### The code editor

The **Code** tab colours the source (keywords, text, numbers, function calls, arguments, comments)
and lists problems by line in the gutter and under the code: brackets never closed or closed by the
wrong bracket, text never closed, Python indentation that mixes tabs and spaces, and, after a run,
the line a Python exception or syntax error points at. Choosing a problem selects its line. Enter
keeps the line's indentation (one level deeper after a Python `:` or an open bracket), Tab and
Shift+Tab indent and outdent the line or selection, and press Escape, then Tab, to leave the
editor. Undo and redo (Mod+Z, Shift+Mod+Z or Mod+Y) are the editor's own and never undo the grid.
The highlighting is drawn from text nodes: source is never interpreted as markup.

The results strip under the panel resizes by its top edge (drag, or the arrow keys on it) and keeps
its height.

For SQL, set **Language** to **SQL** (offered only when the server allows SQL) and use a scalar expression, for example:

```sql
(CAST(revenue AS DOUBLE) - CAST(cost AS DOUBLE)) / CAST(revenue AS DOUBLE)
```

Then call `SELECT MARGIN(100, 60)` in the SQL workspace, or `MARGIN([revenue],[cost])` in a calculated table column. SQL and sheet functions use separate evaluators and cannot call across engines. They share a workbook name namespace, so use different names if both implementations are needed.

### Test values

Each test input and expected result has a type, chosen under it on the **Tests** tab:

| Type | JSON | Formula test | SQL test | Python test |
|---|---|---|---|---|
| **As typed** | `"100"` | Entered like a cell input: `100` is a number, `TRUE` a logical, empty is a blank cell, `=1+2` stays text | A quoted string; the exact text `NULL` binds SQL NULL | `100` → `float`, `TRUE` → `bool`, empty → `None`, other text → `str` |
| **Text** | `{"type":"text","value":"100"}` | Exactly this text; empty is empty text, not a blank cell (`ISBLANK` is FALSE) | This string, even `NULL` | `str` |
| **Blank** | `{"type":"blank"}` | A blank cell | NULL | `None` |
| **Error** | `{"type":"error","value":"#N/A"}` | That error value (`#NULL!`, `#DIV/0!`, `#VALUE!`, `#REF!`, `#NAME?`, `#NUM!`, `#N/A`, `#SPILL!`, `#CALC!`), so `IFERROR`/`IFNA` can be tested | Refused: SQL arguments are values or NULL | Not called: the result is that error, as in cells |

An **As typed** expected result compares the displayed text, as before. A typed expected result also
compares the kind of value: **Text** `#N/A` fails against the error `#N/A`, **Blank** passes only
for SQL NULL, **Error** only for that error. Results say when the kind matters (`#N/A (error)`,
`0.4 (text)`, `empty text`). The **Advanced: tests as JSON** view reads and writes the same forms;
older workbooks, whose tests are all text, load unchanged.

Cast SQL arguments explicitly when numeric or date semantics matter. A formula test previews the
anchor cell for array results; verify the full spill range in a sheet. Empty test lists validate
syntax and registration but do not prove behavior across argument types; publish meaningful examples.

## Versions and dependencies

The uppercase function name is its stable identity within a workbook. Names cannot be renamed or deleted in this release. Publishing creates an immutable, sequential version. A new function activates version 1; publishing a subsequent version leaves its existing pin unchanged. On the **Versions** tab, choose **Activate in this workbook** next to a version to upgrade or roll back. Before anything changes, the panel shows what the switch affects (`function_usage`): the version it replaces, the sheet cells that call the function (count and the first addresses) and the calculated table columns, other functions whose active version calls a different version of this one (activating alone is then refused; use the version map below), and for Python whether the version's runtime is the one this server has. **Activate vN** confirms; **Cancel** leaves the workbook as it was. Other workbooks never change.

Calls to workbook functions capture exact dependency versions at publication. Indirect recursion and incompatible dependency graphs are rejected. A dependency cannot be upgraded underneath an active dependent.

For a coordinated upgrade:

1. Publish `BASE` v2 (v1 remains active).
2. Edit `DERIVED`, set Dependency versions (Versions tab, Advanced) to `{"BASE":2}`, and publish its v2.
3. Under Advanced on the **Versions** tab, set Active versions to `{"BASE":2,"DERIVED":2}` and choose **Activate version map**.
4. Roll back with `{"BASE":1,"DERIVED":1}`.

The map changes all requested pins in one transaction or changes nothing. Registry, persistent SQL macros, sheet definitions, recalculated cells, revision and history commit together. Activating changed sheet definitions resets the cell undo history; use version pins for function rollback. Publishing an inactive version preserves cell undo history.

Each version records the function adapter (`alephgrid-functions-v1/ironcalc-0.9.1`), actual DuckDB version and dependency pins. RuntimeSpec includes the adapter alongside DuckDB and IronCalc. Incompatible runtimes, missing pins or dependencies fail before serving calculations. There is no fallback to the latest available version. Older workbooks migrate on open, keeping a copy of the old file. Engine upgrades need an explicit migration and compatibility review.

## Supported semantics

| Concern | Sheet formulas | SQL expressions |
|---|---|---|
| Inputs | Literal values, cell references, ranges at the call site | Scalar DuckDB values; use casts to declare intended types |
| Blank / NULL | IronCalc blank semantics; `ISBLANK` distinguishes blanks | DuckDB NULL propagation; handle with `COALESCE` or CASE |
| Numbers | Spreadsheet floating-point precision; use ROUND for display conventions | Explicit DECIMAL preserves decimal arithmetic; DOUBLE is floating point |
| Dates | IronCalc date serials and DATE/YEAR/MONTH/DAY | Explicit DATE/TIMESTAMP casts; DATE arithmetic |
| Errors | Spreadsheet errors such as `#DIV/0!`; IFERROR/IFNA can handle them | Binding/cast/query errors returned to the caller; failed test execution prevents publication |
| Arrays | IronCalc array constants and range-returning formulas can spill; occupied cells produce `#SPILL!` without overwriting data | Scalar macros only; aggregate/window/table functions refused; scalar list values use DuckDB semantics |
| Names | Case-insensitive, canonical uppercase, ASCII letter followed by letters/digits/underscore, max 64 bytes | Same namespace and rules |
| Conflicts | Built-ins, cell-like function names, duplicate arguments, reserved `rf_` names and existing imported defined names refused | Built-in SQL functions also reserved |
| External data | Function bodies cannot capture cells or unrelated named variables; pass data as arguments | No subqueries, table references, file/network functions, settings, external parameters or dynamic SQL |

Formula bodies support operators, constants, declared arguments, named workbook functions and this initial built-in set:
`IF`, `IFERROR`, `IFNA`, `SUM`, `AVERAGE`, `MIN`, `MAX`, `COUNT`, `COUNTA`, `ABS`, `ROUND`, `ROUNDUP`, `ROUNDDOWN`, `AND`, `OR`, `NOT`, `ISBLANK`, `ISNUMBER`, `ISTEXT`, `LEN`, `LOWER`, `UPPER`, `TRIM`, `LEFT`, `RIGHT`, `MID`, `DATE`, `YEAR`, `MONTH`, `DAY`, `SQRT`.
Nested named-function calls are supported; dynamically invoked or returned LAMBDAs are not. SQL functions permit catalog scalar functions without side effects, subject to the existing SQL policy; current-time functions are refused. Pass the observation date/time as an argument for reproducible calculations.

Limits: 4 MiB total serialized registry, 128 functions per workbook, 100 versions per function, 1–16 arguments, 4 KiB source (64 KiB for Python), 8 KiB documentation, 32 test cases, 4 KiB per test input/expected value, and a conservative 64 KiB expanded expression budget. Test SQL runs with external access disabled, 64 MiB scratch memory, one thread, no disk spill and a two-second interrupt deadline. Formula validation excludes unbounded looping/dynamic evaluation; the current Local provider still shares a process with the server and is intended for trusted users/teams. These limits are not a hostile-code isolation boundary.

## Export and recovery

The workbook database is the complete durable artifact: it contains definitions, tests, versions, exact pins, macros and sheet state. Back up the database using the documented workbook backup procedure.

XLSX exports include active named LAMBDA formulas and their sheet cells. The tested IronCalc round trip preserves their results, but Alephgrid's registry, tests, history, inactive versions and SQL macros are **not** stored in XLSX. Excel requires a version with LAMBDA support; compatibility with every Excel build is not certified. An imported XLSX named function cannot silently replace a registry function. Exported CSV/Parquet contains data/results, not reusable function definitions.
