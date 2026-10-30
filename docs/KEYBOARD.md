# Keyboard shortcuts and commands

All workbook commands live in one registry (`web/src/commands.ts`). The grid gives every key
to the registry first; menus, the command palette (**⌘K** / **Ctrl+K**) and the shortcut help
(**⌘/** / **Ctrl+/**) are generated from it. A test fails if two commands share a key in the
same context. Keys are ignored while an input method is composing (IME), so composing Japanese,
Chinese or Korean text never triggers a command.

**Mod** means ⌘ on macOS and Ctrl elsewhere (either is accepted, as before this registry).
Letter shortcuts follow the letter printed on the key for Latin layouts (AZERTY's A is A); on
layouts that type another script (Cyrillic, Greek, Hebrew) the physical key position is used, so
Ctrl+C still copies when the key types `с`.

## Grid (sheet unless noted)

| Command | Keys | Notes |
|---|---|---|
| Undo / Redo | Mod+Z / Mod+Shift+Z, Mod+Y | Sheets only; tables explain that undo is unavailable |
| Copy | Mod+C | Tables and sheets. Copies displayed values as text to the system clipboard |
| Cut | Mod+X | Sheets |
| Paste | Mod+V | System paste event. Text copied from this workbook pastes the cells (formulas translated, formats kept); other text pastes as values |
| Paste special… | Mod+Shift+V | All / formulas / values / formats, optional transpose. Copied cells only |
| Fill down / right | Mod+D / Mod+R | Replace the browser's bookmark and reload keys while the grid has focus |
| Edit cell | F2, Enter | Handled by the grid |
| Clear cells | Delete, Backspace | Handled by the grid |
| Select all | Mod+A | |
| Select whole rows / columns | Shift+Space / Ctrl+Space | macOS uses Ctrl+Space for input sources by default; use the palette there |
| Go to… | Mod+G | Cell, range, other sheet or name |
| Formula help | Shift+F3 | |
| Command palette | Mod+K | |
| Keyboard shortcuts | Mod+/ | |
| Move | Arrows, Tab, Home/End, PageUp/PageDown, Mod+arrows | Handled by the grid |

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
| Mod+Shift+V | Paste as plain text | Paste special while the grid has focus |
| Ctrl+Space | macOS input source switch | Bound, documented; OS may take it first |
| Cmd+Space, Mod+T, Mod+W, Mod+N, Mod+Shift+N, Mod+Tab | Spotlight, tabs, windows | Never bound |
| F5, F11, F12 | Reload, full screen, dev tools | Never bound |

Not verified: screen-reader announcements with VoiceOver, NVDA or JAWS (no automated
assistive-technology run is part of the suite), Windows/Linux browsers (the suite runs Chrome
on macOS in development and Linux in CI), and physical non-US keyboards (layouts are simulated
with synthetic key events). The palette and dialogs are checked with axe and keyboard-only
journeys in `web/e2e/commands.spec.ts`.
