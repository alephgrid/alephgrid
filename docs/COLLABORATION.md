# Editing together

Several people (or tabs, or agents) can edit one workbook at once. Every change runs on the
workbook's single writer, one at a time, and commits as one revision. This page is the
contract for what happens when changes overlap. The rule throughout: a
change made from an out-of-date view is **refused, never misapplied**, and refusals are
reported to the person who made them. Nothing is merged behind anyone's back.

Tests: `crates/alephgrid-server/tests/collaboration.rs` (two and three sessions over the
WebSocket), `ws::tests` (dropped events), `web/e2e/collaboration.spec.ts` (two browsers,
frames dropped, repeated, delayed and cut by the test).

## Stable ids and what a change is based on

| Change | Addresses | Based on | Refused when |
|---|---|---|---|
| Table cell edit (`edit`) | record id (`record`) | each cell's value as the user saw it (`base`) | the cell holds another value now, its record was deleted, or its column renamed or removed |
| Appending rows (`edit` at or past the end) | position | the table's structure generation (`generation`) | rows were inserted, deleted or reordered since |
| `insert_rows`, `delete_rows` | position | `generation` | as above |
| Sheet cell edit (`sheet_set`) | sheet index, row, column | each cell's input as the user saw it (`base`) and the sheet's layout (`layout`) | the cell holds something else, or rows, columns or cells of the sheet moved, or its index changed |
| `sheet_insert/delete_rows/columns`, `sheet_fill`, `sheet_paste` (`layout`, `source_layout`), `sheet_move`, `sheet_rename`, `sheet_delete`, `sheet_to_table` | sheet index and positions | `layout` | the layout changed |

* **Record ids** never change and are never reused within a table, so an edit by record lands on
  the same row however rows were inserted, deleted or sorted since. Views return them with every
  row (`RowBlock.ids`).
* **`generation`** (`ViewInfo.generation`, `RowBlock.generation`) counts a table's structural
  changes: inserted, deleted or appended rows and column changes. A view's positions name the same
  rows while it stays the same. Sorted and filtered views keep the order they were built in until
  then, so editing a value in a sort column does not move rows under someone's cursor.
* **`layout`** (`SheetInfo.layout`, `SheetBlock.layout`) is the revision that last inserted,
  deleted or moved a sheet's rows, columns or cells (undo and redo count), or shifted its index
  (deleting an earlier sheet). It is kept with the sheets, so it survives restarts.
* **`base`** values are compared in the column's own type (`1.5` matches a DECIMAL `1.50`); sheet
  inputs match when equal or the same number (`1.50` and `1.5`). `null` (tables) or `""`
  (sheets) means "it was empty". A missing `base` or `generation`/`layout` means "do not check":
  the API stays usable by scripts that want last-writer-wins.

A refusal is a `conflict` error and changes nothing: a request is all or nothing. It carries
`moved: true` when what it addressed moved or is gone, so there is nothing to overwrite, only to
refresh. Without `moved`, cells hold values the user did not see. The browser then refreshes,
shows the other person's value and offers **Overwrite**, which resends the same change without
its bases.

**Different cells never conflict**, even in the same row. **Area changes** (paste, fill, moving
cells, find and replace, SQL `UPDATE`/`DELETE`) replace what is in their area when they run.
They are refused if the area moved (`layout`, `generation`) but not because a cell inside it
changed. Overwriting the area is what they are for, and the history and undo keep what they
replaced.

## Events, gaps and reconnects

* Every committed revision is announced to every open connection by at least one event that
  carries its number. Events are sent from the writer right after the commit, so they go out in
  revision order. A change with nothing to refresh is announced as `committed`. Tables and sheet
  cells changed through formulas get events of their own, with the same revision (`origin: 0`).
* A client keeps the highest revision it has seen (`web/src/revisions.ts`). An event more than
  one past it means something was missed: the client refreshes everything shown, as for `resync`.
  An event at or below it is a repeat or a late copy and is applied as usual (refreshing twice is
  harmless).
* A client that reads too slowly has its events dropped from that point until it catches up, then
  gets one `resync` naming the latest revision announced meanwhile. Replies are never dropped.
* The browser sends one table's or sheet's edits one after another, in the user's order, and
  remembers the values it wrote until a fetch started after they committed replaces them. A
  quick second edit of the same cell is therefore based on the user's own value, not taken for
  someone else's change. Rows it just appended are edited by record (`first_appended`).
* Work a session had queued or running when its workbook closed (idle, restart, shutdown) and
  that did not commit answers `unavailable`, never `cancelled`, so the client sends it again.
  `cancelled` is only ever the answer to the client's own `cancel`.
* `hello.revision` tells a reconnecting client how far the workbook got. The client reopens its
  views, refetches what it shows, and resends changes that were not acknowledged, each with its
  original `mutation_id`. A change that had committed is not applied again: the reply repeats the
  recorded result with `replayed: true`. One that had not is checked against its bases like any
  other, so if someone changed the cell meanwhile it is refused and reported, not silently
  applied over their value. A refused change is not recorded, so its id can be sent again once
  rebased.

## Undo

**Table changes** (`undo`, `docs/PROTOCOL.md`): cell edits, inserted, appended and deleted rows,
and SQL `INSERT`/`UPDATE`/`DELETE`. How each part works:

* **What is kept:** each change keeps copies of the rows it touched as they were before and after
  it, in hidden tables of the workbook (storage format 8).
* **Taking a change back** is a new change, in the history, that restores the "before" copy:
  rows it added are deleted, rows it deleted come back with their ids and places, and cells it set
  get their old values.
* **When undo is refused:** if anything it would take back changed since, it is refused
  (`conflict`) and nothing changes. That means a cell it set holds another value, a row it added
  was edited or deleted, a row it deleted is back or another row took its place, or the table's
  columns changed. Later work by others is never undone as a side effect.
* **Redo:** the undo's own entry swaps the two copies, so undoing an undo is redo.
* **⌘Z / ⌘⇧Z** in a table take back the caller's own latest change and redo their latest undo.
  Redo is only available until they make another change. In `--auth local` every connection is
  the same person.
* **Undoing a specific change:** **History** lists everyone's recent changes. Any change still
  kept can be undone there by revision, under the same rules. An undone change says which revision
  took it back and cannot be undone twice.
* **What is kept, and what is not undoable here:** the last 50 undoable changes are kept,
  across restarts. A change touching more than 1,000,000 rows is made but cannot be undone.
  Column, table and sheet changes are not undoable here: sheets have their own undo, and
  find-and-replace has its own.

**Sheets** keep one shared undo history in memory: IronCalc's own, grouped into user steps.
Undo and redo take back only the caller's own step. If the latest step is someone else's, they
are refused until that person takes it back. Their effect may be anywhere, so they change every
sheet's layout.
