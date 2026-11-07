// The SQL workspace: a schema browser, an editor with completion and `$name` parameters,
// bounded result previews, exports of the complete result, saved and live queries, and
// the ways a result becomes a table or a sheet. Everything here is a convenience; the
// server checks every statement and parameter itself.

import { api, Rpc, RpcError, type CallOptions } from "./api";
import { formatCount, formatValue, isNumericKind } from "./format";
import type {
  ParamType, QueryKind, QueryParam, QueryResult, SavedQuery, SheetCopy, TableSchema,
} from "./protocol";
import { completionAt, findParams, isInternal, mentionedNames, quoteIdent, type Completion } from "./sqltext";
import type { ServerEvent } from "./types";
import { choiceDialog, confirmDialog, el, formDialog, openModal, showMenu, toast, toastError } from "./ui";

/** Rows a preview asks for; the server never returns more than 5,000. */
const PREVIEW_ROWS = 500;
/** Most suggestions shown at once. */
const MAX_SUGGESTIONS = 20;

const PARAM_TYPES: { value: ParamType; label: string; placeholder: string }[] = [
  { value: "text", label: "Text", placeholder: "text" },
  { value: "integer", label: "Integer", placeholder: "e.g. 42" },
  { value: "decimal", label: "Decimal", placeholder: "e.g. 12.50" },
  { value: "double", label: "Double", placeholder: "e.g. 3.14" },
  { value: "boolean", label: "True / false", placeholder: "true or false" },
  { value: "date", label: "Date", placeholder: "YYYY-MM-DD" },
  { value: "timestamp", label: "Date & time", placeholder: "YYYY-MM-DD HH:MM:SS" },
];

const KIND_LABEL: Record<QueryKind, string> = { query: "Saved query", live: "Live query" };

/** What the workspace needs from the workbook screen. */
export interface SqlHost {
  rpc: Rpc;
  workbookId: string;
  /** Rows a sheet copy holds at most (from /api/config). */
  sheetCopyRows: number;
  /** Text for an empty editor (no draft kept in this browser). */
  sampleSql: string;
  /** A table was created: show it. */
  openTable(name: string): Promise<void>;
  /** A sheet was created: show it and say what was copied. */
  openSheet(copy: SheetCopy): Promise<void>;
  /** Start a browser download of `href` (streamed to disk, not into memory). */
  download(href: string): void;
  /** The workspace was closed. */
  closed(): void;
  /** Show the workspace docked beside the grid instead of as a modal dialog (U.05). The host
   *  calls `onClose` when the person closes the dock; `close` removes it. */
  dock?(panel: HTMLElement, onClose: () => void): { close: () => void };
}

type ParamState = { type: ParamType; value: string; isNull: boolean };
type Draft = { sql: string; params: ({ name: string } & ParamState)[]; saved?: string };
/** The saved query (and version) loaded into the editor. */
type Loaded = { id: string; name: string; kind: QueryKind; version: number; latest: number; sql: string; params: QueryParam[] };
/** The request the workspace is waiting for. */
type Job = { seq: number; id: number; label: string; kind: "query" | "other"; cancelling?: boolean };
type Shown = { sql: string; params: QueryParam[]; result: QueryResult; live: boolean };
type Suggestion = { label: string; insert: string; detail: string };

let workspaceSeq = 0;

function plural(n: number, one: string, many = `${one}s`): string {
  return `${formatCount(n)} ${n === 1 ? one : many}`;
}

function sameParams(a: QueryParam[], b: QueryParam[]): boolean {
  const norm = (p: QueryParam[]) => JSON.stringify(p.map((x) => [x.name, x.type, x.value ?? null]));
  return norm(a) === norm(b);
}

function codeOf(e: unknown): string | undefined {
  return e instanceof RpcError ? e.code : undefined;
}

/** Position of `pos` inside a textarea, relative to its padding box (a hidden mirror element). */
function caretCoords(ta: HTMLTextAreaElement, pos: number): { left: number; top: number; height: number } {
  const cs = getComputedStyle(ta);
  const mirror = document.createElement("div");
  for (const p of ["boxSizing", "width", "paddingTop", "paddingRight", "paddingBottom", "paddingLeft", "borderTopWidth",
    "borderRightWidth", "borderBottomWidth", "borderLeftWidth", "fontFamily", "fontSize", "fontWeight", "lineHeight",
    "letterSpacing", "tabSize"] as const) {
    mirror.style[p] = cs[p];
  }
  Object.assign(mirror.style, { position: "absolute", visibility: "hidden", top: "0", left: "-9999px", whiteSpace: "pre-wrap", overflowWrap: "break-word" });
  mirror.textContent = ta.value.slice(0, pos);
  const mark = document.createElement("span");
  mark.textContent = ta.value.slice(pos, pos + 1) || ".";
  mirror.append(mark);
  document.body.append(mirror);
  const height = parseFloat(cs.lineHeight) || parseFloat(cs.fontSize) * 1.4;
  const out = { left: mark.offsetLeft - ta.scrollLeft, top: mark.offsetTop - ta.scrollTop, height };
  mirror.remove();
  return out;
}

export class SqlWorkspace {
  private rpc: Rpc;
  private uid = `sqlws${++workspaceSeq}`;
  private modal: { close: () => void } | null = null;
  private disposed = false;
  private timers = new Set<number>();

  private schema: TableSchema[] = [];
  private saved: SavedQuery[] = [];
  private loaded: Loaded | null = null;
  private paramState = new Map<string, ParamState>();
  private paramNames: string[] = [];
  private job: Job | null = null;
  /** Bumped by every request; a reply whose number is no longer current is ignored. */
  private runSeq = 0;
  private shown: Shown | null = null;
  /** Expanded tables in the schema tree, and the item that has the tree's tab stop. */
  private expanded = new Set<string>();
  private treeFocus = "";
  private suggestions: Suggestion[] = [];
  private completion: Completion | null = null;
  private activeSuggestion = 0;

  private editor = el("textarea", {
    class: "sql-editor mono", spellcheck: "false", rows: 8, role: "combobox", "aria-label": "SQL query",
    "aria-autocomplete": "list", "aria-expanded": "false",
  });
  private listbox = el("ul", { class: "sql-suggest", role: "listbox", "aria-label": "Suggestions", hidden: true });
  private tree = el("ul", { class: "sql-tree", role: "tree", "aria-label": "Tables and columns" });
  private savedList = el("ul", { class: "sql-saved-list", "aria-label": "Saved queries" });
  private loadedEl = el("div", { class: "sql-loaded muted" });
  private paramsEl = el("div", { class: "sql-params", role: "group", "aria-label": "Parameters", hidden: true });
  private paramError = el("div", { class: "dialog-error sql-param-error", role: "alert" });
  private status = el("span", { class: "muted sql-status", role: "status" });
  private cancelBtn = el("button", { class: "btn sql-cancel", hidden: true, onclick: () => this.cancel() }, "Cancel");
  private liveEl = el("div", { class: "sql-live", hidden: true });
  private noticeEl = el("div", { class: "sql-notice", hidden: true });
  private results = el("div", { class: "sql-results", tabindex: 0, role: "region", "aria-label": "Query results" });

  constructor(private host: SqlHost) {
    this.rpc = host.rpc;
  }

  private get draftKey(): string {
    return `alephgrid:sql-draft:${this.host.workbookId}`;
  }

  // ---------------------------------------------------------------- opening

  open(): void {
    const listId = `${this.uid}-list`;
    this.listbox.id = listId;
    this.editor.setAttribute("aria-controls", listId);
    this.restoreDraft();
    this.editor.addEventListener("keydown", (e) => this.editorKey(e));
    this.editor.addEventListener("input", () => {
      this.syncParams();
      this.saveDraft();
      this.updateLoaded();
      this.complete(false);
    });
    this.editor.addEventListener("blur", () => this.hideSuggestions());
    this.editor.addEventListener("click", () => this.hideSuggestions());
    this.tree.addEventListener("keydown", (e) => this.treeKey(e));
    this.results.replaceChildren(el("p", { class: "muted" }, "Run a query to preview its first rows. Export writes every row to a file."));
    const docked = !!this.host.dock;
    const dialog = el("div", { class: docked ? "sql-workspace sql-docked" : "dialog dialog-wide sql-workspace" },
      el("div", { class: "sql-head" },
        el("h2", {}, "SQL"),
        el("p", { class: "sql-hint" },
          "SQL reads this workbook's tables: joins, group-bys, window functions. Files come in through Import; queries cannot read files or URLs. Use $name for parameters."),
      ),
      el("div", { class: "sql-body" },
        el("aside", { class: "sql-side" },
          el("section", { "aria-labelledby": `${this.uid}-tables` },
            el("h3", { id: `${this.uid}-tables` }, "Tables"),
            this.tree,
            el("small", { class: "muted" }, "Enter or click inserts a name at the cursor."),
          ),
          el("section", { "aria-labelledby": `${this.uid}-saved` },
            el("h3", { id: `${this.uid}-saved` }, "Saved queries"),
            this.savedList,
          ),
        ),
        el("div", { class: "sql-main" },
          this.loadedEl,
          el("div", { class: "sql-editor-wrap" }, this.editor, this.listbox),
          this.paramsEl,
          this.paramError,
          el("div", { class: "dialog-actions sql-actions" },
            el("button", { class: "btn btn-primary", onclick: () => this.run(), title: "⌘/Ctrl + Enter" }, "Run"),
            this.cancelBtn,
            this.status,
            el("div", { class: "spacer" }),
            el("button", { class: "btn", onclick: () => this.saveQuery(false) }, "Save query"),
            el("button", { class: "btn", onclick: () => this.saveQuery(true) }, "Save as…"),
            el("button", { class: "btn", onclick: () => this.exportResult() }, "Export…"),
            el("button", { class: "btn", onclick: () => this.toSheet(), title: `Up to ${formatCount(this.host.sheetCopyRows)} rows` }, "Open in sheet"),
            el("button", { class: "btn", onclick: () => this.toTable() }, "Save as table"),
            docked ? null : el("button", { class: "btn", onclick: () => this.close() }, "Close"),
          ),
          this.liveEl,
          this.noticeEl,
          this.results,
        ),
      ),
    );
    this.modal = this.host.dock ? this.host.dock(dialog, () => this.close()) : openModal(dialog, () => this.close());
    this.syncParams();
    this.updateLoaded();
    this.editor.focus();
    this.loadSchema();
    this.loadSaved();
  }

  /** Close the workspace; whatever it is still running is cancelled. */
  close(): void {
    if (this.disposed) return;
    this.disposed = true;
    if (this.job?.id) this.rpc.cancel(this.job.id).catch(() => {});
    this.job = null;
    for (const t of this.timers) clearTimeout(t);
    this.timers.clear();
    this.modal?.close();
    this.host.closed();
  }

  /** Server events while the workspace is open. */
  onEvent(e: ServerEvent): void {
    if (this.disposed) return;
    switch (e.event) {
      case "tables_changed":
      case "table_changed":
        this.debounce("schema", () => this.loadSchema());
        if (e.event === "table_changed") this.sourceChanged(e.table);
        break;
      case "cells_changed":
        this.sourceChanged(e.table);
        break;
      case "queries_changed":
        this.loadSaved();
        break;
      case "resync":
        this.loadSchema();
        this.loadSaved();
        if (this.shown?.live) this.markStale();
        break;
    }
  }

  private debounceTimers = new Map<string, number>();
  private debounce(key: string, fn: () => void, ms = 150): void {
    const old = this.debounceTimers.get(key);
    if (old) { clearTimeout(old); this.timers.delete(old); }
    const t = window.setTimeout(() => {
      this.timers.delete(t);
      this.debounceTimers.delete(key);
      if (!this.disposed) fn();
    }, ms);
    this.timers.add(t);
    this.debounceTimers.set(key, t);
  }

  // ------------------------------------------------------------------ draft

  /** The unsaved editor text stays in this browser only (per workbook). */
  private saveDraft(): void {
    const draft: Draft = {
      sql: this.editor.value,
      params: [...this.paramState].map(([name, s]) => ({ name, ...s })),
      saved: this.loaded?.id,
    };
    try { localStorage.setItem(this.draftKey, JSON.stringify(draft)); } catch { /* private mode or full */ }
  }

  private restoreDraft(): void {
    let draft: Draft | null = null;
    try { draft = JSON.parse(localStorage.getItem(this.draftKey) ?? "null"); } catch { draft = null; }
    if (!draft || typeof draft.sql !== "string" || !draft.sql.trim()) {
      this.editor.value = this.host.sampleSql;
      return;
    }
    this.editor.value = draft.sql;
    for (const p of Array.isArray(draft.params) ? draft.params : []) {
      if (typeof p?.name === "string") this.paramState.set(p.name, { type: p.type ?? "text", value: p.value ?? "", isNull: !!p.isNull });
    }
    if (draft.saved) this.pendingLoaded = draft.saved;
  }

  /** Saved query id from the draft, resolved once the list has loaded. */
  private pendingLoaded: string | null = null;

  // ------------------------------------------------------------- parameters

  /** One row per `$name` in the editor; values typed earlier are kept. */
  private syncParams(): void {
    const names = findParams(this.editor.value);
    if (names.join("\0") === this.paramNames.join("\0")) return;
    this.paramNames = names;
    this.paramsEl.hidden = names.length === 0;
    this.paramsEl.replaceChildren(
      el("div", { class: "sql-params-title" }, "Parameters"),
      ...names.map((name) => this.paramRow(name)),
    );
  }

  private paramRow(name: string): HTMLElement {
    let state = this.paramState.get(name);
    if (!state) {
      state = { type: "text", value: "", isNull: false };
      this.paramState.set(name, state);
    }
    const s = state;
    const id = `${this.uid}-p-${name}`;
    const type = el("select", { id: `${id}-type`, "aria-label": `Type of $${name}` },
      ...PARAM_TYPES.map((t) => el("option", { value: t.value }, t.label)));
    type.value = s.type;
    const value = el("input", { id: `${id}-value`, class: "mono", spellcheck: false, "aria-label": `Value of $${name}`,
      placeholder: PARAM_TYPES.find((t) => t.value === s.type)?.placeholder ?? "" });
    value.value = s.value;
    value.disabled = s.isNull;
    const isNull = el("input", { type: "checkbox", id: `${id}-null` });
    isNull.checked = s.isNull;
    type.addEventListener("change", () => {
      s.type = type.value as ParamType;
      value.placeholder = PARAM_TYPES.find((t) => t.value === s.type)?.placeholder ?? "";
      this.saveDraft();
    });
    value.addEventListener("input", () => { s.value = value.value; this.saveDraft(); });
    value.addEventListener("keydown", (e) => {
      if (e.key === "Enter") { e.preventDefault(); this.run(); }
    });
    isNull.addEventListener("change", () => {
      s.isNull = isNull.checked;
      value.disabled = s.isNull;
      this.saveDraft();
    });
    return el("div", { class: "sql-param-row" },
      el("span", { class: "sql-param-name mono" }, `$${name}`),
      type,
      value,
      el("label", { class: "sql-param-null", for: `${id}-null` }, isNull, "NULL"),
    );
  }

  /** The parameters to send: values as typed (exact strings); no value is SQL NULL. */
  private params(): QueryParam[] {
    return this.paramNames.map((name) => {
      const s = this.paramState.get(name) ?? { type: "text" as const, value: "", isNull: false };
      return s.isNull ? { name, type: s.type } : { name, type: s.type, value: s.value };
    });
  }

  private setParams(params: QueryParam[]): void {
    for (const p of params) this.paramState.set(p.name, { type: p.type, value: p.value ?? "", isNull: p.value === undefined || p.value === null });
    this.paramNames = [];
    this.syncParams();
  }

  // ------------------------------------------------------------------ jobs

  /**
   * Run `work` as the workspace's current request: status, progress and Cancel follow it.
   * A preview still running is cancelled first (its reply is then ignored); another kind
   * of request must finish or be cancelled before a new one starts.
   */
  private async track<T>(label: string, kind: Job["kind"], work: (opts: CallOptions) => Promise<T>): Promise<{ seq: number; value: T } | { seq: number; error: unknown }> {
    const seq = ++this.runSeq;
    const job: Job = { seq, id: 0, label, kind };
    this.job = job;
    this.status.textContent = `${label}…`;
    this.cancelBtn.hidden = false;
    this.cancelBtn.disabled = false;
    this.cancelBtn.removeAttribute("aria-busy");
    try {
      const value = await work({
        onSent: (id) => { job.id = id; },
        onProgress: (pct) => { if (this.job === job && !job.cancelling) this.status.textContent = `${label}… ${Math.round(pct)}%`; },
      });
      return { seq, value };
    } catch (error) {
      return { seq, error };
    } finally {
      if (this.job === job) {
        this.job = null;
        this.cancelBtn.hidden = true;
      }
    }
  }

  /** Can a new request start? A running preview is superseded; anything else is not. */
  private claim(): boolean {
    if (this.disposed) return false;
    if (!this.job) return true;
    if (this.job.kind === "query") {
      if (this.job.id) this.rpc.cancel(this.job.id).catch(() => {});
      this.job = null;
      return true;
    }
    this.inPane(new Error(`Wait for “${this.job.label}” to finish, or cancel it`));
    return false;
  }

  /** U.11a: a cancel is pending until the server answers the request (with "cancelled", or
   *  its result if it finished first): until then it says Cancelling… and cannot be repeated. */
  private cancel(): void {
    const job = this.job;
    if (!job?.id || job.cancelling) return;
    job.cancelling = true;
    this.status.textContent = "Cancelling…";
    this.cancelBtn.disabled = true;
    this.cancelBtn.setAttribute("aria-busy", "true");
    this.rpc.cancel(job.id).catch(() => {});
  }

  private current(seq: number): boolean {
    return !this.disposed && seq === this.runSeq;
  }

  /** An error reply: parameters next to the editor, everything else in the results. */
  /** U.11a: an error of something done in the workspace shows next to Run, not only in a toast;
   *  the results shown stay. */
  private inPane(e: unknown): void {
    toastError(e);
    this.status.textContent = e instanceof Error ? e.message : String(e);
  }

  private showError(e: unknown): void {
    const message = (e as Error).message;
    if (codeOf(e) === "cancelled") {
      this.status.textContent = "Cancelled.";
      return;
    }
    this.status.textContent = "";
    if (codeOf(e) === "bad_parameter") {
      this.paramError.textContent = message;
      return;
    }
    this.noticeEl.hidden = true;
    this.liveEl.hidden = true;
    this.shown = null;
    this.results.replaceChildren(el("pre", { class: "sql-error" }, message));
  }

  // -------------------------------------------------------------------- run

  /** Preview the statement; a change (INSERT/UPDATE/DELETE) is applied only after confirmation. */
  async run(live = false): Promise<void> {
    if (!this.claim()) return;
    this.hideSuggestions();
    const sql = this.editor.value;
    const params = this.params();
    this.paramError.textContent = "";
    const out = await this.track("Running", "query", (opts) => this.rpc.request("query", { sql, params, limit: PREVIEW_ROWS }, opts));
    if (!this.current(out.seq)) return;
    if ("error" in out) {
      if (codeOf(out.error) === "sql_change") await this.applyChange(sql, params);
      else this.showError(out.error);
      return;
    }
    this.showResult(sql, params, out.value, live);
  }

  private showResult(sql: string, params: QueryParam[], r: QueryResult, live: boolean): void {
    this.shown = { sql, params, result: r, live };
    const n = r.rows.length;
    this.status.textContent = `${formatCount(n)}${r.truncated ? "+" : ""} ${n === 1 && !r.truncated ? "row" : "rows"} · ${formatCount(Math.round(r.elapsed_ms))} ms · revision ${r.revision}`;
    this.noticeEl.hidden = !r.truncated;
    this.noticeEl.textContent = r.truncated
      ? `Showing the first ${formatCount(n)} rows — the result has more. Export to get every row.`
      : "";
    this.renderLive(false);
    if (!r.columns.length) {
      this.results.replaceChildren(el("p", { class: "muted" }, "The query returned no columns."));
      return;
    }
    const table = el("table", { class: "result-table" },
      el("thead", {}, el("tr", {}, ...r.columns.map((c) => el("th", { scope: "col", title: c.sql_type }, c.name)))),
      el("tbody", {}, ...r.rows.map((row) => el("tr", {}, ...row.map((v, i) => {
        const kind = r.columns[i].kind;
        return v === null
          ? el("td", { class: "null" }, "NULL")
          : el("td", { class: isNumericKind(kind) ? "num" : "" }, formatValue(v, kind));
      })))),
    );
    this.results.replaceChildren(table, ...(n === 0 ? [el("p", { class: "muted" }, "No rows.")] : []));
  }

  /** The live-query line: which revision was read, and whether a source changed since. */
  private renderLive(stale: boolean): void {
    const s = this.shown;
    if (!s?.live) {
      this.liveEl.hidden = true;
      return;
    }
    this.liveEl.hidden = false;
    this.liveEl.classList.toggle("stale", stale);
    this.liveEl.replaceChildren(
      el("span", {}, `Live · read at revision ${s.result.revision}`),
      ...(stale ? [
        el("strong", { class: "sql-stale" }, "Stale — the source changed"),
        el("button", { class: "btn", onclick: () => this.run(true) }, "Refresh result"),
      ] : []),
    );
  }

  private sourceChanged(table: string): void {
    const s = this.shown;
    if (!s?.live) return;
    const t = table.toLowerCase();
    if (s.result.sources.some((x) => x.table.toLowerCase() === t)) this.markStale();
  }

  private markStale(): void {
    this.renderLive(true);
  }

  /** The statement changes data: say so, and apply it only if the user agrees. */
  private async applyChange(sql: string, params: QueryParam[]): Promise<void> {
    this.status.textContent = "";
    const keyword = /^\s*(?:--[^\n]*\n\s*|\/\*[\s\S]*?\*\/\s*)*(\w+)/.exec(sql)?.[1]?.toUpperCase() ?? "This statement";
    const ok = await confirmDialog(
      "Apply this change?",
      `This statement changes table data (${keyword}). It is applied as one change of the workbook, recorded in its history, and other people see it at once. Apply it?`,
      "Apply change",
    );
    if (this.disposed) return;
    if (!ok) {
      this.status.textContent = "Not applied — nothing changed.";
      return;
    }
    if (!this.claim()) return;
    const out = await this.track("Applying change", "other", (opts) => this.rpc.request("sql_change", { sql, params }, opts));
    if (!this.current(out.seq)) return;
    if ("error" in out) {
      this.showError(out.error);
      return;
    }
    const c = out.value;
    const rows = plural(c.rows, "row");
    const what = c.kind === "insert" ? `Inserted ${rows} into ${c.table}`
      : c.kind === "delete" ? `Deleted ${rows} from ${c.table}`
      : `Updated ${rows} in ${c.table}`;
    this.status.textContent = what;
    this.shown = null;
    this.noticeEl.hidden = true;
    this.liveEl.hidden = true;
    this.results.replaceChildren(el("p", { class: "sql-applied" }, `${what}. Run a SELECT to see the data.`));
    toast(what, "success");
  }

  // ---------------------------------------------------------------- export

  private async exportResult(): Promise<void> {
    const v = await formDialog("Export the query result", [
      { name: "format", label: "Format", type: "select", value: "csv", options: [
        { value: "csv", label: "CSV" }, { value: "csv_excel", label: "CSV for Excel or Sheets" }, { value: "parquet", label: "Parquet" }, { value: "json", label: "JSON lines" },
      ], hint: "Writes every row of the result (not only the preview) from one consistent snapshot, then downloads the file once. \"CSV for Excel or Sheets\" puts a ' before text that a spreadsheet would run as a formula." },
    ], "Export");
    if (!v || !this.claim()) return;
    const sql = this.editor.value;
    const params = this.params();
    this.paramError.textContent = "";
    const format = v.format as "csv" | "csv_excel" | "parquet" | "json";
    const out = await this.track("Exporting", "other", (opts) => this.rpc.request("export_query", { sql, params, format }, opts));
    if (!this.current(out.seq)) return;
    if ("error" in out) {
      this.showError(out.error);
      return;
    }
    const x = out.value;
    this.host.download(api.downloadUrl(this.host.workbookId, x.download));
    const s = this.shown;
    const changed = s && s.sql === sql && sameParams(s.params, params) && s.result.revision !== x.revision
      ? ` The data changed since the preview (revision ${s.result.revision}); the file holds revision ${x.revision}.`
      : "";
    const msg = `Exported ${plural(x.rows, "row")} (revision ${x.revision})`;
    this.status.textContent = msg;
    toast(`${msg}.${changed}`, "success", changed ? 10_000 : 4000);
  }

  // ------------------------------------------------------- table and sheet

  /** The saved query version the editor holds unchanged, to record where a table came from. */
  private origin(sql: string, params: QueryParam[]): { saved?: string; version?: number } {
    const l = this.loaded;
    return l && l.sql === sql && sameParams(l.params, params) ? { saved: l.id, version: l.version } : {};
  }

  private async toTable(): Promise<void> {
    const v = await formDialog("Save result as a table", [{ name: "name", label: "Table name", value: "query_result",
      hint: "Keeps every row of the result as a new table: a snapshot that records the query, so it can be refreshed later. It does not change by itself when the sources do." }],
    "Create table");
    if (!v || !this.claim()) return;
    const sql = this.editor.value;
    const params = this.params();
    this.paramError.textContent = "";
    const out = await this.track("Creating table", "other", (opts) =>
      this.rpc.request("create_table_from_query", { name: v.name, sql, params, ...this.origin(sql, params) }, opts));
    if (!this.current(out.seq)) return;
    if ("error" in out) {
      if (codeOf(out.error) === "cancelled") this.status.textContent = "Cancelled.";
      else if (codeOf(out.error) === "bad_parameter") this.showError(out.error);
      else this.inPane(out.error);
      return;
    }
    const info = out.value;
    this.close();
    await this.host.openTable(info.name);
    toast(`Created ${info.name} (${plural(info.row_count, "row")}, a snapshot of the query)`, "success");
  }

  private async toSheet(): Promise<void> {
    const limit = formatCount(this.host.sheetCopyRows);
    const v = await formDialog("Open result in a new sheet", [{ name: "name", label: "Sheet name", value: "Query result",
      hint: `Copies at most the first ${limit} rows (fewer for very wide results). If the result has more, you are asked before anything is left out. Use “Save as table” to keep every row.` }],
    "Create sheet");
    if (!v) return;
    await this.copyToSheet(v.name, false);
  }

  private async copyToSheet(name: string, allowTruncation: boolean): Promise<void> {
    if (!this.claim()) return;
    const sql = this.editor.value;
    const params = this.params();
    this.paramError.textContent = "";
    const out = await this.track("Creating sheet", "other", (opts) =>
      this.rpc.request("create_sheet_from_query", { name, sql, params, allow_truncation: allowTruncation }, opts));
    if (!this.current(out.seq)) return;
    if ("error" in out) {
      const code = codeOf(out.error);
      if (code === "too_large" && !allowTruncation) {
        this.status.textContent = "";
        const n = /at most (\d+) rows/.exec((out.error as Error).message)?.[1];
        const rows = formatCount(n ? Number(n) : this.host.sheetCopyRows);
        const choice = await choiceDialog(
          `Copy only the first ${rows} rows?`,
          `The result has more than ${rows} rows, and a sheet holds at most ${rows} rows of it. Copying would leave the other rows out. To keep every row, save the result as a table instead.`,
          [{ value: "table", label: "Save as table instead" }, { value: "truncate", label: `Copy first ${rows} rows`, primary: true }],
        );
        if (this.disposed) return;
        if (choice === "truncate") await this.copyToSheet(name, true);
        else if (choice === "table") await this.toTable();
        else this.status.textContent = "Nothing was copied.";
        return;
      }
      if (code === "cancelled") this.status.textContent = "Cancelled.";
      else if (code === "bad_parameter") this.showError(out.error);
      else this.inPane(out.error);
      return;
    }
    this.close();
    await this.host.openSheet(out.value);
  }

  // ---------------------------------------------------------- saved queries

  /** Open saved query `id` (from the workbook explorer), once the list has loaded. */
  async openQuery(id: string): Promise<void> {
    if (!this.saved.some((q) => q.id === id)) await this.loadSaved();
    const q = this.saved.find((x) => x.id === id);
    if (q) this.openSaved(q);
  }

  private async loadSaved(): Promise<void> {
    try {
      this.saved = await this.rpc.call("saved_queries");
    } catch {
      return;
    }
    if (this.disposed) return;
    if (this.pendingLoaded) {
      const q = this.saved.find((x) => x.id === this.pendingLoaded);
      this.pendingLoaded = null;
      if (q) this.loaded = { id: q.id, name: q.name, kind: q.kind, version: q.version, latest: q.version, sql: q.sql, params: q.params };
    }
    if (this.loaded) {
      const q = this.saved.find((x) => x.id === this.loaded!.id);
      if (!q) this.loaded = null;
      else Object.assign(this.loaded, { name: q.name, kind: q.kind, latest: q.version });
    }
    this.renderSaved();
    this.updateLoaded();
  }

  private renderSaved(): void {
    if (!this.saved.length) {
      this.savedList.replaceChildren(el("li", { class: "muted sql-empty" }, "None yet. Save a query to keep it in this workbook."));
      return;
    }
    this.savedList.replaceChildren(...this.saved.map((q) => {
      const more = el("button", { class: "icon-btn sql-saved-more", "aria-label": `More actions for ${q.name}`, title: "More actions",
        "aria-haspopup": "menu", onclick: () => {
          const r = more.getBoundingClientRect();
          showMenu([
            { label: "Open", action: () => this.openSaved(q) },
            { label: "Versions…", action: () => this.showVersions(q) },
            { label: "Rename…", action: () => this.renameSaved(q) },
            "sep",
            { label: "Delete", danger: true, action: () => this.deleteSaved(q) },
          ], r.left, r.bottom + 4);
        } }, "⋯");
      return el("li", { class: `sql-saved${this.loaded?.id === q.id ? " current" : ""}` },
        el("button", { class: "sql-saved-open", title: q.kind === "live" ? "Live query: opening it runs it on current data" : "Saved query: opening it loads the SQL",
          onclick: () => this.openSaved(q) },
          el("span", { class: "sql-saved-name" }, q.name),
          el("span", { class: `kind-badge kind-${q.kind}` }, KIND_LABEL[q.kind]),
          el("span", { class: "muted sql-saved-version" }, `v${q.version}`),
        ),
        more,
      );
    }));
  }

  /** "Editing …" line above the editor. */
  private updateLoaded(): void {
    const l = this.loaded;
    if (!l) {
      this.loadedEl.textContent = "Unsaved query · the draft is kept in this browser only";
      return;
    }
    const changed = l.sql !== this.editor.value || !sameParams(l.params, this.params());
    const old = l.version !== l.latest ? ` (latest is v${l.latest})` : "";
    this.loadedEl.replaceChildren(
      el("span", {}, `${KIND_LABEL[l.kind]} “${l.name}” · v${l.version}${old}${changed ? " · edited, not saved" : ""}`),
    );
  }

  private openSaved(q: { id: string; name: string; kind: QueryKind; version: number; latest?: number; sql: string; params: QueryParam[] }): void {
    if (this.disposed) return;
    this.editor.value = q.sql;
    this.setParams(q.params);
    this.loaded = { id: q.id, name: q.name, kind: q.kind, version: q.version, latest: q.latest ?? q.version, sql: q.sql, params: q.params };
    this.saveDraft();
    this.updateLoaded();
    this.renderSaved();
    this.paramError.textContent = "";
    this.editor.focus();
    if (q.kind === "live") {
      this.run(true);
    } else {
      this.shown = null;
      this.noticeEl.hidden = true;
      this.liveEl.hidden = true;
      this.status.textContent = `Loaded “${q.name}” (v${q.version}).`;
      this.results.replaceChildren(el("p", { class: "muted" }, "A saved query keeps the SQL only. Run it to see current results."));
    }
  }

  private async saveQuery(asNew: boolean): Promise<void> {
    const sql = this.editor.value;
    const params = this.params();
    const l = this.loaded;
    if (l && !asNew) {
      try {
        const q = await this.rpc.call("save_query", { saved: l.id, name: l.name, kind: l.kind, sql, params });
        const fresh = q.version > l.latest;
        this.loaded = { id: q.id, name: q.name, kind: q.kind, version: q.version, latest: q.version, sql: q.sql, params: q.params };
        toast(fresh ? `Saved “${q.name}” as version ${q.version}` : `No changes since version ${q.version} of “${q.name}”`, "success");
        this.afterSave();
      } catch (e) {
        this.inPane(e);
      }
      return;
    }
    let savedQuery: SavedQuery | null = null;
    const v = await formDialog(asNew ? "Save as a new query" : "Save query", [
      { name: "name", label: "Name", value: l && asNew ? `${l.name} copy` : "",
        hint: "Saved in this workbook — visible to anyone who can open it. Your unsaved draft stays in this browser only." },
      { name: "kind", label: "Kind", type: "select", value: l?.kind ?? "query", options: [
        { value: "query", label: "Saved query — keeps the SQL; opening it loads the text" },
        { value: "live", label: "Live query — opening it runs it on current data" },
      ], hint: "Neither is a copy of the data: to keep a snapshot of the rows, use “Save as table”." },
    ], "Save", async (vals) => {
      if (!vals.name.trim()) return "Give the query a name";
      try {
        savedQuery = await this.rpc.call("save_query", { name: vals.name.trim(), kind: vals.kind as QueryKind, sql, params });
        return null;
      } catch (e) {
        return (e as Error).message;
      }
    });
    const q = savedQuery as SavedQuery | null;
    if (!v || !q || this.disposed) return;
    this.loaded = { id: q.id, name: q.name, kind: q.kind, version: q.version, latest: q.version, sql: q.sql, params: q.params };
    toast(`Saved “${q.name}” (${KIND_LABEL[q.kind].toLowerCase()}, version ${q.version})`, "success");
    this.afterSave();
  }

  private afterSave(): void {
    this.saveDraft();
    this.updateLoaded();
    this.loadSaved();
  }

  private async renameSaved(q: SavedQuery): Promise<void> {
    await formDialog("Rename saved query", [{ name: "name", label: "Name", value: q.name }], "Rename", async (vals) => {
      try {
        await this.rpc.call("rename_query", { saved: q.id, name: vals.name.trim() });
        return null;
      } catch (e) {
        return (e as Error).message;
      }
    });
    await this.loadSaved();
  }

  private async deleteSaved(q: SavedQuery): Promise<void> {
    const ok = await confirmDialog(`Delete the saved query “${q.name}”?`,
      "Its SQL and every version are removed from this workbook. Tables made from it keep their rows.");
    if (!ok) return;
    try {
      await this.rpc.call("delete_query", { saved: q.id });
      if (this.loaded?.id === q.id) this.loaded = null;
      this.saveDraft();
      await this.loadSaved();
    } catch (e) {
      this.inPane(e);
    }
  }

  private async showVersions(q: SavedQuery): Promise<void> {
    let versions;
    try {
      versions = await this.rpc.call("query_versions", { saved: q.id });
    } catch (e) {
      this.inPane(e);
      return;
    }
    if (this.disposed) return;
    const closeBtn = el("button", { class: "btn btn-primary", onclick: () => modal.close() }, "Close");
    const rows = [...versions].reverse().map((v) =>
      el("tr", {},
        el("th", { scope: "row" }, `v${v.version}`),
        el("td", {}, new Date(v.created_at).toLocaleString()),
        el("td", { class: "muted" }, `revision ${v.revision}`),
        el("td", { class: "mono sql-version-sql", title: v.sql }, v.sql.length > 80 ? `${v.sql.slice(0, 80)}…` : v.sql),
        el("td", {}, el("button", { class: "btn", "aria-label": `Open version ${v.version}`, onclick: () => {
          modal.close();
          this.openSaved({ id: q.id, name: q.name, kind: q.kind, version: v.version, latest: q.version, sql: v.sql, params: v.params });
        } }, "Open")),
      ));
    const modal = openModal(
      el("div", { class: "dialog dialog-wide" },
        el("h2", {}, `Versions of “${q.name}”`),
        el("p", {}, "Every saved change of the SQL or its parameters is a version; versions never change. Opening an old one and saving makes it the newest version."),
        el("div", { class: "sql-versions" },
          el("table", { class: "result-table" },
            el("thead", {}, el("tr", {}, ...["Version", "Saved", "Revision", "SQL", ""].map((h) => el("th", { scope: "col" }, h)))),
            el("tbody", {}, ...rows),
          ),
        ),
        el("div", { class: "dialog-actions" }, closeBtn),
      ),
      () => modal.close(),
    );
    closeBtn.focus();
  }

  // ----------------------------------------------------------------- schema

  private async loadSchema(): Promise<void> {
    let schema: TableSchema[];
    try {
      schema = await this.rpc.call("schema");
    } catch {
      return;
    }
    if (this.disposed) return;
    this.schema = schema.filter((t) => !isInternal(t.name)).map((t) => ({ ...t, columns: t.columns.filter((c) => !isInternal(c.name)) }));
    this.renderTree();
  }

  private renderTree(): void {
    const hadFocus = this.tree.contains(document.activeElement);
    if (!this.schema.length) {
      this.tree.replaceChildren(el("li", { role: "none", class: "muted sql-empty" }, "No tables yet. Import a file to query it."));
      return;
    }
    const known = new Set(this.schema.map((t) => t.name));
    for (const name of [...this.expanded]) if (!known.has(name)) this.expanded.delete(name);
    // Each tree item is a row named by its own text ("sales 4 rows", "id BIGINT"); a
    // table's columns are a group next to it, at aria-level 2.
    this.tree.replaceChildren(...this.schema.map((t) => {
      const open = this.expanded.has(t.name);
      const item = el("div", { class: "sql-tree-row", role: "treeitem", "aria-expanded": open ? "true" : "false", "aria-level": "1",
        "data-key": `t:${t.name}`, "data-insert": quoteIdent(t.name), tabindex: -1, title: "Insert at the cursor" },
      el("span", { class: "sql-twisty", "aria-hidden": "true", title: open ? "Collapse" : "Expand",
        onclick: (e: Event) => { e.stopPropagation(); this.toggle(t.name); } }, open ? "▾" : "▸"),
      el("span", { class: "sql-tree-name" }, t.name),
      el("span", { class: "muted sql-tree-detail" }, plural(t.row_count, "row")),
      );
      item.addEventListener("click", () => this.insertFromTree(item));
      return el("li", { role: "none" },
        item,
        el("ul", { role: "group", hidden: !open, "aria-label": `Columns of ${t.name}` }, ...t.columns.map((c) => {
          const col = el("div", { class: "sql-tree-row", role: "treeitem", "aria-level": "2", tabindex: -1,
            "data-key": `c:${t.name}\0${c.name}`, "data-insert": quoteIdent(c.name), title: "Insert at the cursor" },
          el("span", { class: "sql-tree-name" }, c.name),
          c.formula ? el("span", { class: "sql-formula", title: `Formula column: ${c.formula}`, "aria-hidden": "true" }, "ƒ") : null,
          el("span", { class: "muted sql-tree-detail" }, c.sql_type),
          c.formula ? el("span", { class: "sr-only" }, "formula column") : null,
          );
          col.addEventListener("click", () => this.insertFromTree(col));
          return el("li", { role: "none" }, col);
        })),
      );
    }));
    const items = this.visibleItems();
    const target = items.find((i) => i.dataset.key === this.treeFocus) ?? items[0];
    if (target) target.tabIndex = 0;
    if (hadFocus) target?.focus();
  }

  /** Tree items that are visible (columns of collapsed tables are not). */
  private visibleItems(): HTMLElement[] {
    return [...this.tree.querySelectorAll<HTMLElement>('[role="treeitem"]')].filter((i) => !i.closest('[role="group"][hidden]'));
  }

  private focusItem(item: HTMLElement | undefined): void {
    if (!item) return;
    for (const i of this.tree.querySelectorAll<HTMLElement>('[role="treeitem"]')) i.tabIndex = -1;
    item.tabIndex = 0;
    this.treeFocus = item.dataset.key ?? "";
    item.focus();
  }

  private toggle(table: string, open?: boolean): void {
    const now = open ?? !this.expanded.has(table);
    if (now) this.expanded.add(table);
    else this.expanded.delete(table);
    this.treeFocus = `t:${table}`;
    this.renderTree();
  }

  private treeKey(e: KeyboardEvent): void {
    const items = this.visibleItems();
    const item = (e.target as HTMLElement).closest<HTMLElement>('[role="treeitem"]');
    if (!item) return;
    const i = items.indexOf(item);
    const isTable = item.getAttribute("aria-level") === "1";
    const table = isTable ? item : item.closest('[role="group"]')?.previousElementSibling as HTMLElement | null ?? null;
    const tableName = (table?.dataset.key ?? "").slice(2);
    switch (e.key) {
      case "ArrowDown": this.focusItem(items[Math.min(items.length - 1, i + 1)]); break;
      case "ArrowUp": this.focusItem(items[Math.max(0, i - 1)]); break;
      case "Home": this.focusItem(items[0]); break;
      case "End": this.focusItem(items[items.length - 1]); break;
      case "ArrowRight":
        if (isTable && !this.expanded.has(tableName)) {
          this.toggle(tableName, true);
          this.focusItem(this.visibleItems().find((x) => x.dataset.key === `t:${tableName}`));
        } else if (isTable) {
          this.focusItem(items[i + 1]?.getAttribute("aria-level") === "2" ? items[i + 1] : item);
        }
        break;
      case "ArrowLeft":
        if (isTable && this.expanded.has(tableName)) {
          this.toggle(tableName, false);
          this.focusItem(this.visibleItems().find((x) => x.dataset.key === `t:${tableName}`));
        } else if (!isTable && table) {
          this.focusItem(table);
        }
        break;
      case "Enter":
      case " ":
        this.insertFromTree(item);
        break;
      default:
        return;
    }
    e.preventDefault();
    e.stopPropagation();
  }

  /** Insert the item's quoted name at the editor's cursor and go back to typing. */
  private insertFromTree(item: HTMLElement): void {
    const text = item.dataset.insert;
    if (!text) return;
    this.treeFocus = item.dataset.key ?? "";
    this.insertAt(text, this.editor.selectionStart, this.editor.selectionEnd);
  }

  private insertAt(text: string, start: number, end: number): void {
    this.editor.focus();
    this.editor.setRangeText(text, start, end, "end");
    this.editor.dispatchEvent(new Event("input"));
    this.hideSuggestions();
  }

  // ------------------------------------------------------------- completion

  private editorKey(e: KeyboardEvent): void {
    if ((e.metaKey || e.ctrlKey) && e.key === "Enter") {
      e.preventDefault();
      this.run();
      return;
    }
    if ((e.metaKey || e.ctrlKey) && e.key === " ") {
      e.preventDefault();
      this.complete(true);
      return;
    }
    if (this.listbox.hidden) return;
    switch (e.key) {
      case "ArrowDown":
        this.setActive((this.activeSuggestion + 1) % this.suggestions.length);
        break;
      case "ArrowUp":
        this.setActive((this.activeSuggestion - 1 + this.suggestions.length) % this.suggestions.length);
        break;
      case "Enter":
      case "Tab":
        this.accept(this.activeSuggestion);
        break;
      case "Escape":
        this.hideSuggestions();
        break;
      default:
        return;
    }
    e.preventDefault();
    e.stopPropagation();
  }

  /** Offer names for the identifier before the cursor (`explicit`: Ctrl+Space). */
  private complete(explicit: boolean): void {
    const ta = this.editor;
    const c = ta.selectionStart === ta.selectionEnd ? completionAt(ta.value, ta.selectionStart) : null;
    if (!c) {
      this.hideSuggestions();
      return;
    }
    const list = this.suggest(c, ta.value);
    // Nothing to add when the only match is what is already typed.
    if (!list.length || (!explicit && list.length === 1 && list[0].insert === ta.value.slice(c.start, ta.selectionStart))) {
      this.hideSuggestions();
      return;
    }
    this.completion = c;
    this.suggestions = list;
    this.listbox.replaceChildren(...list.map((s, i) =>
      el("li", { role: "option", id: `${this.uid}-opt-${i}`, "aria-selected": "false",
        onmousedown: (e: MouseEvent) => { e.preventDefault(); this.accept(i); } },
      el("span", { class: "sql-suggest-label" }, s.label),
      el("span", { class: "muted sql-suggest-detail" }, s.detail))));
    this.listbox.hidden = false;
    this.editor.setAttribute("aria-expanded", "true");
    this.setActive(0);
    this.placeListbox(c.start);
  }

  private suggest(c: Completion, sql: string): Suggestion[] {
    const p = c.prefix.toLowerCase();
    const match = (name: string) => !isInternal(name) && name.toLowerCase().startsWith(p);
    const out: Suggestion[] = [];
    const seen = new Set<string>();
    const add = (label: string, detail: string) => {
      if (seen.has(label) || out.length >= MAX_SUGGESTIONS) return;
      seen.add(label);
      out.push({ label, insert: quoteIdent(label), detail });
    };
    const tables = this.schema.filter((t) => match(t.name));
    const columnsOf = (list: TableSchema[]) => {
      for (const t of list) for (const col of t.columns) if (match(col.name)) add(col.name, `${col.sql_type} · ${t.name}`);
    };
    if (c.qualifier) {
      const q = c.qualifier.toLowerCase();
      const t = this.schema.filter((x) => x.name.toLowerCase() === q);
      columnsOf(t.length ? t : this.schema);
      return out;
    }
    if (c.afterTableKeyword) {
      for (const t of tables) add(t.name, `table · ${formatCount(t.row_count)} rows`);
      return out;
    }
    const mentioned = mentionedNames(sql);
    const inUse = this.schema.filter((t) => mentioned.has(t.name.toLowerCase()));
    columnsOf(inUse);
    for (const t of tables) add(t.name, `table · ${formatCount(t.row_count)} rows`);
    columnsOf(this.schema.filter((t) => !inUse.includes(t)));
    return out;
  }

  private setActive(i: number): void {
    this.activeSuggestion = i;
    [...this.listbox.children].forEach((o, j) => {
      o.setAttribute("aria-selected", j === i ? "true" : "false");
      o.classList.toggle("active", j === i);
      if (j === i) (o as HTMLElement).scrollIntoView?.({ block: "nearest" });
    });
    this.editor.setAttribute("aria-activedescendant", `${this.uid}-opt-${i}`);
  }

  private accept(i: number): void {
    const s = this.suggestions[i];
    const c = this.completion;
    if (!s || !c) return;
    this.insertAt(s.insert, c.start, this.editor.selectionStart);
  }

  private hideSuggestions(): void {
    if (this.listbox.hidden) return;
    this.listbox.hidden = true;
    this.listbox.replaceChildren();
    this.suggestions = [];
    this.completion = null;
    this.editor.setAttribute("aria-expanded", "false");
    this.editor.removeAttribute("aria-activedescendant");
  }

  private placeListbox(at: number): void {
    const ta = this.editor;
    const pos = caretCoords(ta, at);
    const wrap = ta.parentElement!;
    const left = Math.max(0, Math.min(ta.offsetLeft + pos.left, wrap.clientWidth - 260));
    const top = ta.offsetTop + Math.min(pos.top + pos.height + 4, ta.clientHeight);
    this.listbox.style.left = `${left}px`;
    this.listbox.style.top = `${top}px`;
  }
}
