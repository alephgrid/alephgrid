# Alephgrid UI implementation guide

Updated: 1 November 2026. Status: approved product direction and implementation requirements, **not completion evidence**.

This guide expands [phase U and the product contract](IMPLEMENTATION_CHECKLIST.md). Stable checklist IDs remain the tracking authority. Preserve working features and existing changes; implement each surface alongside its backend capabilities. Do not ship decorative controls or infer backend support from a mockup.

## 1. Design authority and references

Priority: explicit interaction/state contracts in this guide → semantic theme tokens ([DESIGN.md](DESIGN.md)) → original vector brand assets (`assets/brand/`). Concept images illustrate visual direction, not exact copy, formula semantics, numeric results, or proof of functionality. Do not extract raster logos from concept boards.

## 2. Product principles

Alephgrid is a spreadsheet for business users, backed by DuckDB. It is not a SQL console with a grid attached. A user can import, calculate, enrich, filter, summarize, chart, refresh and export using spreadsheet interactions alone.

- Open a workbook directly into its last sheet/grid. New workbooks start with a usable blank sheet and a focused cell.
- Type values or formulas directly in cells or the persistent formula bar. Existing custom functions participate in the same entry flow.
- Keep SQL closed by default for a new workbook. Tools → SQL opens optional advanced access. No supported spreadsheet workflow may require SQL expressions or query-to-sheet conversion.
- Prioritize data density and legibility over ornamental cards, large headings or repeated toolbars.
- Prefer inline controls, anchored menus and nonmodal side panels. Reserve blocking dialogs for consequential confirmation or genuinely necessary decisions.
- Keep infrastructure details in authorized settings. Opening a workbook automatically locates/resumes compatible capacity.
- Full Excel/Sheets compatibility is a tracked long-term target, not a launch claim. Clearly disclose unsupported operations without silently changing their meaning.

## 3. Theme, typography and layout foundations — U.01/U.12/U.13

### Colors

Dark mode is **neutral charcoal, never green-tinted**. Do not use forest green as a dark canvas, titlebar, grid, sidebar, menu or panel surface.

| Semantic role | Light | Dark |
|---|---|---|
| Application background | #F8FAF8 | #111113 |
| Grid/panel surface | #FFFFFF | #18181B |
| Raised control/hover surface | #F1F5F2 | #27272A |
| Subtle separator | #D7E3DC | #3F3F46 |
| Primary text | #102D25 | #FAFAFA |
| Secondary text | #52675F | #A1A1AA |
| Action/focus accent | #047857 | #34D399 |
| Text on primary action | #FFFFFF | #111113 |
| Active selection border | #047857 | #10B981 |
| Error text | #B42318 | #FDA29B |
| Warning text | #92400E | #FCD34D |

Green is restricted to brand marks, selected outlines, active tab indicators, primary actions and small status accents. Selection fill must remain subtle and bounded to selected cells. Additional formula-reference colors distinguish multiple ranges and require labels/borders as well as hue. Charts use a small distinguishable palette; green is not the only series color.

Light / Dark / System preference persists. Resolve theme before first visible paint; System follows OS changes. Canvas grid, DOM controls, popovers, charts and portals share tokens. Inverse logos use white lettering with a small emerald accent. No gradients, glow, glass effects or heavy shadows. Decorative separators do not substitute for accessible control boundaries or focus outlines.

### Type and dimensions

Use Manrope for UI when bundled; system sans fallback must not break layout. Use tabular numerals for financial/data columns, system monospace for code. Suggested starting dimensions (validate through usability/accessibility testing):

| Element | Target |
|---|---|
| Body/control text | 13–14px; supporting text at least 12px |
| Grid row | 28px compact, 32px comfortable |
| Header/title row | 44–48px |
| Formatting toolbar | 36px |
| Formula bar | 32–36px, expandable for multiline input |
| Desktop Hub sidebar | 216–240px |
| Workbook explorer | 220–260px expanded; collapsed by default on narrow windows |
| Analysis/settings side panel | 300–360px, resizable where useful |
| Control radius | 6–8px |
| Panel/card radius | 8–12px; not applied to each cell |
| Spacing scale | 4, 8, 12, 16, 24, 32px |

Maintain usable hit targets, keyboard access, visible focus, 200% zoom and screen-reader labeling. Compact visual icons may have larger invisible hit areas. Do not shrink all UI text to fit a large dataset.

## 4. Hub — U.02/U.03

### Desktop anatomy

A quiet left navigation contains the compact Alephgrid identity, Home, Recent, Starred, Spaces, Trash. Settings sits at the bottom. Administration appears only for authorized roles. Do not show nonfunctional Spaces/Trash/admin destinations before their capabilities exist.

The top header contains the current organization/space selector when applicable, a broad workbook search field and account/theme controls. Standalone mode does not require choosing an organization or configuring compute. Notifications appear only when a real notification system is available.

Main content starts with **Your workbooks** and short supporting copy. Place Import and New workbook actions at the right of the heading. Avoid oversized greetings, metrics dashboards, pricing/profile cards or a mandatory onboarding wizard.

Show up to three recent workbook preview cards above a compact workbook list when history exists. Previews are optional, lazy and permission-aware; show a neutral thumbnail if no snapshot is available. Never load full workbook data to draw a card. Rows show name, space (if applicable), modified time, relevant status and an overflow menu. Support list/grid view, space filter and explicit sort. State is preserved when returning from a workbook.

### Behavior and states

- Clicking a workbook opens its grid; queued/starting progress is an inline transition, not a compute-selection wizard.
- Star and overflow controls do not trigger row navigation. Keyboard activation and focus order are predictable.
- Search has a clear button, empty result state and bounded server results; it must not leak names across permissions.
- Rename is inline or in a compact popover. Trash uses reversible deletion where supported; permanent deletion has a consequential confirmation.
- Empty account: one concise explanation, New workbook and Import. Empty search: clear filters. Permission denial: explain access and provide a valid next step without exposing protected metadata.
- Loading uses restrained skeletons without fictitious data. List errors have Retry without losing filters.
- Sleeping is neutral, not a warning or failure. Do not display unsupported billing/autoscaling claims.

## 5. Workbook shell — U.04/F.03/F.04

Order from top to bottom:

1. Compact Alephgrid/home control, editable workbook name, location breadcrumb where useful, durable save status, collaborator indicators when supported, Share and overflow.
2. Familiar File / Edit / View / Insert / Format / Data / Tools / Help menus. Only enabled, meaningful commands appear.
3. One compact toolbar: undo/redo, zoom, number formats, font/size, emphasis, alignment and borders; sort/filter actions remain readily discoverable. Hide lower-priority controls in overflow as width shrinks.
4. Name box, fx indicator and formula bar showing the selected cell's raw expression/value.
5. Grid occupying the remaining space. Optional explorer is collapsible; opening a panel must not erase selection, scroll or draft input.
6. Sheet/table tabs, add action, selected-range summary and zoom/status controls.

Use conventional column letters, row numbers and a top-left select-all corner. Headers, frozen panes, hidden columns and selection must align precisely when scrolling or zooming. Types and read-only restrictions are visible without requiring users to choose a backend engine. If an operation differs between a freeform sheet and a typed table, explain that specific limit at the point of action.

Do not permanently display Queries/Functions/SQL panels beside every workbook. Make advanced surfaces discoverable through Tools and optional explorer sections. At least the grid, tabs and formula bar remain available during ordinary analysis.

## 6. Direct editing, selection and clipboard — F.03/F.04/U.T04

- Typing replaces the selected cell; F2/double-click edits existing input. Enter commits/moves down, Shift+Enter up, Tab right, Shift+Tab left; Escape cancels the current edit before closing outer UI.
- Formula mode begins with `=`. Show a small autocomplete list anchored to the caret/editor and compact argument hints. Arrow keys browse suggestions when that list owns focus; Tab accepts a selected suggestion. Define precedence so cell navigation does not fire simultaneously.
- Clicking/dragging a range during formula editing inserts a reference, including across sheets. Color the corresponding tokens/ranges. Preserve caret position and draft text. F4 cycles `A1 → $A$1 → A$1 → $A1` using parsed references.
- Name box supports direct cell/range navigation and named ranges. Formula bar can expand for long expressions without a modal formula builder.
- Ctrl/Cmd+A selects editor text when editing. In grid context, select contiguous region, then complete logical grid on repeat; top-left corner selects all. Selection beyond the viewport remains logical, not a browser materialization.
- Copy has a visible copied-range outline; active cell and multi-selection have distinct treatments. Fill handle is small but accessible, with keyboard fill alternatives.
- Cut is not destructive until destination commit succeeds. Invalid/oversized/read-only paste is atomic: **nothing is pasted**. Preserve source data and explain limits. Offer a smaller range or explicit export; never silently truncate.
- Clipboard supports rectangular external TSV with quoted/multiline content under documented limits, exact values and formula-aware copy/move. Paste-special offers values/formulas/formats/transpose only when supported.
- One bulk gesture forms one undoable operation. Concurrent edits, reconnects and unsupported history scope must not produce a misleading undo promise.
- Find/replace is a compact nonmodal strip/pane with value/formula scope, match count and navigation. Search the logical dataset, not just fetched cells.
- Formula/validation errors are cell indicators with anchored explanations. Keep recoverable input; don't convert live formulas to values to hide an error.

Use platform-aware shortcuts from the shared command registry. Ctrl is not mechanically replaced by Cmd for every action. Respect OS/browser shortcuts, clipboard permission denial, native text inputs, IME and assistive technology. Menus, toolbar, shortcut help and command search dispatch the same commands.

## 7. Filters, formatting and analysis — F.08/F.11/F.12

Column-header filter popovers contain sort actions, bounded distinct-value search, selected values and explicit clear/apply behavior. Keep active filters visible. If results are sampled or limited, label that fact; don't imply a partial list is exhaustive.

Formatting uses a compact toolbar and anchored number-format/color/border menus. Common number, date, currency and percentage actions should be immediate. Formatting changes display, not raw numeric precision. Validation dropdowns appear at the cell; rules are also enforced through paste/API/import paths as applicable.

Pivot/chart creation uses a nonmodal right panel with source selection, Rows, Columns, Values, aggregate function and Filters. Allow multiple measures/totals only when implemented. Provide readable chart labels and accessible summaries. Show freshness and bounded drill-down; retaining the grid avoids a separate dashboard-builder maze. Charts/pivots do not require user-written SQL.

Cleaning operations (trim, split, deduplicate, type conversion) show a bounded preview and affected-count semantics before consequential changes. Apply atomically/reversibly or create an explicit derived result. Avoid a multi-dialog wizard for each small transformation.

## 8. Secondary surfaces — U.05–U.10

| Surface | Layout and behavior | Capability boundary |
|---|---|---|
| Import | Nonmodal side pane/drop area; file name, bounded preview, detected types, editable overrides, progress/cancel and precise row/column errors; successful import opens its grid | No partial dataset on failure; S3 controls wait for phase 10 |
| SQL | Explicit Tools action opens resizable dock/workspace with editor/results, run/cancel, parameters/history and freshness; closing retains recoverable draft | Advanced only; never a workaround required by F.15 |
| Function library | Searchable function list; selected signature, description, examples, tests, usage and version history; authoring separate from invocation | Draft, Tested, Published are distinct states; Python controls wait for phase 8 |
| Share | Compact side panel with people, roles, access explanation and Copy link; clearly identify who can use the link | Server authorization is authoritative; no fake invite/share flow in standalone mode |
| Space settings | General, Members, Storage, Compute, Activity; plain forms and clear save feedback | Only authorized roles can change settings |
| Compute | Automatic placement/resume is the normal choice; approved profiles and dedicated allocation are advanced; explain quota/start/drain/restart failures | Automatic does not imply dynamic autoscaling; never invent billing/free-storage promises |
| Recovery/activity | Timestamped operations with authors and revision references, inspect/restore preview, consequential restore confirmation | Preserve unsaved work; verify restore semantics and permission checks |
| Administration | Separate authorized area for authentication, members/roles, quotas, active jobs, health and audit; searchable/filterable bounded lists with drill-down | Provider/Kubernetes details belong here, not in the business user's grid |

Administrative screen detail must be elaborated against actual APIs before building it. The component board defines common styling, not an excuse to ship placeholder enterprise screens.

### 8.1 Function and Python workspace — detailed interaction contract

The existing large function modal is the replacement target, not the desired template.

**Layout.** Open from Tools → Functions, the existing Functions action, or a contextual Edit function action. Use one nonmodal right dock, initially 480 px at a 1440 px desktop viewport, adjustable from 360 px while leaving at least 560 px of usable grid. At narrower widths offer an explicit expanded workspace rather than squeezing both beyond usability. A keyboard-operable divider resizes it; remember width per user. One right dock is active at a time; switching tools retains recoverable drafts. Close returns focus to its trigger and does not publish, activate or discard a draft silently.

**Header and navigation.** Compact header: function switcher/search, name/signature, Draft/Published badge, expand and close. New function is explicit; an empty library offers a starter action rather than pretending an example is saved. Optional collapsible function list in expanded view. Beneath it use Code, Tests, Docs, Versions tabs. The language choice is Formula / SQL / Python, with backend engine details in contextual help. Never put code, all tests, documentation, version maps and deployment explanations in one long form.

**Code.** Use a syntax-aware editor with line numbers, indentation, completion where supported, find, and line diagnostics. Keep a compact signature/argument editor and return-type control above it. Preserve typed drafts when changing language; require deliberate conversion or a separate draft rather than silently replacing source. Engine-specific examples must match the selected signature; avoid a formula expression left inside a Python draft. Store editor history independently of grid history. Support IME and a documented way to leave editor Tab navigation. SQL stays opt-in; spreadsheet users invoke functions directly with normal cell formulas.

**Tests.** Structured rows contain one typed input per argument, expected value/type, actual result, status and an expandable difference/error. Add/remove cases inline; distinguish null, empty string, blank cell, zero, booleans and errors. Offer Run selected / Run all when supported. Advanced provides validated JSON import/export without losing typing or precision. Changing the function signature requires an explicit test-input migration rather than silently dropping cases. Test execution never changes the workbook's published version.

**Results and console.** Bottom split starts compact (about 140 px), collapsible/resizable, with Results and Console tabs. Header shows Run tests, focus-scoped Cmd/Ctrl+Enter, Cancel while running, and duration/state. Display bounded output with truncation and clear action; long output cannot grow the entire page. Report queued/running/cancelling/cancelled/succeeded/failed/timed-out distinctly. Only report cancellation after acknowledgement. Include source revision/run identity so a late result cannot overwrite a newer run. Editing source marks old test results stale. Errors have concise explanations and expandable tracebacks with line navigation. Closing the dock does not imply cancellation; keep an activity indicator and an explicit cancel route.

**Docs and versions.** Docs contains description, signature and runnable/copyable invocation examples. Versions shows published versions, current workbook pin, change summary and compatibility. Publish version and Activate in this workbook are separate explicit actions; there is no publish-on-run or silent dependent upgrade. Move dependency/version-map JSON behind Advanced and prefer structured pickers with validation. Show export limitations when exporting or choosing compatibility, not as permanent paragraphs between every control.

**Lifecycle and safety.** Python runs only when phase 8's actual isolation and resource gates pass. Return-type/volatility settings have short contextual descriptions. Background execution must be explicit opt-in, explain its scope, and honor policy/resource limits. Never execute on opening the editor, switching language or importing code. Save draft status and published status are distinct; avoid claiming durable save before acknowledgement. User code remains recoverable through navigation and connection loss.

**Visual treatment.** Match the workbook typography and controls; 13 px monospace code, compact 32 px controls, consistent 8/12/16 px spacing, subtle dividers and no nested decorative cards. Neutral charcoal dark surfaces follow §3; emerald denotes actions/focus only. Status includes text/icons, not color alone. Reduce introductory copy to one short sentence and contextual help. Usability is judged by editing, testing and invoking a function while retaining grid context.

**Acceptance journey.** With a tiny synthetic workbook, create a function, edit code, add typed tests, run, inspect an error, fix and rerun, publish, explicitly activate, then enter its formula in a cell without opening a dialog. Close/reopen the dock to verify retained draft and pin. Repeat keyboard-only and in both themes; test no-network, timeouts, stale results, permission denial and cancellation using controlled fixtures. Check source/version on the server independently. Backend tests remain prerequisites; UI visibility is not evidence of isolation.

### 8.2 Commands and configuration drafts

Each command has one identifier, label, platform shortcut, permission/capability predicate and action shared by menus, toolbar and command search. Every shipped control is functional or has a clear unavailable reason. Record explicit scope for find/replace, recalc, formatting and imports. Workbook settings explain locale/timezone effects before applying them.

Chart/pivot/rule creation must have an explicit persistence boundary. Opening a panel creates only a local draft; closing/cancelling removes its preview. Apply/Create commits once, with an undoable transaction where supported. For editing existing rules, distinguish preview from persisted state. Preserve the selected range across dock switches. Do not silently add empty chart objects merely because a user inspected the tool.

## 9. Component and state contracts

| Family | Required variants/states |
|---|---|
| Buttons | Primary, secondary, ghost, icon, destructive; hover, focus-visible, pressed, disabled, loading; label icon-only actions |
| Inputs | Text/search/number; empty, filled, focus, invalid, disabled/read-only; helper/error text linked accessibly |
| Choices | Checkbox including mixed state, radio group, switch, select/combobox; keyboard navigation and accessible selected state |
| Menus/popovers | Anchored positioning, viewport collision, nested menus, dismissal, focus return; menus list real shortcuts |
| Navigation | Active tabs, overflow tabs, breadcrumb, explorer collapse, account switch; preserve unsaved state |
| Grid | Active cell, range, copied range, frozen/hidden/read-only cells, editor, errors, selection handles; consistent canvas/DOM focus model |
| Feedback | Saving/Saved/Reconnecting, progress/cancel, retry banner, nonblocking toast, empty/loading/denied states |
| Panels | Resizable where appropriate, close/collapse, sensible minimum sizes, scroll containment and keyboard-accessible resize |
| Dialogs | Consequential deletion/restore or necessary conflict decisions only; labeled title, focus trap, Escape policy, focus return |

Never show Saved before durable acknowledgement. If saving fails, keep the draft visible, explain retry/recovery, and prevent misleading navigation. Reconnecting must not replay edits twice. Progress must reflect measured work; use indeterminate progress when totals are unknown. Cancellation is pending until acknowledged. Toasts are supplementary; critical error details remain discoverable.

## 10. Responsive behavior and accessibility

Desktop is the main spreadsheet work surface. At narrower widths collapse explorer, move lower-priority toolbar commands to overflow and allow the grid to scroll horizontally; do not squeeze all columns into unreadable text. Side panels become a single accessible task pane while preserving a way back to the grid. Hub adapts from cards/list to a single-column layout. Make touch/read-only limitations explicit where editing capabilities differ.

Verify keyboard-only selection/editing and command execution, focus restoration, screen-reader cell coordinates and errors, high zoom, reduced motion, color-blind distinguishability and long localized labels. Use at least 4.5:1 for normal text, and appropriate non-text contrast for controls/focus. Generated images are not contrast test evidence. Avoid document-level horizontal overflow; grid scrolling is deliberate.

## 11. Delivery order and acceptance

1. **U.01/U.13 foundations:** semantic tokens, neutral dark surfaces, canonical assets, theme persistence, common primitives. Do not globally rebrand internal crate names as a side effect.
2. **U.04 + F.03/F.04:** workbook shell, cell/formula entry, selection/clipboard and command routing; behavior tests precede visual polish.
3. **U.03:** Hub navigation, search/list/empty states and open/import transitions using actual metadata and permissions.
4. **U.07 + F.11/F.12:** import, filtering, formatting and nonmodal analysis panels.
5. **U.05/U.06:** optional SQL and function authoring, without changing the default business flow.
6. **U.08–U.10:** sharing, spaces, recovery and administration when phase 7/9 capabilities permit them.
7. **U.T01–U.T04/U.14:** test integrated journeys and visual regressions in both themes; assess real usability rather than screenshot similarity alone.

Record coverage in [TEST_MATRIX.md](TEST_MATRIX.md) beside the existing checklist IDs. For each screen test empty, loading, ready, failure/retry, permission and unsaved/reconnect cases where relevant. Exercise supported browsers/platforms, clipboard failures, full logical selection, long formulas, large/wide tables, panel resizing and zoom. Use isolated small fixtures for routine browser tests; performance evidence follows F.T04 with documented resources.

**Release journey:** From Hub import orders/customers; select/copy/paste with keyboard; enter a lookup directly in the grid; fill it; compute conditional totals; filter; build a pivot/chart with a side panel; undo/redo an edit; refresh data; reopen; export. SQL stays closed throughout. Routine formulas require no modal dialogs. Values, persisted state, atomic failure behavior and accessibility are asserted, not just screenshots.

No item becomes checked by adding this guide or generating a picture. Document partial coverage honestly and preserve the existing backend gates.
