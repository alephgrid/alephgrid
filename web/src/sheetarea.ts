// Grid ranges ({ r0, c0, r1, c1 }, inclusive) and protocol cell areas ({ row, column,
// rows, columns }) describe the same cells; these convert between them.

import type { Range } from "./grid";
import type { CellArea } from "./protocol";

export const areaOf = (r: Range): CellArea => ({ row: r.r0, column: r.c0, rows: r.r1 - r.r0 + 1, columns: r.c1 - r.c0 + 1 });

export const rangeOf = (a: CellArea): Range => ({ r0: a.row, c0: a.column, r1: a.row + a.rows - 1, c1: a.column + a.columns - 1 });

/** Whether cell (r, c) is inside `a`. */
export const areaContains = (a: CellArea, r: number, c: number): boolean =>
  r >= a.row && r < a.row + a.rows && c >= a.column && c < a.column + a.columns;
