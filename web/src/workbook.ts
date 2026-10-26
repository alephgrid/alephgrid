import { api, Rpc } from "./api";
import { importFile } from "./importer";

import { FETCH_LIMIT, RowCache, SheetCache } from "./cache";
import { columnLetter, formatCount, formatStat, formatValue, isNumericKind, rawText } from "./format";
import { Grid, type Cell, type Range } from "./grid";
import type {
  CellValue, Column, Filter, FilterOp, Imported, ServerEvent, SheetCopy, SheetInfo, TableInfo, ViewInfo, ViewSpec, WorkbookMeta,
} from "./types";
import { confirmDialog, el, formDialog, openModal, showMenu, toast, type MenuItem } from "./ui";

const FILTER_OPS: { value: FilterOp; label: string; needsValue: boolean }[] = [
  { value: "eq", label: "is equal to", needsValue: true },
  { value: "neq", label: "is not equal to", needsValue: true },
  { value: "contains", label: "contains", needsValue: true },
  { value: "not_contains", label: "does not contain", needsValue: true },
  { value: "starts_with", label: "starts with", needsValue: true },
  { value: "gt", label: "is greater than", needsValue: true },
  { value: "gte", label: "is greater than or equal to", needsValue: true },
  { value: "lt", label: "is less than", needsValue: true },
  { value: "lte", label: "is less than or equal to", needsValue: true },
  { value: "is_empty", label: "is empty", needsValue: false },
  { value: "not_empty", label: "is not empty", needsValue: false },
];

const COLUMN_TYPES = [
  { value: "formula", label: "Formula (computed)" },
  { value: "VARCHAR", label: "Text" },
  { value: "BIGINT", label: "Whole number" },
  { value: "DOUBLE", label: "Decimal number" },
  { value: "DATE", label: "Date" },
  { value: "TIMESTAMP", label: "Date & time" },
  { value: "BOOLEAN", label: "True / false" },
];

const IMPORT_ACCEPT = ".csv,.tsv,.txt,.parquet,.json,.jsonl,.ndjson,.gz,.xlsx";
const SHEET_MIN_ROWS = 1000;
const SHEET_MIN_COLS = 26;
const SHEET_COL_WIDTH = 104;

type Active = { kind: "table"; name: string } | { kind: "sheet"; index: number; name: string };

function defaultWidth(c: Column): number {
  const byName = c.name.length * 8 + 52;
  switch (c.kind) {
    case "int": case "number": return Math.max(110, Math.min(220, byName));
    case "bool": return Math.max(90, Math.min(200, byName));
    case "date": return Math.max(116, byName);
    case "datetime": return Math.max(176, byName);
    default: return Math.max(150, Math.min(320, byName));
  }
}

function describeFilter(f: Filter): string {
  const op = FILTER_OPS.find((o) => o.value === f.op);
  return `${f.column} ${op?.label ?? f.op}${op?.needsValue ? ` “${f.value ?? ""}”` : ""}`;
}

function parseTsv(text: string): string[][] {
  const rows: string[][] = [];
  let row: string[] = [];
  let cell = "";
  let quoted = false;
  for (let i = 0; i < text.length; i++) {
    const ch = text[i];
    if (quoted) {
      if (ch === '"' && text[i + 1] === '"') { cell += '"'; i++; }
      else if (ch === '"') quoted = false;
      else cell += ch;
    } else if (ch === '"' && cell === "") quoted = true;
    else if (ch === "\t") { row.push(cell); cell = ""; }
    else if (ch === "\n" || ch === "\r") {
      if (ch === "\r" && text[i + 1] === "\n") i++;
      row.push(cell); rows.push(row); row = []; cell = "";
    } else cell += ch;
  }
  if (cell !== "" || row.length) { row.push(cell); rows.push(row); }
  return rows;
}

function tsvCell(v: CellValue): string {
  const s = rawText(v);
  return /[\t\n"]/.test(s) ? `"${s.replace(/"/g, '""')}"` : s;
}

function sheetColumns(count: number): Column[] {
  return Array.from({ length: count }, (_, i) => ({ name: columnLetter(i), sql_type: "", kind: "text" as const }));
}

export class WorkbookScreen {
  readonly root: HTMLElement;
  private rpc: Rpc;
  private cache: RowCache;
  private sheetCache: SheetCache;
  private grid: Grid;
  private meta: WorkbookMeta;
  private tables: TableInfo[] = [];
  private sheets: SheetInfo[] = [];
  private active: Active | null = null;
  private view: ViewInfo | null = null;
  private spec: ViewSpec = { table: "", sort: [], filters: [] };
  private sheetDims = { rows: SHEET_MIN_ROWS, cols: SHEET_MIN_COLS };
  private sheetCols: Column[] = sheetColumns(SHEET_MIN_COLS);
  private allowSql = true;
  /** Rows copied into a sheet at most (from /api/config). */
  private sheetCopyRows = 100_000;

  private titleEl = el("button", { class: "wb-title", title: "Rename workbook" });
  private statusDot = el("span", { class: "status-dot", title: "Connecting…" });
  private chips = el("div", { class: "chips" });
  private cellRef = el("div", { class: "cell-ref" });
  private formulaInput = el("input", { class: "formula-input", spellcheck: false, "aria-label": "Cell value or formula" });
  private tabsEl = el("div", { class: "tabs", role: "tablist" });
  private statsEl = el("div", { class: "stats" });
  private rowsEl = el("div", { class: "row-count" });
  /** Says whether the open item is a Table or a Sheet, and what that means. */
  private kindEl = el("span", { class: "kind-badge", hidden: true });
  private gridWrap = el("div", { class: "grid-wrap" });
  private busyEl = el("div", { class: "busy", hidden: true });
  private fileInput!: HTMLInputElement;
  private tableButtons: HTMLElement[] = [];
  private sheetButtons: HTMLElement[] = [];
  private statsTimer = 0;
  private statsSeq = 0;
  private timers = new Set<number>();
  private disposed = false;

  constructor(meta: WorkbookMeta, private goHome: () => void) {
    this.meta = meta;
    this.rpc = new Rpc(meta.id);
    const onLoaded = () => {
      this.grid.invalidate();
      if (document.activeElement !== this.formulaInput) this.refreshFormulaValue();
      // Sheet stats are computed from cached cells; recompute once fresh values arrive.
      if (this.isSheet()) this.sheetSelectionStats(this.grid.selection());
    };
    const onError = (e: Error) => toast(e.message, "error");
    this.cache = new RowCache(this.rpc, onLoaded, onError);
    this.sheetCache = new SheetCache(this.rpc, onLoaded, onError);
    const self = this;
    this.grid = new Grid(
      {
        rowCount: () => (this.isSheet() ? this.sheetDims.rows : this.view?.row_count ?? 0),
        columns: () => (this.isSheet() ? this.sheetCols : this.view?.columns ?? []),
        canAppend: () => !this.isSheet() && (this.view?.can_append ?? false),
        sort: () => (this.isSheet() ? [] : this.spec.sort),
        filters: () => (this.isSheet() ? [] : this.spec.filters),
        row: (r) => this.cache.row(r),
        ensure: (a, b) => (this.isSheet() ? this.sheetCache.ensure(a, b) : this.cache.ensure(a, b)),
        mode: () => (this.isSheet() ? "sheet" : "table"),
        cell: (r, c) => {
          const cell = this.sheetCache.cell(r, c);
          return cell ? { text: cell[0], align: cell[1] } : cell;
        },
        editText: (r, c) => {
          const cell = this.sheetCache.cell(r, c);
          return cell ? cell[2] ?? cell[0] : "";
        },
      },
      {
        selectionChanged: (range, active) => self.onSelection(range, active),
        commitEdits: (edits) => self.commitEdits(edits),
        headerMenu: (c, x, y) => self.headerMenu(c, x, y),
        rowMenu: (r, x, y) => self.rowMenu(r, x, y),
        copy: (range) => self.copy(range),
        paste: (at, text) => self.paste(at, text),
        columnResized: () => self.saveWidths(),
        undo: () => self.undo(false),
        redo: () => self.undo(true),
      },
    );
    this.gridWrap.append(this.grid.host, this.busyEl);
    this.root = this.render();
    this.titleEl.textContent = meta.name;
    this.rpc.onStatus = (s) => {
      this.statusDot.className = `status-dot status-${s}`;
      this.statusDot.title = s === "open" ? "Connected" : s === "connecting" ? "Connecting…" : "Disconnected — retrying";
    };
    this.rpc.onEvent = (e) => this.onEvent(e);
    this.rpc.onReconnect = () => this.reopen();
  }

  private isSheet(): boolean {
    return this.active?.kind === "sheet";
  }

  async start(): Promise<void> {
    try {
      const config = await api.config();
      this.allowSql = config.allow_sql;
      this.sheetCopyRows = config.sheet_copy_rows ?? this.sheetCopyRows;
    } catch { /* keep default */ }
    await this.rpc.connect();
    await this.loadItems();
    const demo = Number(new URLSearchParams(location.search).get("demo"));
    if (demo > 0) history.replaceState(null, "", location.pathname);
    if (this.tables.length) await this.openTable(this.tables[0].name);
    else if (this.sheets.length) await this.openSheet(this.sheets[0].index);
    else if (demo > 0) await this.createDemo(demo);
    else this.renderEmpty();
    this.grid.focus();
  }

  /** Leave the screen: close the socket, drop pending replies, stop timers and listeners. */
  dispose(): void {
    this.disposed = true;
    this.rpc.close();
    clearTimeout(this.statsTimer);
    for (const t of this.timers) clearTimeout(t);
    this.timers.clear();
    this.cache.reset(0);
    this.sheetCache.reset(0, 0);
    this.grid.dispose();
    this.root.remove();
  }

  /** setTimeout that is cancelled by dispose(). */
  private later(fn: () => void, ms: number): void {
    const t = window.setTimeout(() => {
      this.timers.delete(t);
      if (!this.disposed) fn();
    }, ms);
    this.timers.add(t);
  }

  // ---------------------------------------------------------------- layout

  private render(): HTMLElement {
    this.titleEl.addEventListener("click", () => this.renameWorkbook());
    this.tabsEl.addEventListener("keydown", (e) => {
      if (e.key !== "ArrowLeft" && e.key !== "ArrowRight" && e.key !== "Home" && e.key !== "End") return;
      const tabs = [...this.tabsEl.querySelectorAll<HTMLButtonElement>('[role="tab"]')];
      const i = tabs.indexOf(document.activeElement as HTMLButtonElement);
      if (i < 0 || !tabs.length) return;
      e.preventDefault();
      const next = e.key === "Home" ? 0 : e.key === "End" ? tabs.length - 1
        : (i + (e.key === "ArrowRight" ? 1 : -1) + tabs.length) % tabs.length;
      tabs[next].focus();
      tabs[next].click();
    });
    this.formulaInput.addEventListener("keydown", (e) => {
      if (e.key === "Enter") { e.preventDefault(); this.commitFormulaBar(); }
      if (e.key === "Escape") { this.onSelection(this.grid.selection(), this.grid.active); this.grid.focus(); }
    });
    this.fileInput = el("input", { type: "file", hidden: true, multiple: true, accept: IMPORT_ACCEPT,
      onchange: (e: Event) => {
        const files = (e.target as HTMLInputElement).files;
        if (files) this.importFiles([...files]);
        (e.target as HTMLInputElement).value = "";
      } });
    this.tableButtons = [
      el("button", { class: "btn", onclick: () => this.addColumnDialog() }, "Add column"),
      el("button", { class: "btn", onclick: () => this.filterDialog(this.grid.active.c) }, "Filter"),
    ];
    this.sheetButtons = [
      el("button", { class: "btn", title: "Undo (⌘Z)", "aria-label": "Undo", onclick: () => this.undo(false) }, "↶"),
      el("button", { class: "btn", title: "Redo (⇧⌘Z)", "aria-label": "Redo", onclick: () => this.undo(true) }, "↷"),
    ];
    const root = el("div", { class: "workbook" },
      el("header", { class: "toolbar" },
        el("button", { class: "brand", onclick: () => this.goHome(), title: "All workbooks" }, el("span", { class: "logo" }), "Rowfinity"),
        this.titleEl,
        this.statusDot,
        el("div", { class: "spacer" }),
        ...this.sheetButtons,
        el("button", { class: "btn", onclick: () => this.fileInput.click() }, "Import"),
        ...this.tableButtons,
        el("button", { class: "btn", onclick: () => this.sqlPanel() }, "SQL"),
        el("button", { class: "btn", onclick: (e: MouseEvent) => this.exportMenu(e) }, "Export ▾"),
        this.fileInput,
      ),
      this.chips,
      el("div", { class: "formula-bar" }, this.cellRef, el("span", { class: "fx" }, "ƒx"), this.formulaInput),
      this.gridWrap,
      el("footer", { class: "footer" },
        el("button", { class: "tab-add", title: "New table or sheet", "aria-label": "New table or sheet",
          onclick: (e: MouseEvent) => this.newItemMenu(e) }, "+"),
        this.tabsEl,
        el("div", { class: "spacer" }),
        this.statsEl,
        this.kindEl,
        this.rowsEl,
      ),
    );
    root.addEventListener("dragover", (e) => { e.preventDefault(); root.classList.add("dropping"); });
    root.addEventListener("dragleave", (e) => { if (e.target === root) root.classList.remove("dropping"); });
    root.addEventListener("drop", (e) => {
      e.preventDefault();
      root.classList.remove("dropping");
      const files = e.dataTransfer?.files;
      if (files?.length) this.importFiles([...files]);
    });
    this.updateToolbar();
    return root;
  }

  private setKind(kind: "table" | "sheet"): void {
    this.kindEl.hidden = false;
    this.kindEl.className = `kind-badge kind-${kind}`;
    this.kindEl.textContent = kind === "table" ? "Table" : "Sheet";
    this.kindEl.title = kind === "table"
      ? "Table: typed columns of any size. Sort, filter, formula columns and SQL run on the server, so millions of rows stay fast."
      : `Sheet: free-form cells with Excel formulas, kept in memory. Best for thousands of rows; copying a table or query brings in at most ${formatCount(this.sheetCopyRows)} rows.`;
  }

  /** Tell the user how much of the data reached the sheet. */
  private reportCopy(copy: SheetCopy, what: string): void {
    const n = formatCount(copy.rows_copied);
    if (!copy.truncated) {
      toast(`Copied ${n} rows of ${what} into sheet ${copy.sheet.name}`, "success");
    } else {
      const of = copy.total_rows != null ? ` of ${formatCount(copy.total_rows)}` : "";
      toast(`Copied the first ${n}${of} rows of ${what} into sheet ${copy.sheet.name}. Sheets take at most ${formatCount(copy.row_limit)} rows of this width; keep working with the full data in its table.`, "info", 10_000);
    }
  }

  private async copyTableToSheet(t: TableInfo): Promise<void> {
    const current = this.active?.kind === "table" && this.active.name === t.name && this.view;
    const filtered = current && (this.spec.sort.length || this.spec.filters.length);
    const v = await formDialog(`Copy ${t.name} to a sheet`, [
      { name: "name", label: "Sheet name", value: t.name,
        hint: `Copies the first ${formatCount(Math.min(t.row_count, this.sheetCopyRows))} of ${formatCount(t.row_count)} rows${filtered ? " in the current sort and filter" : ""}. The sheet is a separate copy: later table edits do not change it.` },
    ], "Copy to sheet");
    if (!v) return;
    try {
      let viewId = current ? this.view!.view_id : 0;
      let temporary = false;
      if (!current) {
        viewId = (await this.rpc.call("open_view", { spec: { table: t.name, sort: [], filters: [] } })).view_id;
        temporary = true;
      }
      const copy = await this.rpc.call("table_to_sheet", { view_id: viewId, name: v.name });
      if (temporary) this.rpc.call("close_view", { view_id: viewId }).catch(() => {});
      await this.loadItems();
      await this.openSheet(copy.sheet.index);
      this.reportCopy(copy, t.name);
    } catch (e) {
      toast((e as Error).message, "error");
    }
  }

  private updateToolbar(): void {
    const sheet = this.isSheet();
    for (const b of this.tableButtons) b.hidden = sheet || !this.active;
    for (const b of this.sheetButtons) b.hidden = !sheet;
    this.chips.hidden = sheet || this.chips.childElementCount === 0;
  }

  private renderEmpty(): void {
    this.active = null;
    this.view = null;
    this.grid.setColumns([]);
    this.rowsEl.textContent = "";
    this.kindEl.hidden = true;
    this.renderTabs();
    this.renderChips();
    this.updateToolbar();
    const card = el("div", { class: "empty" },
      el("h2", {}, "This workbook is empty"),
      el("p", {}, "Drop a CSV, Parquet, JSON or Excel file anywhere on this page, or:"),
      el("div", { class: "empty-actions" },
        el("button", { class: "btn btn-primary", onclick: () => this.fileInput.click() }, "Import a file"),
        el("button", { class: "btn", onclick: () => this.createSheet() }, "New blank sheet"),
        el("button", { class: "btn", onclick: () => this.createDemo(1_000_000) }, "Generate 1M demo rows"),
      ),
    );
    this.gridWrap.querySelector(".empty")?.remove();
    this.gridWrap.append(card);
  }

  private busy(message: string | null): void {
    this.busyEl.hidden = message === null;
    this.busyEl.textContent = message ?? "";
  }

  // ------------------------------------------------------------ tabs/items

  private async loadItems(): Promise<void> {
    [this.tables, this.sheets] = await Promise.all([
      this.rpc.call("tables"),
      this.rpc.call("sheets"),
    ]);
    this.renderTabs();
  }

  private renderTabs(): void {
    const a = this.active;
    const tableTabs = this.tables.map((t) => {
      const on = a?.kind === "table" && a.name === t.name;
      return el("button", {
        class: `tab${on ? " active" : ""}`, role: "tab", "aria-selected": on ? "true" : "false",
        title: `Table · ${formatCount(t.row_count)} rows · ${t.column_count} columns`,
        onclick: () => this.openTable(t.name),
        oncontextmenu: (e: MouseEvent) => { e.preventDefault(); this.tableTabMenu(t, e.clientX, e.clientY); },
      }, el("span", { class: "tab-icon", "aria-hidden": "true" }, "▦"), t.name);
    });
    const sheetTabs = this.sheets.map((s) => {
      const on = a?.kind === "sheet" && a.index === s.index;
      return el("button", {
        class: `tab tab-sheet${on ? " active" : ""}`, role: "tab", "aria-selected": on ? "true" : "false",
        title: "Sheet (Excel formulas)",
        onclick: () => this.openSheet(s.index),
        oncontextmenu: (e: MouseEvent) => { e.preventDefault(); this.sheetTabMenu(s, e.clientX, e.clientY); },
      }, el("span", { class: "tab-icon", "aria-hidden": "true" }, "ƒ"), s.name);
    });
    const tabsHadFocus = this.tabsEl.contains(document.activeElement);
    this.tabsEl.replaceChildren(...tableTabs, ...sheetTabs);
    // ARIA tabs: only the active tab is in the Tab order; arrow keys move between tabs.
    const all = [...tableTabs, ...sheetTabs];
    all.forEach((t) => (t.tabIndex = t.classList.contains("active") || (!all.some((x) => x.classList.contains("active")) && t === all[0]) ? 0 : -1));
    if (tabsHadFocus) all.find((t) => t.tabIndex === 0)?.focus();
  }

  private closeTableView(): void {
    if (this.view) this.rpc.call("close_view", { view_id: this.view.view_id }).catch(() => {});
    this.view = null;
  }

  private async openTable(name: string): Promise<void> {
    this.gridWrap.querySelector(".empty")?.remove();
    this.closeTableView();
    this.active = { kind: "table", name };
    this.grid.setLabel(`Table ${name}`);
    this.spec = { table: name, sort: [], filters: [] };
    this.updateToolbar();
    this.busy("Opening…");
    try {
      const info = await this.rpc.call("open_view", { spec: this.spec });
      this.applyView(info, true);
      this.grid.scrollToTop();
      this.grid.select({ r: 0, c: 0 });
    } catch (e) {
      toast((e as Error).message, "error");
    } finally {
      this.busy(null);
    }
    this.renderTabs();
  }

  private async openSheet(index: number): Promise<void> {
    this.gridWrap.querySelector(".empty")?.remove();
    this.closeTableView();
    const info = this.sheets.find((s) => s.index === index);
    if (!info) return;
    this.active = { kind: "sheet", index, name: info.name };
    this.grid.setLabel(`Sheet ${info.name}`);
    this.applySheetInfo(info, true);
    this.renderChips();
    this.updateToolbar();
    this.renderTabs();
    this.grid.scrollToTop();
    this.grid.select({ r: 0, c: 0 });
  }

  private applySheetInfo(info: SheetInfo, reset: boolean): void {
    const rows = Math.max(SHEET_MIN_ROWS, info.rows + 200);
    const cols = Math.max(SHEET_MIN_COLS, info.columns + 6);
    const colsChanged = cols !== this.sheetDims.cols;
    this.sheetDims = { rows, cols };
    if (reset || colsChanged) {
      this.sheetCols = sheetColumns(cols);
      if (reset) this.sheetCache.reset(info.index, cols);
      else { this.sheetCache.columns = cols; this.sheetCache.refresh(); }
      const saved = this.loadWidths();
      this.grid.setColumns(this.sheetCols.map((c) => saved[c.name] ?? SHEET_COL_WIDTH));
    }
    this.rowsEl.textContent = info.rows ? `Used ${columnLetter(Math.max(0, info.columns - 1))}${info.rows}` : "Empty sheet";
    this.setKind("sheet");
    this.grid.invalidate();
  }

  private async refreshSheet(): Promise<void> {
    if (this.active?.kind !== "sheet") return;
    const index = this.active.index;
    try {
      this.sheets = await this.rpc.call("sheets");
      const info = this.sheets.find((s) => s.index === index);
      if (!info) return;
      this.applySheetInfo(info, false);
      this.sheetCache.refresh();
      this.grid.invalidate();
    } catch (e) {
      toast((e as Error).message, "error");
    }
  }

  private applyView(info: ViewInfo, reset: boolean): void {
    this.view = info;
    this.spec = info.spec;
    if (reset) {
      this.cache.reset(info.view_id);
    } else {
      this.cache.viewId = info.view_id;
      this.cache.refresh();
    }
    const saved = this.loadWidths();
    this.grid.setColumns(info.columns.map((c) => saved[c.name] ?? defaultWidth(c)));
    this.rowsEl.textContent = `${formatCount(info.row_count)} rows`;
    this.setKind("table");
    this.renderChips();
    this.grid.invalidate();
    this.onSelection(this.grid.selection(), this.grid.active);
  }

  private async reopen(): Promise<void> {
    try {
      await this.loadItems();
      if (this.active?.kind === "table") {
        const info = await this.rpc.call("open_view", { spec: this.spec });
        this.applyView(info, true);
      } else if (this.active?.kind === "sheet") {
        await this.refreshSheet();
      }
    } catch (e) {
      toast((e as Error).message, "error");
    }
  }

  private async setSpec(spec: ViewSpec): Promise<void> {
    if (!this.view) return;
    this.busy(spec.sort.length ? "Sorting…" : "Filtering…");
    const started = performance.now();
    try {
      const info = await this.rpc.call("update_view", { view_id: this.view.view_id, spec });
      this.applyView(info, true);
      this.grid.scrollToTop();
      this.grid.select({ r: 0, c: this.grid.active.c });
      const ms = performance.now() - started;
      if (ms > 400) toast(`${formatCount(info.row_count)} rows in ${(ms / 1000).toFixed(1)}s`);
    } catch (e) {
      toast((e as Error).message, "error");
    } finally {
      this.busy(null);
    }
  }

  private renderChips(): void {
    const chips: HTMLElement[] = [];
    if (!this.isSheet()) {
      this.spec.sort.forEach((s, i) => chips.push(
        el("span", { class: "chip" }, `Sort: ${s.column} ${s.desc ? "↓" : "↑"}`,
          el("button", { "aria-label": "Remove sort", onclick: () => this.setSpec({ ...this.spec, sort: this.spec.sort.filter((_, j) => j !== i) }) }, "×")),
      ));
      this.spec.filters.forEach((f, i) => chips.push(
        el("span", { class: "chip chip-filter" }, describeFilter(f),
          el("button", { "aria-label": "Remove filter", onclick: () => this.setSpec({ ...this.spec, filters: this.spec.filters.filter((_, j) => j !== i) }) }, "×")),
      ));
      if (chips.length > 1) {
        chips.push(el("button", { class: "chip-clear", onclick: () => this.setSpec({ ...this.spec, sort: [], filters: [] }) }, "Clear all"));
      }
    }
    this.chips.replaceChildren(...chips);
    this.chips.hidden = chips.length === 0;
  }

  // -------------------------------------------------------------- selection

  private onSelection(range: Range, active: Cell): void {
    clearTimeout(this.statsTimer);
    this.statsEl.textContent = "";
    if (this.isSheet()) {
      this.cellRef.textContent = range.r0 === range.r1 && range.c0 === range.c1
        ? `${columnLetter(active.c)}${active.r + 1}`
        : `${columnLetter(range.c0)}${range.r0 + 1}:${columnLetter(range.c1)}${range.r1 + 1}`;
      this.formulaInput.title = "";
      this.refreshFormulaValue();
      this.sheetSelectionStats(range);
      return;
    }
    const col = this.view?.columns[active.c];
    if (!col) {
      this.cellRef.textContent = "";
      this.formulaInput.value = "";
      return;
    }
    this.cellRef.textContent = `${col.name} · ${formatCount(active.r + 1)}`;
    if (col.formula) {
      this.formulaInput.value = col.formula.startsWith("=") ? col.formula : `=${col.formula}`;
      this.formulaInput.title = "Formula column: editing this changes every row";
    } else {
      this.refreshFormulaValue();
      this.formulaInput.title = "";
    }
    const rows = range.r1 - range.r0 + 1;
    if (rows < 2 || !this.view) return;
    const seq = ++this.statsSeq;
    this.statsTimer = window.setTimeout(async () => {
      try {
        const s = await this.rpc.call("stats", { view_id: this.view!.view_id, column: col.name, start: range.r0, end: range.r1 + 1 });
        if (seq !== this.statsSeq) return;
        const parts = [`Count ${formatCount(s.count)}`];
        if (isNumericKind(col.kind) && s.count) {
          parts.unshift(`Sum ${formatStat(s.sum)}`, `Avg ${formatStat(s.avg)}`);
          parts.push(`Min ${formatStat(s.min)}`, `Max ${formatStat(s.max)}`);
        }
        this.statsEl.textContent = `${col.name}: ${parts.join("  ·  ")}`;
      } catch { /* selection moved on */ }
    }, 250);
  }

  /** Sum/avg/count of the selected sheet cells that are already on screen. */
  private sheetSelectionStats(range: Range): void {
    const cells = (range.r1 - range.r0 + 1) * (range.c1 - range.c0 + 1);
    if (cells < 2 || cells > 200_000) return;
    let count = 0, numbers = 0, sum = 0;
    for (let r = range.r0; r <= range.r1; r++) {
      for (let c = range.c0; c <= range.c1; c++) {
        const cell = this.sheetCache.cell(r, c);
        if (!cell) continue;
        count++;
        const n = Number(cell[0].replace(/[, $€£%]/g, ""));
        if (cell[1] === "right" && Number.isFinite(n)) { numbers++; sum += n; }
      }
    }
    if (!count) return;
    const parts = [`Count ${formatCount(count)}`];
    if (numbers) parts.unshift(`Sum ${formatStat(sum)}`, `Avg ${formatStat(sum / numbers)}`);
    this.statsEl.textContent = parts.join("  ·  ");
  }

  private refreshFormulaValue(): void {
    if (this.isSheet()) {
      const cell = this.sheetCache.cell(this.grid.active.r, this.grid.active.c);
      this.formulaInput.value = cell ? cell[2] ?? cell[0] : "";
      return;
    }
    const col = this.view?.columns[this.grid.active.c];
    if (!col || col.formula) return;
    const row = this.cache.row(this.grid.active.r);
    this.formulaInput.value = row ? rawText(row[this.grid.active.c]) : "";
  }

  // ------------------------------------------------------------------ edits

  private async commitEdits(edits: { r: number; c: number; value: string | null }[]): Promise<boolean> {
    if (!edits.length) return true;
    if (this.active?.kind === "sheet") return this.commitSheetEdits(this.active.index, edits);
    if (!this.view) return true;
    const cols = this.view.columns;
    for (const e of edits) {
      const kind = cols[e.c].kind;
      const num = e.value !== null && isNumericKind(kind) ? Number(e.value) : NaN;
      this.cache.patch(e.r, e.c, e.value === null ? null : Number.isFinite(num) ? num : e.value);
    }
    this.grid.invalidate();
    try {
      const out = await this.rpc.call("edit", {
        view_id: this.view.view_id,
        edits: edits.map((e) => ({ row: e.r, column: cols[e.c].name, value: e.value })),
      });
      if (out.rows_appended > 0) {
        this.view = await this.rpc.call("view_info", { view_id: this.view.view_id });
        this.rowsEl.textContent = `${formatCount(this.view.row_count)} rows`;
      }
      this.cache.refresh();
      this.grid.invalidate();
      return true;
    } catch (e) {
      toast((e as Error).message, "error");
      this.cache.refresh();
      this.grid.invalidate();
      return false;
    }
  }

  private async commitSheetEdits(sheet: number, edits: { r: number; c: number; value: string | null }[]): Promise<boolean> {
    for (const e of edits) this.sheetCache.patch(e.r, e.c, e.value ?? "");
    this.grid.invalidate();
    try {
      const info = await this.rpc.call("sheet_set", {
        sheet, inputs: edits.map((e) => ({ row: e.r, column: e.c, value: e.value ?? "" })),
      });
      this.sheets = this.sheets.map((s) => (s.index === info.index ? info : s));
      this.applySheetInfo(info, false);
      this.sheetCache.refresh();
      this.grid.invalidate();
      return true;
    } catch (e) {
      toast((e as Error).message, "error");
      this.sheetCache.refresh();
      this.grid.invalidate();
      return false;
    }
  }

  private async undo(redo: boolean): Promise<void> {
    if (!this.isSheet()) {
      toast("Undo is available in sheets. Table undo is on the roadmap.");
      return;
    }
    try {
      const changed = await this.rpc.call(redo ? "sheet_redo" : "sheet_undo");
      if (!changed) toast(redo ? "Nothing to redo" : "Nothing to undo");
      await this.refreshSheet();
    } catch (e) {
      toast((e as Error).message, "error");
    }
  }

  private async commitFormulaBar(): Promise<void> {
    const value = this.formulaInput.value;
    const { r, c } = this.grid.active;
    if (this.isSheet()) {
      await this.commitEdits([{ r, c, value: value === "" ? null : value }]);
      this.grid.focus();
      return;
    }
    const col = this.view?.columns[c];
    if (!col || !this.view) return;
    if (col.formula) {
      try {
        await this.rpc.call("update_formula", { table: this.spec.table, name: col.name, formula: value });
        toast(`Updated formula for ${col.name}`, "success");
      } catch (e) {
        toast((e as Error).message, "error");
      }
    } else {
      await this.commitEdits([{ r, c, value: value === "" ? null : value }]);
    }
    this.grid.focus();
  }

  private async fetchRange(range: Range): Promise<CellValue[][]> {
    const out: CellValue[][] = [];
    for (let start = range.r0; start <= range.r1; start += FETCH_LIMIT) {
      const count = Math.min(FETCH_LIMIT, range.r1 - start + 1);
      if (this.active?.kind === "sheet") {
        const block = await this.rpc.call("sheet_fetch", {
          sheet: this.active.index, row_start: start, row_count: count, column_count: range.c1 + 1,
        });
        for (let i = 0; i < count; i++) {
          const row = block.rows[i] ?? [];
          out.push(Array.from({ length: range.c1 + 1 }, (_, c) => row[c]?.[0] ?? null));
        }
      } else {
        const block = await this.rpc.call("fetch", { view_id: this.view!.view_id, row_start: start, row_count: count });
        out.push(...block.rows);
      }
    }
    return out;
  }

  private async copy(range: Range): Promise<void> {
    const last = this.isSheet()
      ? Math.min(range.r1, Math.max(0, (this.sheets.find((s) => this.active?.kind === "sheet" && s.index === this.active.index)?.rows ?? 1) - 1))
      : Math.min(range.r1, (this.view?.row_count ?? 1) - 1);
    const r1 = Math.max(range.r0, last);
    const rows = r1 - range.r0 + 1;
    if (rows > 200_000) {
      toast("That's a lot of rows for the clipboard — use Export instead", "error");
      return;
    }
    try {
      const data = await this.fetchRange({ ...range, r1 });
      const text = data.map((row) => row.slice(range.c0, range.c1 + 1).map(tsvCell).join("\t")).join("\n");
      await navigator.clipboard.writeText(text);
      toast(`Copied ${formatCount(rows * (range.c1 - range.c0 + 1))} cells`);
    } catch (e) {
      toast(`Copy failed: ${(e as Error).message}`, "error");
    }
  }

  private async paste(at: Cell, text: string): Promise<void> {
    const rows = parseTsv(text.replace(/\n$/, ""));
    const sheet = this.isSheet();
    const cols = this.view?.columns ?? [];
    const edits: { r: number; c: number; value: string | null }[] = [];
    rows.forEach((row, i) => row.forEach((value, j) => {
      const c = at.c + j;
      if (sheet || (c < cols.length && !cols[c].formula)) edits.push({ r: at.r + i, c, value: value === "" ? null : value });
    }));
    if (edits.length > 500_000) { toast("Paste is too large (max 500,000 cells)", "error"); return; }
    if (await this.commitEdits(edits)) toast(`Pasted ${formatCount(edits.length)} cells`, "success");
  }

  // ------------------------------------------------------------------ menus

  private async sheetOp(
    op: "sheet_insert_rows" | "sheet_delete_rows" | "sheet_insert_columns" | "sheet_delete_columns",
    params: { at: number; count: number },
  ): Promise<void> {
    if (this.active?.kind !== "sheet") return;
    try {
      await this.rpc.call(op, { sheet: this.active.index, ...params });
      await this.refreshSheet();
    } catch (e) {
      toast((e as Error).message, "error");
    }
  }

  private headerMenu(c: number, x: number, y: number): void {
    if (this.isSheet()) {
      const sel = this.grid.selection();
      const count = Math.max(1, sel.c1 - sel.c0 + 1);
      showMenu([
        { label: "Insert column left", action: () => this.sheetOp("sheet_insert_columns", { at: c, count: 1 }) },
        { label: "Insert column right", action: () => this.sheetOp("sheet_insert_columns", { at: c + 1, count: 1 }) },
        "sep",
        { label: count > 1 ? `Delete ${count} columns` : "Delete column", danger: true,
          action: () => this.sheetOp("sheet_delete_columns", { at: sel.c0, count }) },
      ], x, y);
      return;
    }
    const col = this.view?.columns[c];
    if (!col) return;
    if (x < 0) { // keyboard edit on a formula column
      this.editFormulaDialog(col);
      return;
    }
    const activeRow = this.cache.row(this.grid.active.r);
    const activeValue = activeRow && this.grid.active.c === c ? activeRow[c] : undefined;
    const items: MenuItem[] = [
      { label: "Sort ascending", hint: "A→Z", action: () => this.setSpec({ ...this.spec, sort: [{ column: col.name, desc: false }] }) },
      { label: "Sort descending", hint: "Z→A", action: () => this.setSpec({ ...this.spec, sort: [{ column: col.name, desc: true }] }) },
      { label: "Then sort by this", disabled: !this.spec.sort.length || this.spec.sort.some((s) => s.column === col.name),
        action: () => this.setSpec({ ...this.spec, sort: [...this.spec.sort, { column: col.name, desc: false }] }) },
      "sep",
      { label: "Filter…", action: () => this.filterDialog(c) },
    ];
    if (activeValue !== undefined && activeValue !== null) {
      const shown = formatValue(activeValue, col.kind);
      items.push({ label: `Only “${shown.length > 24 ? shown.slice(0, 24) + "…" : shown}”`,
        action: () => this.setSpec({ ...this.spec, filters: [...this.spec.filters, { column: col.name, op: "eq", value: rawText(activeValue) }] }) });
    }
    if (this.spec.filters.some((f) => f.column === col.name)) {
      items.push({ label: "Clear filters on this column", action: () => this.setSpec({ ...this.spec, filters: this.spec.filters.filter((f) => f.column !== col.name) }) });
    }
    items.push(
      "sep",
      { label: "Column summary", action: () => this.columnSummary(col) },
      { label: "Add formula column…", action: () => this.addColumnDialog("formula") },
    );
    if (col.formula) items.push({ label: "Edit formula…", action: () => this.editFormulaDialog(col) });
    items.push(
      { label: "Rename…", action: () => this.renameColumn(col) },
      "sep",
      { label: "Delete column", danger: true, action: () => this.deleteColumn(col) },
    );
    showMenu(items, x, y);
  }

  private rowMenu(r: number, x: number, y: number): void {
    const sel = this.grid.selection();
    if (this.isSheet()) {
      const count = Math.max(1, sel.r1 - sel.r0 + 1);
      showMenu([
        { label: "Copy", hint: "⌘C", action: () => this.copy(sel) },
        "sep",
        { label: "Insert row above", action: () => this.sheetOp("sheet_insert_rows", { at: r, count: 1 }) },
        { label: "Insert row below", action: () => this.sheetOp("sheet_insert_rows", { at: r + 1, count: 1 }) },
        { label: count > 1 ? `Delete ${count} rows` : "Delete row", danger: true,
          action: () => this.sheetOp("sheet_delete_rows", { at: sel.r0, count }) },
      ], x, y);
      return;
    }
    if (!this.view) return;
    const count = Math.max(1, Math.min(sel.r1, this.view.row_count - 1) - sel.r0 + 1);
    const identity = this.view.can_append;
    const run = async (op: "insert_rows" | "delete_rows", params: { at: number; count: number } | { start: number; count: number }) => {
      try {
        if (op === "insert_rows" && "at" in params) await this.rpc.call(op, { view_id: this.view!.view_id, ...params });
        else if (op === "delete_rows" && "start" in params) await this.rpc.call(op, { view_id: this.view!.view_id, ...params });
      } catch (e) {
        toast((e as Error).message, "error");
      }
    };
    showMenu([
      { label: "Copy", hint: "⌘C", action: () => this.copy(sel) },
      "sep",
      { label: "Insert row above", disabled: !identity, action: () => run("insert_rows", { at: r, count: 1 }) },
      { label: "Insert row below", disabled: !identity, action: () => run("insert_rows", { at: r + 1, count: 1 }) },
      { label: count > 1 ? `Delete ${formatCount(count)} rows` : "Delete row", danger: true,
        disabled: r >= this.view.row_count, action: () => run("delete_rows", { start: sel.r0, count }) },
    ], x, y);
  }

  private tableTabMenu(t: TableInfo, x: number, y: number): void {
    showMenu([
      { label: "Rename…", action: async () => {
        const v = await formDialog("Rename table", [{ name: "name", label: "Name", value: t.name }], "Rename");
        if (!v) return;
        try {
          const name = await this.rpc.call("rename_table", { table: t.name, new_name: v.name });
          await this.loadItems();
          if (this.active?.kind === "table" && this.active.name === t.name) await this.openTable(name);
        } catch (e) { toast((e as Error).message, "error"); }
      } },
      { label: "Export CSV", action: () => this.download({ table: t.name, sort: [], filters: [] }, "csv") },
      { label: "Copy to a sheet…", action: () => this.copyTableToSheet(t) },
      "sep",
      { label: "Delete table", danger: true, action: async () => {
        if (!(await confirmDialog(`Delete “${t.name}”?`, `${formatCount(t.row_count)} rows will be permanently removed from this workbook.`))) return;
        try {
          await this.rpc.call("drop_table", { table: t.name });
        } catch (e) { toast((e as Error).message, "error"); }
      } },
    ], x, y);
  }

  private sheetTabMenu(s: SheetInfo, x: number, y: number): void {
    showMenu([
      { label: "Rename…", action: async () => {
        const v = await formDialog("Rename sheet", [{ name: "name", label: "Name", value: s.name }], "Rename");
        if (!v) return;
        try {
          await this.rpc.call("sheet_rename", { sheet: s.index, name: v.name });
          await this.loadItems();
          if (this.active?.kind === "sheet" && this.active.index === s.index) this.active.name = v.name;
        } catch (e) { toast((e as Error).message, "error"); }
      } },
      { label: "Convert to table", hint: "first row = header", action: async () => {
        try {
          const info = await this.rpc.call("sheet_to_table", { sheet: s.index, name: s.name });
          await this.loadItems();
          await this.openTable(info.name);
          toast(`Created table ${info.name} (${formatCount(info.row_count)} rows)`, "success");
        } catch (e) { toast((e as Error).message, "error"); }
      } },
      { label: "Download all sheets (.xlsx)", action: () => this.downloadXlsx() },
      "sep",
      { label: "Delete sheet", danger: true, action: async () => {
        if (!(await confirmDialog(`Delete sheet “${s.name}”?`, "Its cells and formulas will be removed. Formulas in other sheets that point at it will show #REF!."))) return;
        try {
          await this.rpc.call("sheet_delete", { sheet: s.index });
        } catch (e) { toast((e as Error).message, "error"); }
      } },
    ], x, y);
  }

  private newItemMenu(e: MouseEvent): void {
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    showMenu([
      { label: "New sheet", hint: "Excel formulas", action: () => this.createSheet() },
      { label: "Import file…", hint: "CSV · Parquet · JSON · XLSX", action: () => this.fileInput.click() },
      { label: "From SQL query…", disabled: !this.allowSql, action: () => this.sqlPanel() },
      "sep",
      { label: "Demo data: 1M rows", action: () => this.createDemo(1_000_000) },
      { label: "Demo data: 10M rows", action: () => this.createDemo(10_000_000) },
      { label: "Demo data: 100M rows", action: () => this.createDemo(100_000_000) },
    ], r.left, r.top - 260);
  }

  private exportMenu(e: MouseEvent): void {
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    if (this.isSheet()) {
      showMenu([{ label: "Excel workbook (.xlsx), all sheets", action: () => this.downloadXlsx() }], r.left, r.bottom + 4);
      return;
    }
    if (!this.view) return;
    const filtered = this.spec.sort.length || this.spec.filters.length ? " (current view)" : "";
    const items: MenuItem[] = [
      { label: `CSV${filtered}`, action: () => this.download(this.spec, "csv") },
      { label: `Parquet${filtered}`, action: () => this.download(this.spec, "parquet") },
      { label: `JSON lines${filtered}`, action: () => this.download(this.spec, "json") },
    ];
    if (this.sheets.length) items.push("sep", { label: "Sheets as .xlsx", action: () => this.downloadXlsx() });
    showMenu(items, r.left, r.bottom + 4);
  }

  private download(spec: ViewSpec, format: string): void {
    this.navigateDownload(api.exportUrl(this.meta.id, spec, format));
  }

  private downloadXlsx(): void {
    this.navigateDownload(api.xlsxUrl(this.meta.id));
  }

  private navigateDownload(href: string): void {
    const a = el("a", { href, download: "" });
    document.body.append(a);
    a.click();
    a.remove();
    toast("Preparing download…");
  }

  // ---------------------------------------------------------------- dialogs

  private async createSheet(): Promise<void> {
    try {
      const info = await this.rpc.call("sheet_create", { name: `Sheet ${this.sheets.length + 1}` });
      await this.loadItems();
      await this.openSheet(info.index);
      this.grid.focus();
    } catch (e) {
      toast((e as Error).message, "error");
    }
  }

  private async filterDialog(c: number): Promise<void> {
    if (!this.view) return;
    const cols = this.view.columns;
    const v = await formDialog("Filter rows", [
      { name: "column", label: "Column", type: "select", value: cols[c]?.name, options: cols.map((x) => ({ value: x.name, label: x.name })) },
      { name: "op", label: "Condition", type: "select", options: FILTER_OPS.map((o) => ({ value: o.value, label: o.label })) },
      { name: "value", label: "Value", placeholder: "e.g. DL, 100, 2024-01-31" },
    ], "Apply filter");
    if (!v) return;
    await this.setSpec({ ...this.spec, filters: [...this.spec.filters, { column: v.column, op: v.op as FilterOp, value: v.value }] });
  }

  private async addColumnDialog(preset?: string): Promise<void> {
    if (!this.view) return;
    const example = this.view.columns.find((c) => isNumericKind(c.kind))?.name ?? this.view.columns[0]?.name ?? "column";
    const v = await formDialog("Add column", [
      { name: "name", label: "Name", placeholder: "new_column" },
      { name: "type", label: "Type", type: "select", value: preset ?? "formula", options: COLUMN_TYPES },
      { name: "formula", label: "Formula (for formula columns)", mono: true, placeholder: `=[${example}] * 2`,
        hint: "Any DuckDB expression. [Column] refers to a column, e.g. =round([fare_usd] / [distance_mi], 3), =upper([origin]) || '-' || [dest], =CASE WHEN [dep_delay_min] > 15 THEN 'late' ELSE 'on time' END" },
    ], "Add column", async (vals) => {
      if (!vals.name.trim()) return "Give the column a name";
      try {
        if (vals.type === "formula") {
          if (!vals.formula.trim()) return "Write a formula";
          await this.rpc.call("add_formula_column", { table: this.spec.table, name: vals.name.trim(), formula: vals.formula });
        } else {
          await this.rpc.call("add_column", { table: this.spec.table, name: vals.name.trim(), sql_type: vals.type });
        }
        return null;
      } catch (e) {
        return (e as Error).message;
      }
    });
    if (v) {
      toast(`Added column ${v.name}`, "success");
      this.later(() => this.grid.select({ r: this.grid.active.r, c: (this.view?.columns.length ?? 1) - 1 }), 300);
    }
  }

  private async editFormulaDialog(col: Column): Promise<void> {
    await formDialog(`Edit formula: ${col.name}`, [
      { name: "formula", label: "Formula", type: "textarea", mono: true, value: col.formula ?? "" },
    ], "Save", async (vals) => {
      try {
        await this.rpc.call("update_formula", { table: this.spec.table, name: col.name, formula: vals.formula });
        return null;
      } catch (e) {
        return (e as Error).message;
      }
    });
  }

  private async renameColumn(col: Column): Promise<void> {
    const v = await formDialog("Rename column", [{ name: "name", label: "Name", value: col.name }], "Rename", async (vals) => {
      try {
        await this.rpc.call("rename_column", { table: this.spec.table, name: col.name, new_name: vals.name.trim() });
        return null;
      } catch (e) {
        return (e as Error).message;
      }
    });
    if (v) {
      const widths = this.loadWidths();
      widths[v.name.trim()] = widths[col.name];
      this.storeWidths(widths);
    }
  }

  private async deleteColumn(col: Column): Promise<void> {
    if (!(await confirmDialog(`Delete column “${col.name}”?`, col.formula ? "The formula will be removed." : "Its values in every row will be permanently removed."))) return;
    try {
      await this.rpc.call("drop_column", { table: this.spec.table, name: col.name });
    } catch (e) {
      toast((e as Error).message, "error");
    }
  }

  private async columnSummary(col: Column): Promise<void> {
    if (!this.view) return;
    this.busy("Summarizing…");
    try {
      const s = await this.rpc.call("stats", { view_id: this.view.view_id, column: col.name, start: 0, end: this.view.row_count });
      const rows: [string, string][] = [
        ["Rows", formatCount(s.rows)],
        ["Non-empty", formatCount(s.count)],
        ["Empty", formatCount(s.rows - s.count)],
      ];
      if (isNumericKind(col.kind)) {
        rows.push(["Sum", formatStat(s.sum)], ["Average", formatStat(s.avg)], ["Min", formatStat(s.min)], ["Max", formatStat(s.max)]);
      } else {
        rows.push(["Min", String(s.min ?? "–")], ["Max", String(s.max ?? "–")]);
      }
      const closeBtn = el("button", { class: "btn btn-primary", onclick: () => modal.close() }, "Close");
      const modal = openModal(
        el("div", { class: "dialog" },
          el("h2", {}, col.name),
          el("table", { class: "kv" }, ...rows.map(([k, v]) => el("tr", {}, el("th", { scope: "row" }, k), el("td", {}, v)))),
          el("div", { class: "dialog-actions" }, closeBtn),
        ),
        () => modal.close(),
      );
      closeBtn.focus();
    } catch (e) {
      toast((e as Error).message, "error");
    } finally {
      this.busy(null);
    }
  }

  private sqlPanel(): void {
    if (!this.allowSql) { toast("SQL is disabled on this server", "error"); return; }
    const tableName = this.active?.kind === "table" ? this.active.name : this.tables[0]?.name;
    const t = tableName ? `"${tableName}"` : "my_table";
    const firstCol = this.active?.kind === "table" ? this.view?.columns.find((c) => c.kind === "text")?.name : undefined;
    const sample = firstCol
      ? `SELECT "${firstCol}", count(*) AS rows\nFROM ${t}\nGROUP BY 1\nORDER BY rows DESC`
      : `SELECT *\nFROM ${t}\nLIMIT 100`;
    const editor = el("textarea", { class: "sql-editor mono", spellcheck: false, rows: 8 });
    editor.value = sample;
    const results = el("div", { class: "sql-results" }, el("p", { class: "muted" },
      "Run a query to preview results. Any DuckDB SQL works: joins across tables, group-bys, window functions, read_parquet() on local files."));
    const status = el("span", { class: "muted" });
    const run = async () => {
      status.textContent = "Running…";
      const started = performance.now();
      try {
        const p = await this.rpc.call("query", { sql: editor.value, limit: 500 });
        status.textContent = `${p.rows.length}${p.truncated ? "+" : ""} rows · ${Math.round(performance.now() - started)} ms`;
        results.replaceChildren(el("table", { class: "result-table" },
          el("thead", {}, el("tr", {}, ...p.columns.map((c) => el("th", {}, c.name)))),
          el("tbody", {}, ...p.rows.map((row) => el("tr", {}, ...row.map((v, i) =>
            el("td", { class: isNumericKind(p.columns[i].kind) ? "num" : "" }, formatValue(v, p.columns[i].kind)))))),
        ));
      } catch (e) {
        status.textContent = "";
        results.replaceChildren(el("pre", { class: "sql-error" }, (e as Error).message));
      }
    };
    const save = async (target: "table" | "sheet") => {
      const v = await formDialog(target === "table" ? "Save result as a table" : "Open result in a new sheet",
        [{ name: "name", label: target === "table" ? "Table name" : "Sheet name", value: target === "table" ? "query_result" : "Query result",
          hint: target === "table"
            ? "Keeps every row of the result as a new table (a snapshot: it does not change when the source does)."
            : `Copies at most the first ${formatCount(this.sheetCopyRows)} rows (fewer for very wide results). Use “Save as table” to keep all of them.` }],
        target === "table" ? "Create table" : "Create sheet");
      if (!v) return;
      try {
        if (target === "table") {
          const info = await this.rpc.call("create_table_from_query", { name: v.name, sql: editor.value });
          panel.close();
          await this.loadItems();
          await this.openTable(info.name);
          toast(`Created ${info.name} (${formatCount(info.row_count)} rows)`, "success");
        } else {
          const copy = await this.rpc.call("create_sheet_from_query", { name: v.name, sql: editor.value });
          panel.close();
          await this.loadItems();
          await this.openSheet(copy.sheet.index);
          this.reportCopy(copy, "the query result");
        }
      } catch (e) {
        toast((e as Error).message, "error");
      }
    };
    editor.addEventListener("keydown", (e) => {
      if ((e.metaKey || e.ctrlKey) && e.key === "Enter") { e.preventDefault(); run(); }
    });
    const panel = openModal(
      el("div", { class: "dialog dialog-wide" },
        el("h2", {}, "SQL"),
        editor,
        el("div", { class: "dialog-actions" },
          status,
          el("div", { class: "spacer" }),
          el("button", { class: "btn", onclick: () => panel.close() }, "Close"),
          el("button", { class: "btn", onclick: () => save("sheet"), title: `Up to ${formatCount(this.sheetCopyRows)} rows` }, "Open in sheet"),
          el("button", { class: "btn", onclick: () => save("table") }, "Save as table"),
          el("button", { class: "btn btn-primary", onclick: run, title: "⌘/Ctrl + Enter" }, "Run"),
        ),
        results,
      ),
      () => panel.close(),
    );
    editor.focus();
  }

  private async renameWorkbook(): Promise<void> {
    const v = await formDialog("Rename workbook", [{ name: "name", label: "Name", value: this.meta.name }], "Rename");
    if (!v) return;
    this.meta = await api.rename(this.meta.id, v.name);
    this.titleEl.textContent = this.meta.name;
    document.title = `${this.meta.name} · Rowfinity`;
  }

  // ----------------------------------------------------------- import/demo

  private async importFiles(files: File[]): Promise<void> {
    let last: Imported | null = null;
    for (const file of files) {
      try {
        const result = await importFile(this.meta.id, file);
        if (!result) continue;
        last = result;
        toast(result.kind === "table"
          ? `Imported ${file.name}: ${formatCount(result.table.row_count)} rows`
          : `Imported ${file.name}: ${result.sheets.length} sheet${result.sheets.length === 1 ? "" : "s"}`, "success");
      } catch (e) {
        toast(`${file.name}: ${(e as Error).message}`, "error");
      }
    }
    if (this.disposed) return;
    await this.loadItems();
    if (last?.kind === "table") await this.openTable(last.table.name);
    else if (last?.kind === "sheets" && last.sheets.length) await this.openSheet(last.sheets[0].index);
  }

  private async createDemo(rows: number): Promise<void> {
    this.busy(`Generating ${formatCount(rows)} rows…`);
    const started = performance.now();
    try {
      const info = await this.rpc.call("create_demo_table", { rows });
      await this.loadItems();
      await this.openTable(info.name);
      toast(`Generated ${formatCount(rows)} rows in ${((performance.now() - started) / 1000).toFixed(1)}s`, "success");
    } catch (e) {
      toast((e as Error).message, "error");
    } finally {
      this.busy(null);
    }
  }

  // ---------------------------------------------------------------- events

  private async onEvent(e: ServerEvent): Promise<void> {
    const a = this.active;
    switch (e.event) {
      case "cells_changed":
        if (a?.kind === "table" && e.table === a.name && e.origin !== this.rpc.session) {
          this.cache.refresh();
          this.grid.invalidate();
        }
        break;
      case "table_changed":
        if (a?.kind === "table" && e.table === a.name && this.view) {
          try {
            const info = await this.rpc.call("view_info", { view_id: this.view.view_id });
            this.applyView(info, false);
          } catch { await this.reopen(); }
        }
        this.loadItems().catch(() => {});
        break;
      case "sheet_changed":
        if (a?.kind === "sheet" && e.origin !== this.rpc.session && (e.sheet === null || e.sheet === a.index)) {
          await this.refreshSheet();
        }
        break;
      case "workbook_deleted":
        this.rpc.close();
        toast(`“${this.meta.name}” was moved to the trash. Restore it from Trash on the home screen.`, "error");
        this.goHome();
        break;
      case "tables_changed": {
        await this.loadItems();
        const gone = (a?.kind === "table" && !this.tables.some((t) => t.name === a.name))
          || (a?.kind === "sheet" && !this.sheets.some((s) => s.index === a.index && s.name === a.name));
        if (!a || gone) {
          if (this.tables.length) await this.openTable(this.tables[0].name);
          else if (this.sheets.length) await this.openSheet(this.sheets[0].index);
          else this.renderEmpty();
        }
        break;
      }
    }
  }

  // ---------------------------------------------------------------- widths

  private widthKey(): string {
    const a = this.active;
    const item = a?.kind === "sheet" ? `sheet:${a.name}` : a?.kind === "table" ? a.name : "";
    return `rowfinity:widths:${this.meta.id}:${item}`;
  }

  private loadWidths(): Record<string, number> {
    try { return JSON.parse(localStorage.getItem(this.widthKey()) ?? "{}"); } catch { return {}; }
  }

  private storeWidths(w: Record<string, number>): void {
    try { localStorage.setItem(this.widthKey(), JSON.stringify(w)); } catch { /* ignore */ }
  }

  private saveWidths(): void {
    const cols = this.isSheet() ? this.sheetCols : this.view?.columns;
    if (!cols) return;
    const widths = this.grid.columnWidths();
    const out: Record<string, number> = {};
    cols.forEach((c, i) => (out[c.name] = widths[i]));
    this.storeWidths(out);
  }
}
