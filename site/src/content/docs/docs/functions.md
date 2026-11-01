---
title: Your own functions
description: Reusable workbook functions as sheet formulas, SQL expressions or sandboxed Python, with tests, published versions, pinning and rollback.
---

A workbook can define its own functions and call them from sheet cells and table columns.
A function is a sheet formula, a SQL scalar expression, or Python that runs in an OS sandbox.
Definitions belong to the workbook; open one and choose **Functions** to manage them.

## Define and call a function

1. Choose **Sheet formula**, name it `MARGIN`, and declare arguments `revenue, cost`.
2. Enter the body — `IF(revenue=0,0,(revenue-cost)/revenue)` — and describe its purpose
   in the documentation field.
3. Add test cases, for example:

```json
[{"arguments": ["100", "60"], "expected": "0.4"}]
```

4. Choose **Run tests**, then **Publish version**. A failed expected value prevents
   publication.
5. In a sheet, enter `=MARGIN(B2,C2)`.

The formula bar suggests active functions: type `=MAR`, press Tab to insert `MARGIN(`, and
see its signature and documentation. Sheet tabs suggest formula functions; table tabs suggest
SQL functions.

For SQL, choose **SQL expression** and write a scalar expression:

```sql
-- casts declare types
(CAST(revenue AS DOUBLE) - CAST(cost AS DOUBLE))
  / CAST(revenue AS DOUBLE)
```

Call it as `SELECT MARGIN(100, 60)` in the SQL workspace, or `MARGIN([revenue],[cost])` in a
calculated table column. SQL and sheet functions use separate evaluators and cannot call
across engines; they share one name namespace per workbook.

Formula bodies support operators, constants, declared arguments, other workbook functions and
a built-in set: `IF`, `IFERROR`, `IFNA`, `SUM`, `AVERAGE`, `MIN`, `MAX`, `COUNT`, `COUNTA`,
`ABS`, `ROUND`, `ROUNDUP`, `ROUNDDOWN`, `AND`, `OR`, `NOT`, `ISBLANK`, `ISNUMBER`, `ISTEXT`,
`LEN`, `LOWER`, `UPPER`, `TRIM`, `LEFT`, `RIGHT`, `MID`, `DATE`, `YEAR`, `MONTH`, `DAY`,
`SQRT`. Bodies cannot capture cells or unrelated names — pass data as arguments. SQL bodies
allow catalog scalar functions without side effects: no subqueries, table references,
file or network functions, or dynamic SQL. Current-time functions are refused in both; pass
the observation date as an argument so results stay reproducible.

## Testing

Test arguments are literal strings. Formula tests enter them as cell inputs into an isolated
scratch sheet, so `""` is a blank cell and `"=1+2"` stays literal text; SQL tests pass quoted
strings, with the exact marker `NULL` binding SQL NULL. Expected results are exact formatted
strings. Test SQL runs with external access disabled, 64 MiB scratch memory, one thread and a
two-second deadline. An empty test list validates syntax and registration but proves nothing
about behaviour — publish meaningful examples.

## Versions, pinning and rollback

The uppercase name is the function's stable identity in its workbook; names cannot be renamed
or deleted in this release. Publishing creates an immutable, sequential version. A new function
activates version 1; publishing a later version leaves the existing pin unchanged until you
choose a version and **Activate selected version** — the same control upgrades and rolls back.

Calls to other workbook functions capture exact dependency versions at publication, and a
dependency cannot be upgraded underneath an active dependent. For a coordinated upgrade,
publish the new versions, then activate a version map such as `{"BASE":2,"DERIVED":2}` —
it changes all requested pins in one transaction or changes nothing. Roll back with
`{"BASE":1,"DERIVED":1}`.

Each version records the engine versions it was published with, and incompatible runtimes or
missing pins fail before serving calculations — there is no silent fallback to a latest
version.

## Python functions

A function can be Python instead: `source` defines a function with the lowercase name
(`NORMALIZE_SKU` → `def normalize_sku(sku):`), importing the standard library and the
packages of the server's runtime, and declares its return type (`number`, `text` or
`boolean`). Call it like any function: `=NORMALIZE_SKU(A2)` in a sheet,
`=NORMALIZE_SKU([sku])` as a table column's formula.

Python never runs inside the spreadsheet engine or the database. Results are cached by
function, version, runtime and exact arguments; a missing result shows `#CALC!`
"calculating" while a background job computes it in batches — a table column computes one
call per distinct argument tuple, so a million rows with 100,000 distinct values is 100
batches, not a million calls. An exception becomes `#VALUE!` with the exception's first line
as the cell's message.

The sandbox:

- Each job runs in an isolated worker process under an OS sandbox — Seatbelt on macOS,
  bubblewrap on Linux. It can read only the Python runtime and its own empty working
  directory, write nothing outside it, open no sockets and start no processes; the
  environment is cleared of server credentials and paths.
- Without a sandbox available the server refuses to run Python (fail closed), unless started
  with `--python-isolation none` in `--auth local` mode for one trusted person.
- Wall time, CPU time, memory and output bytes are limited per job; a fixed number of workers
  run at once. A slow, crashing or hostile function cannot affect other workbooks.
- Publishing, pinning and running tests need the space's publish role, and an organization
  admin can turn Python off for a space; open workbooks stop at once.

Runtimes are built offline from a folder of wheels with required hashes
(`alephgrid python-runtime create`); each version records the runtime it was published with.
A workbook needing another runtime still opens — cached results show, new calls say which
runtime they need. Mark a function `volatile: true` when its result can change without its
arguments changing; it is then recomputed only on **Refresh**, never silently.

Python limits: one value per call (no spilling arrays), text results up to 32,767 characters,
and cached results are kept per version with no size-based eviction yet.

## Limits and export

Per workbook: 128 functions, 100 versions each, 1–16 arguments, 4 KiB source, 8 KiB
documentation, 32 test cases, and a 4 MiB serialized registry.

:::caution
These limits are not a hostile-code isolation boundary for formula and SQL functions: the
current Local provider shares a process with the server and is intended for trusted users and
teams. Python is the sandboxed path.
:::

The workbook database is the complete durable artifact — definitions, tests, versions, pins
and sheet state. XLSX exports include active formula functions as named LAMBDAs (Excel needs
a LAMBDA-capable version), but the registry, tests, history, inactive versions and SQL macros
are not stored in XLSX. Python functions are not Excel functions: exported cells that call
one show `#NAME?` in Excel unless you export cached values instead, and the compatibility
report lists them. CSV and Parquet exports contain data, not function definitions.
