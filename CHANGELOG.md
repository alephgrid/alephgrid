# Changelog

Versions follow [Semantic Versioning](https://semver.org). Before 1.0, a minor version may change
the storage format; such versions migrate workbooks on open and keep the old file (see
docs/OPERATIONS.md, Upgrades). Only the latest release receives fixes.

## Unreleased (launch v1)

### Sheets
- Formatting: bold, italic, underline, strikethrough, size, colours, alignment, wrap, borders, number formats, merged cells; kept through XLSX.
- Layout: frozen rows and columns, row heights and column widths, hide and unhide, auto-fit, zoom.
- Find and replace in a sheet or all sheets; range sort (formulas move with their rows); range filters.
- Tabs: duplicate, colour, hide, reorder.
- Charts placed on the sheet, colour rules and scales, allowed values, comments with replies.
- Insert chart shows a preview that is saved only on Create chart; deleting a chart offers Undo.
- Fill handle: drag the selection's corner to continue a series, count text such as Week 1, or repeat a pattern; one undo step.
- Mod+A selects the filled block first, then the sheet; the copied range is outlined until Escape; paste special is a menu at the cell.
- Copying, filling or dragging a formula that reads a table gives each copy its own formula with its references shifted.

### Workbooks and functions
- New workbooks open straight into a blank sheet; SQL stays closed until opened.
- Functions are edited in a resizable side panel beside the grid (Code, Tests, Docs, Versions) with editable test rows and a results strip.
- Disabled menu items say why they are unavailable.
- Hub: one-line empty state, Clear search, Try again on errors.

### Tables and analysis
- Analyze side pane, header value filters, find bar, charts, first-run guidance.
- Table imports reviewed in a side pane with column types and samples.

### Agents
- Read-only MCP access for local workbooks: agent connections, `/mcp` (Streamable HTTP) and `alephgrid mcp` (stdio), schema cards, catalog search, read-only SQL, saved queries and analyses, pinned results with receipts, activity.

### Teams and administration
- Share on a workbook, space settings, server administration (resources, sessions, backups, usage, audit).

### Security
- The remediation from the 31 October 2026 audit (docs/SECURITY.md).
