---
title: Keyboard shortcuts
description: Every grid, editing and navigation shortcut, with browser conflict notes. Mod is ⌘ on macOS and Ctrl elsewhere.
---

**Mod** means ⌘ on macOS and Ctrl elsewhere (either is accepted). All commands live in one
registry; menus, the command palette (Mod+K) and the in-app shortcut help (Mod+/) are generated
from it, and a test fails if two commands share a key in the same context. Keys are ignored
while an input method is composing (IME), so composing Japanese, Chinese or Korean text never
triggers a command. Letter shortcuts follow the letter printed on the key for Latin layouts;
on layouts that type another script (Cyrillic, Greek, Hebrew) the physical key position is
used, so Ctrl+C still copies when the key types `с`.

## Grid

Sheet unless noted.

| Command | Keys | Notes |
|---|---|---|
| Undo / Redo | Mod+Z / Mod+Shift+Z, Mod+Y | Sheets: your own last step. Tables: your own last table change; refused when someone changed it since |
| Copy | Mod+C | Tables and sheets. Copies displayed values as text; the copied range is outlined until Escape |
| Cut | Mod+X | Sheets |
| Paste | Mod+V | Text copied from this workbook pastes the cells (formulas translated, formats kept); other text pastes as values |
| Paste special… | Mod+Shift+V | A menu at the active cell: all / formulas / values / formats, and all or values transposed. Copied cells only |
| Fill down / right | Mod+D / Mod+R | Replace the browser's bookmark and reload keys while the grid has focus |
| Edit cell | F2, Enter | |
| Clear cells | Delete, Backspace | |
| Select all | Mod+A | Sheets: first the block of filled cells around the active cell, then the whole sheet; from an empty cell, the whole sheet. Tables: everything |
| Select whole rows / columns | Shift+Space / Ctrl+Space | macOS uses Ctrl+Space for input sources by default; use the palette there |
| Go to… | Mod+G | Sheets. Cell, range, other sheet or name |
| Bold / Italic / Underline | Mod+B / Mod+I / Mod+U | Sheets |
| Find… | Mod+F | Tables search every row on the server; sheets this sheet or all sheets |
| Find and replace… | Mod+H, Mod+Shift+H | macOS keeps ⌘H for hiding the browser; use ⌘⇧H |
| Formula help | Shift+F3 | |
| Command palette | Mod+K | |
| Keyboard shortcuts | Mod+/ | |
| Move | Arrows, Tab, Home/End, PageUp/PageDown, Mod+arrows | |

### Fill handle

Drag the square at the selection's bottom-right corner (sheets) — down, up, right or left.
Two or more numbers continue their trend (1, 2 → 3, 4), one text cell ending in a number
counts on (Week 1 → Week 2), anything else repeats with formulas shifted. One undo step.
The keyboard alternative is Fill down / right.

## While editing

In the cell editor or the formula bar.

| Command | Keys |
|---|---|
| Save and move down | Enter |
| Save and move right | Tab (accepts a function suggestion first) |
| Discard | Escape |
| Cycle `$` anchors | F4 |

## Browser and OS conflicts

Keys that browsers or the OS also use, and how Alephgrid handles each:

| Key | Browser / OS default | Decision |
|---|---|---|
| Mod+D | Bookmark page | Fill down, only while the grid has focus |
| Mod+R | Reload | Fill right, only while the grid has focus; reload with the browser button or F5 |
| Mod+G | Find next | Go to, while the grid has focus |
| Mod+K | Focus search (some browsers) | Command palette, while the grid has focus |
| Mod+F | Find in page | Find bar, while the grid has focus |
| Mod+H | Hide the browser (macOS), history (some browsers) | Find and replace; on macOS use Mod+Shift+H |
| Mod+Shift+V | Paste as plain text | Paste special, while the grid has focus |
| Ctrl+Space | macOS input source switch | Bound and documented; the OS may take it first |
| Cmd+Space, Mod+T, Mod+W, Mod+N, Mod+Shift+N, Mod+Tab | Spotlight, tabs, windows | Never bound |
| F5, F11, F12 | Reload, full screen, dev tools | Never bound |

## Browser differences

The suite runs in Chrome, Firefox and WebKit (the engine of Safari). What users of those
browsers should know:

| Browser | Behaviour | What to do |
|---|---|---|
| Safari | Tab moves only between text fields unless *Settings → Advanced → Press Tab to highlight each item* is on | Option+Tab reaches buttons, checkboxes and the schema tree |
| Safari | Clicking a button does not focus it | Dialogs keep Escape and Tab working wherever focus is |
| Safari, Firefox (macOS) | End and Home scroll a text field instead of moving the caret, as in native Mac apps | ⌘→ / ⌘← move to the end or start of the line; ⌘↓ to the end of the SQL editor |

:::note
Not verified yet: screen-reader announcements (VoiceOver, NVDA, JAWS), Windows browsers, and
physical non-US keyboards — layouts are simulated with synthetic key events in the test suite.
The authoritative source is the repository's
[keyboard reference](https://github.com/alephgrid/alephgrid/blob/main/docs/KEYBOARD.md).
:::
