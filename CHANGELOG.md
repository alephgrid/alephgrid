# Changelog

Versions follow [Semantic Versioning](https://semver.org). Before 1.0, a minor version may change
the storage format; such versions migrate workbooks on open and keep the old file (see
docs/OPERATIONS.md, Upgrades). Only the latest release receives fixes.

## Unreleased

The first release of Alephgrid: an open-source, self-hosted spreadsheet for data that doesn't
fit in a spreadsheet. Open a CSV or Parquet file with hundreds of millions of rows, scroll,
sort, filter and edit it in your browser, and build ordinary Excel-style sheets next to it —
with formulas that read the big tables directly. Everything runs in one Apache-2.0 binary on a
DuckDB file on your machine; your data never leaves it. There are no seats, tiers or paywalls.

### Install

- `npx alephgrid` (also pnpm dlx / yarn dlx / bunx)
- `docker run -p 7878:7878 -v alephgrid-data:/data ghcr.io/alephgrid/alephgrid`
- Downloads for Linux (x86_64, arm64) and macOS (Apple silicon, Intel) with `SHA256SUMS`,
  attached to this release. On Windows, use Docker or WSL.
- From source: see the [README](https://github.com/alephgrid/alephgrid#from-source).

Docs: [alephgrid.dev/docs](https://alephgrid.dev/docs/getting-started/)

### Sheets

- Excel-syntax formulas on an Excel-compatible engine ([IronCalc](https://github.com/ironcalc/IronCalc)):
  `XLOOKUP`, `SUMIFS`, dynamic arrays, `LET`, `LAMBDA`, cross-sheet references, named ranges,
  autocomplete and argument help. Compatibility is tracked publicly:
  [formula compatibility](https://alephgrid.dev/docs/reference/formulas/).
- Formatting (bold to number formats, borders, merged cells), frozen panes, row/column sizing,
  hide/unhide, zoom — all kept through XLSX import and export.
- Charts (column, bar, line, area, scatter, pie) with a preview before they are saved, colour
  rules and scales, allowed-value lists, comments with replies.
- Keyboard-first editing: fill handle that continues series, paste special, find and replace
  across sheets, range sort and filters, undo/redo. Full list:
  [keyboard shortcuts](https://alephgrid.dev/docs/reference/keyboard/).

### Big tables

- A virtual grid over any number of rows: opening a workbook reads nothing into memory, sorts
  build a permutation instead of a copy, calculated columns compute only what you look at.
- Typed edits, insert/delete rows, add/rename/delete columns, multi-column sort, header value
  filters with counts, saved views.
- **Table formulas**: `=SUMIFS(orders[amount], orders[region], A2)` in a sheet cell runs as one
  query over the whole table, not one lookup per row.
- The Analyze pane: summaries, pivots, joins, charts and column profiles without SQL;
  deduplicate, split, trim and convert types with previews.

### Import and export

- CSV/TSV (auto-detected), Parquet, JSON/JSONL and gzip as tables; XLSX as editable sheets.
  Streamed, with column types and samples reviewed before commit.
- Export any table or filtered view as CSV, Parquet or JSON lines; sheets as XLSX with a
  report of anything XLSX cannot hold.

### SQL, when you want it

- A dockable SQL workspace — schema browser, autocomplete, `$parameters`, cancellable
  previews, saved and live queries with versions, tables from queries. It reads the workbook's
  own tables only, stays closed until opened, and `--disable-sql` turns it off server-wide.

### Your own functions

- Reusable functions as sheet formulas, SQL expressions or sandboxed Python (macOS Seatbelt /
  Linux bubblewrap, no network, fail-closed), with tests, published versions, pinning and
  rollback.

### Collaboration, teams and administration

- Several people on one workbook see each other's edits live, with history and revision
  restore.
- `--auth accounts`: organizations, spaces, owner/editor/viewer roles (also from
  identity-provider groups), invitations, service tokens, password or OpenID Connect sign-in,
  quotas, audit log, backups and an administration screen.
- Per-workbook compute that sleeps when idle and wakes when opened; admins grant larger sizes
  with `--profile`.

### Agents (MCP)

- Read-only [Model Context Protocol](https://alephgrid.dev/docs/agents-mcp/) access per space
  or workbook, over Streamable HTTP (`/mcp`) or stdio (`alephgrid mcp`): schema cards, catalog
  search, reads, read-only SQL, saved queries and analyses — every result with a
  content-hashed receipt. Listed in the official MCP registry as `dev.alephgrid/alephgrid`.

### Security

- Ships the remediation from the 31 October 2026 security audit. Reporting:
  [SECURITY.md](https://github.com/alephgrid/alephgrid/blob/main/SECURITY.md).

### Known limits

- Full Excel parity is a long-term target, not a claim — the
  [compatibility page](https://alephgrid.dev/docs/reference/formulas/) says exactly what is
  verified, partial and missing.
- One server process per data directory; no distributed compute or multi-writer DuckDB.
- Native Windows builds, Homebrew/WinGet packages and a Helm chart come after this release.
- Before 1.0, a minor version may migrate the storage format on open (the old file is kept).
