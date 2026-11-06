# Interface design

The rules the web UI follows: grid first, quiet chrome, brand green only where it carries
meaning. This page is the contract for the shell, its states and its brand assets; screens add
their own detail on top. Concept images are design proposals, never evidence that something
works.

## Principles

- **The spreadsheet is the product.** Opening a workbook lands in its grid; a new workbook opens
  into a blank sheet with the grid focused. Importing, editing, calculating, filtering,
  summarizing, charting and exporting never require SQL.
- **SQL is optional.** The SQL workspace stays closed until someone opens it, and a server can
  turn it off. No spreadsheet workflow needs a query as a workaround.
- **Direct manipulation.** Type values and formulas in the cell or the formula bar. Prefer
  anchored menus and side panels to dialogs; keep blocking dialogs for consequential decisions.
- **Density over decoration.** No large greetings, metric cards, gradients, glow or heavy
  shadows. Decorative separators never replace control boundaries or focus outlines.
- **Honest states.** Never show **Saved** before the server acknowledged the change. Every
  shipped control works or says why it cannot. Sleeping compute is normal, not a failure.
- **Compatibility is a target, not a claim.** Unsupported operations are disclosed where they
  happen, never silently changed in meaning.

## Themes and tokens

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

Dark surfaces are neutral charcoal: no green wash on bars, panels or tables. Green is for the
brand mark, selection outlines, active tab indicators, primary actions and small status
accents. Formula reference colors (blue, purple, orange, cyan) stay distinct from the accent and
also carry labels, so hue is never the only signal. Charts use a small palette in which green
is not the only series color.

The type is Manrope (OFL, `web/public/fonts`), with a monospace face for formulas, SQL and the
name box and tabular numerals for data. Body text is 13–14 px and never below 12 px; controls
are compact (about 32 px) with 6–8 px radii and a 4/8/12/16/24/32 px spacing scale.

## Workbook layout

1. **Top bar**: the mark (back to all workbooks), the workbook name (click to rename), the
   connection and save state, the menu bar, and on the right command search (⌘K), Share,
   History, Compute and theme.
2. **Menu bar**: File, Edit, View, Insert, Format, Data, Tools, Help, built from the same command
   registry as the toolbar, the palette and the shortcut help. Commands that do not apply to
   the current tab are disabled with the reason in place of their shortcut ("Sheets only",
   "SQL is off on this server"). The Format menu shows sheet or table formatting to match the
   tab. Clipboard and selection commands live on the keyboard, the cell menu and the palette.
3. **Toolbar**: the explorer toggle, then the commands used most as icon buttons with tooltips.
   Sheets show undo, redo, named ranges and go to, with a cell-format row below; tables show
   views, filter, add column, format and find / replace. Import, Analyze, Functions and Export
   follow; SQL appears only when the server allows it.
4. **Formula bar**: name box, `fx`, the cell's value or formula.
5. **Grid**, then the **tab bar** with the selection's statistics and the row count.

The **explorer** (collapsed by default) lists tables with row counts, sheets, saved queries and
functions; each opens from there. The **SQL workspace** docks under the grid: drag its top edge
or use the arrow keys on it to resize, maximize it over the grid, close it with its button or
Escape. **Analyze** and **Functions** open as side panels beside the grid, one at a time; the
table import review is a side pane too. Dialogs are for XLSX imports, formatting and data
tools, history, compute and confirmations.

## Hub

A quiet sidebar (all workbooks, recent, starred, spaces, trash, theme; Administration for
administrators) and a main column: search, the current scope's title with **Open file** and
**New workbook**, up to three cards for recently opened workbooks (icons only; no workbook data
is loaded to draw them), and a sortable list with a per-workbook menu that respects roles. A
file dropped anywhere on the page is imported. Search, sort and scope are kept when you come
back from a workbook.

## Editing, selection and clipboard

- Typing replaces the selected cell; F2 or double-click edits it. Enter, Shift+Enter, Tab and
  Shift+Tab commit and move; Escape cancels the edit before it closes anything else.
- Formula mode starts with `=`: an anchored suggestion list, argument hints, click-and-drag
  reference picking with colored ranges, and F4 to cycle `$` anchors. The draft and caret
  survive picking.
- Mod+A selects text in an editor; in the grid it selects the filled block, then everything.
- Copy outlines the copied range until Escape. A paste that is invalid, too large or read-only
  pastes nothing and says why; nothing is silently truncated. One gesture is one undo step.
- Shortcuts are platform-aware and come from one registry; see [KEYBOARD.md](KEYBOARD.md).

## Panels, tools and drafts

Opening a tool never changes the workbook. Insert chart shows an unsaved preview until
**Create chart**; rules and allowed values are saved by their explicit Add button; Analyze
stores nothing until Save. Cancel, Escape or switching sheets leaves the workbook as it was.
Changes to an already placed object save at once and say so, and deleting one offers Undo.

Side panels keep their draft, tab and scroll while closed, return focus to their trigger, and
can be resized by pointer or arrow keys while leaving the grid usable (the Functions panel stops
where 560 px of grid would remain). Opening a panel keeps the selection, scroll position and any
draft in the grid. Function authoring (Code, Tests, Docs, Versions) is described in
[FUNCTIONS.md](FUNCTIONS.md); calling a function never opens the panel.

## Component states

| Family | States every instance handles |
|---|---|
| Buttons | Primary, secondary, ghost, icon, destructive; hover, focus-visible, pressed, disabled with a reason, busy; icon-only buttons have a label |
| Inputs and choices | Empty, filled, focus, invalid with linked error text, disabled or read-only; keyboard-operable lists and groups |
| Menus and popovers | Anchored, kept inside the viewport, dismissed by Escape or outside click, focus returned to the trigger; real shortcuts shown |
| Grid | Active cell, range, copied range, frozen, hidden and read-only cells, editor, errors |
| Feedback | Saving and saved, progress with cancel, retry, toasts (never the only place an error is shown), empty, loading and denied |
| Dialogs | Labelled title, focus kept inside, Escape closes, focus returned; used for consequential or necessary decisions only |

Progress reflects measured work, or is indeterminate when the total is unknown. A cancel is
pending until the server acknowledges it.

## Connection and save state

Shown next to the workbook name (`renderStatus` in `web/src/workbook.ts`). The first matching
line wins:

| State | Text | Meaning |
| --- | --- | --- |
| Save failed | **Not saved** | A change was lost while the connection was down; the tooltip asks to check and redo the last edits |
| Unsent changes | **Unsaved changes** | Kept in the page and sent again when the connection returns |
| Paused | **Paused** | The workbook's compute sleeps; continuing resumes it |
| Connecting | **Connecting…** | First connection, or after a server restart |
| Disconnected | **Reconnecting…** | Retrying; edits are queued |
| Sending | **Saving…** | Changes are on their way |
| Acknowledged | **Saved** | Shown only after the server has acknowledged every change |

Unacknowledged edits are resent after a reconnect and applied at most once
(`e2e/savestate.spec.ts`, `e2e/resilience.spec.ts`). Leaving the workbook before they are
acknowledged asks first, whether through the mark, a menu link or the browser's Back button;
closing the tab gets the browser's own warning.

## Screen states

| Screen | Empty | Loading | Working | Failed / refused |
| --- | --- | --- | --- | --- |
| Hub | "No workbooks here yet. Drop a file on this page to start."; no spaces: "Ask an admin to add you to one."; search: "No workbooks match …" with Clear search | — | — | The reason with Try again (search, sort and scope kept); unauthenticated → sign-in page, then back to the same path |
| Grid (tables) | Empty table with the append row | Placeholder bars in cells being fetched | Busy overlay for long view changes | Error toast; the view keeps its last good state |
| Grid (sheets) | "Empty sheet" in the status bar | Placeholder bars in cells being fetched | — | Refused edits explain why (allowed values, covered merged cells, a stale layout, someone else's change) and keep the cell's value |
| Formula bar | — | — | — | "Formula not saved: … Fix it, press Escape to discard…" with the draft kept |
| Import | — | Upload progress | Import progress bar with **Stop import**; the pane stays open to retry | The problem and, when known, a one-click fix ("Import “col” as … and try again"); cancelling leaves no staged file |
| SQL workspace, Analyze | Hints for a first query or analysis | Running indicator with Cancel | Queued and running jobs | The error with its position; results stay until replaced |
| Compute | Sleeping is described as normal | Starting / waiting in line | Running, resizing (restart) | Failed with the reason, Cancel and Restart |
| Agent requests | — | — | A request that outlives its wait is a job (`job_status`) | Tool errors carry a recovery hint; cancelled jobs are discarded |
| Dialogs and panes | "No … yet." lines (rules, comments, connections, proposals, backups) | "Loading…" | Buttons disabled while a change is in flight | Errors in the dialog's alert line; input kept |

Objects a person cannot see are reported as **not found**, never as forbidden, so an id from
another space reveals nothing. Actions the role does not allow are disabled in menus and refused
by the server (`tests/teams.rs`, `e2e/teams.spec.ts`).

## Navigation and object identity

```
Hub (/)                 All workbooks · Recent · Starred · Spaces (sidebar) · Trash
 └─ Space               sidebar entry; menu: Rename…, New workbook here, Library…,
                        Members… (or Who has access…), Service tokens…, Agent connections…,
                        Settings…, Delete space
     └─ Workbook        /w/<id>; title, connection/save state, Share
         ├─ Tables      tabs; header menus, filters, views (saved per table)
         ├─ Sheets      tabs; colour, hide, duplicate, reorder
         ├─ Queries     SQL workspace (saved and live queries, versions)
         └─ Functions   Functions panel (versions, pins, publication)
```

- A workbook is addressed by its id; renaming or moving it keeps its link. Deep links survive
  sign-in: the login page returns to the path that sent it.
- Tables and sheets are addressed by name inside a workbook. The active tab is marked
  (`aria-selected`), and the explorer highlights the same object.
- Sheets keep a stable internal id. Comments, rules and charts attach to it, so they follow
  renames and tab moves.

## Accessibility and narrow windows

Keyboard-only use of selection, editing and every command; visible focus; screen-reader names
for cells, errors and controls; 200% zoom; reduced motion (transitions off); at least 4.5:1
contrast for text and suitable contrast for controls, focus, charts and selection in both
themes. At narrow widths the explorer collapses, the menu bar scrolls and the page never scrolls
sideways; the grid scrolls on purpose. Automated checks run axe in both themes; the manual
script is [ACCESSIBILITY_TESTING.md](ACCESSIBILITY_TESTING.md).

## Release journey

The end-to-end check of the spreadsheet-first contract (`e2e/spreadsheetfirst.spec.ts`, plus
`e2e/analyst.spec.ts` for refresh and export): from the hub, create a workbook and import orders
and customers; select, copy and paste with the keyboard; type a lookup into the grid and fill
it; compute conditional totals; filter; build a chart from a preview; undo and redo; refresh
data; reopen; export. SQL stays closed throughout, routine formulas need no dialog, and values,
persisted state and failure behaviour are asserted, not just screenshots.

## Icons and brand assets

`web/src/icons.ts` holds line icons drawn for Alephgrid on a 24-unit grid, 1.75 stroke,
`currentColor`; they are not copied from an icon set. Icons are decorative (`aria-hidden`); the
control holding one carries the accessible name.

| Asset | In the repository | Origin |
|---|---|---|
| Mark | `web/src/icons.ts` `brandMark`, `web/public/favicon.svg`, `assets/brand/` | Supplied by the project owner and refined in the Alephgrid brand kit (October 2026). The app draws it without its tile, colored by `--brand-ink` and `--accent` so it reads on both themes |
| Wordmark ("alephgrid") | `assets/brand/*.svg`, text in the app | Brand kit; logo lettering outlined from Manrope. In the app the wordmark is live text |
| Typeface | `web/public/fonts/Manrope-variable.woff2` with `OFL-Manrope.txt` | Manrope, SIL Open Font License 1.1, from google/fonts (retrieved 30 October 2026). Keep the OFL notice with the font |
| Colors | `web/src/styles.css`, this page | Brand kit tokens (forest, emerald, mint, paper, ink) and a neutral dark palette |
| Screenshots | `assets/screenshots/` | Taken from the running product by `web/scripts/readme-screenshots.mjs` |

The brand kit's concept boards are AI-generated design proposals: they guide the design, are not
shipped assets, and are not evidence of shipped behaviour. Reuse terms for the original brand
artwork (outside the font's OFL) are the project owner's decision and have not been chosen yet.
