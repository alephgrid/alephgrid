// Canvas-rendered, virtualized grid. Only visible rows are ever requested or drawn,
// so the same code scrolls 100 rows or 1,000,000,000 rows.

import { formatValue, isNumericKind, rawText } from "./format";
import { applyReferenceLock } from "./references";
import type { Align, CellValue, Column, Filter, SortKey } from "./types";
import type { CellStyle } from "./protocol";

export interface Cell { r: number; c: number }
export interface Range { r0: number; c0: number; r1: number; c1: number }

export interface GridModel {
  rowCount(): number;
  frozen?(): number;
  /** Rows kept in view while scrolling down (sheets, S.05). */
  frozenRows?(): number;
  format?(value: CellValue, column: Column): string;
  color?(value: CellValue, column: Column): string | undefined;
  columns(): Column[];
  canAppend(): boolean;
  /** Values a list rule allows in a column (shown as a dropdown while editing). */
  choices?(column: string): string[];
  sort(): SortKey[];
  filters(): Filter[];
  /** Cached value of a table cell, or undefined while loading. */
  value(r: number, c: number): CellValue | undefined;
  /** Make sure rows [first, last] of columns [c0, c1] are (being) loaded. */
  ensure(first: number, last: number, c0: number, c1: number): void;
  /** "sheet" switches to A/B/C headers and per-cell rendering via `cell`. */
  mode?(): "table" | "sheet";
  /** Sheet mode: rendered cell, null when empty, undefined while loading. */
  cell?(r: number, c: number): { text: string; align: Align } | null | undefined;
  /** Sheet mode: the cell's format (S.01), if any. */
  style?(r: number, c: number): CellStyle | undefined;
  /** Sheet mode: whether the cell has a comment (S.11). */
  comment?(r: number, c: number): boolean;
  /** Text shown in the editor (formula source for sheets). */
  editText?(r: number, c: number): string;
  /** What an edit of the cell is checked against on the server (the value shown when
   *  editing starts), or undefined while it is loading. */
  base?(r: number, c: number): CellValue | undefined;
}

/** One cell edit; `base` is the cell's value when the user started editing it. */
export type GridEdit = { r: number; c: number; value: string | null; base?: CellValue };

export interface GridHandlers {
  referencePick?(range: Range, phase: "start" | "move" | "end"): boolean;
  selectionChanged(range: Range, active: Cell): void;
  commitEdits(edits: GridEdit[]): Promise<boolean>;
  headerMenu(col: number, clientX: number, clientY: number): void;
  /** The filter popup of a table column (its header's dropdown, or Alt+↓); x < 0 from the keyboard. */
  headerFilter?(col: number, clientX: number, clientY: number): void;
  rowMenu(row: number, clientX: number, clientY: number): void;
  copy(range: Range): void;
  paste(at: Cell, text: string): void;
  columnResized(col: number, width: number): void;
  /** A row's height was dragged or auto-fitted (sheets, S.05). */
  rowResized?(row: number, height: number): void;
  /** Auto-fit a column from all of its cells (double-click on its edge); true when handled. */
  autoFit?(col: number): boolean;
  undo?(): void;
  redo?(): void;
  /** The command registry gets each grid key first; true when it used the key. */
  command?(e: KeyboardEvent): boolean;
}

const ROW_H = 24;
const HEADER_H = 32;
const MAX_SCROLL_PX = 8_000_000;
const APPEND_ROWS = 40;

interface Palette {
  references: string[];
  bg: string; grid: string; headerBg: string; headerText: string; text: string; muted: string;
  selFill: string; selBorder: string; activeHeader: string; placeholder: string; formulaBg: string;
  ui: string; mono: string; danger: string;
}

function palette(): Palette {
  const s = getComputedStyle(document.documentElement);
  const v = (name: string) => s.getPropertyValue(name).trim();
  return {
    references: [0, 1, 2, 3].map(i => v(`--reference-${i}`)),
    bg: v("--grid-bg"), grid: v("--grid-line"), headerBg: v("--grid-header-bg"), headerText: v("--text"),
    text: v("--text"), muted: v("--muted"), selFill: v("--sel-fill"), selBorder: v("--accent"),
    activeHeader: v("--grid-header-active"), placeholder: v("--grid-placeholder"), formulaBg: v("--grid-formula-bg"),
    ui: v("--font-ui") || "system-ui, sans-serif", danger: v("--danger"), mono: v("--font-mono") || "ui-monospace, monospace",
  };
}

export class Grid {
  readonly host: HTMLDivElement;
  private canvas: HTMLCanvasElement;
  private ctx: CanvasRenderingContext2D;
  private scroller: HTMLDivElement;
  private spacer: HTMLDivElement;
  private editor: HTMLInputElement;
  private choiceList = Object.assign(document.createElement("datalist"), { id: `grid-choices-${Math.random().toString(36).slice(2)}` });
  private colors = palette();

  private widths: number[] = [];
  private colX: number[] = [0];
  private scrollX = 0;
  private scrollY = 0;
  private expectTop = -1;
  private width = 0;
  private height = 0;
  private dirty = false;

  active: Cell = { r: 0, c: 0 };
  private anchor: Cell = { r: 0, c: 0 };
  private dragging: "cells" | "cols" | "rows" | null = null;
  private resizing: { col: number; startX: number; startW: number } | null = null;
  private resizingRow: { row: number; startY: number; startH: number } | null = null;
  /** S.05: rows whose height differs from ROW_H (0 = hidden), sorted, with the running
   *  height difference before each, so positions stay O(log n) for a million rows. */
  private rowIdx: number[] = [];
  private rowSize: number[] = [];
  private rowCum: number[] = [0];
  private editing: Cell | null = null;
  /** The edited cell's value when editing started (see `GridModel.base`). */
  private editingBase: CellValue | undefined;
  private referenceAnchor: Cell | null = null;
  private referencePicked = false;
  private references: Range[] = [];
  /** Removes every listener this grid added (including window and media-query ones). */
  private abort = new AbortController();
  private observer: ResizeObserver;
  private frame = 0;
  private disposed = false;
  /** Screen-reader view of the canvas: the active cell and a polite announcer. */
  private a11yRow: HTMLDivElement;
  private a11yCell: HTMLDivElement;
  private announcer: HTMLDivElement;
  private announced = "";
  private announceTimer = 0;
  private static nextId = 1;

  constructor(private model: GridModel, private handlers: GridHandlers) {
    this.host = document.createElement("div");
    this.host.className = "grid-host";
    this.host.tabIndex = 0;
    this.canvas = document.createElement("canvas");
    this.canvas.className = "grid-canvas";
    this.ctx = this.canvas.getContext("2d")!;
    this.scroller = document.createElement("div");
    this.scroller.className = "grid-scroller";
    this.spacer = document.createElement("div");
    this.spacer.className = "grid-spacer";
    this.scroller.appendChild(this.spacer);
    this.editor = document.createElement("input");
    document.body.append(this.choiceList);
    this.editor.className = "grid-editor";
    this.editor.spellcheck = false;
    this.editor.setAttribute("aria-label", "Edit cell");
    this.host.setAttribute("role", "grid");
    this.host.setAttribute("aria-multiselectable", "true");
    this.host.setAttribute("aria-roledescription", "spreadsheet");
    const cellId = `grid-active-${Grid.nextId++}`;
    this.a11yRow = document.createElement("div");
    this.a11yRow.className = "sr-only";
    this.a11yRow.setAttribute("role", "row");
    this.a11yCell = document.createElement("div");
    this.a11yCell.id = cellId;
    this.a11yCell.setAttribute("role", "gridcell");
    this.a11yCell.setAttribute("aria-selected", "true");
    this.a11yRow.append(this.a11yCell);
    this.host.setAttribute("aria-activedescendant", cellId);
    this.announcer = document.createElement("div");
    this.announcer.className = "sr-only";
    this.announcer.setAttribute("aria-live", "polite");
    this.announcer.setAttribute("aria-atomic", "true");
    this.host.append(this.canvas, this.scroller, this.editor, this.a11yRow);
    // A grid may only own rows, so the announcer lives outside it.
    document.body.append(this.announcer);

    this.observer = new ResizeObserver(() => this.resize());
    this.observer.observe(this.host);
    const signal = this.abort.signal;
    // The theme can change by the system or by the user's choice (theme.ts).
    const repaint = () => { this.colors = palette(); this.invalidate(); };
    matchMedia("(prefers-color-scheme: dark)").addEventListener("change", repaint, { signal });
    window.addEventListener("alephgrid-theme", repaint, { signal });
    this.scroller.addEventListener("scroll", () => this.onScroll(), { signal });
    this.scroller.addEventListener("wheel", (e) => this.onWheel(e), { passive: false, signal });
    this.scroller.addEventListener("mousedown", (e) => this.onMouseDown(e), { signal });
    this.scroller.addEventListener("mousemove", (e) => this.onHover(e), { signal });
    this.scroller.addEventListener("dblclick", (e) => this.onDoubleClick(e), { signal });
    this.scroller.addEventListener("contextmenu", (e) => this.onContextMenu(e), { signal });
    window.addEventListener("mousemove", (e) => this.onDrag(e), { signal });
    window.addEventListener("mouseup", () => this.onMouseUp(), { signal });
    this.host.addEventListener("keydown", (e) => this.onKey(e), { signal });
    this.host.addEventListener("paste", (e) => {
      if (this.editing) return;
      const text = e.clipboardData?.getData("text/plain");
      if (text) {
        e.preventDefault();
        this.handlers.paste(this.active, text);
      }
    }, { signal });
    this.editor.addEventListener("keydown", (e) => this.onEditorKey(e), { signal });
    this.editor.addEventListener("blur", () => this.editing && this.commitEditor(0, 0), { signal });
  }

  /** Detach everything: listeners, the resize observer and any pending frame. */
  dispose(): void {
    this.disposed = true;
    this.abort.abort();
    this.observer.disconnect();
    cancelAnimationFrame(this.frame);
    clearTimeout(this.announceTimer);
    this.announcer.remove();
    this.editing = null;
    this.host.remove();
  }

  // ------------------------------------------------------------ public API

  /** Call after the column set changed. Keeps widths of columns that still exist. */
  setColumns(widths: number[]): void {
    this.widths = widths;
    this.layoutColumns();
    this.active = { r: Math.min(this.active.r, Math.max(0, this.displayRows() - 1)), c: Math.min(this.active.c, Math.max(0, widths.length - 1)) };
    this.anchor = { ...this.active };
    this.invalidate();
  }

  columnWidths(): number[] {
    return this.widths.slice();
  }

  setReferences(ranges: Range[]): void {
    this.references = ranges.slice(0, 128);
    this.invalidate();
  }

  /** The polite live region that announces the active cell. */
  get liveRegion(): HTMLElement {
    return this.announcer;
  }

  /** Accessible name of the grid, e.g. the table or sheet name. */
  setLabel(label: string): void {
    this.host.setAttribute("aria-label", label);
  }

  /** Column label as announced: the table column name, or the sheet letter. */
  private columnLabel(c: number): string {
    return this.model.columns()[c]?.name ?? "";
  }

  /** Displayed text of a cell, as drawn on the canvas. */
  displayText(cell: Cell): string {
    const cols = this.model.columns();
    if (this.isSheet() && this.model.cell) return this.model.cell(cell.r, cell.c)?.text ?? "";
    const v = this.model.value(cell.r, cell.c);
    return v !== undefined && cols[cell.c] ? this.format(v, cols[cell.c]) : "";
  }

  /** Keep the accessible active cell and the announcement in step with the canvas. */
  private updateA11y(): void {
    const cols = this.model.columns();
    this.host.setAttribute("aria-rowcount", String(this.model.rowCount() + 1));
    this.host.setAttribute("aria-colcount", String(cols.length));
    if (!cols.length) {
      this.a11yCell.textContent = "";
      this.a11yCell.removeAttribute("aria-label");
      return;
    }
    const { r, c } = this.active;
    const loaded = this.isSheet() && this.model.cell ? this.model.cell(r, c) !== undefined : this.model.value(r, c) !== undefined || r >= this.model.rowCount();
    const text = this.displayText(this.active);
    const where = this.isSheet() ? `${this.columnLabel(c)}${r + 1}` : `${this.columnLabel(c)}, row ${r + 1}`;
    const label = `${where}: ${loaded ? text || "empty" : "loading"}`;
    this.a11yRow.setAttribute("aria-rowindex", String(r + 2));
    this.a11yCell.setAttribute("aria-colindex", String(c + 1));
    this.a11yCell.setAttribute("aria-label", label);
    this.a11yCell.textContent = text;
    const sel = this.selection();
    const cells = (sel.r1 - sel.r0 + 1) * (sel.c1 - sel.c0 + 1);
    const message = cells > 1 ? `${label}. ${sel.r1 - sel.r0 + 1} by ${sel.c1 - sel.c0 + 1} selected` : label;
    if (message !== this.announced && loaded) {
      this.announced = message;
      clearTimeout(this.announceTimer);
      this.announceTimer = window.setTimeout(() => (this.announcer.textContent = message), 150);
    }
  }

  invalidate(): void {
    if (this.dirty || this.disposed) return;
    this.dirty = true;
    this.frame = requestAnimationFrame(() => {
      this.dirty = false;
      this.updateSpacer();
      this.draw();
      this.updateA11y();
    });
  }

  selection(): Range {
    return {
      r0: Math.min(this.anchor.r, this.active.r), r1: Math.max(this.anchor.r, this.active.r),
      c0: Math.min(this.anchor.c, this.active.c), c1: Math.max(this.anchor.c, this.active.c),
    };
  }

  select(active: Cell, anchor: Cell = active, scroll = true): void {
    const maxR = Math.max(0, this.displayRows() - 1);
    const maxC = Math.max(0, this.widths.length - 1);
    const clamp = (p: Cell): Cell => ({ r: Math.max(0, Math.min(maxR, p.r)), c: Math.max(0, Math.min(maxC, p.c)) });
    this.active = clamp(active);
    this.anchor = clamp(anchor);
    if (scroll) this.scrollIntoView(this.active);
    this.handlers.selectionChanged(this.selection(), this.active);
    this.invalidate();
  }

  focus(): void {
    this.host.focus({ preventScroll: true });
  }

  selectAll(): void {
    this.select({ r: Math.max(0, this.model.rowCount() - 1), c: this.widths.length - 1 }, { r: 0, c: 0 }, false);
  }

  /** Extend the selection to whole rows (Shift+Space) or whole columns (Ctrl+Space). */
  selectEntire(what: "rows" | "columns"): void {
    const s = this.selection();
    const lastRow = Math.max(0, this.model.rowCount() - 1), lastCol = this.widths.length - 1;
    if (what === "rows") this.select({ r: s.r0, c: lastCol }, { r: s.r1, c: 0 }, false);
    else this.select({ r: lastRow, c: s.c0 }, { r: 0, c: s.c1 }, false);
  }

  scrollToTop(): void {
    this.scrollY = 0;
    this.syncScroller();
    this.invalidate();
  }

  startEdit(initial?: string): void {
    const cols = this.model.columns();
    const col = cols[this.active.c];
    if (!col) return;
    if (col.formula) {
      this.handlers.headerMenu(this.active.c, -1, -1);
      return;
    }
    if (this.active.r >= this.model.rowCount() && !this.model.canAppend()) return;
    this.editing = { ...this.active };
    this.editingBase = this.model.base?.(this.active.r, this.active.c);
    // A column with a list rule (F.12) offers its values as a dropdown.
    const choices = this.model.choices?.(col.name) ?? [];
    this.choiceList.replaceChildren(...choices.map(v => { const o = document.createElement("option"); o.value = v; return o; }));
    if (choices.length) this.editor.setAttribute("list", this.choiceList.id); else this.editor.removeAttribute("list");
    this.editor.value = initial ?? this.currentText(this.active);
    this.placeEditor();
    this.editor.style.display = "block";
    this.editor.style.textAlign = isNumericKind(col.kind) && initial === undefined ? "right" : "left";
    this.editor.focus();
    if (initial === undefined) this.editor.select();
  }

  private isSheet(): boolean {
    return this.model.mode?.() === "sheet";
  }

  private currentText(cell: Cell): string {
    if (this.isSheet() && this.model.editText) return this.model.editText(cell.r, cell.c);
    const v = this.model.value(cell.r, cell.c);
    return v !== undefined ? rawText(v) : "";
  }

  // -------------------------------------------------------------- geometry

  private displayRows(): number {
    return this.model.rowCount() + (this.model.canAppend() ? APPEND_ROWS : 0);
  }

  private rowHeaderWidth(): number {
    const digits = String(Math.max(1, this.displayRows())).length;
    return Math.max(48, digits * 8 + 22);
  }

  private bodyHeight(): number {
    return Math.max(0, this.height - HEADER_H);
  }

  private contentHeight(): number {
    return this.rowTop(this.displayRows());
  }

  /** S.05: set the rows whose height differs from the default (size 0 hides a row). */
  setRowSizes(list: { index: number; size: number }[]): void {
    const sorted = list.filter((x) => x.size !== ROW_H).sort((a, b) => a.index - b.index);
    this.rowIdx = sorted.map((x) => x.index);
    this.rowSize = sorted.map((x) => Math.max(0, x.size));
    this.rowCum = [0];
    for (const h of this.rowSize) this.rowCum.push(this.rowCum[this.rowCum.length - 1] + h - ROW_H);
    this.updateSpacer();
    this.invalidate();
  }

  rowSizes(): { index: number; size: number }[] {
    return this.rowIdx.map((index, i) => ({ index, size: this.rowSize[i] }));
  }

  private setRowSize(row: number, size: number): void {
    const list = this.rowSizes().filter((x) => x.index !== row);
    list.push({ index: row, size });
    this.setRowSizes(list);
  }

  /** Number of size overrides before row `r`. */
  private before(r: number): number {
    let lo = 0, hi = this.rowIdx.length;
    while (lo < hi) { const m = (lo + hi) >> 1; if (this.rowIdx[m] < r) lo = m + 1; else hi = m; }
    return lo;
  }

  /** Top of row `r` in content coordinates (unscrolled). */
  private rowTop(r: number): number {
    return r * ROW_H + this.rowCum[this.before(r)];
  }

  rowHeight(r: number): number {
    const k = this.before(r);
    return this.rowIdx[k] === r ? this.rowSize[k] : ROW_H;
  }

  /** The row at content `y`: the last row whose top is at or above it (hidden rows are skipped). */
  private rowAtContent(y: number): number {
    if (!this.rowIdx.length || y < 0) return Math.floor(y / ROW_H);
    let lo = 0, hi = this.displayRows();
    while (lo < hi) { const m = (lo + hi + 1) >> 1; if (this.rowTop(m) <= y) lo = m; else hi = m - 1; }
    return lo;
  }

  private frozenRowCount(): number {
    const requested = Math.min(this.model.frozenRows?.() ?? 0, this.displayRows(), 100);
    let n = 0;
    while (n < requested && this.rowTop(n + 1) < this.bodyHeight() - 80) n++;
    return n;
  }

  private frozenHeight(): number { return this.rowTop(this.frozenRowCount()); }

  private rowY(r: number): number {
    return HEADER_H + this.rowTop(r) - (r < this.frozenRowCount() ? 0 : this.scrollY);
  }

  /** The nearest shown row or column from `i` in direction `dir` (then the other way). */
  private shownLine(i: number, dir: number, last: number, hidden: (i: number) => boolean): number {
    i = Math.max(0, Math.min(last, i));
    for (const d of [dir || 1, -(dir || 1)]) {
      let j = i;
      while (j >= 0 && j <= last && hidden(j)) j += d;
      if (j >= 0 && j <= last) return j;
    }
    return i;
  }

  private maxScrollY(): number {
    return Math.max(0, this.contentHeight() - this.bodyHeight() + ROW_H);
  }

  private ratio(): number {
    const content = this.contentHeight() + ROW_H;
    if (content <= MAX_SCROLL_PX) return 1;
    return (content - this.bodyHeight()) / (MAX_SCROLL_PX - this.bodyHeight());
  }

  private layoutColumns(): void {
    this.colX = [0];
    for (const w of this.widths) this.colX.push(this.colX[this.colX.length - 1] + w);
  }

  private format(value: CellValue, column: Column): string { return this.model.format?.(value, column) ?? formatValue(value, column.kind); }
  private frozenCount(): number {
    const requested = Math.min(this.model.frozen?.() ?? 0, this.widths.length, 8);
    let n = 0;
    while (n < requested && this.colX[n + 1] < this.width - this.rowHeaderWidth() - 80) n++;
    return n;
  }
  private frozenWidth(): number { return this.colX[this.frozenCount()] ?? 0; }
  private columnX(c: number): number { return this.rowHeaderWidth() + this.colX[c] - (c < this.frozenCount() ? 0 : this.scrollX); }
  private colAt(x: number): number {
    const target = x + (x < this.rowHeaderWidth() + this.frozenWidth() ? 0 : this.scrollX) - this.rowHeaderWidth();
    if (target < 0) return -1;
    let lo = 0, hi = this.widths.length - 1;
    while (lo <= hi) {
      const mid = (lo + hi) >> 1;
      if (this.colX[mid + 1] <= target) lo = mid + 1;
      else if (this.colX[mid] > target) hi = mid - 1;
      else return mid;
    }
    return -1;
  }

  private rowAt(y: number): number {
    const fh = this.frozenHeight();
    if (fh && y >= HEADER_H && y < HEADER_H + fh) return this.rowAtContent(y - HEADER_H);
    return this.rowAtContent(y - HEADER_H + this.scrollY);
  }

  private cellRect(cell: Cell): { x: number; y: number; w: number; h: number } {
    return {
      x: this.columnX(cell.c),
      y: this.rowY(cell.r),
      w: this.widths[cell.c] ?? 0,
      h: this.rowHeight(cell.r),
    };
  }

  /** S.08: drawing scale (0.5–2); layout works in unscaled units, the canvas stays crisp. */
  private zoom = 1;

  setZoom(z: number): void {
    this.zoom = Math.max(0.5, Math.min(2, Math.round(z * 100) / 100));
    this.resize();
    this.updateSpacer();
    this.syncScroller();
    if (this.editing) this.placeEditor();
  }

  getZoom(): number {
    return this.zoom;
  }

  private resize(): void {
    const dpr = window.devicePixelRatio || 1;
    const pw = this.host.clientWidth, ph = this.host.clientHeight;
    this.width = pw / this.zoom;
    this.height = ph / this.zoom;
    this.canvas.width = Math.round(pw * dpr);
    this.canvas.height = Math.round(ph * dpr);
    this.canvas.style.width = `${pw}px`;
    this.canvas.style.height = `${ph}px`;
    this.ctx.setTransform(dpr * this.zoom, 0, 0, dpr * this.zoom, 0, 0);
    this.invalidate();
  }

  private updateSpacer(): void {
    const h = Math.min(this.contentHeight() + ROW_H, MAX_SCROLL_PX) + HEADER_H;
    const w = this.colX[this.colX.length - 1] + this.rowHeaderWidth() + 80;
    this.spacer.style.height = `${h * this.zoom}px`;
    this.spacer.style.width = `${w * this.zoom}px`;
    this.scrollY = Math.min(this.scrollY, this.maxScrollY());
  }

  private syncScroller(): void {
    const top = Math.round((this.scrollY / this.ratio()) * this.zoom);
    this.scroller.scrollTop = top;
    this.scroller.scrollLeft = this.scrollX * this.zoom;
    this.expectTop = this.scroller.scrollTop;
  }

  private scrollIntoView(cell: Cell): void {
    const rect = this.cellRect(cell);
    const rhw = this.rowHeaderWidth();
    if (cell.r < this.frozenRowCount()) { /* Frozen rows need no vertical scrolling. */ }
    else if (rect.y < HEADER_H + this.frozenHeight()) this.scrollY = this.rowTop(cell.r) - this.frozenHeight();
    else if (rect.y + rect.h > this.height) this.scrollY = this.rowTop(cell.r) + rect.h - this.bodyHeight();
    if (cell.c < this.frozenCount()) { /* Frozen cells need no horizontal scrolling. */ }
    else if (rect.x < rhw + this.frozenWidth()) this.scrollX = this.colX[cell.c] - this.frozenWidth();
    else if (rect.x + rect.w > this.width) this.scrollX = Math.max(0, this.colX[cell.c + 1] - (this.width - rhw));
    this.scrollY = Math.max(0, Math.min(this.maxScrollY(), this.scrollY));
    this.syncScroller();
  }

  // ---------------------------------------------------------------- events

  private onScroll(): void {
    if (Math.abs(this.scroller.scrollTop - this.expectTop) > 1) {
      this.scrollY = Math.min(this.maxScrollY(), (this.scroller.scrollTop / this.zoom) * this.ratio());
    }
    this.expectTop = -1;
    this.scrollX = this.scroller.scrollLeft / this.zoom;
    if (this.editing) this.placeEditor();
    this.invalidate();
  }

  private onWheel(e: WheelEvent): void {
    // Wheel scrolling is exact (pixel/row precise) even when the scrollbar is scaled.
    e.preventDefault();
    const unit = e.deltaMode === 1 ? ROW_H : e.deltaMode === 2 ? this.bodyHeight() : 1;
    let dx = e.deltaX * unit;
    let dy = e.deltaY * unit;
    if (e.shiftKey && dx === 0) { dx = dy; dy = 0; }
    this.scrollY = Math.max(0, Math.min(this.maxScrollY(), this.scrollY + dy));
    const maxX = Math.max(0, this.colX[this.colX.length - 1] + this.rowHeaderWidth() + 80 - this.width);
    this.scrollX = Math.max(0, Math.min(maxX, this.scrollX + dx));
    this.syncScroller();
    if (this.editing) this.placeEditor();
    this.invalidate();
  }

  private point(e: MouseEvent): { x: number; y: number } {
    const r = this.host.getBoundingClientRect();
    return { x: (e.clientX - r.left) / this.zoom, y: (e.clientY - r.top) / this.zoom };
  }

  private resizeHandleAt(x: number, y: number): number {
    if (y > HEADER_H) return -1;
    const rhw = this.rowHeaderWidth();
    for (let c = 0; c < this.widths.length; c++) {
      const edge = this.columnX(c) + this.widths[c];
      if ((c < this.frozenCount() || edge > rhw + this.frozenWidth()) && Math.abs(x - edge) <= 4) return c;
    }
    return -1;
  }

  /** The row whose bottom edge is under (x, y) in the row header, or -1. */
  private rowResizeHandleAt(x: number, y: number): number {
    if (!this.isSheet() || x >= this.rowHeaderWidth() || y < HEADER_H) return -1;
    const r = this.rowAt(y);
    if (r < 0) return -1;
    const top = this.rowY(r), bottom = top + this.rowHeight(r);
    if (Math.abs(bottom - y) <= 3) return r;
    if (Math.abs(y - top) <= 3 && r > 0) return this.shownLine(r - 1, -1, r - 1, (i) => this.rowHeight(i) === 0);
    return -1;
  }

  private onHover(e: MouseEvent): void {
    if (this.dragging || this.resizing || this.resizingRow) return;
    const { x, y } = this.point(e);
    this.scroller.style.cursor = this.resizeHandleAt(x, y) >= 0 ? "col-resize" : this.rowResizeHandleAt(x, y) >= 0 ? "row-resize" : "default";
  }

  private onMouseDown(e: MouseEvent): void {
    if (e.button !== 0) return;
    // Let clicks on the native scrollbar through.
    if (e.offsetX > this.scroller.clientWidth || e.offsetY > this.scroller.clientHeight) return;
    e.preventDefault();
    this.referencePicked = false;
    const { x, y } = this.point(e);
    const pickColumn = this.colAt(x), pickRow = this.rowAt(y);
    if (this.isSheet() && y >= HEADER_H && x >= this.rowHeaderWidth() && pickColumn >= 0
      && this.handlers.referencePick?.({ r0: pickRow, r1: pickRow, c0: pickColumn, c1: pickColumn }, "start")) {
      this.referenceAnchor = { r: pickRow, c: pickColumn };
      this.referencePicked = true;
      return;
    }
    this.focus();
    const handle = this.resizeHandleAt(x, y);
    if (handle >= 0) {
      this.resizing = { col: handle, startX: e.clientX, startW: this.widths[handle] };
      return;
    }
    const rowHandle = this.rowResizeHandleAt(x, y);
    if (rowHandle >= 0) {
      this.resizingRow = { row: rowHandle, startY: e.clientY, startH: this.rowHeight(rowHandle) };
      return;
    }
    const rhw = this.rowHeaderWidth();
    const c = this.colAt(x);
    const r = this.rowAt(y);
    const last = Math.max(0, this.displayRows() - 1);
    if (y < HEADER_H && x >= rhw && c >= 0) {
      const cell = this.cellRect({ r: 0, c });
      if (!this.isSheet() && x > cell.x + cell.w - 22) {
        if (this.handlers.headerFilter) this.handlers.headerFilter(c, e.clientX, e.clientY);
        else this.handlers.headerMenu(c, e.clientX, e.clientY);
        return;
      }
      this.dragging = "cols";
      const anchorC = e.shiftKey ? this.anchor.c : c;
      this.select({ r: last, c }, { r: 0, c: anchorC }, false);
      return;
    }
    if (x < rhw && y >= HEADER_H) {
      this.dragging = "rows";
      const anchorR = e.shiftKey ? this.anchor.r : r;
      this.select({ r, c: this.widths.length - 1 }, { r: anchorR, c: 0 }, false);
      return;
    }
    if (x < rhw && y < HEADER_H) {
      this.select({ r: last, c: this.widths.length - 1 }, { r: 0, c: 0 }, false);
      return;
    }
    if (c < 0) return;
    this.dragging = "cells";
    this.select({ r, c }, e.shiftKey ? this.anchor : { r, c }, false);
  }

  private onDrag(e: MouseEvent): void {
    if (this.referenceAnchor) {
      const { x, y } = this.point(e);
      const c = this.colAt(Math.min(Math.max(x, this.rowHeaderWidth() + 1), this.width - 1));
      const r = Math.max(0, this.rowAt(Math.min(Math.max(y, HEADER_H + 1), this.height - 1)));
      if (c >= 0) this.handlers.referencePick?.({ r0: Math.min(r, this.referenceAnchor.r), r1: Math.max(r, this.referenceAnchor.r), c0: Math.min(c, this.referenceAnchor.c), c1: Math.max(c, this.referenceAnchor.c) }, "move");
      return;
    }
    if (this.resizingRow) {
      const h = Math.max(8, Math.min(2000, Math.round(this.resizingRow.startH + (e.clientY - this.resizingRow.startY) / this.zoom)));
      this.setRowSize(this.resizingRow.row, h);
      return;
    }
    if (this.resizing) {
      const w = Math.max(40, this.resizing.startW + (e.clientX - this.resizing.startX) / this.zoom);
      this.widths[this.resizing.col] = w;
      this.layoutColumns();
      this.invalidate();
      return;
    }
    if (!this.dragging) return;
    const { x, y } = this.point(e);
    let c = this.colAt(Math.min(Math.max(x, this.rowHeaderWidth() + 1), this.width - 1));
    if (c < 0) c = this.active.c;
    const r = Math.max(0, this.rowAt(Math.min(Math.max(y, HEADER_H + 1), this.height - 1)));
    if (this.dragging === "cells") this.select({ r, c }, this.anchor, false);
    else if (this.dragging === "cols") this.select({ r: this.active.r, c }, this.anchor, false);
    else this.select({ r, c: this.active.c }, this.anchor, false);
  }

  private onMouseUp(): void {
    if (this.referenceAnchor) {
      const a = this.referenceAnchor;
      this.referenceAnchor = null;
      this.handlers.referencePick?.({ r0: a.r, r1: a.r, c0: a.c, c1: a.c }, "end");
    }
    if (this.resizing) {
      this.handlers.columnResized(this.resizing.col, this.widths[this.resizing.col]);
      this.resizing = null;
    }
    if (this.resizingRow) {
      this.handlers.rowResized?.(this.resizingRow.row, this.rowHeight(this.resizingRow.row));
      this.resizingRow = null;
    }
    this.dragging = null;
  }

  private onDoubleClick(e: MouseEvent): void {
    if (this.referencePicked) return;
    const { x, y } = this.point(e);
    const handle = this.resizeHandleAt(x, y);
    if (handle >= 0) {
      if (!this.handlers.autoFit?.(handle)) this.autoFit(handle);
      return;
    }
    const rowHandle = this.rowResizeHandleAt(x, y);
    if (rowHandle >= 0) {
      this.autoFitRow(rowHandle);
      return;
    }
    if (y < HEADER_H || x < this.rowHeaderWidth()) return;
    this.startEdit();
  }

  private onContextMenu(e: MouseEvent): void {
    const { x, y } = this.point(e);
    e.preventDefault();
    if (y < HEADER_H) {
      const c = this.colAt(x);
      if (c >= 0) this.handlers.headerMenu(c, e.clientX, e.clientY);
    } else {
      const r = this.rowAt(y);
      const sel = this.selection();
      if (r < sel.r0 || r > sel.r1) this.select({ r, c: Math.max(0, this.colAt(x)) }, undefined, false);
      this.handlers.rowMenu(r, e.clientX, e.clientY);
    }
  }

  /** Fit a column to texts measured in the grid font (S.05: the server sends the longest). */
  fitColumn(col: number, texts: string[]): void {
    if (col < 0 || col >= this.widths.length) return;
    this.ctx.font = `13px ${this.colors.ui}`;
    let w = 48;
    for (const t of texts) w = Math.max(w, this.ctx.measureText(t).width + 20);
    this.widths[col] = Math.min(600, Math.ceil(w));
    this.layoutColumns();
    this.updateSpacer();
    this.handlers.columnResized(col, this.widths[col]);
    this.invalidate();
  }

  /** Fit a row to the largest font among its loaded cells (S.05). */
  autoFitRow(row: number): void {
    let size = 13;
    for (let c = 0; c < this.widths.length && c < 200; c++) size = Math.max(size, Math.min(this.model.style?.(row, c)?.size ?? 13, 72));
    const h = Math.max(ROW_H, Math.ceil(size * 1.5 + 4));
    this.setRowSize(row, h);
    this.handlers.rowResized?.(row, h);
  }

  private autoFit(col: number): void {
    const cols = this.model.columns();
    const first = Math.max(0, this.rowAtContent(this.scrollY));
    const last = Math.min(this.model.rowCount() - 1, this.rowAtContent(this.scrollY + this.bodyHeight()));
    this.ctx.font = `600 13px ${this.colors.ui}`;
    let w = this.ctx.measureText(cols[col].name).width + 48;
    this.ctx.font = `13px ${this.colors.ui}`;
    for (let r = first; r <= last; r++) {
      let text: string | undefined;
      if (this.isSheet() && this.model.cell) text = this.model.cell(r, col)?.text;
      else {
        const v = this.model.value(r, col);
        if (v !== undefined) text = this.format(v, cols[col]);
      }
      if (text) w = Math.max(w, this.ctx.measureText(text).width + 20);
    }
    this.widths[col] = Math.min(600, Math.ceil(w));
    this.layoutColumns();
    this.handlers.columnResized(col, this.widths[col]);
    this.invalidate();
  }

  private onKey(e: KeyboardEvent): void {
    if (this.editing) return;
    if (this.handlers.command?.(e)) { e.preventDefault(); return; }
    const mod = e.metaKey || e.ctrlKey;
    // Alt+↓ opens the column's filter, as in other spreadsheets.
    if (e.altKey && e.key === "ArrowDown" && !this.isSheet() && this.handlers.headerFilter) {
      e.preventDefault();
      this.handlers.headerFilter(this.active.c, -1, -1);
      return;
    }
    const pageRows = Math.max(1, Math.floor(this.bodyHeight() / ROW_H) - 1);
    const lastRow = Math.max(0, this.model.rowCount() - 1);
    const lastCol = this.widths.length - 1;
    const move = (dr: number, dc: number) => {
      let r = this.active.r + dr;
      let c = this.active.c + dc;
      if (mod && dr) r = dr > 0 ? lastRow : 0;
      if (mod && dc) c = dc > 0 ? lastCol : 0;
      if (dr) r = this.shownLine(r, Math.sign(dr), Math.max(lastRow, this.displayRows() - 1), (i) => this.rowHeight(i) === 0);
      if (dc) c = this.shownLine(c, Math.sign(dc), lastCol, (i) => this.widths[i] === 0);
      this.select({ r, c }, e.shiftKey ? this.anchor : { r, c });
    };
    switch (e.key) {
      case "ArrowDown": move(1, 0); break;
      case "ArrowUp": move(-1, 0); break;
      case "ArrowLeft": move(0, -1); break;
      case "ArrowRight": move(0, 1); break;
      case "PageDown": move(pageRows, 0); break;
      case "PageUp": move(-pageRows, 0); break;
      case "Home": this.select(mod ? { r: 0, c: 0 } : { r: this.active.r, c: 0 }); break;
      case "End": this.select(mod ? { r: lastRow, c: lastCol } : { r: this.active.r, c: lastCol }); break;
      case "Tab":
        // At the grid's edge Tab leaves the grid, so keyboard users are never trapped.
        if (e.shiftKey ? this.active.c === 0 : this.active.c >= lastCol) return;
        move(0, e.shiftKey ? -1 : 1);
        break;
      case "Enter": case "F2": this.startEdit(); break;
      case "Escape": this.select(this.active); break;
      case "Delete": case "Backspace": this.clearSelection(); break;
      default:
        if (mod && e.key.toLowerCase() === "z") {
          if (e.shiftKey) this.handlers.redo?.();
          else this.handlers.undo?.();
        } else if (mod && e.key.toLowerCase() === "y") {
          this.handlers.redo?.();
        } else if (mod && e.key.toLowerCase() === "c") {
          this.handlers.copy(this.selection());
        } else if (mod && e.key.toLowerCase() === "a") {
          this.select({ r: lastRow, c: lastCol }, { r: 0, c: 0 }, false);
        } else if (!mod && !e.altKey && e.key.length === 1) {
          this.startEdit(e.key);
        } else {
          return;
        }
    }
    e.preventDefault();
  }

  /** Clear the selected cells (Delete), for menus. */
  clearSelection(): void {
    const s = this.selection();
    const cells = (s.r1 - s.r0 + 1) * (s.c1 - s.c0 + 1);
    if (cells > 100_000) return;
    const edits = [];
    const cols = this.model.columns();
    for (let r = s.r0; r <= Math.min(s.r1, this.model.rowCount() - 1); r++)
      for (let c = s.c0; c <= s.c1; c++) if (!cols[c].formula) edits.push({ r, c, value: null });
    if (edits.length) this.handlers.commitEdits(edits);
  }

  private placeEditor(): void {
    if (!this.editing) return;
    const rect = this.cellRect(this.editing);
    Object.assign(this.editor.style, {
      left: `${rect.x * this.zoom}px`, top: `${rect.y * this.zoom}px`,
      width: `${Math.max(rect.w, 120) * this.zoom}px`, height: `${rect.h * this.zoom}px`, fontSize: `${13 * this.zoom}px`,
    });
  }

  private onEditorKey(e: KeyboardEvent): void {
    // Keys the editor handles must not bubble to the grid, which would treat the same
    // Enter as "start editing the next cell".
    if (e.key === "F4" && this.isSheet() && !e.isComposing) { e.preventDefault(); e.stopPropagation(); applyReferenceLock(this.editor); }
    else if (e.key === "Enter") { e.preventDefault(); e.stopPropagation(); this.commitEditor(e.shiftKey ? -1 : 1, 0); }
    else if (e.key === "Tab") { e.preventDefault(); e.stopPropagation(); this.commitEditor(0, e.shiftKey ? -1 : 1); }
    else if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); this.cancelEditor(); }
    else e.stopPropagation();
  }

  private cancelEditor(): void {
    this.editing = null;
    this.editor.style.display = "none";
    this.focus();
  }

  private async commitEditor(dr: number, dc: number): Promise<void> {
    const cell = this.editing;
    if (!cell) return;
    this.editing = null;
    this.editor.style.display = "none";
    const value = this.editor.value;
    const before = this.currentText(cell);
    this.focus();
    // Move first: keys pressed while the server confirms must not be overridden by a late
    // jump. A rejected value is reported by the handler and its cell is refreshed.
    if (dr || dc) {
      const next = { r: Math.max(0, cell.r + dr), c: Math.max(0, cell.c + dc) };
      this.select(next, next);
    }
    if (value !== before) {
      await this.handlers.commitEdits([{ r: cell.r, c: cell.c, value: value === "" ? null : value, base: this.editingBase }]);
    }
  }

  // --------------------------------------------------------------- drawing

  private draw(): void {
    const { ctx, colors } = this;
    const W = this.width, H = this.height;
    if (!W || !H) return;
    const cols = this.model.columns();
    const rhw = this.rowHeaderWidth();
    const rowCount = this.model.rowCount();
    const display = this.displayRows();
    const fr = this.frozenRowCount(), fh = this.frozenHeight();
    const first = Math.max(fr, this.rowAtContent(this.scrollY + fh));
    const last = Math.min(display - 1, this.rowAtContent(this.scrollY + this.bodyHeight()));
    // Scrolled rows first, then the frozen rows over them (S.05).
    const rowList: number[] = [];
    for (let r = first; r <= last; r++) if (this.rowHeight(r) > 0) rowList.push(r);
    for (let r = 0; r < fr; r++) if (this.rowHeight(r) > 0) rowList.push(r);

    let c0 = Math.max(this.frozenCount(), this.colAt(rhw + this.frozenWidth() + 1));
    let c1 = this.colAt(W - 1);
    if (c1 < 0) c1 = cols.length - 1;
    if (!cols.length) { c0 = 0; c1 = -1; }
    if (rowCount > 0) this.model.ensure(first, Math.min(last, rowCount - 1), c0, Math.max(c0, c1));
    if (rowCount > 0 && fr) this.model.ensure(0, Math.min(fr, rowCount) - 1, c0, Math.max(c0, c1));

    const frozen = this.frozenCount();
    if (rowCount > 0 && frozen) this.model.ensure(first, Math.min(last, rowCount - 1), 0, frozen - 1);
    const visible = [...Array.from({ length: Math.max(0, c1 - c0 + 1) }, (_, i) => c0 + i), ...Array.from({ length: frozen }, (_, i) => i)].filter((c) => (this.widths[c] ?? 0) > 0);
    ctx.fillStyle = colors.bg;
    ctx.fillRect(0, 0, W, H);
    const sel = this.selection();

    // Body
    const sheetCells = this.isSheet() ? this.model.cell?.bind(this.model) : undefined;
    ctx.textBaseline = "middle";
    ctx.font = `13px ${colors.ui}`;
    for (const c of visible) {
      const col = cols[c];
      const x = this.columnX(c);
      const w = this.widths[c];
      const numeric = isNumericKind(col.kind);
      ctx.save();
      ctx.beginPath();
      ctx.rect(Math.max(x, c < frozen ? rhw : rhw + this.frozenWidth()), HEADER_H, Math.max(0, x + w - Math.max(x, c < frozen ? rhw : rhw + this.frozenWidth())), H - HEADER_H);
      ctx.clip();
      if (c < frozen) { ctx.fillStyle = colors.bg; ctx.fillRect(x, HEADER_H, w, H - HEADER_H); }
      if (col.formula) {
        ctx.fillStyle = colors.formulaBg;
        ctx.fillRect(x, HEADER_H, w, H - HEADER_H);
      }
      for (const r of rowList) {
        const y = this.rowY(r), rh = this.rowHeight(r);
        if (r < fr) { ctx.fillStyle = col.formula ? colors.formulaBg : colors.bg; ctx.fillRect(x, y, w, rh); }
        if (r >= rowCount) continue;
        if (sheetCells) {
          const cell = sheetCells(r, c);
          const st = this.model.style?.(r, c);
          if (st?.fill) { ctx.fillStyle = st.fill; ctx.fillRect(x, y, w, rh); }
          if (st?.borders) {
            ctx.fillStyle = colors.text;
            if (st.borders.includes("t")) ctx.fillRect(x, y, w, 1);
            if (st.borders.includes("b")) ctx.fillRect(x, y + rh - 1, w, 1);
            if (st.borders.includes("l")) ctx.fillRect(x, y, 1, rh);
            if (st.borders.includes("r")) ctx.fillRect(x + w - 1, y, 1, rh);
          }
          if (this.model.comment?.(r, c)) {
            ctx.fillStyle = "#D97706";
            ctx.beginPath(); ctx.moveTo(x + w - 7, y); ctx.lineTo(x + w, y); ctx.lineTo(x + w, y + 7); ctx.closePath(); ctx.fill();
          }
          if (cell === undefined) {
            if (c === c0) {
              ctx.fillStyle = colors.placeholder;
              ctx.fillRect(x + 8, y + 9, 24, 6);
            }
            continue;
          }
          if (!cell || !cell.text) continue;
          ctx.fillStyle = st?.color ?? (cell.text.startsWith("#") && cell.align === "center" ? colors.danger : colors.text);
          const align: CanvasTextAlign = st?.h === "left" || st?.h === "center" || st?.h === "right" ? st.h : cell.align;
          ctx.textAlign = align;
          if (st) ctx.font = `${st.i ? "italic " : ""}${st.b ? "700 " : ""}${Math.min(st.size ?? 13, 20)}px ${colors.ui}`;
          const tx = align === "right" ? x + w - 8 : align === "center" ? x + w / 2 : x + 8;
          const ty = st?.v === "top" ? y + 9 : y + rh / 2 + 1;
          ctx.fillText(cell.text, tx, ty);
          if (st?.u || st?.s) {
            const tw = Math.min(ctx.measureText(cell.text).width, w - 16);
            const lx = align === "right" ? tx - tw : align === "center" ? tx - tw / 2 : tx;
            ctx.fillRect(lx, st.u ? ty + 7 : ty, tw, 1);
            if (st.u && st.s) ctx.fillRect(lx, ty, tw, 1);
          }
          if (st) ctx.font = `13px ${colors.ui}`;
          continue;
        }
        const v = this.model.value(r, c);
        if (v === undefined) {
          ctx.fillStyle = colors.placeholder;
          ctx.fillRect(x + 8, y + 9, Math.min(w - 16, 40 + ((r * 7 + c * 13) % 50)), 6);
          continue;
        }
        const background = this.model.color?.(v, col);
        if (background) { ctx.fillStyle = background; ctx.fillRect(x, y, w, rh); }
        const text = this.format(v, col);
        if (!text) continue;
        ctx.fillStyle = colors.text;
        if (numeric || col.kind === "bool") {
          ctx.textAlign = "right";
          ctx.fillText(text, x + w - 8, y + rh / 2 + 1);
        } else {
          ctx.textAlign = "left";
          ctx.fillText(text, x + 8, y + rh / 2 + 1);
        }
      }
      ctx.restore();
    }

    // Grid lines
    ctx.strokeStyle = colors.grid;
    ctx.lineWidth = 1;
    ctx.beginPath();
    for (const r of rowList) {
      const y = Math.round(this.rowY(r) + this.rowHeight(r)) + 0.5;
      if (r >= fr && y < HEADER_H + fh) continue;
      ctx.moveTo(rhw, y);
      ctx.lineTo(W, y);
    }
    for (const c of visible) {
      const x = Math.round(this.columnX(c) + this.widths[c]) - 0.5;
      ctx.moveTo(x, HEADER_H);
      ctx.lineTo(x, H);
    }
    ctx.stroke();
    if (fr) {
      ctx.fillStyle = colors.grid;
      ctx.fillRect(0, HEADER_H + fh - 1, W, 2);
    }
    const selTop = (r: number) => (r < fr ? HEADER_H : HEADER_H + fh);

    // Clip selection to each pane so scrolling cells cannot highlight frozen cells.
    if (cols.length && frozen) {
      const y = this.rowY(sel.r0);
      const h = Math.max(0, this.rowY(sel.r1) + this.rowHeight(sel.r1) - y);
      for (const c of visible) {
        if (c < sel.c0 || c > sel.c1) continue;
        const x = this.columnX(c), w = this.widths[c];
        ctx.save(); ctx.beginPath();
        const left = c < frozen ? rhw : rhw + this.frozenWidth();
        ctx.rect(left, selTop(sel.r0), W - left, H - selTop(sel.r0)); ctx.clip();
        if (sel.r0 !== sel.r1 || sel.c0 !== sel.c1) { ctx.fillStyle = colors.selFill; ctx.fillRect(x, y, w, h); }
        ctx.strokeStyle = colors.selBorder; ctx.lineWidth = 2; ctx.beginPath();
        ctx.moveTo(x, y + 1); ctx.lineTo(x + w, y + 1); ctx.moveTo(x, y + h - 1); ctx.lineTo(x + w, y + h - 1);
        if (c === sel.c0) { ctx.moveTo(x + 1, y); ctx.lineTo(x + 1, y + h); }
        if (c === sel.c1) { ctx.moveTo(x + w - 1, y); ctx.lineTo(x + w - 1, y + h); }
        ctx.stroke(); ctx.restore();
      }
      const active = this.cellRect(this.active), left = this.active.c < frozen ? rhw : rhw + this.frozenWidth();
      ctx.save(); ctx.beginPath(); ctx.rect(left, selTop(this.active.r), W - left, H - selTop(this.active.r)); ctx.clip();
      ctx.strokeStyle = colors.selBorder; ctx.lineWidth = 2; ctx.strokeRect(active.x + 1, active.y + 1, active.w - 2, active.h - 2); ctx.restore();
    }
    if (cols.length && !frozen) {
      const a = this.cellRect({ r: sel.r0, c: sel.c0 });
      const b = this.cellRect({ r: sel.r1, c: sel.c1 });
      const sx = a.x, sy = a.y, sw = b.x + b.w - a.x, sh = Math.max(0, b.y + b.h - a.y);
      ctx.save();
      ctx.beginPath();
      ctx.rect(rhw, selTop(sel.r0), W - rhw, H - selTop(sel.r0));
      ctx.clip();
      if (sel.r0 !== sel.r1 || sel.c0 !== sel.c1) {
        ctx.fillStyle = colors.selFill;
        ctx.fillRect(sx, sy, sw, sh);
      }
      ctx.strokeStyle = colors.selBorder;
      ctx.lineWidth = 2;
      ctx.strokeRect(sx + 1, sy + 1, sw - 2, sh - 2);
      const act = this.cellRect(this.active);
      ctx.lineWidth = 2;
      ctx.strokeRect(act.x + 1, act.y + 1, act.w - 2, act.h - 2);
      ctx.restore();
    }

    // Reference outlines never fetch cells or change the active edit destination.
    if (this.isSheet()) {
      ctx.save(); ctx.beginPath(); ctx.rect(rhw, HEADER_H, W - rhw, H - HEADER_H); ctx.clip();
      for (const [i, ref] of this.references.entries()) {
        if (ref.c0 >= cols.length || ref.r1 < first || ref.r0 > last) continue;
        const a = this.cellRect({ r: ref.r0, c: ref.c0 });
        const b = this.cellRect({ r: ref.r1, c: Math.min(ref.c1, cols.length - 1) });
        ctx.strokeStyle = colors.references[i % 4];
        ctx.lineWidth = 2; ctx.setLineDash([5, 3]);
        ctx.strokeRect(a.x + 2, a.y + 2, b.x + b.w - a.x - 4, b.y + b.h - a.y - 4);
      }
      ctx.restore();
    }

    // Row header
    ctx.fillStyle = colors.headerBg;
    ctx.fillRect(0, HEADER_H, rhw, H - HEADER_H);
    ctx.font = `12px ${colors.mono}`;
    ctx.textAlign = "right";
    for (const r of rowList) {
      const y = this.rowY(r), rh = this.rowHeight(r);
      if (r < fr) { ctx.fillStyle = colors.headerBg; ctx.fillRect(0, y, rhw, rh); }
      if (r >= sel.r0 && r <= sel.r1) {
        ctx.fillStyle = colors.activeHeader;
        ctx.fillRect(0, y, rhw, rh);
      }
      ctx.fillStyle = r < rowCount ? colors.muted : colors.placeholder;
      ctx.fillText(String(r + 1), rhw - 8, y + rh / 2 + 1);
    }
    ctx.strokeStyle = colors.grid;
    ctx.beginPath();
    ctx.moveTo(rhw - 0.5, 0);
    ctx.lineTo(rhw - 0.5, H);
    ctx.stroke();

    // Column header
    ctx.fillStyle = colors.headerBg;
    ctx.fillRect(0, 0, W, HEADER_H);
    const sortBy = new Map(this.model.sort().map((s, i) => [s.column, { ...s, i }]));
    const filtered = new Set(this.model.filters().map((f) => f.column));
    const sheetMode = this.isSheet();
    for (const c of visible) {
      const col = cols[c];
      const x = this.columnX(c);
      const w = this.widths[c];
      if (c < frozen) { ctx.fillStyle = colors.headerBg; ctx.fillRect(x, 0, w, HEADER_H); }
      if (c >= sel.c0 && c <= sel.c1) {
        ctx.fillStyle = colors.activeHeader;
        ctx.fillRect(x, 0, w, HEADER_H);
      }
      if (sheetMode) {
        ctx.fillStyle = colors.muted;
        ctx.font = `600 12px ${colors.ui}`;
        ctx.textAlign = "center";
        ctx.fillText(col.name, x + w / 2, HEADER_H / 2 + 1);
        continue;
      }
      ctx.save();
      ctx.beginPath();
      ctx.rect(Math.max(x, c < frozen ? rhw : rhw + this.frozenWidth()), 0, Math.max(0, x + w - 22 - Math.max(x, c < frozen ? rhw : rhw + this.frozenWidth())), HEADER_H);
      ctx.clip();
      ctx.textAlign = "left";
      ctx.fillStyle = colors.muted;
      ctx.font = `11px ${colors.mono}`;
      const glyph = col.formula ? "ƒ" : isNumericKind(col.kind) ? "#" : col.kind === "text" ? "Aa" : col.kind === "bool" ? "✓" : "◷";
      ctx.fillText(glyph, x + 8, HEADER_H / 2 + 1);
      ctx.fillStyle = colors.headerText;
      ctx.font = `600 13px ${colors.ui}`;
      let label = col.name;
      const s = sortBy.get(col.name);
      if (s) label += s.desc ? "  ↓" : "  ↑";
      if (filtered.has(col.name)) label += "  ⧩";
      ctx.fillText(label, x + 28, HEADER_H / 2 + 1);
      ctx.restore();
      ctx.fillStyle = colors.muted;
      ctx.font = `11px ${colors.ui}`;
      ctx.textAlign = "center";
      ctx.fillText("▾", x + w - 11, HEADER_H / 2 + 1);
    }
    ctx.strokeStyle = colors.grid;
    ctx.beginPath();
    ctx.moveTo(0, HEADER_H - 0.5);
    ctx.lineTo(W, HEADER_H - 0.5);
    for (const c of visible) {
      const x = Math.round(this.columnX(c) + this.widths[c]) - 0.5;
      ctx.moveTo(x, 0);
      ctx.lineTo(x, HEADER_H);
    }
    ctx.stroke();
    ctx.fillStyle = colors.headerBg;
    ctx.fillRect(0, 0, rhw - 1, HEADER_H - 1);
  }
}

export const DEFAULT_ROW_HEIGHT = ROW_H;
