# Design

The web UI follows the Alephgrid brand kit and the v2 UI direction: grid first, quiet chrome,
brand green only where it carries meaning. This page is the contract for the shell; screens
add their own detail on top.

## Themes

Light, dark, or the system's choice (`web/src/theme.ts`). The choice is kept in this browser
only and set as `data-theme` on `<html>`; every color is a CSS token in `web/src/styles.css`,
and the canvas grid reads the same tokens and repaints when the theme changes.

| Token | Light | Dark |
|---|---|---|
| `--canvas` (hub background) | `#F8FAF8` paper | `#111113` |
| `--bg` (surfaces, dialogs) | `#FFFFFF` | `#18181B` |
| `--panel` (bars, sidebar) | `#F6F8F7` | `#18181B` |
| `--control` (hover, pressed) | `#EEF3F0` | `#27272A` |
| `--border` / `--border-strong` | `#E1E8E4` / `#CBD8D1` | `#2E2E33` / `#3F3F46` |
| `--text` / `--muted` | `#102D25` / `#5A6B64` | `#FAFAFA` / `#A1A1AA` |
| `--accent` (actions, selection, focus) | `#047857` emerald | `#34D399` mint |
| `--danger` | `#B42318` | `#FDA29B` |

Dark surfaces are neutral charcoal: no green wash on bars, panels or tables. Formula reference
colors (blue, purple, orange, cyan) stay distinct from the accent. The type is Manrope (OFL,
`web/public/fonts`), with a monospace face for formulas, SQL and the name box.

## Workbook layout

1. **Top bar**: the mark (back to all workbooks), the workbook name (click to rename), the
   connection state, the menu bar, and on the right command search (⌘K), history, compute and
   theme. The save state is honest (U.11): **Saved** only when the server acknowledged every
   change this page sent (it acknowledges after commit), **Saving…** while a reply is
   outstanding, **Unsaved changes** while changes wait for the connection, **Not saved** when one
   was dropped after the connection stayed down too long.
2. **Menu bar**: File, Edit, View, Insert, Format, Data, Tools, Help. Every workbook command
   is in a menu; commands that only apply to tables or sheets are disabled, not hidden, so the
   menus keep their shape.
3. **Toolbar**: the commands used most, as icon buttons with tooltips. Sheets show undo, redo,
   named ranges and go to; tables show views, filter, add column, format and find / replace.
   Import, Analyze, SQL, Functions and Export are always there.
4. **Formula bar**: name box, `fx`, the cell's value or formula.
5. **Grid**, then the **tab bar** with the selection's statistics and the row count.

The **explorer** (toolbar button or View → Explorer, collapsed by default) lists tables with row
counts, sheets, saved queries and functions; each opens from there. The **SQL workspace** docks
under the grid, closed by default (Tools → SQL workspace, or the toolbar): drag its top edge or
use the arrow keys on it to resize, maximize it over the grid, close it with its button or Escape.

Other advanced work (functions, analysis, import) opens in dialogs. Calling a function happens directly in the cell editor.

The mark is the brand-kit-v1 favicon geometry without its tile, colored by `--brand-ink` and
`--accent` so it reads on both themes.

## Hub

A sidebar (all workbooks, recent, starred, spaces, trash, theme) and a main column: search, the
current scope's title and actions, cards for the most recently opened workbooks, and the full
list. A file dropped anywhere on the page is imported.

## Icons

`web/src/icons.ts`: line icons drawn for Alephgrid on a 24-unit grid, 1.75 stroke, `currentColor`.
Icons are decorative (`aria-hidden`); the control holding one carries the accessible name.
