// U.17: the find bar (⌘F) anchored to the top right of the grid, with match count,
// previous / next and match case; ⌘H opens its replace row, which uses the table's
// preview-then-apply replacement with undo. The provider says where to look: a table view
// (searched on the server over every row) or, with S.04, a sheet.

import { clearError, el, toast, toastError } from "./ui";
import { icon } from "./icons";
import { formatCount } from "./format";
import type { FindHit, FindResult } from "./protocol";

export interface FindProvider {
  label: string;
  /** Sheets offer whole-cell and in-formulas matching (S.04). */
  sheetOptions?: boolean;
  find(text: string, matchCase: boolean, from: number, backwards: boolean, count: boolean): Promise<FindResult>;
  active(): { r: number; c: number };
  go(row: number, column: number): void;
  columnName?(c: number): string;
  /** All sheets (S.04): go to the first (or last) match of the next sheet that has one;
   *  false when no other sheet matches. */
  nextSheet?(backwards: boolean): Promise<boolean>;
  replace?: {
    /** Tables replace in one column; sheets in every matching cell (no column). */
    columns?(): string[];
    preview(column: string, find: string, replacement: string): Promise<number>;
    apply(): Promise<string>;
    undo(): Promise<string>;
    /** Replace in the current match only (sheets). */
    one?(find: string, replacement: string, matchCase: boolean): Promise<void>;
  };
}

export class FindBar {
  readonly el: HTMLElement;
  private input = el("input", { type: "search", class: "find-input", placeholder: "Find", "aria-label": "Find" }) as HTMLInputElement;
  private count = el("span", { class: "find-count", role: "status", "aria-live": "polite" });
  private caseBtn = el("button", { class: "icon-btn find-case", type: "button", title: "Match case", "aria-label": "Match case", "aria-pressed": "false" }, "Aa");
  private replaceRow = el("div", { class: "find-replace", hidden: true });
  private column = el("select", { "aria-label": "Replace in column" }) as HTMLSelectElement;
  private replacement = el("input", { type: "text", placeholder: "Replace with", "aria-label": "Replace with" }) as HTMLInputElement;
  private applyBtn = el("button", { class: "btn btn-primary", type: "button", disabled: true }, "Replace all");
  private matchCase = false;
  private oneBtn: HTMLElement | null = null;
  /** Sheet options, read by the sheet provider. */
  whole = false;
  formulas = false;
  allSheets = false;
  private allBtn = el("button", { class: "icon-btn find-case", type: "button", title: "All sheets", "aria-label": "All sheets", "aria-pressed": "false", hidden: true }, "⧉");
  private wholeBtn = el("button", { class: "icon-btn find-case", type: "button", title: "Whole cell", "aria-label": "Whole cell", "aria-pressed": "false", hidden: true }, "[ ]");
  private formulaBtn = el("button", { class: "icon-btn find-case", type: "button", title: "In formulas", "aria-label": "In formulas", "aria-pressed": "false", hidden: true }, "ƒ");
  private seq = 0;
  private timer = 0;
  private provider: FindProvider | null = null;

  /** The text in the "Replace with" box. */
  get replacementText(): string {
    return this.replacement.value;
  }

  constructor(private onClose: () => void) {
    const prev = el("button", { class: "icon-btn", type: "button", title: "Previous match (Shift+Enter)", "aria-label": "Previous match" }, icon("chevron", 14));
    prev.classList.add("find-prev");
    const next = el("button", { class: "icon-btn", type: "button", title: "Next match (Enter)", "aria-label": "Next match" }, icon("chevron", 14));
    next.classList.add("find-next");
    const toggle = el("button", { class: "icon-btn", type: "button", title: "Replace (⌘H)", "aria-label": "Show replace", "aria-expanded": "false" }, "⇄");
    const close = el("button", { class: "icon-btn", type: "button", title: "Close (Esc)", "aria-label": "Close find" }, icon("x", 14));
    const preview = el("button", { class: "btn", type: "button" }, "Preview");
    const undo = el("button", { class: "btn btn-ghost", type: "button" }, "Undo replace");
    const one = el("button", { class: "btn", type: "button" }, "Replace");
    one.addEventListener("click", async () => {
      const r = this.provider?.replace;
      if (!r?.one || !this.input.value) return;
      try { await r.one(this.input.value, this.replacement.value, this.matchCase); await this.step(false); await this.refresh(); }
      catch (e) { toastError(e, this.el); }
    });
    this.oneBtn = one;
    this.replaceRow.append(this.column, this.replacement, one, preview, this.applyBtn, undo);
    // Errors of a find or replace show in the bar, not only in a toast (U.11a).
    this.el = el("div", { class: "find-bar", role: "search", "aria-label": "Find in table", hidden: true, "data-errors": true },
      el("div", { class: "find-main" }, toggle, this.input, this.count, this.caseBtn, this.wholeBtn, this.formulaBtn, this.allBtn, prev, next, close),
      this.replaceRow);

    this.input.addEventListener("input", () => { window.clearTimeout(this.timer); this.timer = window.setTimeout(() => this.refresh(), 200); });
    this.input.addEventListener("keydown", (e) => {
      if (e.key === "Enter") { e.preventDefault(); void this.step(e.shiftKey); }
    });
    this.el.addEventListener("keydown", (e) => {
      if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); this.close(); }
    });
    this.caseBtn.addEventListener("click", () => {
      this.matchCase = !this.matchCase;
      this.caseBtn.setAttribute("aria-pressed", String(this.matchCase));
      void this.refresh();
    });
    this.wholeBtn.addEventListener("click", () => { this.whole = !this.whole; this.wholeBtn.setAttribute("aria-pressed", String(this.whole)); void this.refresh(); });
    this.allBtn.addEventListener("click", () => { this.allSheets = !this.allSheets; this.allBtn.setAttribute("aria-pressed", String(this.allSheets)); void this.refresh(); });
    this.formulaBtn.addEventListener("click", () => { this.formulas = !this.formulas; this.formulaBtn.setAttribute("aria-pressed", String(this.formulas)); void this.refresh(); });
    prev.addEventListener("click", () => void this.step(true));
    next.addEventListener("click", () => void this.step(false));
    close.addEventListener("click", () => this.close());
    toggle.addEventListener("click", () => this.showReplace(this.replaceRow.hidden));
    for (const i of [this.column, this.replacement]) i.addEventListener("input", () => { this.applyBtn.disabled = true; });
    preview.addEventListener("click", async () => {
      const r = this.provider?.replace;
      if (!r || !this.input.value) return;
      try {
        const n = await r.preview(this.column.value, this.input.value, this.replacement.value);
        this.count.textContent = n === 0 ? "No cell to replace" : `${formatCount(n)} cells will change in ${this.column.value}`;
        this.applyBtn.disabled = n === 0 || n > 10_000;
        if (n > 10_000) this.count.textContent = `${formatCount(n)} cells: at most 10,000 can be replaced at once`;
      } catch (e) { toastError(e, this.el); }
    });
    this.applyBtn.addEventListener("click", async () => {
      const r = this.provider?.replace;
      if (!r) return;
      this.applyBtn.disabled = true;
      try { toast(await r.apply(), "success"); void this.refresh(); } catch (e) { toastError(e, this.el); }
    });
    undo.addEventListener("click", async () => {
      const r = this.provider?.replace;
      if (!r) return;
      try { toast(await r.undo(), "success"); void this.refresh(); } catch (e) { toastError(e, this.el); }
    });
  }

  get isOpen(): boolean { return !this.el.hidden; }

  open(provider: FindProvider, replace: boolean): void {
    this.provider = provider;
    this.el.setAttribute("aria-label", `Find in ${provider.label}`);
    this.el.hidden = false;
    this.wholeBtn.hidden = this.formulaBtn.hidden = !provider.sheetOptions;
    this.allBtn.hidden = !provider.nextSheet;
    this.showReplace(replace && !!provider.replace);
    this.input.focus();
    this.input.select();
    if (this.input.value) void this.refresh();
  }

  close(): void {
    if (this.el.hidden) return;
    this.el.hidden = true;
    this.seq++;
    this.onClose();
  }

  private showReplace(on: boolean): void {
    const r = this.provider?.replace;
    const show = on && !!r;
    this.replaceRow.hidden = !show;
    this.el.querySelector(".find-main .icon-btn")?.setAttribute("aria-expanded", String(show));
    this.column.hidden = !r?.columns;
    if (this.oneBtn) this.oneBtn.hidden = !r?.one;
    if (show && r && !r.columns) this.count.textContent = "";
    if (show && r && r.columns) {
      const cols = r.columns();
      this.column.replaceChildren(...cols.map((c) => el("option", { value: c }, c)));
      const a = this.provider!.active();
      const here = this.provider!.columnName?.(a.c);
      if (here && cols.includes(here)) this.column.value = here;
      if (!cols.length) this.count.textContent = "Replace works in text columns";
    }
  }

  /** Count the matches and move to the first one from the current cell. */
  async refresh(): Promise<void> {
    const p = this.provider;
    if (!p) return;
    clearError(this.el);
    const text = this.input.value;
    const mine = ++this.seq;
    if (!text) { this.count.textContent = ""; return; }
    try {
      const a = p.active();
      const out = await p.find(text, this.matchCase, a.r, false, true);
      if (mine !== this.seq) return;
      const n = out.matches ?? 0;
      this.count.textContent = n === 0 ? "No matches" : `${formatCount(n)} ${n === 1 ? "match" : "matches"}`;
      const hit = out.hits[0] ?? (await p.find(text, this.matchCase, 0, false, false)).hits[0];
      if (mine === this.seq && !hit && n > 0 && this.allSheets && p.nextSheet) { await p.nextSheet(false); return; }
      if (mine === this.seq && hit && n > 0) {
        const c = hit.row === a.r && hit.columns.includes(a.c) ? a.c : hit.columns[0];
        p.go(hit.row, c);
      }
    } catch (e) {
      if (mine === this.seq) this.count.textContent = (e as Error).message;
    }
  }

  /** The next (or previous) matching cell after the current one, wrapping around. */
  async step(backwards: boolean): Promise<void> {
    const p = this.provider;
    if (!p || !this.input.value) return;
    const text = this.input.value;
    const a = p.active();
    const pick = (hits: FindHit[], wrapped: boolean): { row: number; c: number } | null => {
      for (const h of hits) {
        const cols = backwards ? [...h.columns].reverse() : h.columns;
        for (const c of cols) {
          if (wrapped || h.row !== a.r || (backwards ? c < a.c : c > a.c)) return { row: h.row, c };
        }
      }
      return null;
    };
    try {
      let target = pick((await p.find(text, this.matchCase, a.r, backwards, false)).hits, false);
      if (!target && this.allSheets && p.nextSheet && await p.nextSheet(backwards)) return;
      if (!target) {
        const from = backwards ? Number.MAX_SAFE_INTEGER : 0;
        target = pick((await p.find(text, this.matchCase, from, backwards, false)).hits, true);
      }
      if (target) p.go(target.row, target.c);
      else this.count.textContent = "No matches";
    } catch (e) {
      this.count.textContent = (e as Error).message;
    }
  }
}
