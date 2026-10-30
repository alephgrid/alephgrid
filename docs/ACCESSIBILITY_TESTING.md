# Manual accessibility and keyboard testing

The automated suite checks roles, names and focus with axe and keyboard-only journeys in
Chrome, Firefox and WebKit (see [KEYBOARD.md](KEYBOARD.md#browsers-and-platforms)). It cannot
tell what a screen reader actually says, how a physical keyboard behaves, or how Windows
browsers behave. This script covers those. Run it before a release that changes the grid,
dialogs, the formula bar or keyboard handling, and record the result below; a release must not
claim these were verified without a row here.

## Setup

1. Start Alephgrid (`alephgrid --demo-rows 1000`) and open the demo workbook.
2. Add a sheet named `Data` with A1 = `10`, A2 = `20`, B1 = `=A1+A2`.
3. Screen readers: VoiceOver (macOS, Safari: ⌘F5), NVDA (Windows, Firefox and Chrome), JAWS
   (Windows, Chrome) if available. Use default settings; note the version.

## Screen-reader checks

| # | Do | Expected |
|---|---|---|
| S1 | Tab into the sheet grid | "spreadsheet" (or "grid"), its name, then the active cell, e.g. "A1: 10" |
| S2 | Arrow right, then down | Each move reads the new cell: "B1: 30", "B2: empty" |
| S3 | Open the demo table, move to a cell | Column and row: "amount, row 1: …"; "loading" only while the row loads |
| S4 | Shift+Arrow to select A1:B2 | The selection size is announced once (polite), without repeating every cell |
| S5 | Type `=A1*` in B2, press Enter | The cell is saved; moving back reads "B2: …" with the computed value |
| S6 | Type `=1+` in B3, press Enter | The problem is announced ("Formula not saved: … at character 4") and focus stays in the editor |
| S7 | Mod+K, type "fill", Enter | The palette is read as a dialog with a search box; results are announced; the command runs |
| S8 | Mod+/ | The shortcut list is read as a dialog; Escape closes it and focus returns to the grid |
| S9 | Formula help (Shift+F3), search "XLOOKUP" | The match count is announced; the details region reads the signature and examples |
| S10 | SQL, Shift+Tab from the editor into the schema tree | The tree and items are announced ("sales, 4 rows", expanded/collapsed); Enter inserts a column |
| S11 | Paste special (Mod+Shift+V after a copy) | Radio group and checkbox are named; Enter pastes; Escape cancels |
| S12 | Any dialog: click a button, then press Escape | The dialog closes (Safari keeps focus on the page body after a click) |

## Keyboard and platform checks

| # | Setup | Do | Expected |
|---|---|---|---|
| K1 | French AZERTY (macOS and Windows) | Mod+A, Mod+Z, Mod+C/V, Mod+D in the grid | Select all, undo, copy/paste and fill down act on the printed letters |
| K2 | German QWERTZ | Mod+Z, Mod+Y (Windows) | Undo / redo by the printed letters (Z and Y are swapped against US) |
| K3 | Russian (ЙЦУКЕН) | Ctrl/⌘+C, Ctrl/⌘+V on the keys printed C and V | Copy and paste by key position |
| K4 | Japanese IME | Type かな into a cell with the IME on; press Enter to confirm the conversion | The first Enter confirms the text; no command or cell move fires while composing |
| K5 | Dvorak | Mod+C on the key printed C | Copies (letter shortcuts follow the typed letter on Latin layouts) |
| K6 | Safari, default settings | Tab and Option+Tab through a dialog | Tab visits text fields only; Option+Tab reaches every control |
| K7 | macOS Safari or Firefox | In the formula bar, ⌘→ then type | The caret goes to the end (End only scrolls there) |
| K8 | Firefox | Copy A1:A2 (Mod+C), select C3, paste (Mod+V); cut and paste | Formulas are translated; cut moves cells and updates references (not automatable in Playwright's Firefox) |
| K9 | Windows Chrome, Edge and Firefox | Ctrl+D, Ctrl+R, Ctrl+G, Ctrl+K in the grid | Fill down/right, Go to and the palette run; the browser's bookmark, reload and search do not |
| K10 | Any browser, zoom 200% and a narrow window | Use the grid, formula bar and a dialog | Nothing is cut off; focus stays visible |

## Results

| Date | Tester | OS / browser | Assistive technology / layout | Checks | Result and notes |
|---|---|---|---|---|---|
| — | — | — | — | — | Not run yet |
