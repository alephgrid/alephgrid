# Workbook functions

Phase 5 adds reusable sheet formulas and SQL scalar expressions. Open a workbook and choose **Functions**.
Definitions belong to that workbook. Python, space-wide sharing, and an Excel add-in are separate later features.

## Create and call

1. Choose **Sheet formula**, name `MARGIN`, and arguments `revenue, cost`.
2. Enter `IF(revenue=0,0,(revenue-cost)/revenue)` and describe its purpose in Documentation.
3. Add test cases, for example `[{"arguments":["100","60"],"expected":"0.4"}]`.
4. Choose **Run tests**, then **Publish version**. Failed expected values prevent publication.
5. In a sheet enter `=MARGIN(B2,C2)`. Cells and ranges are passed to IronCalc's named LAMBDA evaluator, with no source substitution.

The formula bar suggests active functions: type `=MAR`, press Tab to insert `MARGIN(`, and see its signature and documentation. Sheet tabs suggest formula functions; table tabs suggest SQL functions. Suggestions refresh when the formula bar receives focus.

For SQL, choose **SQL expression** and use a scalar expression, for example:

```sql
(CAST(revenue AS DOUBLE) - CAST(cost AS DOUBLE)) / CAST(revenue AS DOUBLE)
```

Then call `SELECT MARGIN(100, 60)` in the SQL workspace, or `MARGIN([revenue],[cost])` in a calculated table column. SQL and sheet functions use separate evaluators and cannot call across engines. They share a workbook name namespace, so use different names if both implementations are needed.

Test arguments are literal strings. SQL tests pass quoted strings, with the exact marker `NULL` binding SQL NULL; cast arguments explicitly when numeric or date semantics matter. Formula tests enter literal cell inputs into an isolated scratch sheet, so `""` is a blank cell and `"=1+2"` remains literal text. Expected results are exact formatted strings. A formula test previews the anchor cell for array results; verify the full spill range in a sheet. Empty test lists validate syntax and registration but do not prove behavior across argument types; publish meaningful examples.

## Versions and dependencies

The uppercase function name is its stable identity within a workbook. Names cannot be renamed or deleted in this release. Publishing creates an immutable, sequential version. A new function activates version 1; publishing a subsequent version leaves its existing pin unchanged. Choose a version and **Activate selected version** to upgrade or roll back. Other workbooks never change.

Calls to workbook functions capture exact dependency versions at publication. Indirect recursion and incompatible dependency graphs are rejected. A dependency cannot be upgraded underneath an active dependent.

For a coordinated upgrade:

1. Publish `BASE` v2 (v1 remains active).
2. Edit `DERIVED`, set Dependency versions to `{"BASE":2}`, and publish its v2.
3. Set Active versions to `{"BASE":2,"DERIVED":2}` and choose **Activate version map**.
4. Roll back with `{"BASE":1,"DERIVED":1}`.

The map changes all requested pins in one transaction or changes nothing. Registry, persistent SQL macros, sheet definitions, recalculated cells, revision and history commit together. Activating changed sheet definitions resets the cell undo history; use version pins for function rollback. Publishing an inactive version preserves cell undo history.

Each version records the function adapter (`rowfinity-functions-v1/ironcalc-0.9.1`), actual DuckDB version and dependency pins. RuntimeSpec includes the adapter alongside DuckDB and IronCalc. Incompatible runtimes, missing pins or dependencies fail before serving calculations. There is no fallback to the latest available version. Storage format 4 adds this contract; older workbooks migrate with the existing backup procedure. Engine upgrades need an explicit future migration/compatibility process.

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

Limits: 4 MiB total serialized registry, 128 functions per workbook, 100 versions per function, 1–16 arguments, 4 KiB source, 8 KiB documentation, 32 test cases, 4 KiB per test input/expected value, and a conservative 64 KiB expanded expression budget. Test SQL runs with external access disabled, 64 MiB scratch memory, one thread, no disk spill and a two-second interrupt deadline. Formula validation excludes unbounded looping/dynamic evaluation; the current Local provider still shares a process with the server and is intended for trusted users/teams. These limits are not a hostile-code isolation boundary.

## Export and recovery

The workbook database is the complete durable artifact: it contains definitions, tests, versions, exact pins, macros and sheet state. Back up the database using the documented workbook backup procedure.

XLSX exports include active named LAMBDA formulas and their sheet cells. The tested IronCalc round trip preserves their results, but Rowfinity's registry, tests, history, inactive versions and SQL macros are **not** stored in XLSX. Excel requires a version with LAMBDA support; compatibility with every Excel build is not certified. An imported XLSX named function cannot silently replace a registry function. Exported CSV/Parquet contains data/results, not reusable function definitions.
