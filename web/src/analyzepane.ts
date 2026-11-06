// U.15: Analyze as a side pane beside the grid. Rows, Values, Columns (pivot) and Filters
// are built from pickers ("Add field"), each value with its own aggregate; calculated values
// take a formula field. The result and its chart open docked under the grid, with drill-down
// to the rows behind any group. Plans are saved in the workbook and run on the server.

import type { Rpc } from "./api";
import { el, toast } from "./ui";
import { icon } from "./icons";
import { drawChart, resultTable } from "./analysis";
import type {
  Aggregate, AnalysisResult, AnalysisSpec, Calculated, ChartKind, ColumnValues, Filter, FilterOp, JoinKind, Measure, SavedAnalysis, TableSchema, ViewSpec,
} from "./protocol";
import type { ServerEvent } from "./types";

export interface AnalyzeHost {
  /** Show `panel` beside the grid. */
  pane(panel: HTMLElement, onClose: () => void): { close: () => void };
  /** Show `panel` docked under the grid. */
  results(panel: HTMLElement, onClose: () => void): { close: () => void };
  /** The table open now, the default source. */
  table(): string | null;
  openTable(name: string): Promise<void>;
  openView(spec: ViewSpec): Promise<void>;
}

const AGGREGATES: { value: Aggregate; label: string }[] = [
  { value: "sum", label: "Sum" }, { value: "count", label: "Count" }, { value: "average", label: "Average" },
  { value: "min", label: "Minimum" }, { value: "max", label: "Maximum" },
];
const FILTER_OPS: { value: FilterOp; label: string; value_?: boolean }[] = [
  { value: "eq", label: "is", value_: true }, { value: "neq", label: "is not", value_: true }, { value: "contains", label: "contains", value_: true },
  { value: "gt", label: ">", value_: true }, { value: "gte", label: "≥", value_: true }, { value: "lt", label: "<", value_: true }, { value: "lte", label: "≤", value_: true },
  { value: "is_empty", label: "is empty" }, { value: "not_empty", label: "is not empty" },
];
const CHARTS: ChartKind[] = ["column", "bar", "line", "area", "scatter", "pie"];
const MAX_PIVOT = 24;

type Mode = "pivot" | "join" | "profile";
interface ValueField { column: string | null; aggregate: Aggregate; name: string }

const option = (value: string, label = value, selected = false) => el("option", { value, selected }, label);
const sel = (label: string, opts: HTMLOptionElement[], cls = "") => el("select", { "aria-label": label, class: cls }, ...opts) as HTMLSelectElement;

export class AnalyzePane {
  private schema: TableSchema[] = [];
  private saved: SavedAnalysis[] = [];
  private current: SavedAnalysis | null = null;
  private mode: Mode = "pivot";
  private source = "";
  private rows: string[] = [];
  private values: ValueField[] = [];
  private calculated: Calculated[] = [];
  private filters: Filter[] = [];
  private pivot: { column: string; values: (string | null)[] } | null = null;
  private pivotChoices: ColumnValues | null = null;
  private totals = true;
  private chart = true;
  private chartKind: ChartKind = "column";
  private join = { right: "", left_key: "", right_key: "", kind: "inner" as JoinKind };
  private profile: string[] = [];
  private name = "My analysis";

  private body = el("div", { class: "analyze-body" });
  private status = el("p", { class: "analyze-status", role: "status", "aria-live": "polite" });
  private panel: HTMLElement;
  private paneHandle: { close: () => void };
  private resultsHandle: { close: () => void } | null = null;
  private resultsEl = el("section", { class: "analyze-results", "aria-label": "Analysis result" });
  private seq = 0;
  private job = 0;
  private closed = false;

  constructor(private rpc: Rpc, private host: AnalyzeHost) {
    const close = el("button", { class: "icon-btn", type: "button", "aria-label": "Close Analyze", title: "Close" }, icon("x", 14));
    close.addEventListener("click", () => this.close());
    this.panel = el("aside", { class: "analyze-pane", "aria-label": "Analyze" },
      el("header", { class: "analyze-head" }, el("h2", {}, "Analyze"), close),
      this.body, this.status);
    this.panel.addEventListener("keydown", (e) => { if (e.key === "Escape" && !(e.target instanceof HTMLSelectElement)) { e.preventDefault(); this.close(); } });
    this.paneHandle = host.pane(this.panel, () => this.close());
    void this.load();
  }

  private async load(): Promise<void> {
    try {
      [this.schema, this.saved] = await Promise.all([this.rpc.call("schema", {}), this.rpc.call("analyses", {})]);
    } catch (e) { this.status.textContent = (e as Error).message; return; }
    if (!this.schema.length) {
      // U.T05: no data source yet: say what Analyze needs instead of empty pickers.
      this.body.replaceChildren();
      this.status.textContent = "Analyze summarizes tables, and this workbook has none yet. Import a file, or turn a sheet into a table from its tab menu (Convert to table).";
      this.panel.querySelector<HTMLElement>("button")?.focus();
      return;
    }
    this.source = this.host.table() ?? this.schema[0]?.name ?? "";
    this.defaults();
    this.render();
    this.panel.querySelector<HTMLElement>("select, input, button")?.focus();
  }

  private columns(table = this.source) { return this.schema.find((t) => t.name === table)?.columns ?? []; }
  private numeric(table = this.source) { return this.columns(table).filter((c) => c.kind === "int" || c.kind === "number"); }

  private defaults(): void {
    const text = this.columns().find((c) => c.kind === "text");
    const num = this.numeric()[0];
    this.rows = text ? [text.name] : [];
    this.values = [num ? { column: num.name, aggregate: "sum", name: `sum_${num.name}` } : { column: null, aggregate: "count", name: "rows" }];
    this.calculated = []; this.filters = []; this.pivot = null; this.pivotChoices = null;
    this.profile = this.columns().slice(0, 8).map((c) => c.name);
    this.join = { right: this.schema.find((t) => t.name !== this.source)?.name ?? this.source, left_key: this.columns()[0]?.name ?? "", right_key: "", kind: "inner" };
    this.join.right_key = this.columns(this.join.right)[0]?.name ?? "";
  }

  private changed(): void { this.seq++; this.status.textContent = "Run to see the result."; }

  // ------------------------------------------------------------- rendering

  private render(): void {
    const sections: HTMLElement[] = [];
    // Saved analyses and the plan's name.
    const open = sel("Open a saved analysis", [option("", this.saved.length ? "Open saved…" : "No saved analyses yet"), ...this.saved.map((a) => option(a.id, a.name, a.id === this.current?.id))]);
    open.disabled = !this.saved.length;
    open.addEventListener("change", () => { const a = this.saved.find((x) => x.id === open.value); if (a) { this.loadSaved(a); void this.run().catch((e) => (this.status.textContent = (e as Error).message)); } });
    const name = el("input", { value: this.name, "aria-label": "Analysis name", maxlength: "200" }) as HTMLInputElement;
    name.addEventListener("input", () => { this.name = name.value; });
    sections.push(el("div", { class: "analyze-row" }, open), this.labelled("Name", name));

    const mode = sel("Analysis kind", [option("pivot", "Pivot table", this.mode === "pivot"), option("join", "Join two tables", this.mode === "join"), option("profile", "Profile columns", this.mode === "profile")]);
    mode.addEventListener("change", () => { this.mode = mode.value as Mode; this.changed(); this.render(); });
    const source = sel("Source table", this.schema.map((t) => option(t.name, t.name, t.name === this.source)));
    source.addEventListener("change", () => { this.source = source.value; this.defaults(); this.changed(); this.render(); });
    sections.push(this.labelled("Kind", mode), this.labelled(this.mode === "join" ? "Left table" : "Source", source));

    if (this.mode === "pivot") sections.push(...this.pivotSections());
    if (this.mode === "join") sections.push(this.joinSection());
    if (this.mode === "profile") sections.push(this.fieldList("Columns", this.profile, this.columns().map((c) => c.name), (v) => { this.profile = v; }, 32));

    const run = this.button("Run", () => this.run(), "btn btn-primary");
    const save = this.button("Save", () => this.save(false));
    const saveAs = this.button("Save as new", () => this.save(true));
    const table = this.button("Result as table", () => this.materialize());
    const del = this.button("Delete", () => this.remove(), "btn btn-ghost");
    del.disabled = !this.current;
    sections.push(el("div", { class: "analyze-actions" }, run, save, saveAs, table, del));
    this.body.replaceChildren(...sections);
  }

  private pivotSections(): HTMLElement[] {
    const names = this.columns().map((c) => c.name);
    const out: HTMLElement[] = [];
    out.push(this.fieldList("Rows", this.rows, names, (v) => { this.rows = v; }, 4));

    // The calculated-values hint names the first value, so it follows that value's name.
    const hint = el("p", { class: "analyze-hint" });
    const showHint = () => { hint.textContent = this.values.length ? `Refer to values as [@${this.values[0].name}]` : "Add a value first"; };
    showHint();

    // Values: each with its column and aggregate.
    const values = el("div", { class: "analyze-list" }, ...this.values.map((v, i) => {
      const agg = sel(`Aggregate of value ${i + 1}`, AGGREGATES.map((a) => option(a.value, a.label, a.value === v.aggregate)), "analyze-agg");
      const col = sel(`Column of value ${i + 1}`, [option("", "(rows)", v.column === null), ...this.columns().map((c) => option(c.name, c.name, c.name === v.column))]);
      const nm = el("input", { value: v.name, "aria-label": `Name of value ${i + 1}`, class: "analyze-name" }) as HTMLInputElement;
      const sync = () => {
        v.aggregate = agg.value as Aggregate; v.column = col.value || null;
        if (v.column === null) v.aggregate = "count";
        nm.value = v.name = nm.dataset.typed ? nm.value : `${v.aggregate}_${v.column ?? "rows"}`;
        showHint();
        this.changed();
      };
      agg.addEventListener("change", () => { sync(); agg.value = v.aggregate; });
      col.addEventListener("change", sync);
      nm.addEventListener("input", () => { nm.dataset.typed = "1"; v.name = nm.value; showHint(); this.changed(); });
      return el("div", { class: "analyze-item" }, agg, el("span", { class: "muted" }, "of"), col, nm, this.removeBtn(`Remove value ${v.name}`, () => { this.values.splice(i, 1); this.changed(); this.render(); }));
    }));
    const addValue = this.button("Add value", async () => {
      const num = this.numeric()[0];
      this.values.push(num ? { column: num.name, aggregate: "sum", name: `sum_${num.name}` } : { column: null, aggregate: "count", name: "rows" });
      this.changed(); this.render();
    }, "btn btn-ghost analyze-add");
    out.push(this.section("Values", values, addValue));

    // Calculated values: a name and a formula over the other values.
    const calc = el("div", { class: "analyze-list" }, ...this.calculated.map((c, i) => {
      const nm = el("input", { value: c.name, "aria-label": `Name of calculated value ${i + 1}`, class: "analyze-name" }) as HTMLInputElement;
      const f = el("input", { value: c.formula, "aria-label": `Formula of calculated value ${i + 1}`, class: "analyze-formula mono", placeholder: "=[@revenue]/[@orders]" }) as HTMLInputElement;
      nm.addEventListener("input", () => { c.name = nm.value; this.changed(); });
      f.addEventListener("input", () => { c.formula = f.value; this.changed(); });
      return el("div", { class: "analyze-item" }, nm, f, this.removeBtn(`Remove calculated value ${c.name}`, () => { this.calculated.splice(i, 1); this.changed(); this.render(); }));
    }));
    const addCalc = this.button("Add calculated value", async () => {
      const a = this.values[0]?.name ?? "value", b = this.values[1]?.name;
      this.calculated.push({ name: "calculated", formula: b ? `=[@${a}]/[@${b}]` : `=[@${a}]*1` });
      this.changed(); this.render();
    }, "btn btn-ghost analyze-add");
    out.push(this.section("Calculated values", calc, el("div", {}, addCalc, hint)));

    // Columns: a pivot over one column's values, picked from its distinct values.
    const pcol = sel("Pivot column", [option("", "No pivot", !this.pivot), ...names.map((n) => option(n, n, n === this.pivot?.column))]);
    pcol.addEventListener("change", () => {
      this.pivot = pcol.value ? { column: pcol.value, values: [] } : null;
      this.pivotChoices = null; this.changed(); this.render();
      if (this.pivot) void this.loadPivotChoices();
    });
    const choices = el("div", { class: "analyze-checks", role: "group", "aria-label": "Pivot values" });
    if (this.pivot && this.pivotChoices) {
      const chosen = new Set(this.pivot.values.map((v) => v ?? "\u0000"));
      choices.append(...this.pivotChoices.values.map((v) => {
        const key = v.value ?? "\u0000";
        const box = el("input", { type: "checkbox", checked: chosen.has(key) }) as HTMLInputElement;
        box.addEventListener("change", () => {
          const list = this.pivot!.values;
          if (box.checked) {
            if (list.length >= MAX_PIVOT) { box.checked = false; toast(`A pivot takes at most ${MAX_PIVOT} values`); return; }
            list.push(v.value);
          } else this.pivot!.values = list.filter((x) => (x ?? "\u0000") !== key);
          this.changed();
        });
        return el("label", { class: "analyze-check" }, box, el("span", {}, v.value === null ? "(Empty)" : v.value), el("span", { class: "muted" }, String(v.count)));
      }));
      if (this.pivotChoices.truncated) choices.append(el("p", { class: "analyze-hint" }, `First ${this.pivotChoices.values.length} of ${this.pivotChoices.distinct} values`));
    } else if (this.pivot) choices.append(el("p", { class: "analyze-hint" }, "Loading values…"));
    out.push(this.section("Columns (pivot)", el("div", {}, pcol, choices)));

    // Filters over the source rows.
    const filters = el("div", { class: "analyze-list" }, ...this.filters.map((f, i) => {
      const col = sel(`Filter ${i + 1} column`, names.map((n) => option(n, n, n === f.column)));
      const op = sel(`Filter ${i + 1} condition`, FILTER_OPS.map((o) => option(o.value, o.label, o.value === f.op)), "analyze-op");
      const val = el("input", { value: f.value === null || f.value === undefined ? "" : String(f.value), "aria-label": `Filter ${i + 1} value`, class: "analyze-name" }) as HTMLInputElement;
      const showVal = () => { val.hidden = !FILTER_OPS.find((o) => o.value === op.value)?.value_; };
      col.addEventListener("change", () => { f.column = col.value; this.changed(); });
      op.addEventListener("change", () => { f.op = op.value as FilterOp; showVal(); this.changed(); });
      val.addEventListener("input", () => { f.value = val.value; this.changed(); });
      showVal();
      return el("div", { class: "analyze-item" }, col, op, val, this.removeBtn(`Remove filter ${i + 1}`, () => { this.filters.splice(i, 1); this.changed(); this.render(); }));
    }));
    const addFilter = this.button("Add filter", async () => { this.filters.push({ column: names[0] ?? "", op: "eq", value: "" }); this.changed(); this.render(); }, "btn btn-ghost analyze-add");
    out.push(this.section("Filters", filters, addFilter));

    const totals = el("input", { type: "checkbox", checked: this.totals }) as HTMLInputElement;
    totals.addEventListener("change", () => { this.totals = totals.checked; this.changed(); });
    const chart = el("input", { type: "checkbox", checked: this.chart }) as HTMLInputElement;
    chart.addEventListener("change", () => { this.chart = chart.checked; this.changed(); });
    const kind = sel("Chart kind", CHARTS.map((k) => option(k, k[0].toUpperCase() + k.slice(1), k === this.chartKind)));
    kind.addEventListener("change", () => { this.chartKind = kind.value as ChartKind; this.changed(); });
    out.push(el("div", { class: "analyze-options" },
      el("label", { class: "analyze-check" }, totals, el("span", {}, "Totals and subtotals")),
      el("label", { class: "analyze-check" }, chart, el("span", {}, "Chart")), kind));
    return out;
  }

  private joinSection(): HTMLElement {
    const right = sel("Right table", this.schema.map((t) => option(t.name, t.name, t.name === this.join.right)));
    const lk = sel("Left key", this.columns().map((c) => option(c.name, c.name, c.name === this.join.left_key)));
    const rk = sel("Right key", this.columns(this.join.right).map((c) => option(c.name, c.name, c.name === this.join.right_key)));
    const kind = sel("Join kind", [option("inner", "Only matching rows", this.join.kind === "inner"), option("left", "All left rows", this.join.kind === "left")]);
    right.addEventListener("change", () => { this.join.right = right.value; this.join.right_key = this.columns(right.value)[0]?.name ?? ""; this.changed(); this.render(); });
    lk.addEventListener("change", () => { this.join.left_key = lk.value; this.changed(); });
    rk.addEventListener("change", () => { this.join.right_key = rk.value; this.changed(); });
    kind.addEventListener("change", () => { this.join.kind = kind.value as JoinKind; this.changed(); });
    return el("div", { class: "analyze-section" }, this.labelled("Right table", right), this.labelled("Left key", lk), this.labelled("Right key", rk), this.labelled("Rows", kind));
  }

  /** A list of field chips with "Add field". */
  private fieldList(title: string, chosen: string[], options: string[], set: (v: string[]) => void, max: number): HTMLElement {
    const chips = el("div", { class: "analyze-chips" }, ...chosen.map((c, i) => el("span", { class: "analyze-chip" }, c,
      this.removeBtn(`Remove ${c} from ${title}`, () => { const next = [...chosen]; next.splice(i, 1); set(next); this.changed(); this.render(); }))));
    const free = options.filter((o) => !chosen.includes(o));
    const add = sel(`Add field to ${title}`, [option("", chosen.length >= max ? `At most ${max}` : "Add field…"), ...free.map((o) => option(o))], "analyze-addfield");
    add.disabled = chosen.length >= max || !free.length;
    add.addEventListener("change", () => { if (add.value) { set([...chosen, add.value]); this.changed(); this.render(); this.focusLater(`[aria-label="Add field to ${title}"]`); } });
    return this.section(title, chips, add);
  }

  private section(title: string, ...children: HTMLElement[]): HTMLElement {
    return el("section", { class: "analyze-section", "aria-label": title }, el("h3", {}, title), ...children);
  }
  private labelled(text: string, input: HTMLElement): HTMLElement { return el("label", { class: "analyze-field" }, el("span", {}, text), input); }
  private removeBtn(label: string, action: () => void): HTMLElement {
    const b = el("button", { class: "icon-btn analyze-remove", type: "button", "aria-label": label, title: label }, icon("x", 12));
    b.addEventListener("click", action);
    return b;
  }
  private button(label: string, action: () => Promise<void>, cls = "btn"): HTMLButtonElement {
    const b = el("button", { class: cls, type: "button" }, label) as HTMLButtonElement;
    b.addEventListener("click", async () => {
      b.disabled = true;
      try { await action(); } catch (e) { this.status.textContent = (e as Error).message; }
      finally { b.disabled = false; }
    });
    return b;
  }
  private focusLater(selector: string): void { setTimeout(() => this.panel.querySelector<HTMLElement>(selector)?.focus()); }

  private async loadPivotChoices(): Promise<void> {
    if (!this.pivot) return;
    const column = this.pivot.column;
    try {
      const out = await this.rpc.call("column_values", { spec: { table: this.source, sort: [], filters: [], hidden: [] }, column, limit: 200 }) as ColumnValues;
      if (this.pivot?.column !== column) return;
      this.pivotChoices = out;
      if (!this.pivot.values.length) this.pivot.values = out.values.slice(0, 12).map((v) => v.value);
      this.render();
    } catch (e) { this.status.textContent = (e as Error).message; }
  }

  // ------------------------------------------------------------- the plan

  spec(): AnalysisSpec {
    if (this.mode === "join") return { kind: "join", left: this.source, right: this.join.right, left_key: this.join.left_key, right_key: this.join.right_key, join: this.join.kind };
    if (this.mode === "profile") return { kind: "profile", table: this.source, columns: this.profile };
    if (!this.values.length) throw new Error("Add a value to calculate");
    const measures: Measure[] = this.values.map((v) => ({ name: v.name.trim() || `${v.aggregate}_${v.column ?? "rows"}`, aggregate: v.aggregate, ...(v.column ? { value: v.column } : {}) }));
    const calculated = this.calculated.filter((c) => c.name.trim() && c.formula.trim()).map((c) => ({ name: c.name.trim(), formula: c.formula.trim().startsWith("=") ? c.formula.trim() : `=${c.formula.trim()}` }));
    const filters = this.filters.filter((f) => f.column).map((f) => ({ ...f, value: FILTER_OPS.find((o) => o.value === f.op)?.value_ ? (f.value ?? "") : null }));
    return {
      kind: "report", table: this.source, groups: this.rows, measures, calculated, totals: this.totals, filters,
      ...(this.pivot && this.pivot.values.length ? { pivot: { column: this.pivot.column, values: this.pivot.values } } : {}),
    };
  }

  private loadSaved(a: SavedAnalysis): void {
    this.current = a; this.name = a.name; this.chart = a.chart; this.chartKind = a.chart_kind ?? "column";
    const s = a.spec;
    if (s.kind === "join") { this.mode = "join"; this.source = s.left; this.join = { right: s.right, left_key: s.left_key, right_key: s.right_key, kind: s.join }; }
    else if (s.kind === "profile") { this.mode = "profile"; this.source = s.table; this.profile = s.columns; }
    else if (s.kind === "summary") {
      this.mode = "pivot"; this.source = s.table; this.rows = s.groups;
      this.values = [{ column: s.value, aggregate: s.aggregate, name: `${s.aggregate}_${s.value ?? "rows"}` }];
      this.calculated = []; this.filters = []; this.totals = false;
      this.pivot = s.pivot ? { column: s.pivot.column, values: s.pivot.values } : null;
    } else {
      this.mode = "pivot"; this.source = s.table; this.rows = s.groups;
      this.values = s.measures.map((m) => ({ column: m.value ?? null, aggregate: m.aggregate, name: m.name }));
      this.calculated = s.calculated.map((c) => ({ ...c })); this.filters = s.filters.map((f) => ({ ...f })); this.totals = s.totals;
      this.pivot = s.pivot ? { column: s.pivot.column, values: s.pivot.values } : null;
    }
    this.pivotChoices = null;
    this.render();
    if (this.pivot) void this.loadPivotChoices();
  }

  private async run(): Promise<void> {
    const spec = this.spec();
    const seq = ++this.seq;
    this.status.textContent = "Calculating…";
    if (this.job) void this.rpc.cancel(this.job).catch(() => {});
    let data: AnalysisResult;
    try { data = await this.rpc.request("analyze", { spec }, { onSent: (id) => (this.job = id) }); }
    finally { if (seq === this.seq) this.job = 0; }
    if (this.closed || seq !== this.seq) return;
    const drill = spec.kind === "summary" || spec.kind === "report"
      ? (row: unknown[]) => void this.rpc.call("drill", { spec, row: row as never }).then((v) => this.host.openView(v)).catch((e) => (this.status.textContent = String(e)))
      : undefined;
    const chartEl = el("div", { class: "analysis-chart", role: "figure", "aria-label": "Analysis chart" });
    if (this.chart && (spec.kind === "summary" || spec.kind === "report")) drawChart(chartEl, spec, data, this.chartKind);
    const summary = `${data.result.rows.length} rows · revision ${data.result.revision}${data.result.truncated ? " · more results exist" : ""}`;
    this.resultsEl.replaceChildren(
      el("h2", {}, this.name || "Analysis"),
      el("p", { class: "analyze-status" }, summary),
      ...(chartEl.childElementCount ? [chartEl] : []),
      ...(data.diagnostics ? [el("h3", {}, "Join cardinality (empty keys do not match)"), resultTable(data.diagnostics)] : []),
      resultTable(data.result, drill));
    if (!this.resultsHandle) this.resultsHandle = this.host.results(this.resultsEl, () => { this.resultsHandle = null; });
    this.status.textContent = `Done: ${summary}`;
  }

  private async save(asNew: boolean): Promise<void> {
    const spec = this.spec();
    const charted = this.mode === "pivot" && this.chart;
    this.current = await this.rpc.call("save_analysis", { saved: asNew ? null : this.current?.id ?? null, name: this.name, spec, chart: charted, chart_kind: this.chartKind });
    this.saved = await this.rpc.call("analyses", {});
    this.render();
    this.status.textContent = asNew ? "Saved as a new analysis." : "Saved. Opening it runs against current data.";
  }

  private async materialize(): Promise<void> {
    const result = await this.rpc.call("materialize_analysis", { name: this.name, spec: this.spec() });
    await this.host.openTable(result.name);
    this.status.textContent = `Created table ${result.name}.`;
  }

  private async remove(): Promise<void> {
    if (!this.current) return;
    await this.rpc.call("delete_analysis", { analysis: this.current.id });
    this.current = null;
    this.saved = await this.rpc.call("analyses", {});
    this.render();
    this.status.textContent = "Saved analysis deleted.";
  }

  onEvent(e: ServerEvent): void {
    if (this.closed) return;
    if (["table_changed", "cells_changed", "tables_changed", "resync"].includes(e.event)) {
      this.seq++;
      this.status.textContent = "The workbook changed. Run again to update the result.";
      if (e.event === "tables_changed" || e.event === "resync") void this.rpc.call("schema", {}).then((s) => { this.schema = s; }).catch(() => {});
    }
  }

  close(): void {
    if (this.closed) return;
    this.closed = true;
    if (this.job) void this.rpc.cancel(this.job).catch(() => {});
    this.resultsHandle?.close();
    this.paneHandle.close();
  }
}
