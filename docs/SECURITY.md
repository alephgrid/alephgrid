# Security model and hardening

How Alephgrid treats what it is given, and the limits that keep one request from taking the
server down. Each protection here has a regression test (`crates/alephgrid-core/src/tests/security.rs`,
`crates/alephgrid-server/tests/security.rs`, `web/e2e/security.spec.ts`). To report a problem,
see [SECURITY.md](../SECURITY.md).

## Workbook files are untrusted input

A workbook is a DuckDB file, and a file can carry views and macros. It may come from a backup
archive someone else made or from a data directory someone else could write to.

* **The engine is locked down before anything in the file is read.** Every workbook is opened
  with extension installs and loads off and external access disabled; the only directories it
  may read or write are the workbook's own scratch directories. The settings are applied as
  the connection's first statements, before format detection, migration or the sheets are
  read, and SQL cannot re-enable them (`storage::Lockdown`, `Workbook::open_locked`).
  Restoring a backup and duplicating a workbook open the copy the same way.
* **What is stored is checked again when it is read back.** A SQL formula column is checked
  with the same rules as when it was written before it reaches a query; one that fails reads
  as empty. A SQL function from the registry is checked again before it becomes a macro.
* **A workbook's id is its directory's name.** `workbook.json` must name the directory it is
  in, or the workbook does not open; restore refuses an archive whose `workbook.json` names a
  different workbook than its manifest. Paths are never built from an id read out of a file.
* **Restore copies only plain files** (no links) into a staging directory of its own.
* **The control database is opened the same way.** `control.duckdb` (users, roles, tokens) can
  come from an archive too: no extensions, and files only in `.control/` and `.backups/`.

## The browser boundary

* Every response carries `Content-Security-Policy` (scripts and connections only to this
  server, no framing, no plugins), `Referrer-Policy: no-referrer`, `X-Content-Type-Options:
  nosniff` and `X-Frame-Options: DENY`.
* Requests that change something, and WebSocket upgrades, are refused when their `Origin` is
  another site, and a change sent with a session cookie must show it comes from this server's
  own pages; the `Host` is checked too ([TEAMS.md](TEAMS.md#browser-and-network-protections)). No secret is ever put in a URL.
* The web client uses an id from the server in a URL only if it is a plain path segment
  (`[A-Za-z0-9_-]{1,64}`); a listing entry with any other id is not shown at all, so a crafted
  id such as `../trash` cannot turn one request into another.

## Bounded work

| What | Limit |
|---|---|
| A sheet evaluation | reads at most 20 M cells (`SHEET_EVALUATION_CELLS`); a range may declare at most 120 M cells and may become an array of at most 2 M; functions over ranges walk only the used area. An edit that goes over is taken back whole, with a message. A deadline or cancel stops a running evaluation too. |
| A formula | at most 8,192 characters, 100 levels of nesting and 1,000 operators, in both the sheet engine and table formulas |
| LAMBDA calls | at most 1,000 inside one another and within the evaluation's stack limit (a lambda that calls itself is `#CALC!`, not a crash); each call counts against the evaluation's budget |
| An .xlsx import | parts may expand to at most 256 MiB and 100× the file's size (at least 16 MiB is always allowed); at most 2 M cells; the preview is parsed off the workbook's writer, two at a time per server, once per upload |
| Sheet undo | the newest 200 steps and 500,000 cell changes; the engine keeps no queue of changes for remote copies |
| A workbook's database | `--max-workbook-size` (no limit by default): past it, operations that add data are refused and deleting stays possible; shown in `/api/admin/resources` |
| Demo tables | 3 per workbook and 10 M rows each over the protocol (`--demo-rows` is the operator's and not capped) |
| Online backups | one at a time, not more often than once a minute, only with free disk for them, and only the newest five kept; they can be listed and deleted |

## `--disable-sql`

Refuses every operation that runs user-written SQL: the SQL console, saved queries, tables from
queries, SQL formula columns (adding or changing one) and SQL functions (testing or publishing),
also when a saved query or SQL function is taken in from a space library.
Spreadsheet formulas, no-code analysis and Python functions stay available. A new column's type
must be one of the import types (`BIGINT`, `VARCHAR`, `DECIMAL(p,s)`, …), never an expression.

## Exports and spreadsheet formulas

A CSV export is the data as it is: a value such as `-5`, `+1 555` or `=x` round-trips exactly,
which is what a data tool owes its users. Spreadsheet applications may run text that starts
with `=`, `+`, `-` or `@` as a formula when such a file is double-clicked, so the export menu
also offers a CSV export for spreadsheet apps (`csv_excel`): text starting with `=`, `+`, `-`, `@`, a
tab or a carriage return gets a leading `'`, column names in the header row included; numbers
are unchanged. Hand that one to people
who will open it in a spreadsheet. Text copied from a table into a sheet always stays text,
even when it looks like a formula.

Faithful CSV by default, neutralized CSV on request, is the project's current stance.

## Ids

Workbook, upload, export and download ids are 64 random bits from the operating system's
source. A panic while opening a workbook fails that start, not the server.

## Not adopted

* DuckDB's `lock_configuration`: memory and thread limits are re-applied while a workbook runs
  (as sheet memory changes), which a locked configuration would refuse. External access cannot
  be re-enabled from SQL either way (tested).
