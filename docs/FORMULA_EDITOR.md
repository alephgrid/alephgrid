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

- Point-and-drag reference insertion is in the formula bar, on the current sheet. Cross-sheet
  tab picking and inline-editor picking are not implemented.
- Whole-row/whole-column and structured table references are outside this reference editor.
- Dragging is bounded to the visible viewport; automatic scrolling during a drag is pending.
- Named-range management and richer parser error locations remain unchecked in F.03.
- Named-range APIs exist upstream, but their integration needs explicit collision protection
  against registered LAMBDAs, scope rules, transactional undo and rename/delete/reopen tests.
- The existing inline-array XLOOKUP engine limitation remains unchanged.

Verification: `web/e2e/references.spec.ts` tests reference-token boundaries, all four anchor states,
qualified/quoted names, invalid coordinates, source-preserving range insertion, original edit
destination, evaluation/reopen, Escape, inline F4, and light/dark accessibility. Existing
catalog and custom-function journeys verify that completion and versioned functions still work.
