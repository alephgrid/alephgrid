# Formula reference editing — phase F increment

In a sheet's formula bar, type a formula up to an operand position, such as `=SUM(`.
Click a cell or drag across cells to insert a same-sheet A1 reference. The active destination
cell stays unchanged. Continue typing, then press Enter to evaluate and save. Escape discards
the draft. A selected A1 reference/range can also be replaced by a new picked range.

While the formula bar is focused, same-sheet references have colored dashed outlines and
matching text labels. Rendering is bounded to 128 reference ranges and visible grid geometry;
it does not download or evaluate the referenced range. Labels remain available without relying
on color alone, with light/dark theme colors tested for accessibility.

Press **F4** with the caret on a reference, or with reference text selected, to cycle:

```text
A1 → $A$1 → A$1 → $A1 → A1
```

A selection containing several references cycles each token independently. F4 works in both
the formula bar and the inline cell editor. Reference tokens retain their original letter case;
quoted strings, quoted sheet names, function names, structured-reference brackets and invalid
A1 coordinates are not rewritten. Qualified sheet references can be locked without changing
the sheet name, but are not shown as outlines on the current sheet. This editor tokenizer is
not an expression evaluator and does not claim to validate the full formula grammar.

Current boundaries:

- Point-and-drag reference insertion is in the formula bar. Clicking another sheet's tab while
  a formula can take a reference shows that sheet without leaving the edit; picked ranges are
  inserted as `'Sheet name'!A1:B3`, Enter saves to the original cell and returns to its sheet,
  and Escape (or leaving the formula bar) discards the draft and returns. Inline-editor picking
  is not implemented.
- Whole-row/whole-column and structured table references are outside this reference editor.
- Dragging is bounded to the visible viewport; automatic scrolling during a drag is pending.
- Inline-array XLOOKUP is handled by the local engine patch described in [FORMULA_GAPS.md](FORMULA_GAPS.md).

Verification: `web/e2e/references.spec.ts` tests reference-token boundaries, all four anchor states,
qualified/quoted names, invalid coordinates, source-preserving range insertion, original edit
destination, evaluation/reopen, Escape, inline F4, and light/dark accessibility. Existing
catalog and custom-function journeys verify that completion and versioned functions still work.

## Named ranges, Go to and formula problems (F.03)

**Names** (sheet toolbar) lists named ranges and defines one from the selection: the
"Refers to" field starts as the absolute selection, e.g. `='Q1 data'!$B$1:$B$3`. A name is
workbook-wide or belongs to one sheet; on that sheet its own name wins over a workbook name of
the same spelling. Names are case-insensitive, must refer to cells or ranges, and cannot reuse
a built-in function name (`SUM`, `RATE`, `BASE` …), a workbook function (LAMBDA) or a cell-like
name (`A1`). Renaming a name rewrites the formulas that use it; deleting it leaves `#NAME?`.
Each change is one undo step and survives reopening. A formula typed before its name became a
multi-cell range is entered again after the name change so it spills like a newly typed one.
Workbook functions stay in **Functions**; publishing a function whose name is already a named
range is refused.

**Go to** (toolbar, ⌘G / Ctrl+G, or the command palette) accepts `A1`, `B2:C9`,
`Sheet1!A1`, `'Q1 data'!A1:B4` or a name, and grows the visible grid if the target is beyond it.

**Formula problems.** Before a formula is saved from the formula bar or a cell editor, the
server parses it as IronCalc would store it (one missing final `)` is completed, as before). A
formula that does not parse is not saved: the draft stays in the formula bar, a message says
what is wrong and at which character, and the caret moves there. Start with an apostrophe to
enter the text instead. The local engine patch makes input after a complete expression an error
(`=1 2` used to be stored as `=1` and show 1) and reports real token positions.

**Error origins.** Selecting a cell that shows an error displays its message and, when it comes
from another cell, that cell with a **Go to source** button (`#DIV/0!: Divide by 0 · comes from
Calc!A1`). One bounded request per selection; nothing is downloaded for other cells.

Verification: engine `sheets::editing` tests (names: renames, scopes, collisions, undo/redo,
reopen, re-spill; problems: positions, trailing input, completion; error origins);
`tests/api.rs` result types; `web/e2e/names.spec.ts` (address parsing, the names journey with
reload/undo, formula problems in both editors, error origins, cross-sheet picking).
