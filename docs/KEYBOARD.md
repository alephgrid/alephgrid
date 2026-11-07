# Keyboard shortcuts and commands

All workbook commands live in one registry (`web/src/commands.ts`, with the commands defined in
`web/src/workbookcommands.ts`). The grid gives every key
to the registry first; menus, the command palette (**⌘K** / **Ctrl+K**) and the shortcut help
(**⌘/** / **Ctrl+/**) are generated from it. A test fails if two commands share a key in the
same context. Keys are ignored while an input method is composing (IME), so composing Japanese,
Chinese or Korean text never triggers a command.

**Mod** means ⌘ on macOS and Ctrl elsewhere (either is accepted).
Letter shortcuts follow the letter printed on the key for Latin layouts (AZERTY's A is A); on
layouts that type another script (Cyrillic, Greek, Hebrew) the physical key position is used, so
Ctrl+C still copies when the key types `с`.

## Grid (sheet unless noted)

| Command | Keys | Notes |
|---|---|---|
| Undo / Redo | Mod+Z / Mod+Shift+Z, Mod+Y | Sheets: the caller's own last step. Tables: the caller's own last table change, and redo of their last undo; refused when someone changed it since ([COLLABORATION.md](COLLABORATION.md)) |
| Copy | Mod+C | Tables and sheets. Copies displayed values as tab-separated text to the system clipboard, and from a sheet also an HTML table with raw values and basic formats for other spreadsheets (up to 100,000 cells); the copied range is outlined until Escape. Only shown rows are copied: a sheet's hidden rows and columns (a range filter's included) are left out of the text and of a paste in this workbook, and a filtered table copies the rows its filter keeps (at most 200,000 rows; use Export beyond that) |
| Cut | Mod+X | Sheets. Pasted on the same or another sheet of the workbook, the cells move (one undo step) and formulas that read them follow. Refused, with nothing changed and the reason at the cell, when the cut range has hidden rows or columns, is too large, or a sheet changed since the cut |
| Paste | Mod+V | System paste event. Cells copied in this workbook paste as cells (formulas translated, formats kept), on this or another sheet. Cells copied from a table paste into a sheet with their types (text stays text, so `007` keeps its zeros). Cells copied in other spreadsheets paste from their HTML table data: raw values, number formats, bold, italic, underline, strikethrough, text colour and fill; their formulas paste as values (Paste special takes them); in a table only the raw values. Other text pastes as tab-separated values; text starting with `=` stays text. One undo step. A paste that is invalid, too large or read-only pastes nothing and says why at the cell |
| Paste special… | Mod+Shift+V | A menu at the active cell: all / formulas / values / formats, and all or values transposed. Cells copied in this workbook, and text or HTML from other apps (All and Formulas take their R1C1 formulas when they parse; transposed, their values). The page reads the clipboard where the browser allows; otherwise it asks for Mod+V and pastes that as chosen. Not cut cells; formats only of plain text is refused |
| Fill down / right | Mod+D / Mod+R | Replace the browser's bookmark and reload keys while the grid has focus |
| Fill handle | Drag the square at the selection's bottom-right corner | Sheets. Down, up, right or left: two or more numbers continue their trend (1, 2 → 3, 4); one text cell ending in a number counts on (Week 1 → Week 2); weekday, month and quarter names continue by name in the same case (Mon → Tue, January → February, Q4 → Q1; Mon, Wed → Fri); one date counts on by day, dates on the same day of the month continue by month (Jan 15, Feb 15 → Mar 15; the 31st becomes the month's last day), other dates follow their trend; anything else repeats with formulas shifted. One undo step. Keyboard alternative: Fill down / right |
| Edit cell | F2, Enter | Handled by the grid |
| Clear cells | Delete, Backspace | Handled by the grid |
| Select all | Mod+A | Sheets: first the block of filled cells around the active cell, then the whole sheet; from an empty cell, the whole sheet. Rows hidden by a filter inside the block are selected but left out of a copy. Tables: every row the current filter keeps |
| Select whole rows / columns | Shift+Space / Ctrl+Space | macOS uses Ctrl+Space for input sources by default; use the palette there |
| Go to… | Mod+G | Sheets. Cell, range, other sheet or name |
| Bold / Italic / Underline | Mod+B / Mod+I / Mod+U | Sheets |
| Find… | Mod+F | Tables search every row on the server; sheets this sheet or all sheets |
| Find and replace… | Mod+H, Mod+Shift+H | macOS keeps ⌘H for hiding the browser; use ⌘⇧H |
| Formula help | Shift+F3 | |
| Command palette | Mod+K | |
| Keyboard shortcuts | Mod+/ | |
| Move | Arrows, Tab, Home/End, PageUp/PageDown, Mod+arrows | Handled by the grid |

## Focus

Keys go to whatever has focus, in this order of precedence:

1. **An open menu or dialog.** Menus take Escape first (it closes them); a dialog keeps Tab
   and Escape inside it even when a click left focus elsewhere, and the dialogs beneath the
   topmost one are inert. Grid shortcuts do not run.
2. **The cell editor or the formula bar** while editing. They keep every key, so Mod+A, Mod+C,
   Mod+V and Mod+Z act on the text, except Enter, Tab, Escape and F4 (below). In the formula
   bar, Escape and a saved Enter return focus to the grid (a formula that does not parse keeps
   it there, with the problem shown); leaving the cell editor by clicking elsewhere saves the
   edit.
3. **The grid.** The command registry gets each key first, then movement keys; typing a
   character replaces the active cell. Escape drops the copied outline.
4. **Other fields** (find bar, side panels, the SQL editor) keep their own keys; grid
   shortcuts do not run there.

## While editing (cell editor or formula bar)

| Command | Keys |
|---|---|
| Save and move down | Enter |
| Save and move right | Tab (accepts a function suggestion first) |
| Discard | Escape |
| Cycle `$` anchors | F4 |

## Audit of conflicts

| Key | Browser / OS default | Decision |
|---|---|---|
| Mod+D | Bookmark page | Used for fill down only while the grid has focus |
| Mod+R | Reload | Used for fill right only while the grid has focus; reload with the browser button or F5 |
| Mod+G | Find next | Used for Go to while the grid has focus |
| Mod+K | Focus search (some browsers) | Command palette while the grid has focus |
| Mod+F | Find in page | Find bar while the grid has focus |
| Mod+H | Hide the browser (macOS), history (some browsers) | Find and replace; on macOS use Mod+Shift+H |
| Mod+B, Mod+I, Mod+U | Bookmarks, page info, view source (some browsers) | Cell formatting in sheets while the grid has focus |
| Mod+Shift+V | Paste as plain text | Paste special while the grid has focus |
| Ctrl+Space | macOS input source switch | Bound, documented; OS may take it first |
| Cmd+Space, Mod+T, Mod+W, Mod+N, Mod+Shift+N, Mod+Tab | Spotlight, tabs, windows | Never bound |
| F5, F11, F12 | Reload, full screen, dev tools | Never bound |

## Browsers and platforms

The browser suite runs in Chrome (macOS in development, Linux in CI), and in Playwright's
Firefox and WebKit (the engine of Safari) on macOS with `E2E_BROWSER=firefox` or `webkit`.
Differences found there, and what users of those browsers should know:

| Browser | Behaviour | What to do |
|---|---|---|
| Safari | Tab moves only between text fields unless *Settings → Advanced → Press Tab to highlight each item* is on | Option+Tab reaches buttons, checkboxes and the schema tree |
| Safari | Clicking a button does not focus it | Dialogs keep Escape and Tab working wherever focus is |
| Safari, Firefox (macOS) | End and Home scroll a text field instead of moving the caret, as in native Mac apps | ⌘→ / ⌘← move to the end or start of the line; ⌘↓ to the end of the SQL editor |

Not verified: screen-reader announcements with VoiceOver, NVDA or JAWS (no automated
assistive-technology run is part of the suite), Windows browsers, physical non-US keyboards
(layouts are simulated with synthetic key events), and a real Mod+V into the grid in Firefox
(Playwright's Firefox does not turn Mod+V into a paste; the paste handling itself is tested in
Firefox with script-made paste events), and copying into other spreadsheet apps by hand. [ACCESSIBILITY_TESTING.md](ACCESSIBILITY_TESTING.md)
is the manual script for these. The palette and dialogs are checked with axe and keyboard-only
journeys in `web/e2e/commands.spec.ts`.
