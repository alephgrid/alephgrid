import { formulaHelp } from "./catalog";
import { applyReferenceLock, referenceInsertion, referenceRanges } from "./references";
import { goToDialog, namesDialog, parseAddress, quoteSheet, type Address, type NamesContext } from "./names";
import { CommandRegistry, commandPalette, shortcutHelp } from "./commands";
import { workbookCommands } from "./workbookcommands";
import type { PasteMode } from "./protocol";
import { displayValue, cellColor, emptyPresentation, presentationDialog } from "./presentation";
import { AnalysisWorkspace, dashboardsDialog, replaceDialog } from "./analysis";
import { dataToolsDialog } from "./tools";
import type { Validation } from "./protocol";
import { functionCompletion, functionLibrary } from "./functions";
import { api, HttpError, isRuntimeFailure, Rpc, RpcError } from "./api";
import { importFile } from "./importer";
import { historyDialog } from "./history";
import { computeDialog, pausedText, profileLabel, waitForCompute } from "./compute";
import type { RuntimeStatus } from "./protocol";

import { cellKey, FETCH_LIMIT, RowCache, SheetCache } from "./cache";
import { columnLetter, formatCount, formatStat, formatValue, isNumericKind, rawText } from "./format";
import { Grid, type Cell, type GridEdit, type Range } from "./grid";
import type {
  CellValue, Column, Filter, FilterOp, Imported, SavedView, ServerEvent, SheetCopy, SheetInfo, TableInfo, ViewInfo, ViewSpec, WorkbookMeta,
} from "./types";
import type { MaterializedTable, SavedQuery, WorkbookFunction } from "./protocol";
import { quoteIdent } from "./sqltext";
import { SqlWorkspace } from "./sqlworkspace";
import { confirmDialog, el, formDialog, openModal, showMenu, toast, type MenuItem } from "./ui";
import { brandMark, icon, type IconName } from "./icons";
import { dockPanel } from "./dock";

/** "1.2M rows" for the explorer. */
function compactRows(n: number): string {
  return `${new Intl.NumberFormat(undefined, { notation: "compact", maximumFractionDigits: 1 }).format(n)} rows`;
}
import { setTheme, THEME_CHOICES, themeButton, themePreference } from "./theme";

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

/** The workbook no longer exists (or is in the trash): the server answers "not found". */
function isGone(e: unknown): boolean {
  return e instanceof HttpError && (e.status === 404 || e.status === 410 || (e.status === 400 && /not found/i.test(e.message)));
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
  private spec: ViewSpec = { table: "", sort: [], filters: [], hidden: [] };
  /** Saved views of the open table, and the one applied (if any). */
  private savedViews: SavedView[] = [];
  /** The saved-views request in flight, so the Views menu never shows a list that is still loading. */
  private savedViewsLoading: Promise<void> = Promise.resolve();
  private currentSaved: SavedView | null = null;
  private sheetDims = { rows: SHEET_MIN_ROWS, cols: SHEET_MIN_COLS };
  private sheetCols: Column[] = sheetColumns(SHEET_MIN_COLS);
  private allowSql = true;
  /** Rows copied into a sheet at most (from /api/config). */
  private sheetCopyRows = 100_000;
  /** Largest request frame and edit the server accepts (from /api/config). */
  private maxMessageBytes = 16 << 20;
  private maxEditCells = 1_000_000;

  private titleEl = el("button", { class: "wb-title", title: "Rename workbook" });
  /** The menu bar button whose menu is open, for submenus that reopen beside it. */
  private menuAnchor: HTMLElement = document.body;
  private themeBtn: HTMLButtonElement | null = null;
  /** Separates the table or sheet tools from the rest; hidden when neither is shown. */
  private groupSep: HTMLElement = document.createElement("span");
  private exportBtn: HTMLButtonElement | null = null;
  /** The workbook explorer (U.04): tables, sheets, saved queries and functions; collapsible. */
  private explorerEl = el("nav", { class: "workbook-explorer", "aria-label": "Workbook explorer" });
  private explorerBtn: HTMLButtonElement | null = null;
  private explorerOpen = (() => {
    try { const v = localStorage.getItem("alephgrid-explorer"); if (v) return v === "open"; } catch { /* default */ }
    return false; // collapsed until the person opens it (UI direction v2)
  })();
  private functionsList: WorkbookFunction[] = [];
  private functionsTimer = 0;
  /** The column that holds the formula bar, grid and docked panels. */
  private mainEl = el("div", { class: "workbook-main" });
  private statusDot = el("span", { class: "status-dot", title: "Connecting…" });
  private statusText = el("span", { class: "status-text" }, "Connecting…");
  private chips = el("div", { class: "chips" });
  private cellRef = el("div", { class: "cell-ref" });
  private formulaInput = el("input", { class: "formula-input", spellcheck: false, "aria-label": "Cell value or formula" });
  private referenceLegend = el("div", { class: "reference-legend", role: "region", "aria-label": "Formula references", hidden: true });
  private referencePickState: { base: string; start: number; end: number } | null = null;
  /** The sheet cell a formula-bar edit belongs to; picking may show another sheet meanwhile. */
  private formulaOrigin: { index: number; r: number; c: number } | null = null;
  private formulaProblem = el("p", { class: "formula-problem", role: "alert", hidden: true });
  private cellNote = el("p", { class: "cell-note", role: "status", hidden: true });
  /** Shown while a sorted or filtered view reads stored formula results that are stale. */
  private staleNote = el("p", { class: "stale-note", role: "status", hidden: true });
  private cellNoteSeq = 0;
  /** The cell the note was last looked up for, so data loads do not repeat the request. */
  private cellNoteKey = "";
  private closeNames: (() => void) | null = null;
  private closeDashboards: (() => void) | null = null;
  private closeTools: (() => void) | null = null;
  /** Validation rules of the open table (F.12). */
  private rules: Validation[] = [];
  private commands = new CommandRegistry();
  /** Cells last copied or cut in this workbook, so pasting their text pastes the cells
   *  (formulas, formats) rather than the displayed values. */
  /** Copied sheet cells, with the sheet's layout when copied (a paste is refused once it moved). */
  private clip: { sheet: number; range: Range; text: string; cut: boolean; layout?: number } | null = null;
  /** Sheet changes run one after another in the order the user made them: a formula's parse
   *  check must not let a later edit (or undo) reach the server first. */
  private sheetQueue: Promise<unknown> = Promise.resolve();

  private inSheetOrder<T>(task: () => Promise<T>): Promise<T> {
    const run = this.sheetQueue.then(task, task);
    this.sheetQueue = run.catch(() => undefined);
    return run;
  }
  /** Table edits, like sheet changes, are sent one after another in the user's order: an
   *  edit of a row the previous one appended then knows its record id. */
  private tableQueue: Promise<unknown> = Promise.resolve();
  private inTableOrder<T>(task: () => Promise<T>): Promise<T> {
    const run = this.tableQueue.then(task, task);
    this.tableQueue = run.catch(() => undefined);
    return run;
  }
  private tabsEl = el("div", { class: "tabs", role: "tablist" });
  private statsEl = el("div", { class: "stats" });
  private rowsEl = el("div", { class: "row-count" });
  /** Says whether the open item is a Table or a Sheet, and what that means. */
  private kindEl = el("span", { class: "kind-badge", hidden: true });
  private gridWrap = el("div", { class: "grid-wrap" });
  private busyEl = el("div", { class: "busy", hidden: true });
  /** Shown when the workbook cannot be opened (incompatible, held elsewhere, no capacity). */
  private failureEl = el("div", { class: "fatal workbook-failure", role: "alert", hidden: true });
  private fileInput!: HTMLInputElement;
  private tableButtons: HTMLButtonElement[] = [];
  private sheetButtons: HTMLElement[] = [];
  /** The open SQL workspace, if any (it receives server events). */
  private sql: SqlWorkspace | null = null;
  private analysis: AnalysisWorkspace | null = null;
  private presentation = emptyPresentation();
  private closePresentation: (() => void) | null = null;
  private closeReplace: (() => void) | null = null;
  /** Tables made from a query, and the saved queries they may name. */
  private materialized: MaterializedTable[] = [];
  private savedQueries: SavedQuery[] = [];
  private snapshotEl = el("div", { class: "snapshot-banner", role: "region", "aria-label": "Snapshot", hidden: true });
  private statsTimer = 0;
  private statsSeq = 0;
  private timers = new Set<number>();
  private disposed = false;
  /** The server is restarting this workbook's compute (a new size): say so once it is back. */
  private restarting = false;
  private closeFormulaHelp: (() => void) | null = null;
  private closeFunctions: (() => void) | null = null;
  private disposeFunctionCompletion: (() => void) | null = null;

  constructor(meta: WorkbookMeta, private goHome: () => void) {
    this.meta = meta;
    this.rpc = new Rpc(meta.id);
    const onLoaded = () => {
      this.grid.invalidate();
      if (document.activeElement !== this.formulaInput) this.refreshFormulaValue();
      // Sheet stats are computed from cached cells; recompute once fresh values arrive.
      if (this.isSheet()) { this.sheetSelectionStats(this.grid.selection()); this.showCellNote(false); }
    };
    const onError = (e: Error) => toast(e.message, "error");
    this.cache = new RowCache(this.rpc, onLoaded, onError);
    this.sheetCache = new SheetCache(this.rpc, onLoaded, onError);
    const self = this;
    this.grid = new Grid(
      {
        frozen: () => this.isSheet() ? 0 : this.presentation.frozen,
        format: (value, column) => displayValue(value, column, this.presentation.formats[column.name]),
        color: (value, column) => cellColor(value, column.name, this.presentation.rules),
        rowCount: () => (this.isSheet() ? this.sheetDims.rows : this.view?.row_count ?? 0),
        columns: () => (this.isSheet() ? this.sheetCols : this.view?.columns ?? []),
        canAppend: () => !this.isSheet() && (this.view?.can_append ?? false),
        choices: (column: string) => this.isSheet() ? [] : this.rules.filter(r => r.column === column && r.rule.kind === "one_of").flatMap(r => r.rule.kind === "one_of" ? r.rule.values : []),
        sort: () => (this.isSheet() ? [] : this.spec.sort),
        filters: () => (this.isSheet() ? [] : this.spec.filters),
        value: (r, c) => this.cache.value(r, c),
        ensure: (a, b, c0, c1) => (this.isSheet() ? this.sheetCache.ensure(a, b) : this.cache.ensure(a, b, c0, c1)),
        mode: () => (this.isSheet() ? "sheet" : "table"),
        cell: (r, c) => {
          const cell = this.sheetCache.cell(r, c);
          return cell ? { text: cell[0], align: cell[1] } : cell;
        },
        editText: (r, c) => {
          const cell = this.sheetCache.cell(r, c);
          return cell ? cell[2] ?? cell[0] : "";
        },
        base: (r, c) => this.baseOf(r, c),
      },
      {
        selectionChanged: (range, active) => self.onSelection(range, active),
        referencePick: (range, phase) => self.pickReference(range, phase),
        commitEdits: (edits) => self.commitEdits(edits),
        headerMenu: (c, x, y) => self.headerMenu(c, x, y),
        rowMenu: (r, x, y) => self.rowMenu(r, x, y),
        copy: (range) => self.copy(range),
        paste: (at, text) => self.paste(at, text),
        columnResized: () => self.saveWidths(),
        undo: () => self.undo(false),
        redo: () => self.undo(true),
        command: (e) => self.commands.handle(e, "grid", (m) => toast(m)),
      },
    );
    this.registerCommands();
    this.gridWrap.append(this.grid.host, this.busyEl, this.failureEl);
    this.root = this.render();
    this.titleEl.textContent = meta.name;
    this.rpc.onSaveState = () => this.renderStatus();
    this.rpc.onStatus = (s) => {
      this.setStatus(this.rpc.isPaused && s !== "open" ? "paused" : s);
      if (s === "open") this.failureEl.hidden = true;
    };
    this.rpc.onEvent = (e) => this.onEvent(e);
    this.rpc.onReconnect = async () => {
      await this.reopen();
      if (this.restarting) {
        this.restarting = false;
        try {
          const s = await api.runtime(this.meta.id);
          toast(`Back, running with the ${profileLabel(s.allocation?.profile ?? s.profile)} profile.`, "success");
        } catch { /* the reconnect itself worked */ }
      }
    };
    this.rpc.onConnectFailed = () => this.checkFailure();
  }

  private isSheet(): boolean {
    return this.active?.kind === "sheet";
  }

  /** The top bar's theme button shows the current choice after the View menu changes it. */
  private syncThemeButton(): void {
    const next = themeButton();
    this.themeBtn?.replaceWith(next);
    this.themeBtn = next;
  }

  private connection: "connecting" | "open" | "closed" | "paused" = "connecting";

  private setStatus(s: "connecting" | "open" | "closed" | "paused"): void {
    this.connection = s;
    this.statusDot.className = `status-dot status-${s}`;
    this.statusDot.title = s === "open" ? "Connected" : s === "connecting" ? "Connecting…" : s === "paused" ? "Paused — resumes when you continue" : "Disconnected — retrying";
    this.renderStatus();
  }

  /** Connection and save state in words (U.11): "Saved" only once the server acknowledged every
   *  change this page sent, which it does after commit. */
  private renderStatus(): void {
    const s = this.connection;
    const save = this.rpc.saveState;
    const [text, tone, title] =
      save === "failed" ? ["Not saved", "failed", "A change was lost while the connection was down. Check the last edits and make them again."]
      : save === "unsaved" ? ["Unsaved changes", "unsaved", "Changes are kept and sent again when the connection is back."]
      : s === "paused" ? ["Paused", "paused", this.statusDot.title]
      : s === "connecting" ? ["Connecting…", "connecting", this.statusDot.title]
      : s === "closed" ? ["Reconnecting…", "closed", this.statusDot.title]
      : save === "saving" ? ["Saving…", "saving", "Sending changes to the server."]
      : ["Saved", "saved", "Every change is saved on the server."];
    this.statusText.textContent = text;
    this.statusText.title = title;
    this.statusText.dataset.state = tone;
  }

  /**
   * The server closed this workbook's compute after a time without activity (7.16): stop
   * reconnecting, say what is kept, and resume when the person continues.
   */
  private async pause(): Promise<void> {
    this.rpc.pause();
    this.setStatus("paused");
    let status: RuntimeStatus | null = null;
    try { status = await api.runtime(this.meta.id); } catch { /* say it without the numbers */ }
    if (this.disposed || !this.rpc.isPaused) return;
    const resume = (e?: Event) => {
      if (e instanceof KeyboardEvent && ["Shift", "Control", "Alt", "Meta"].includes(e.key)) return;
      this.root.removeEventListener("keydown", resume, true);
      this.root.removeEventListener("pointerdown", resume, true);
      void this.resume();
    };
    this.failureEl.replaceChildren(
      el("h2", {}, "This workbook is paused"),
      el("p", {}, pausedText(status)),
      el("div", { class: "empty-actions" }, el("button", { class: "btn btn-primary", onclick: () => resume() }, "Resume")),
    );
    this.failureEl.hidden = false;
    this.root.addEventListener("keydown", resume, true);
    this.root.addEventListener("pointerdown", resume, true);
  }

  private async resume(): Promise<void> {
    const ready = await waitForCompute(this.meta.id, this.failureEl, () => this.goHome(), () => !this.disposed);
    if (!ready || this.disposed) return;
    this.rpc.reconnectNow();
  }

  private leaveDeleted(): void {
    if (this.disposed || this.rpc.isClosed) return;
    this.rpc.close();
    toast(`“${this.meta.name}” was moved to the trash. Restore it from Trash on the home screen.`, "error");
    this.goHome();
  }

  /** True (and the screen is left) when the workbook is no longer there. */
  private async goneToTrash(): Promise<boolean> {
    // While a deletion is under way the server may still answer "unavailable"; ask again
    // for a little while until it says either way.
    for (let attempt = 0; attempt < 10 && !this.disposed; attempt++) {
      try {
        await api.get(this.meta.id);
        return false;
      } catch (e) {
        if (isGone(e)) {
          this.leaveDeleted();
          return true;
        }
        if (!isRuntimeFailure(e)) return false;
      }
      await new Promise((r) => setTimeout(r, 200));
    }
    return false;
  }

  /** A reconnect failed: if the server refuses to open the workbook, say why. */
  private async checkFailure(): Promise<void> {
    try {
      await api.get(this.meta.id);
    } catch (e) {
      if (this.disposed) return;
      if (isGone(e)) { this.leaveDeleted(); return; }
      if (!isRuntimeFailure(e)) return;
      this.failureEl.replaceChildren(
        el("h2", {}, "This workbook can't be opened right now"),
        el("p", {}, (e as Error).message),
        el("button", { class: "btn btn-primary", onclick: () => { this.failureEl.hidden = true; this.rpc.reconnectNow(); } }, "Try again"),
      );
      this.failureEl.hidden = false;
    }
  }

  async start(): Promise<void> {
    try {
      const config = await api.config();
      this.allowSql = config.allow_sql;
      this.sheetCopyRows = config.sheet_copy_rows ?? this.sheetCopyRows;
      this.maxMessageBytes = config.max_message_bytes ?? this.maxMessageBytes;
      this.maxEditCells = config.max_edit_cells ?? this.maxEditCells;
    } catch { /* keep default */ }
    try {
      await this.rpc.connect();
    } catch (e) {
      // Never connected: stop retrying (the caller shows what went wrong). The upgrade's
      // HTTP status is not visible to scripts, so ask the REST API why.
      this.rpc.close();
      await api.get(this.meta.id);
      throw e;
    }
    try {
      await this.loadItems();
    } catch (e) {
      // Deleted while connecting (before its event could reach this session)?
      if (await this.goneToTrash()) return;
      throw e;
    }
    const demo = Number(new URLSearchParams(location.search).get("demo"));
    if (demo > 0) history.replaceState(null, "", location.pathname);
    if (this.tables.length) await this.openTable(this.tables[0].name);
    else if (this.sheets.length) await this.openSheet(this.sheets[0].index);
    else if (demo > 0) await this.createDemo(demo);
    else this.renderEmpty();
    // Start on the grid, unless the user has already moved focus while it was loading.
    const focused = document.activeElement;
    if (!focused || focused === document.body) this.grid.focus();
  }

  /** Leave the screen: close the socket, drop pending replies, stop timers and listeners. */
  dispose(): void {
    this.disposed = true;
    this.closeFunctions?.();
    this.closeFormulaHelp?.();
    this.disposeFunctionCompletion?.();
    this.sql?.close();
    this.analysis?.close();
    this.closeReplace?.();
    this.closePresentation?.();
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
    this.disposeFunctionCompletion = functionCompletion(this.formulaInput, this.rpc, () => this.isSheet() ? "formula" : "sql");
    this.formulaInput.addEventListener("keydown", (e) => {
      if (e.key === "F4" && this.isSheet() && !e.isComposing) { e.preventDefault(); applyReferenceLock(this.formulaInput); }
      if (e.key === "Enter") { e.preventDefault(); this.commitFormulaBar(); }
      if (e.key === "Escape") { void this.cancelFormulaBar(); }
    });
    this.formulaInput.addEventListener("focus", () => this.markFormulaOrigin());
    this.formulaInput.addEventListener("input", () => { this.markFormulaOrigin(); this.updateReferenceHints(); });
    this.formulaInput.addEventListener("focus", () => this.updateReferenceHints());
    this.formulaInput.addEventListener("blur", () => {
      this.referencePickState = null;
      this.updateReferenceHints();
      // Leaving the formula bar while another sheet is shown for picking returns to the edit's sheet.
      const origin = this.formulaOrigin;
      this.formulaOrigin = null;
      if (origin && this.active?.kind === "sheet" && this.active.index !== origin.index) void this.returnToOrigin(origin);
    });
    this.fileInput = el("input", { type: "file", hidden: true, multiple: true, accept: IMPORT_ACCEPT,
      onchange: (e: Event) => {
        const files = (e.target as HTMLInputElement).files;
        if (files) this.importFiles([...files]);
        (e.target as HTMLInputElement).value = "";
      } });
    // Every command once; the menus, the toolbar and the top bar share them.
    const act = {
      formatTable: async () => { if (this.active?.kind !== "table") return; const name = this.active.name; this.closePresentation?.(); try { this.closePresentation = await presentationDialog(this.rpc, name, () => this.reloadPresentation(name)); } catch (e) { toast(String(e), "error"); } },
      findReplace: async () => { if (this.active?.kind !== "table") return; this.closeReplace?.(); try { this.closeReplace = await replaceDialog(this.rpc, this.active.name, this.meta.id); } catch (e) { toast(String(e), "error"); } },
      addColumn: () => this.addColumnDialog(),
      filter: () => this.filterDialog(this.grid.active.c),
      names: async () => { this.closeNames?.(); try { this.closeNames = await namesDialog(this.namesContext()); } catch (e) { toast((e as Error).message, "error"); } },
      goTo: () => void this.goTo(),
      importFile: () => this.fileInput.click(),
      analyze: () => { this.analysis?.close(); this.analysis = new AnalysisWorkspace(this.rpc, async name => { await this.loadItems(); await this.openTable(name); },
        async spec => { await this.loadItems(); await this.openTable(spec.table); await this.setSpec(spec); }); },
      dashboards: () => { this.closeDashboards?.(); this.closeDashboards = dashboardsDialog(this.rpc); },
      dataTools: () => { if (this.active?.kind !== "table") { toast("Open a table first", "error"); return; } this.closeTools?.(); this.closeTools = dataToolsDialog(this.rpc, this.active.name, this.view?.columns ?? [], async name => { await this.loadItems(); await this.openTable(name); }); },
      sql: () => this.sqlPanel(),
      functions: () => { this.closeFunctions?.(); this.closeFunctions = functionLibrary(this.rpc, this.allowSql); },
      formulaHelp: () => { this.closeFormulaHelp?.(); this.closeFormulaHelp = formulaHelp(); },
      commands: () => { commandPalette(this.commands); },
      shortcuts: () => { shortcutHelp(this.commands); },
      history: () => historyDialog(this.rpc, () => void this.resync()),
      compute: () => computeDialog(this.meta.id),
    };
    const tool = (name: IconName, label: string, run: (e: MouseEvent) => void, o: { text?: string; title?: string; menu?: boolean } = {}) =>
      el("button", { class: `tool${o.text ? " tool-text" : ""}`, "aria-label": o.text ?? label, title: o.title ?? label,
        "aria-haspopup": o.menu ? "menu" : false, onclick: run },
        icon(name), o.text ? el("span", {}, o.text) : null, o.menu ? icon("chevron", 12) : null);
    const sep = () => el("span", { class: "tool-sep", "aria-hidden": "true" });
    const anchor = (e: MouseEvent) => e.currentTarget as HTMLElement;
    this.groupSep = sep();

    this.tableButtons = [
      tool("layers", "Views", (e) => this.viewsMenu(anchor(e)), { text: "Views", title: "Saved views of this table", menu: true }),
      tool("filter", "Filter", act.filter, { text: "Filter", title: "Filter rows of the whole table" }),
      tool("columnPlus", "Add column", act.addColumn, { text: "Add column", title: "Add a column or formula column" }),
      tool("brush", "Format table", act.formatTable, { title: "Format table: number formats, hidden and frozen columns, rules" }),
      tool("search", "Find / replace", act.findReplace, { title: "Find / replace across the table" }),
    ];
    this.sheetButtons = [
      tool("undo", "Undo", () => this.undo(false), { title: "Undo (⌘Z)" }),
      tool("redo", "Redo", () => this.undo(true), { title: "Redo (⇧⌘Z)" }),
      sep(),
      tool("tag", "Names", act.names, { title: "Named ranges" }),
      tool("target", "Go to", act.goTo, { title: "Go to a cell, range or name (⌘G)" }),
    ];

    const sheetOnly = () => !this.isSheet();
    const tableOnly = () => this.active?.kind !== "table";
    const menus: [string, () => MenuItem[]][] = [
      ["File", () => [
        { label: "New sheet", action: () => this.createSheet() },
        { label: "Import file…", hint: "CSV · Parquet · JSON · XLSX", action: act.importFile },
        "sep",
        ...this.exportItems().map((i): MenuItem => i === "sep" ? i : { ...i, label: `Export ${i.label}` }),
        ...(this.exportItems().length ? ["sep" as const] : []),
        { label: "Rename workbook…", action: () => this.renameWorkbook() },
        { label: "All workbooks", action: () => this.goHome() },
      ]],
      ["Edit", () => [
        { label: "Undo", hint: "⌘Z", disabled: sheetOnly(), action: () => this.undo(false) },
        { label: "Redo", hint: "⇧⌘Z", disabled: sheetOnly(), action: () => this.undo(true) },
        "sep",
        { label: "Paste special…", hint: "⇧⌘V", disabled: sheetOnly(), action: () => this.pasteSpecialDialog() },
        { label: "Go to…", hint: "⌘G", disabled: sheetOnly(), action: act.goTo },
        { label: "Find / replace…", disabled: tableOnly(), action: act.findReplace },
      ]],
      ["View", () => [
        { label: `${this.explorerOpen ? "✓ " : ""}Explorer`, action: () => this.toggleExplorer() },
        { label: "Saved views…", disabled: tableOnly(), action: () => this.viewsMenu(this.menuAnchor) },
        "sep",
        ...THEME_CHOICES.map((t): MenuItem => ({ label: `${themePreference() === t.value ? "✓ " : ""}${t.label}`, action: () => { setTheme(t.value); this.syncThemeButton(); } })),
      ]],
      ["Insert", () => [
        { label: "Sheet", hint: "Excel formulas", action: () => this.createSheet() },
        { label: "Column…", disabled: tableOnly(), action: act.addColumn },
        { label: "Named range…", disabled: sheetOnly(), action: act.names },
        { label: "Table from SQL query…", disabled: !this.allowSql, action: act.sql },
        "sep",
        { label: "Demo data: 1M rows", action: () => this.createDemo(1_000_000) },
      ]],
      ["Format", () => [
        { label: "Format table…", hint: tableOnly() ? "Tables" : undefined, disabled: tableOnly(), action: act.formatTable },
      ]],
      ["Data", () => [
        { label: "Filter…", disabled: tableOnly(), action: act.filter },
        { label: "Analyze…", hint: "Pivot · chart · join", action: act.analyze },
        { label: "Dashboards…", action: act.dashboards },
        { label: "Data tools…", disabled: tableOnly(), action: act.dataTools },
      ]],
      ["Tools", () => [
        { label: "SQL workspace", disabled: !this.allowSql, action: act.sql },
        { label: "Functions…", action: act.functions },
        { label: "Change history…", action: act.history },
        { label: "Compute and size…", action: act.compute },
        "sep",
        { label: "Command palette", hint: "⌘K", action: act.commands },
      ]],
      ["Help", () => [
        { label: "Formula help", hint: "⇧F3", action: act.formulaHelp },
        { label: "Keyboard shortcuts", hint: "⌘/", action: act.shortcuts },
      ]],
    ];
    // A menu bar of menu buttons: click or ArrowDown opens a menu; ArrowLeft/Right move between
    // the buttons, or between open menus (U.12).
    const menuButtons: HTMLButtonElement[] = [];
    const openMenuAt = (i: number) => {
      const b = menuButtons[(i + menuButtons.length) % menuButtons.length];
      const [label, items] = menus[menuButtons.indexOf(b)];
      this.menuAnchor = b;
      b.focus({ preventScroll: true });
      const r = b.getBoundingClientRect();
      b.setAttribute("aria-expanded", "true");
      showMenu(items(), r.left, r.bottom + 2, { label,
        onArrow: (d) => openMenuAt(menuButtons.indexOf(b) + d),
        onClose: () => b.setAttribute("aria-expanded", "false") });
    };
    const menubar = el("nav", { class: "menubar", "aria-label": "Workbook menus" },
      ...menus.map(([label], i) => {
        const b = el("button", { class: "menubar-item", "aria-haspopup": "menu", "aria-expanded": "false" }, label);
        b.addEventListener("click", () => openMenuAt(i));
        b.addEventListener("keydown", (e) => {
          if (e.key === "ArrowDown") { e.preventDefault(); openMenuAt(i); }
          else if (e.key === "ArrowRight" || e.key === "ArrowLeft") {
            e.preventDefault();
            menuButtons[(i + (e.key === "ArrowRight" ? 1 : -1) + menuButtons.length) % menuButtons.length].focus();
          }
        });
        menuButtons.push(b);
        return b;
      }));
    this.themeBtn = themeButton();

    const root = el("div", { class: "workbook" },
      el("header", { class: "appbar" },
        el("button", { class: "brand", onclick: () => this.goHome(), title: "All workbooks", "aria-label": "Alephgrid — all workbooks" }, brandMark(24)),
        el("div", { class: "appbar-title" },
          el("div", { class: "appbar-name" }, this.titleEl, el("span", { class: "status" }, this.statusDot, this.statusText)),
          menubar),
        el("div", { class: "spacer" }),
        el("button", { class: "command-search", title: "Search all commands (⌘K)", "aria-keyshortcuts": "Meta+K Control+K", onclick: act.commands },
          icon("search", 14), el("span", { class: "command-search-label" }, "Search commands"), el("kbd", { "aria-hidden": "true" }, "⌘K")),
        tool("clock", "History", act.history, { title: "Everyone's recent changes; undo table changes" }),
        tool("cpu", "Compute", act.compute, { title: "This workbook's compute: state, size, idle shutdown" }),
        this.themeBtn,
      ),
      el("div", { class: "toolbar", role: "toolbar", "aria-label": "Workbook tools" },
        this.explorerBtn = tool("sidebar", "Explorer", () => this.toggleExplorer(), { title: "Show or hide the workbook explorer" }),
        sep(),
        ...this.sheetButtons,
        ...this.tableButtons,
        this.groupSep,
        tool("upload", "Import", act.importFile, { text: "Import", title: "Import CSV, Parquet, JSON or XLSX" }),
        sep(),
        tool("chart", "Analyze", act.analyze, { text: "Analyze", title: "Summaries, pivots, charts and joins without SQL" }),
        tool("database", "SQL", act.sql, { text: "SQL", title: "SQL workspace" }),
        tool("fx", "Functions", act.functions, { text: "Functions", title: "Define, test and publish reusable functions" }),
        el("div", { class: "spacer" }),
        this.exportBtn = tool("download", "Export", (e) => this.exportMenu(anchor(e)), { text: "Export", menu: true, title: "Download this table or the sheets" }),
        this.fileInput,
      ),
      this.chips,
      this.snapshotEl,
      el("div", { class: "workbook-body" },
        this.explorerEl,
        this.withChildren(this.mainEl,
          el("div", { class: "formula-bar" }, this.cellRef, el("span", { class: "fx", "aria-hidden": "true" }, icon("fx", 15)), this.formulaInput, this.referenceLegend),
          this.formulaProblem,
          this.cellNote,
          this.staleNote,
          this.gridWrap)),
      el("footer", { class: "footer" },
        el("button", { class: "tab-add", title: "New table or sheet", "aria-label": "New table or sheet",
          onclick: (e: MouseEvent) => this.newItemMenu(e) }, icon("plus", 15)),
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
    this.applyExplorer();
    return root;
  }

  private withChildren<T extends HTMLElement>(parent: T, ...children: HTMLElement[]): T {
    parent.append(...children);
    return parent;
  }

  private toggleExplorer(): void {
    this.explorerOpen = !this.explorerOpen;
    try { localStorage.setItem("alephgrid-explorer", this.explorerOpen ? "open" : "closed"); } catch { /* this page only */ }
    this.applyExplorer();
  }

  private applyExplorer(): void {
    this.explorerEl.hidden = !this.explorerOpen;
    this.explorerBtn?.setAttribute("aria-pressed", this.explorerOpen ? "true" : "false");
    this.explorerBtn?.setAttribute("aria-expanded", this.explorerOpen ? "true" : "false");
    if (this.explorerOpen) this.renderExplorer();
  }

  /** Refresh the explorer's function list a moment after changes (no event names functions). */
  private refreshFunctionsSoon(): void {
    clearTimeout(this.functionsTimer);
    this.functionsTimer = window.setTimeout(async () => {
      try { this.functionsList = await this.rpc.call("functions", {}); } catch { return; }
      if (!this.disposed) this.renderExplorer();
    }, 400);
  }

  private renderExplorer(): void {
    if (!this.explorerOpen) return;
    const a = this.active;
    const item = (glyph: IconName, name: string, detail: string, current: boolean, open: () => void, title?: string) =>
      el("li", {}, el("button", { class: `explorer-item${current ? " current" : ""}`, "aria-current": current ? "true" : false, title: title ?? name, onclick: open },
        icon(glyph, 15), el("span", { class: "explorer-name" }, name), el("span", { class: "explorer-detail" }, detail)));
    const section = (title: string, items: HTMLElement[], empty: string, action?: [string, () => void]) => {
      const id = `explorer-${title.toLowerCase().replace(/\W+/g, "-")}`;
      return el("section", { class: "explorer-section", "aria-labelledby": id },
        el("h3", { id }, el("span", {}, title), el("span", { class: "explorer-count" }, String(items.length)),
          action ? el("button", { class: "icon-btn small", "aria-label": action[0], title: action[0], onclick: action[1] }, icon("plus", 13)) : null),
        items.length ? el("ul", {}, ...items) : el("p", { class: "explorer-empty" }, empty));
    };
    this.explorerEl.replaceChildren(...[
      section("Tables", this.tables.map((t) => item("table", t.name, compactRows(t.row_count), a?.kind === "table" && a.name === t.name, () => void this.openTable(t.name),
        `${t.name} · ${formatCount(t.row_count)} rows · ${t.column_count} columns`)), "Import a file to add a table.", ["Add a table from a file", () => this.fileInput.click()]),
      section("Sheets", this.sheets.map((sh) => item("sheet", sh.name, "", a?.kind === "sheet" && a.index === sh.index,
        () => this.pickingFormula() ? this.showSheetForPicking(sh.index) : void this.openSheet(sh.index))), "No sheets yet.", ["Add a sheet", () => void this.createSheet()]),
      this.allowSql ? section("Queries", this.savedQueries.map((q) => item("query", q.name, `v${q.version}`, false, () => this.sqlPanel(q.id),
        `${q.name} · ${q.kind} query, version ${q.version}`)), "Saved SQL queries appear here.", ["Write a query", () => this.sqlPanel()]) : null,
      section("Functions", this.functionsList.map((f) => item("fx", f.name, `v${f.pinned}`, false, () => { this.closeFunctions?.(); this.closeFunctions = functionLibrary(this.rpc, this.allowSql); },
        `${f.name} · version ${f.pinned} in use`)), "Reusable functions appear here.", ["Define a function", () => { this.closeFunctions?.(); this.closeFunctions = functionLibrary(this.rpc, this.allowSql); }]),
    ].filter((x): x is HTMLElement => !!x));
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

  private savedViewModified(): boolean {
    const v = this.currentSaved;
    if (!v) return false;
    const norm = (s: ViewSpec) => JSON.stringify({ sort: s.sort, filters: s.filters, hidden: [...s.hidden].sort() });
    return norm(v.spec) !== norm(this.spec);
  }

  private loadSavedViews(): Promise<void> {
    this.savedViewsLoading = this.fetchSavedViews();
    return this.savedViewsLoading;
  }

  private async fetchSavedViews(): Promise<void> {
    if (this.active?.kind !== "table") return;
    try {
      this.savedViews = await this.rpc.call("saved_views", { table: this.active.name });
    } catch { this.savedViews = []; }
    if (this.currentSaved) this.currentSaved = this.savedViews.find((v) => v.id === this.currentSaved!.id) ?? null;
    this.renderChips();
  }

  private currentWidths(): Record<string, number> {
    const widths = this.grid.columnWidths();
    const out: Record<string, number> = {};
    this.view?.columns.forEach((c, i) => (out[c.name] = widths[i]));
    return out;
  }

  private async applySaved(v: SavedView): Promise<void> {
    this.storeWidths({ ...this.loadWidths(), ...v.widths });
    this.currentSaved = v;
    await this.setSpec(v.spec);
  }

  private async viewsMenu(anchor: HTMLElement): Promise<void> {
    if (this.active?.kind !== "table") return;
    const r = anchor.getBoundingClientRect();
    await this.savedViewsLoading;
    const current = this.currentSaved;
    const items: MenuItem[] = [
      { label: "Live views always show the table's current data", disabled: true, action: () => {} },
      ...this.savedViews.map((v): MenuItem => ({ label: `${v.id === current?.id ? "✓ " : ""}${v.name}`, action: () => this.applySaved(v) })),
      ...(this.savedViews.length ? [] : [{ label: "No saved views yet", disabled: true, action: () => {} } as MenuItem]),
      "sep",
      { label: "Save view as…", action: () => this.saveViewAs() },
    ];
    if (current) {
      items.push(
        { label: `Update “${current.name}”`, disabled: !this.savedViewModified(), action: () => this.saveView(current.id, current.name) },
        { label: `Rename “${current.name}”…`, action: async () => {
          const v = await formDialog("Rename view", [{ name: "name", label: "Name", value: current.name }], "Rename");
          if (!v) return;
          try { this.currentSaved = await this.rpc.call("rename_saved_view", { saved: current.id, name: v.name }); await this.loadSavedViews(); }
          catch (err) { toast((err as Error).message, "error"); }
        } },
        { label: `Delete “${current.name}”`, danger: true, action: async () => {
          if (!(await confirmDialog(`Delete the view “${current.name}”?`, "Only the saved view goes; the table and its data stay."))) return;
          await this.rpc.call("delete_saved_view", { saved: current.id }).catch((err) => toast((err as Error).message, "error"));
          this.currentSaved = null;
          await this.loadSavedViews();
        } },
      );
    }
    items.push("sep", { label: "Save rows as a new table (snapshot)…", action: () => this.snapshotView() });
    showMenu(items, r.left, r.bottom + 4);
  }

  private async saveViewAs(): Promise<void> {
    const v = await formDialog("Save view", [{ name: "name", label: "View name", placeholder: "e.g. Late flights by fare",
      hint: "Saves the sort, filters, hidden columns and column widths. The view is live: it always shows current data." }], "Save view");
    if (v) await this.saveView(undefined, v.name);
  }

  private async saveView(id: string | undefined, name: string): Promise<void> {
    try {
      this.currentSaved = await this.rpc.call("save_view", { saved: id, name, spec: this.spec, widths: this.currentWidths() });
      await this.loadSavedViews();
      toast(`Saved view “${this.currentSaved.name}”`, "success");
    } catch (e) {
      toast((e as Error).message, "error");
    }
  }

  private async snapshotView(): Promise<void> {
    if (!this.view || this.active?.kind !== "table") return;
    const v = await formDialog("Save rows as a new table", [{ name: "name", label: "Table name", value: `${this.active.name}_snapshot`,
      hint: `Copies the ${formatCount(this.view.row_count)} rows of this view, in this order, into a new table. It is a snapshot: later changes to ${this.active.name} do not reach it.` }], "Create table");
    if (!v) return;
    try {
      const info = await this.rpc.call("table_from_view", { view_id: this.view.view_id, name: v.name });
      await this.loadItems();
      await this.openTable(info.name);
      toast(`Created ${info.name} (${formatCount(info.row_count)} rows, snapshot)`, "success");
    } catch (e) {
      toast((e as Error).message, "error");
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
        viewId = (await this.rpc.call("open_view", { spec: { table: t.name, sort: [], filters: [], hidden: [] } })).view_id;
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
    for (const b of this.tableButtons) {
      b.hidden = sheet || !this.active;
      b.disabled = !this.view;
    }
    for (const b of this.sheetButtons) b.hidden = !sheet;
    this.groupSep.hidden = !this.active;
    if (this.exportBtn) this.exportBtn.disabled = !this.active;
    this.chips.hidden = sheet || this.chips.childElementCount === 0;
  }

  private renderEmpty(): void {
    this.active = null;
    this.view = null;
    this.grid.setColumns([]);
    this.rowsEl.textContent = "";
    this.kindEl.hidden = true;
    this.renderTabs();
    this.renderSnapshot();
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
    if (this.allowSql) this.loadMaterialized();
    this.refreshFunctionsSoon();
  }

  private renderTabs(): void {
    const a = this.active;
    const tableTabs = this.tables.map((t) => {
      const on = a?.kind === "table" && a.name === t.name;
      return el("button", {
        class: `tab${on ? " active" : ""}`, role: "tab", "aria-selected": on ? "true" : "false",
        title: `Table · ${formatCount(t.row_count)} rows · ${t.column_count} columns${this.snapshotNote(t.name)}`,
        onclick: () => this.openTable(t.name),
        oncontextmenu: (e: MouseEvent) => { e.preventDefault(); this.tableTabMenu(t, e.clientX, e.clientY); },
      }, el("span", { class: "tab-icon" }, icon("table", 13)), t.name,
      this.materialized.some((m) => m.table === t.name && m.stale) ? el("span", { class: "tab-stale", "aria-hidden": "true" }, " •") : null);
    });
    const sheetTabs = this.sheets.map((s) => {
      const on = a?.kind === "sheet" && a.index === s.index;
      return el("button", {
        class: `tab tab-sheet${on ? " active" : ""}`, role: "tab", "aria-selected": on ? "true" : "false",
        title: "Sheet (Excel formulas)",
        // While a formula is being written, a tab shows that sheet for picking references
        // and keeps the formula bar focused (the edit still belongs to its own cell).
        onmousedown: (e: MouseEvent) => { if (this.pickingFormula()) e.preventDefault(); },
        onclick: () => this.pickingFormula() ? this.showSheetForPicking(s.index) : this.openSheet(s.index),
        oncontextmenu: (e: MouseEvent) => { e.preventDefault(); this.sheetTabMenu(s, e.clientX, e.clientY); },
      }, el("span", { class: "tab-icon" }, icon("sheet", 13)), s.name);
    });
    const tabsHadFocus = this.tabsEl.contains(document.activeElement);
    this.tabsEl.replaceChildren(...tableTabs, ...sheetTabs);
    // ARIA tabs: only the active tab is in the Tab order; arrow keys move between tabs.
    const all = [...tableTabs, ...sheetTabs];
    all.forEach((t) => (t.tabIndex = t.classList.contains("active") || (!all.some((x) => x.classList.contains("active")) && t === all[0]) ? 0 : -1));
    if (tabsHadFocus) all.find((t) => t.tabIndex === 0)?.focus();
    this.renderExplorer();
  }

  private snapshotNote(table: string): string {
    const m = this.materialized.find((x) => x.table === table);
    if (!m) return "";
    return ` · Snapshot of a query, computed at revision ${m.revision}${m.stale ? " (stale: a source changed)" : ""}`;
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
    this.spec = { table: name, sort: [], filters: [], hidden: [] };
    this.currentSaved = null;
    this.savedViews = [];
    this.updateToolbar();
    this.renderSnapshot();
    this.loadSavedViews();
    this.busy("Opening…");
    try {
      this.presentation = await this.rpc.call("presentation", { table: name });
      this.spec.hidden = this.presentation.hidden;
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

  private async reloadPresentation(table: string): Promise<void> {
    const p = await this.rpc.call("presentation", { table });
    if (this.active?.kind !== "table" || this.active.name !== table) return;
    if (JSON.stringify(p) === JSON.stringify(this.presentation)) return;
    this.presentation = p;
    await this.setSpec({ ...this.spec, hidden: p.hidden });
    this.grid.invalidate();
  }

  private async openSheet(index: number): Promise<void> {
    this.gridWrap.querySelector(".empty")?.remove();
    this.closeTableView();
    const info = this.sheets.find((s) => s.index === index);
    if (!info) return;
    this.active = { kind: "sheet", index, name: info.name };
    this.presentation = emptyPresentation();
    this.grid.setLabel(`Sheet ${info.name}`);
    this.applySheetInfo(info, true);
    this.renderSnapshot();
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
    // Cell values may have changed: look the active cell's error up again when data arrives.
    this.cellNoteKey = "";
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
    this.cache.viewGeneration = info.generation;
    this.updateToolbar();
    this.spec = info.spec;
    if (reset) {
      this.cache.reset(info.view_id, info.columns.length);
    } else {
      this.cache.viewId = info.view_id;
      this.cache.columns = info.columns.length;
      this.cache.refresh();
    }
    const saved = this.loadWidths();
    this.grid.setColumns(info.columns.map((c) => saved[c.name] ?? defaultWidth(c)));
    this.rowsEl.textContent = `${formatCount(info.row_count)} rows`;
    this.renderStale(info);
    void this.rpc.call("validations", { table: info.spec.table }).then(r => { this.rules = r; }).catch(() => { this.rules = []; });
    this.setKind("table");
    this.renderChips();
    this.grid.invalidate();
    this.onSelection(this.grid.selection(), this.grid.active);
  }

  /** Say when the view's order and formula values are the last committed results, with a
   *  way to stop the recalculation or try it again. */
  private renderStale(info: ViewInfo): void {
    const why = info.stale;
    this.gridWrap.classList.toggle("stale-results", !!why);
    if (!why) { this.staleNote.hidden = true; this.staleNote.replaceChildren(); return; }
    const table = info.spec.table;
    const running = why === "recalculating";
    const action = running
      ? el("button", { class: "btn btn-small", onclick: () => this.rpc.call("cancel_recalculation", { table }).catch((e: Error) => toast(e.message, "error")) }, "Cancel")
      : el("button", { class: "btn btn-small", onclick: () => this.rpc.call("recalculate", { table }).then(() => this.reopen()).catch((e: Error) => toast(e.message, "error")) }, "Recalculate");
    this.staleNote.replaceChildren(
      `Sorted and filtered by the last committed formula results: ${why}. Values may be out of date. `, action);
    this.staleNote.hidden = false;
  }

  private async reopen(): Promise<void> {
    try {
      await this.loadItems();
      if (this.active?.kind === "table") {
        const old = this.view;
        const info = await this.rpc.call("open_view", { spec: this.spec });
        // Changes sent for the old view (kept across the reconnect) go to the new one.
        if (old) this.rpc.aliasView(old.view_id, info.view_id);
        const same = !!old && JSON.stringify(old.columns) === JSON.stringify(info.columns);
        this.applyView(info, !same);
        await this.loadSavedViews();
      } else if (this.active?.kind === "sheet") {
        await this.refreshSheet();
      }
    } catch (e) {
      toast((e as Error).message, "error");
    }
  }

  /** Refresh the item list, the open view's shape and the visible data. */
  private async resync(): Promise<void> {
    try {
      await this.loadItems();
      if (this.active?.kind === "table" && this.view) {
        try {
          this.applyView(await this.rpc.call("view_info", { view_id: this.view.view_id }), false);
        } catch {
          await this.reopen();
          return;
        }
        await this.loadSavedViews();
      } else if (this.active?.kind === "sheet") {
        await this.refreshSheet();
      }
    } catch (e) {
      toast((e as Error).message, "error");
    }
  }

  private async setSpec(spec: ViewSpec): Promise<void> {
    if (!this.view) return;
    const label = spec.sort.length ? "Sorting…" : "Filtering…";
    this.busy(label);
    const started = performance.now();
    try {
      const info = await this.rpc.request("update_view", { view_id: this.view.view_id, spec },
        { onProgress: (pct) => this.busy(`${label} ${Math.round(pct)}%`) });
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
      if (this.spec.hidden.length) {
        chips.push(el("span", { class: "chip" }, `${this.spec.hidden.length} hidden column${this.spec.hidden.length === 1 ? "" : "s"}`,
          el("button", { "aria-label": "Show hidden columns", onclick: () => this.setSpec({ ...this.spec, hidden: [] }) }, "×")));
      }
      if (this.currentSaved) {
        chips.unshift(el("span", { class: "chip chip-view", title: "A saved live view: it always shows the table's current data" },
          `View: ${this.currentSaved.name}${this.savedViewModified() ? " (changed)" : ""}`));
      }
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
      if (!this.formulaOrigin || document.activeElement !== this.formulaInput) {
        this.refreshFormulaValue();
        this.formulaProblem.hidden = true;
      }
      this.sheetSelectionStats(range);
      this.showCellNote();
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
    const v = this.cache.value(this.grid.active.r, this.grid.active.c);
    this.formulaInput.value = v !== undefined ? rawText(v) : "";
  }

  // ------------------------------------------------------------------ edits

  /** What an edit of a cell is checked against: the value shown now (see `CellEdit.base`),
   *  undefined while it is loading or for a row about to be appended. */
  private baseOf(r: number, c: number): CellValue | undefined {
    if (this.isSheet()) {
      const cell = this.sheetCache.cell(r, c);
      return cell === undefined ? undefined : cell === null ? "" : cell[2] ?? cell[0];
    }
    if (!this.view || r >= this.view.row_count) return undefined;
    return this.cache.value(r, c);
  }

  /** The table generation of view rows [r0, r1] as shown (see `ViewInfo.generation`), or
   *  null while the rows on screen belong to another arrangement than the view (a refresh is
   *  under way): then the change is not sent. */
  private shownGeneration(r0: number, r1: number): number | null {
    if (!this.view) return null;
    const g = this.view.generation;
    const shown = this.cache.generations(r0, Math.max(r0, r1));
    if (shown.size === 0 || (shown.size === 1 && shown.has(g))) return g;
    toast("The rows changed while you were working; they are being refreshed. Try again in a moment.");
    void this.resync();
    return null;
  }

  /** Sheet `index`'s layout for rows [r0, r1] as shown (see `SheetInfo.layout`), or null
   *  while a refresh mixes layouts (or the sheet is unknown). */
  private shownLayout(index: number, r0: number, r1: number): number | null {
    const info = this.sheets.find((s) => s.index === index);
    if (this.active?.kind !== "sheet" || this.active.index !== index) return info?.layout ?? null;
    const shown = this.sheetCache.layoutsIn(r0, Math.max(r0, r1));
    if (shown.size === 1) return [...shown][0];
    if (shown.size === 0) return info?.layout ?? null;
    toast("The sheet changed while you were working; it is being refreshed. Try again in a moment.");
    void this.refreshSheet();
    return null;
  }

  /** Report a refused change. A conflict over values offers to overwrite them (`retry`
   *  resends without the values the edit was based on); one over moved rows or cells only
   *  refreshes, since the positions it named are out of date. */
  private reportConflict(e: unknown, retry?: () => void): void {
    const err = e as RpcError;
    if (err.code === "conflict" && !err.moved && retry) {
      toast(`${err.message}. Refreshed to show their change.`, "error", 4000, { label: "Overwrite", run: retry });
    } else {
      toast(err.message, "error");
    }
  }

  private async commitEdits(edits: GridEdit[], force = false, rebased = false): Promise<boolean> {
    if (!edits.length) return true;
    if (this.active?.kind === "sheet") {
      const index = this.active.index;
      const [only] = edits;
      return this.inSheetOrder(async () => {
        if (edits.length === 1 && only.value?.trimStart().startsWith("=")) {
          if (!await this.formulaParses({ index, r: only.r, c: only.c }, only.value)) return false;
        }
        return this.commitSheetEdits(index, edits, force);
      });
    }
    return this.inTableOrder(() => this.commitTableEdits(edits, force, rebased));
  }

  private async commitTableEdits(edits: GridEdit[], force: boolean, rebased: boolean): Promise<boolean> {
    if (!this.view) return true;
    const cols = this.view.columns;
    const rowCount = this.view.row_count;
    // Existing rows are addressed by record id, so the edit lands on the right row even if
    // rows moved meanwhile; rows at or after the end append by position. Each cell carries
    // the value the user saw, so a collaborator's change to it is not silently overwritten.
    const cellEdits = edits.map((e) => {
      const record = e.r < rowCount ? this.cache.record(e.r) : undefined;
      const base = force || e.r >= rowCount ? undefined : e.base !== undefined ? e.base : this.cache.value(e.r, e.c);
      return { row: e.r, column: cols[e.c].name, value: e.value, ...(record !== undefined ? { record } : {}), ...(base !== undefined ? { base } : {}) };
    });
    // Rows addressed by position need the arrangement they were read in: appends that of the
    // view's row count; existing rows without a known record id that of the rows shown.
    const positional = cellEdits.filter((e) => e.record === undefined).map((e) => e.row);
    let generation: number | undefined;
    if (positional.length) generation = this.view.generation;
    const unknown = positional.filter((r) => r < rowCount);
    if (unknown.length) {
      const g = this.shownGeneration(unknown.reduce((a, r) => Math.min(a, r), rowCount), unknown.reduce((a, r) => Math.max(a, r), 0));
      if (g === null) return false;
      generation = g;
    }
    if (!this.fits(cellEdits.length, { op: "edit", view_id: this.view.view_id, edits: cellEdits, generation })) return false;
    for (const e of edits) {
      const kind = cols[e.c].kind;
      const num = e.value !== null && isNumericKind(kind) ? Number(e.value) : NaN;
      this.cache.patch(e.r, e.c, e.value === null ? null : Number.isFinite(num) ? num : e.value);
    }
    this.grid.invalidate();
    try {
      const out = await this.rpc.call("edit", { view_id: this.view.view_id, edits: cellEdits, ...(generation !== undefined ? { generation } : {}) });
      if (out.rows_appended > 0) {
        this.view = await this.rpc.call("view_info", { view_id: this.view.view_id });
        this.cache.viewGeneration = this.view.generation;
        this.rowsEl.textContent = `${formatCount(this.view.row_count)} rows`;
        // The new rows sit after the old last one; the next edit of them goes by record.
        if (out.first_appended !== undefined) this.cache.learn(rowCount, out.first_appended, out.rows_appended, this.view.generation);
      }
      this.cache.refresh();
      this.cache.settled(edits.map((x) => cellKey(x.r, x.c)), true);
      this.grid.invalidate();
      return true;
    } catch (e) {
      this.cache.settled(edits.map((x) => cellKey(x.r, x.c)), false);
      this.cache.refresh();
      this.grid.invalidate();
      const err = e as RpcError;
      // Someone else appended rows first: new rows go after theirs (once).
      if (err.code === "conflict" && err.moved && !rebased && this.view && edits.every((x) => x.r >= rowCount)) {
        try {
          this.view = await this.rpc.call("view_info", { view_id: this.view.view_id });
          this.applyView(this.view, false);
          const shift = this.view.row_count - rowCount;
          return await this.commitTableEdits(edits.map((x) => ({ ...x, r: x.r + shift })), force, true);
        } catch (again) {
          this.reportConflict(again);
          return false;
        }
      }
      this.reportConflict(e, () => void this.commitEdits(edits, true));
      return false;
    }
  }

  /** Paste guard: refuse an edit the server would reject as too large, before sending it. */
  private fits(cells: number, frame: unknown): boolean {
    const tooMany = cells > this.maxEditCells;
    // The sent frame also carries its id and mutation id.
    const bytes = tooMany ? 0 : new TextEncoder().encode(JSON.stringify(frame)).length + 100;
    if (!tooMany && bytes <= this.maxMessageBytes) return true;
    const limit = tooMany ? this.maxEditCells : Math.max(1, Math.floor((cells * this.maxMessageBytes) / bytes));
    toast(`That paste is too large (${formatCount(cells)} cells; the limit is ${formatCount(limit)})`, "error");
    return false;
  }

  private async commitSheetEdits(sheet: number, edits: GridEdit[], force = false): Promise<boolean> {
    // Each cell carries what it showed when editing began, and the request the sheet layout
    // its addresses were read at, so a collaborator's edit or inserted row is not overwritten.
    const inputs = edits.map((e) => {
      const base = force ? undefined : e.base !== undefined ? e.base : this.baseOf(e.r, e.c);
      return { row: e.r, column: e.c, value: e.value ?? "", ...(base !== undefined ? { base: String(base ?? "") } : {}) };
    });
    const layout = this.shownLayout(sheet, edits.reduce((a, e) => Math.min(a, e.r), Infinity), edits.reduce((a, e) => Math.max(a, e.r), 0));
    if (layout === null && this.sheets.some((s) => s.index === sheet)) return false;
    if (!this.fits(inputs.length, { op: "sheet_set", sheet, inputs, layout })) return false;
    for (const e of edits) this.sheetCache.patch(e.r, e.c, e.value ?? "");
    this.grid.invalidate();
    try {
      const info = await this.rpc.call("sheet_set", { sheet, inputs, ...(layout !== null ? { layout } : {}) });
      this.sheets = this.sheets.map((s) => (s.index === info.index ? info : s));
      this.applySheetInfo(info, false);
      this.sheetCache.refresh();
      this.sheetCache.settled(edits.map((x) => cellKey(x.r, x.c)), true);
      this.grid.invalidate();
      return true;
    } catch (e) {
      this.sheetCache.settled(edits.map((x) => cellKey(x.r, x.c)), false);
      await this.refreshSheet();
      this.reportConflict(e, () => void this.inSheetOrder(() => this.commitSheetEdits(sheet, edits, true)));
      return false;
    }
  }


  private async undo(redo: boolean): Promise<void> {
    if (!this.isSheet()) {
      // Tables: take back the caller's own latest change (or redo their latest undo).
      try {
        const out = await this.rpc.call("undo", { redo });
        if (!out) toast(redo ? "Nothing to redo" : "Nothing of yours to undo");
        else toast(`${out.kind === "redo" ? "Redid" : "Undid"} your change to ${out.table}`, "success");
        await this.resync();
      } catch (e) {
        this.reportConflict(e);
      }
      return;
    }
    try {
      const changed = await this.inSheetOrder(() => this.rpc.call(redo ? "sheet_redo" : "sheet_undo"));
      if (!changed) toast(redo ? "Nothing to redo" : "Nothing to undo");
      await this.refreshSheet();
    } catch (e) {
      toast((e as Error).message, "error");
    }
  }

  private updateReferenceHints(): void {
    const ranges = this.isSheet() && document.activeElement === this.formulaInput ? referenceRanges(this.formulaInput.value) : [];
    this.grid.setReferences(ranges);
    this.referenceLegend.hidden = !ranges.length;
    this.referenceLegend.replaceChildren(...ranges.map((r, i) => el("span", { class: `reference-color-${i % 4}` },
      `${columnLetter(r.c0)}${r.r0 + 1}${r.r0 === r.r1 && r.c0 === r.c1 ? "" : `:${columnLetter(r.c1)}${r.r1 + 1}`}`)));
  }

  private pickReference(range: Range, phase: "start" | "move" | "end"): boolean {
    if (!this.isSheet() || document.activeElement !== this.formulaInput) return false;
    if (phase === "end") { this.referencePickState = null; return true; }
    if (phase === "start") {
      const selection = referenceInsertion(this.formulaInput.value, this.formulaInput.selectionStart ?? 0, this.formulaInput.selectionEnd ?? 0);
      if (!selection) return false;
      this.referencePickState = { base: this.formulaInput.value, ...selection };
    }
    const state = this.referencePickState;
    if (!state) return false;
    const a = `${columnLetter(range.c0)}${range.r0 + 1}`;
    const b = `${columnLetter(range.c1)}${range.r1 + 1}`;
    const other = this.active?.kind === "sheet" && this.formulaOrigin && this.active.index !== this.formulaOrigin.index;
    const text = `${other ? `${quoteSheet(this.active!.name)}!` : ""}${a === b ? a : `${a}:${b}`}`;
    this.formulaInput.value = state.base.slice(0, state.start) + text + state.base.slice(state.end);
    this.formulaInput.setSelectionRange(state.start + text.length, state.start + text.length);
    this.formulaInput.dispatchEvent(new Event("input", { bubbles: true }));
    return true;
  }

  private async commitFormulaBar(): Promise<void> {
    const value = this.formulaInput.value;
    const { r, c } = this.grid.active;
    if (this.isSheet() && this.active?.kind === "sheet") {
      const origin = this.formulaOrigin ?? { index: this.active.index, r, c };
      const saved = await this.inSheetOrder(async () => {
        if (!await this.formulaParses(origin, value)) return null;
        return this.commitSheetEdits(origin.index, [{ r: origin.r, c: origin.c, value: value === "" ? null : value }]);
      });
      if (saved === null) return;
      this.formulaOrigin = null;
      if (this.active.index !== origin.index) await this.returnToOrigin(origin);
      if (saved) this.grid.focus();
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

  /** A formula that does not parse is not saved: the draft stays in the formula bar with
   *  the caret where parsing stopped. Text starting with an apostrophe is saved as text. */
  private async formulaParses(origin: { index: number; r: number; c: number }, value: string): Promise<boolean> {
    if (!value.trimStart().startsWith("=")) { this.formulaProblem.hidden = true; return true; }
    let problem;
    try {
      problem = await this.rpc.call("sheet_check_formula", { sheet: origin.index, row: origin.r, column: origin.c, formula: value });
    } catch (e) {
      toast((e as Error).message, "error");
      return false;
    }
    if (!problem) { this.formulaProblem.hidden = true; return true; }
    this.formulaOrigin = origin;
    this.formulaInput.value = value;
    this.formulaProblem.hidden = false;
    this.formulaProblem.textContent = `Formula not saved: ${problem.message} (at character ${problem.position + 1}). Fix it, press Escape to discard, or start with an apostrophe to enter text.`;
    this.formulaInput.focus();
    const at = [...value].slice(0, problem.position).join("").length;
    this.formulaInput.setSelectionRange(at, at);
    return false;
  }

  private async cancelFormulaBar(): Promise<void> {
    const origin = this.formulaOrigin;
    this.formulaOrigin = null;
    this.formulaProblem.hidden = true;
    if (origin && this.active?.kind === "sheet" && this.active.index !== origin.index) await this.returnToOrigin(origin);
    else this.onSelection(this.grid.selection(), this.grid.active);
    this.grid.focus();
  }

  /** True while the formula bar holds a sheet formula that a reference could be inserted into. */
  private pickingFormula(): boolean {
    return this.isSheet() && document.activeElement === this.formulaInput && this.formulaInput.value.startsWith("=")
      && !!referenceInsertion(this.formulaInput.value, this.formulaInput.selectionStart ?? 0, this.formulaInput.selectionEnd ?? 0);
  }

  /** Remember which cell a formula-bar edit belongs to (the input may already have focus). */
  private markFormulaOrigin(): void {
    if (this.active?.kind === "sheet" && !this.formulaOrigin) this.formulaOrigin = { index: this.active.index, r: this.grid.active.r, c: this.grid.active.c };
  }

  /** Show another sheet's cells for picking without leaving the formula edit. */
  private async showSheetForPicking(index: number): Promise<void> {
    this.markFormulaOrigin();
    const draft = this.formulaInput.value;
    const caret = [this.formulaInput.selectionStart ?? draft.length, this.formulaInput.selectionEnd ?? draft.length] as const;
    await this.openSheet(index);
    this.formulaInput.value = draft;
    this.formulaInput.focus();
    this.formulaInput.setSelectionRange(caret[0], caret[1]);
    this.updateReferenceHints();
  }

  private async returnToOrigin(origin: { index: number; r: number; c: number }): Promise<void> {
    await this.openSheet(origin.index);
    this.grid.select({ r: origin.r, c: origin.c });
  }

  /** Errors show their origin and message under the formula bar (one bounded request per selection). */
  private showCellNote(selectionChanged = true): void {
    if (this.active?.kind !== "sheet") { this.cellNote.hidden = true; return; }
    const { r, c } = this.grid.active;
    const sheet = this.active.index;
    const key = `${sheet}:${r}:${c}`;
    if (!selectionChanged && key === this.cellNoteKey) return;
    const cell = this.sheetCache.cell(r, c);
    if (selectionChanged || cell !== undefined) {
      this.cellNoteKey = cell === undefined ? "" : key;
      ++this.cellNoteSeq;
      this.cellNote.hidden = true;
    }
    if (!cell || !/^#[A-Z0-9/!?]+[!?]?$/.test(cell[0]) || cell[1] !== "center") return;
    const seq = this.cellNoteSeq;
    void this.rpc.call("sheet_cell_detail", { sheet, row: r, column: c }).then((d) => {
      if (seq !== this.cellNoteSeq || !d.error) return;
      const e = d.error;
      this.cellNote.replaceChildren(`${e.code}: ${e.message || "error"}`);
      if (e.origin && e.origin.replace(/^.*!/, "") !== `${columnLetter(c)}${r + 1}`) {
        this.cellNote.append(` · comes from ${e.origin} `, el("button", { class: "btn btn-small", onclick: () => void this.goToAddress(e.origin) }, "Go to source"));
      }
      this.cellNote.hidden = false;
    }).catch(() => { /* selection moved on or the cell changed */ });
  }

  private namesContext(): NamesContext {
    return {
      rpc: this.rpc,
      sheets: () => this.sheets,
      current: () => this.active?.kind === "sheet" ? { index: this.active.index, name: this.active.name, selection: this.grid.selection() } : null,
      goTo: (t) => this.goToTarget(t),
    };
  }

  private async goTo(): Promise<void> {
    try { await goToDialog(this.namesContext()); } catch (e) { toast((e as Error).message, "error"); }
  }

  private async goToAddress(text: string): Promise<void> {
    const t = parseAddress(text);
    if (t) await this.goToTarget(t);
  }

  private async goToTarget(t: Address): Promise<void> {
    if (t.sheet !== null) {
      const s = this.sheets.find((x) => x.name.toLowerCase() === t.sheet!.toLowerCase());
      if (!s) { toast(`There is no sheet named ${t.sheet}`, "error"); return; }
      if (this.active?.kind !== "sheet" || this.active.index !== s.index) await this.openSheet(s.index);
    } else if (this.active?.kind !== "sheet") return;
    // Going past the shown grid grows it, as typing there would.
    if (t.r1 >= this.sheetDims.rows || t.c1 >= this.sheetDims.cols) {
      const cols = Math.max(this.sheetDims.cols, t.c1 + 6);
      this.sheetDims = { rows: Math.max(this.sheetDims.rows, t.r1 + 200), cols };
      if (cols !== this.sheetCols.length) {
        this.sheetCols = sheetColumns(cols);
        this.sheetCache.columns = cols;
        const saved = this.loadWidths();
        this.grid.setColumns(this.sheetCols.map((c) => saved[c.name] ?? SHEET_COL_WIDTH));
      }
    }
    this.grid.select({ r: t.r0, c: t.c0 }, { r: t.r1, c: t.c1 });
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
        // Only the copied columns; a reply may stop short (`truncated`), so continue from there.
        for (let got = 0; got < count;) {
          const block = await this.rpc.call("fetch", {
            view_id: this.view!.view_id, row_start: start + got, row_count: count - got,
            column_start: range.c0, column_count: range.c1 - range.c0 + 1,
          });
          for (const row of block.rows) out.push([...Array<CellValue>(range.c0).fill(null), ...row]);
          got += block.rows.length;
          if (!block.truncated || !block.rows.length) break;
        }
      }
    }
    return out;
  }

  /** The copy or cut in progress; a paste waits for it, so it sees what was just copied. */
  private clipboardWork: Promise<void> = Promise.resolve();

  private copy(range: Range): Promise<void> {
    const work = this.copyNow(range);
    this.clipboardWork = work.catch(() => undefined);
    return work;
  }

  private async copyNow(range: Range): Promise<void> {
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
      this.clip = this.active?.kind === "sheet" ? { sheet: this.active.index, range: { ...range, r1 }, text, cut: false, layout: this.shownLayout(this.active.index, range.r0, r1) ?? undefined } : null;
      toast(`Copied ${formatCount(rows * (range.c1 - range.c0 + 1))} cells`);
    } catch (e) {
      toast(`Copy failed: ${(e as Error).message}`, "error");
    }
  }

  private async paste(at: Cell, text: string): Promise<void> {
    await this.clipboardWork;
    const same = (a: string) => a.replace(/\r\n/g, "\n").replace(/\n$/, "");
    if (this.isSheet() && this.clip && same(text) === same(this.clip.text)) {
      await this.pasteCells(at, "all", false);
      return;
    }
    const rows = parseTsv(text.replace(/\n$/, ""));
    const sheet = this.isSheet();
    const cols = this.view?.columns ?? [];
    const edits: { r: number; c: number; value: string | null }[] = [];
    rows.forEach((row, i) => row.forEach((value, j) => {
      const c = at.c + j;
      if (sheet || (c < cols.length && !cols[c].formula)) edits.push({ r: at.r + i, c, value: value === "" ? null : value });
    }));
    if (await this.commitEdits(edits)) toast(`Pasted ${formatCount(edits.length)} cells`, "success");
  }

  /** Paste the copied (or cut) cells at `at`: formulas are translated, cut cells move. */
  private async pasteCells(at: Cell, mode: PasteMode, transpose: boolean): Promise<void> {
    const clip = this.clip;
    if (!clip || this.active?.kind !== "sheet") return;
    const sheet = this.active.index;
    const r = clip.range;
    const source = { row: r.r0, column: r.c0, rows: r.r1 - r.r0 + 1, columns: r.c1 - r.c0 + 1 };
    if (source.rows * source.columns > this.maxEditCells) { toast(`That paste is too large (the limit is ${formatCount(this.maxEditCells)} cells)`, "error"); return; }
    try {
      let area;
      if (clip.cut) {
        if (clip.sheet !== sheet) { toast("Moving cells to another sheet is not supported yet. Copy them, then clear the originals.", "error"); return; }
        if (mode !== "all" || transpose) { toast("Paste special applies to copied cells. Cut cells move as they are.", "error"); return; }
        const layout = this.shownLayout(sheet, Math.min(source.row, at.r), Math.max(source.row, at.r) + source.rows - 1);
        if (layout === null) return;
        area = await this.inSheetOrder(() => this.rpc.call("sheet_move", { sheet, source, row: at.r, column: at.c, layout }));
        this.clip = null;
      } else {
        const layout = this.shownLayout(sheet, at.r, at.r + (transpose ? source.columns : source.rows) - 1);
        if (layout === null) return;
        area = await this.inSheetOrder(() => this.rpc.call("sheet_paste", {
          from_sheet: clip.sheet, source, sheet, row: at.r, column: at.c, mode, transpose, layout,
          ...(clip.layout !== undefined ? { source_layout: clip.layout } : {}),
        }));
      }
      await this.refreshSheet();
      this.grid.select({ r: area.row, c: area.column }, { r: area.row + area.rows - 1, c: area.column + area.columns - 1 });
      toast(`${clip.cut ? "Moved" : "Pasted"} ${formatCount(area.rows * area.columns)} cells`, "success");
    } catch (e) {
      this.reportConflict(e);
      if ((e as RpcError).code === "conflict") await this.refreshSheet();
    }
  }

  private cut(): Promise<void> {
    const work = (async () => {
      await this.copyNow(this.grid.selection());
      if (this.clip) {
        this.clip.cut = true;
        toast("Cut. Paste to move these cells; formulas that refer to them follow.");
      }
    })();
    this.clipboardWork = work.catch(() => undefined);
    return work;
  }

  private pasteSpecialDialog(): void {
    if (!this.clip || this.clip.cut) {
      toast(this.clip ? "Paste special applies to copied cells. Cut cells move as they are." : "Copy cells in this workbook first. Text from other apps pastes as values with ⌘V.");
      return;
    }
    const at = this.grid.active;
    const modes: [PasteMode, string][] = [["all", "All (values, formulas and formats)"], ["formulas", "Formulas (no formats)"], ["values", "Values only"], ["formats", "Formats only"]];
    const radios = modes.map(([m, label], i) => el("label", { class: "radio" }, el("input", { type: "radio", name: "paste-mode", value: m, ...(i === 0 ? { checked: true } : {}) }), label));
    const transpose = el("input", { type: "checkbox" });
    const go = () => {
      const mode = (panel.querySelector<HTMLInputElement>('input[name="paste-mode"]:checked')?.value ?? "all") as PasteMode;
      modal.close();
      void this.pasteCells(at, mode, transpose.checked);
    };
    const panel = el("section", { class: "dialog" }, el("h2", {}, "Paste special"),
      el("fieldset", {}, el("legend", {}, "Paste"), ...radios),
      el("label", { class: "radio" }, transpose, "Transpose rows and columns"),
      el("div", { class: "dialog-actions" }, el("button", { class: "btn", onclick: () => modal.close() }, "Cancel"), el("button", { class: "btn btn-primary", onclick: go }, "Paste")));
    panel.addEventListener("keydown", (e) => { if (e.key === "Enter") { e.preventDefault(); go(); } });
    const modal = openModal(panel, () => modal.close());
    panel.querySelector<HTMLInputElement>("input")?.focus();
  }

  /** Ctrl/Cmd+D and +R: copy the selection's first row (column) over the rest. With a single
   *  row (column) selected, copy the one above (to the left), as other spreadsheets do. */
  private async fill(direction: "down" | "right"): Promise<void> {
    if (this.active?.kind !== "sheet") return;
    const s = this.grid.selection();
    let area = { row: s.r0, column: s.c0, rows: s.r1 - s.r0 + 1, columns: s.c1 - s.c0 + 1 };
    if (direction === "down" && area.rows === 1) {
      if (s.r0 === 0) { toast("There is no row above to fill down from"); return; }
      area = { ...area, row: s.r0 - 1, rows: 2 };
    }
    if (direction === "right" && area.columns === 1) {
      if (s.c0 === 0) { toast("There is no column to the left to fill right from"); return; }
      area = { ...area, column: s.c0 - 1, columns: 2 };
    }
    if (area.rows * area.columns > this.maxEditCells) { toast(`That fill is too large (the limit is ${formatCount(this.maxEditCells)} cells)`, "error"); return; }
    try {
      const sheet = this.active.index;
      const layout = this.shownLayout(sheet, area.row, area.row + area.rows - 1);
      if (layout === null) return;
      const info = await this.inSheetOrder(() => this.rpc.call("sheet_fill", { sheet, area, direction, layout }));
      this.sheets = this.sheets.map((x) => (x.index === info.index ? info : x));
      this.applySheetInfo(info, false);
      this.sheetCache.refresh();
      this.grid.invalidate();
    } catch (e) {
      toast((e as Error).message, "error");
    }
  }

  private registerCommands(): void {
    this.commands.register(...workbookCommands({
      isSheet: () => this.isSheet(),
      allowSql: () => this.allowSql,
      undo: () => this.undo(false),
      redo: () => this.undo(true),
      copy: () => this.copy(this.grid.selection()),
      cut: () => this.cut(),
      pasteSpecial: () => this.pasteSpecialDialog(),
      fill: (d) => this.fill(d),
      selectAll: () => this.grid.selectAll(),
      selectEntire: (w) => this.grid.selectEntire(w),
      goTo: () => this.goTo(),
      names: async () => { this.closeNames?.(); this.closeNames = await namesDialog(this.namesContext()); },
      formulaHelp: () => { this.closeFormulaHelp?.(); this.closeFormulaHelp = formulaHelp(); },
      sql: () => this.sqlPanel(),
      palette: () => { commandPalette(this.commands); },
      shortcuts: () => { shortcutHelp(this.commands); },
      history: () => historyDialog(this.rpc, () => void this.resync()),
      compute: () => computeDialog(this.meta.id),
    }));
  }

  // ------------------------------------------------------------------ menus

  private async sheetOp(
    op: "sheet_insert_rows" | "sheet_delete_rows" | "sheet_insert_columns" | "sheet_delete_columns",
    params: { at: number; count: number },
  ): Promise<void> {
    if (this.active?.kind !== "sheet") return;
    const sheet = this.active.index;
    const sel = this.grid.selection();
    const rows = op.endsWith("_rows") ? [params.at, params.at + params.count - 1] : [sel.r0, sel.r1];
    const layout = this.shownLayout(sheet, rows[0], rows[1]);
    if (layout === null) return;
    try {
      await this.rpc.call(op, { sheet, ...params, layout });
      await this.refreshSheet();
    } catch (e) {
      this.reportConflict(e);
      if ((e as RpcError).code === "conflict") await this.refreshSheet();
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
    const activeValue = this.grid.active.c === c ? this.cache.value(this.grid.active.r, c) : undefined;
    const items: MenuItem[] = [
      { label: "Sort ascending", hint: "A→Z", action: () => this.setSpec({ ...this.spec, sort: [{ column: col.name, desc: false }] }) },
      { label: "Sort descending", hint: "Z→A", action: () => this.setSpec({ ...this.spec, sort: [{ column: col.name, desc: true }] }) },
      { label: "Then sort by this", disabled: !this.spec.sort.length || this.spec.sort.some((s) => s.column === col.name),
        action: () => this.setSpec({ ...this.spec, sort: [...this.spec.sort, { column: col.name, desc: false }] }) },
      "sep",
      { label: "Filter…", action: () => this.filterDialog(c) },
      { label: "Hide column", disabled: (this.view?.columns.length ?? 0) <= 1,
        action: () => this.setSpec({ ...this.spec, hidden: [...this.spec.hidden, this.view!.columns[c].name] }) },
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
        // Positions name rows only in the arrangement they were read in.
        const first = "at" in params ? Math.max(0, params.at - 1) : params.start;
        const generation = this.shownGeneration(first, first + params.count);
        if (generation === null) return;
        if (op === "insert_rows" && "at" in params) await this.rpc.call(op, { view_id: this.view!.view_id, ...params, generation });
        else if (op === "delete_rows" && "start" in params) await this.rpc.call(op, { view_id: this.view!.view_id, ...params, generation });
      } catch (e) {
        this.reportConflict(e);
        if ((e as RpcError).code === "conflict") await this.resync();
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
      { label: "Export CSV", action: () => this.download({ table: t.name, sort: [], filters: [], hidden: [] }, "csv") },
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
          await this.rpc.call("sheet_rename", { sheet: s.index, name: v.name, layout: s.layout });
          await this.loadItems();
          if (this.active?.kind === "sheet" && this.active.index === s.index) this.active.name = v.name;
        } catch (e) { toast((e as Error).message, "error"); }
      } },
      { label: "Convert to table", hint: "first row = header", action: async () => {
        try {
          const info = await this.rpc.call("sheet_to_table", { sheet: s.index, name: s.name, layout: s.layout });
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
          await this.rpc.call("sheet_delete", { sheet: s.index, layout: s.layout });
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

  private exportItems(): MenuItem[] {
    if (this.isSheet()) return [{ label: "Excel workbook (.xlsx), all sheets", action: () => this.downloadXlsx() }];
    if (!this.view) return [];
    const filtered = this.spec.sort.length || this.spec.filters.length ? " (current view)" : "";
    const items: MenuItem[] = [
      { label: `CSV${filtered}`, action: () => this.download(this.spec, "csv") },
      { label: `Parquet${filtered}`, action: () => this.download(this.spec, "parquet") },
      { label: `JSON lines${filtered}`, action: () => this.download(this.spec, "json") },
    ];
    if (this.sheets.length) items.push("sep", { label: "Sheets as .xlsx", action: () => this.downloadXlsx() });
    return items;
  }

  private exportMenu(anchor: HTMLElement): void {
    const items = this.exportItems();
    if (!items.length) return;
    const r = anchor.getBoundingClientRect();
    showMenu(items, Math.min(r.left, innerWidth - 260), r.bottom + 4);
  }

  private download(spec: ViewSpec, format: string): void {
    this.navigateDownload(api.exportUrl(this.meta.id, spec, format));
  }

  /** XLSX holds the sheets only and cannot hold table formulas: say what it leaves out or
   *  replaces by values before downloading (F.13), never silently. */
  private async downloadXlsx(): Promise<void> {
    let notes: string[] = [];
    try { notes = await this.rpc.call("xlsx_compatibility", {}); } catch (e) { toast((e as Error).message, "error"); return; }
    if (notes.length) {
      const shown = notes.slice(0, 20).join("\n") + (notes.length > 20 ? `\n… and ${notes.length - 20} more` : "");
      if (!(await confirmDialog("XLSX compatibility", `${notes.length} note${notes.length === 1 ? "" : "s"} for this download:\n${shown}`, "Download anyway"))) return;
    }
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
      { name: "type", label: "Type", type: "select", value: preset ?? "sheet", options: [{ value: "sheet", label: "Spreadsheet formula (computed)" }, ...COLUMN_TYPES.map((t) => t.value === "formula" ? { ...t, label: "SQL formula (computed)" } : t)] },
      { name: "formula", label: "Formula (for formula columns)", mono: true, placeholder: `=IF([@${example}] > 100, "large", "small")`,
        hint: "Spreadsheet formula: [@Column] is this row's value, e.g. =IFERROR([@revenue]/[@units], \"\"), =ROUND([@price]*1.2, 2). SQL formula: any DuckDB expression, e.g. =upper([origin]) || '-' || [dest]." },
    ], "Add column", async (vals) => {
      if (!vals.name.trim()) return "Give the column a name";
      try {
        if (vals.type === "formula" || vals.type === "sheet") {
          if (!vals.formula.trim()) return "Write a formula";
          await this.rpc.call("add_formula_column", { table: this.spec.table, name: vals.name.trim(), formula: vals.formula, language: vals.type === "sheet" ? "sheet" : "sql" });
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

  private sqlPanel(queryId?: string): void {
    if (!this.allowSql) { toast("SQL is disabled on this server", "error"); return; }
    if (this.sql) {
      if (queryId) void this.sql.openQuery(queryId);
      return;
    }
    const tableName = this.active?.kind === "table" ? this.active.name : this.tables[0]?.name;
    const t = tableName ? quoteIdent(tableName) : "my_table";
    const firstCol = this.active?.kind === "table" ? this.view?.columns.find((c) => c.kind === "text")?.name : undefined;
    const sampleSql = firstCol
      ? `SELECT ${quoteIdent(firstCol)}, count(*) AS rows\nFROM ${t}\nGROUP BY 1\nORDER BY rows DESC`
      : `SELECT *\nFROM ${t}\nLIMIT 100`;
    const ws = new SqlWorkspace({
      rpc: this.rpc,
      workbookId: this.meta.id,
      sheetCopyRows: this.sheetCopyRows,
      sampleSql,
      openTable: async (name) => {
        await this.loadItems();
        await this.openTable(name);
      },
      openSheet: async (copy) => {
        await this.loadItems();
        await this.openSheet(copy.sheet.index);
        this.reportCopy(copy, "the query result");
      },
      download: (href) => {
        const a = el("a", { href, download: "" });
        document.body.append(a);
        a.click();
        a.remove();
      },
      closed: () => { if (this.sql === ws) this.sql = null; },
      dock: (panel, onClose) => dockPanel(this.mainEl, panel, "SQL", onClose),
    });
    this.sql = ws;
    ws.open();
    if (queryId) void ws.openQuery(queryId);
  }

  // ------------------------------------------------------ materialized tables

  /** Banner over a table that was made from a query: when, whether stale, Refresh. */
  private renderSnapshot(): void {
    const a = this.active;
    const m = a?.kind === "table" ? this.materialized.find((x) => x.table === a.name) : undefined;
    if (!m) {
      this.snapshotEl.hidden = true;
      this.snapshotEl.replaceChildren();
      return;
    }
    const changed = m.sources.filter((s) => s.current_rev === undefined || s.current_rev === null || s.current_rev !== s.data_rev);
    const describe = (s: { table: string; current_rev?: number }) => (s.current_rev === undefined || s.current_rev === null ? `${s.table} (gone)` : s.table);
    const origin = m.query ? this.queryName(m.query, m.query_version) : "a query";
    this.snapshotEl.hidden = false;
    this.snapshotEl.className = `snapshot-banner${m.stale ? " stale" : ""}`;
    this.snapshotEl.replaceChildren(
      el("span", { class: "kind-badge kind-snapshot", title: `Snapshot: the rows were computed once from SQL and stored. It changes only when refreshed.\n\n${m.sql}` }, "Snapshot"),
      el("span", {}, `Snapshot of ${origin} · computed at revision ${m.revision}`),
      m.stale ? el("strong", { class: "snapshot-stale" },
        `Stale: ${changed.length ? changed.map(describe).join(", ") : "a source"} changed`) : el("span", { class: "muted" }, "Up to date with its sources"),
      ...(m.edited ? [el("span", { class: "snapshot-edited" }, "Changed by hand since; refreshing replaces those changes")] : []),
      el("div", { class: "spacer" }),
      el("button", { class: "btn", onclick: () => this.refreshSnapshot(m) }, "Refresh snapshot"),
    );
  }

  private queryName(id: string, version?: number): string {
    const q = this.savedQueries.find((x) => x.id === id);
    return q ? `“${q.name}”${version ? ` v${version}` : ""}` : "a saved query";
  }

  /** Coalesce bursts of edit events into one reload. */
  private matPending = false;
  private reloadMaterializedSoon(): void {
    if (this.matPending) return;
    this.matPending = true;
    this.later(() => { this.matPending = false; this.loadMaterialized(); }, 150);
  }

  private async loadMaterialized(): Promise<void> {
    try {
      const [m, q] = await Promise.all([this.rpc.call("materialized"), this.rpc.call("saved_queries").catch(() => this.savedQueries)]);
      this.materialized = m;
      this.savedQueries = q;
    } catch {
      return;
    }
    if (this.disposed) return;
    this.renderSnapshot();
    this.renderTabs();
  }

  private async refreshSnapshot(m: MaterializedTable): Promise<void> {
    if (m.edited && !(await confirmDialog(`Refresh “${m.table}”?`,
      "This table was changed by hand since it was computed. Refreshing re-runs its query and replaces every row, including those changes.",
      "Refresh and replace"))) return;
    this.busy(`Refreshing ${m.table}…`);
    try {
      const out = await this.rpc.request("refresh_materialized", { table: m.table },
        { onProgress: (pct) => this.busy(`Refreshing ${m.table}… ${Math.round(pct)}%`) });
      toast(`Refreshed ${out.table}: computed at revision ${out.revision}`, "success");
    } catch (e) {
      toast(`${m.table} was not refreshed: ${(e as Error).message}`, "error", 8000);
    } finally {
      this.busy(null);
      await this.loadMaterialized();
    }
  }

  private async renameWorkbook(): Promise<void> {
    const v = await formDialog("Rename workbook", [{ name: "name", label: "Name", value: this.meta.name }], "Rename");
    if (!v) return;
    this.meta = await api.rename(this.meta.id, v.name);
    this.titleEl.textContent = this.meta.name;
    document.title = `${this.meta.name} · Alephgrid`;
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
      const info = await this.rpc.request("create_demo_table", { rows },
        { onProgress: (pct) => this.busy(`Generating ${formatCount(rows)} rows… ${Math.round(pct)}%`) });
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
    this.sql?.onEvent(e);
    this.analysis?.onEvent(e);
    switch (e.event) {
      case "cells_changed":
        if (a?.kind === "table" && e.table === a.name && e.origin !== this.rpc.session) {
          this.cache.refresh();
          this.grid.invalidate();
        }
        // A sorted or filtered view may now read stale stored results: say so.
        if (a?.kind === "table" && e.table === a.name && this.view && !this.view.can_append) {
          const id = this.view.view_id;
          this.rpc.call("view_info", { view_id: id }).then((info) => { if (this.view?.view_id === id) { this.view = info; this.renderStale(info); } }).catch(() => {});
        }
        // A snapshot or one of its sources was edited.
        if (this.materialized.length) this.reloadMaterializedSoon();
        break;
      case "queries_changed":
        // Snapshots and the explorer's saved queries.
        if (this.allowSql) this.loadMaterialized();
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
      case "views_changed":
        if (a?.kind === "table" && e.table === a.name && e.origin !== this.rpc.session) await this.loadSavedViews();
        break;
      case "workbook_closed":
        // The server ends the connection next; the reconnect reopens views and refetches,
        // and changes not yet acknowledged are resent. An idle workbook waits for the person.
        if (e.reason === "idle") { void this.pause(); break; }
        this.setStatus("connecting");
        if (e.reason === "shutdown") toast("The server is restarting…");
        if (e.reason === "restarting") {
          this.restarting = true;
          toast("This workbook is restarting with a new size; it reconnects by itself.");
        }
        break;
      case "resync":
        if (a?.kind === "table") await this.reloadPresentation(a.name);
        // Events were dropped: refresh everything shown.
        await this.resync();
        break;
      case "workbook_deleted":
        this.leaveDeleted();
        break;
      case "access_revoked":
        // Signed out or no longer allowed to see this workbook; the server closed the connection.
        location.assign(e.reason === "signed_out" ? `/login?next=${encodeURIComponent(location.pathname)}` : "/");
        break;
      case "role_changed":
        toast(e.role === "viewer" ? "You can now only view this workbook." : `Your role in this space is now ${e.role}.`);
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
    return `alephgrid:widths:${this.meta.id}:${item}`;
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
