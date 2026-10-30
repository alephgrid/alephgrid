import { Rpc } from "./api";
import { el, openModal } from "./ui";
import type { Aggregate, AnalysisSpec, AnalysisResult, SavedAnalysis, TableSchema, QueryResult, ReplacePreview, JoinKind, ChartKind, Measure, Calculated, ViewSpec, Dashboard, DashboardResult } from "./protocol";
import type { ServerEvent } from "./types";

const field = (label: string, input: HTMLElement) => el("label", { class: "analysis-field" }, label, input);
const select = (label: string, values: string[]) => el("select", { "aria-label": label }, ...values.map(value => el("option", { value }, value)));
export function resultTable(result: QueryResult, onRow?: (row: unknown[]) => void): HTMLElement {
  return el("div", { class: "analysis-result" }, el("table", { class: "result-table" },
    el("thead", {}, el("tr", {}, ...result.columns.map(c => el("th", { scope: "col" }, c.name)), ...(onRow ? [el("th", { scope: "col" }, "Rows")] : []))),
    el("tbody", {}, ...result.rows.map(row => el("tr", {}, ...row.map(value => el("td", {}, value === null ? "NULL" : String(value))),
      ...(onRow ? [el("td", {}, el("button", { class: "btn btn-small", onclick: () => onRow(row) }, "Show rows"))] : []))))));
}

/** Measures written one per line: `revenue = sum(amount)`, `orders = count()`. */
export function parseMeasures(text: string): Measure[] {
  return text.split("\n").map(l => l.trim()).filter(Boolean).map(line => {
    const m = /^(.+?)\s*=\s*(count|sum|average|min|max)\s*\(\s*(.*?)\s*\)$/i.exec(line);
    if (!m) throw new Error(`measure "${line}": write name = sum(column), average(column), min(column), max(column) or count()`);
    return { name: m[1], aggregate: m[2].toLowerCase() as Aggregate, ...(m[3] ? { value: m[3] } : {}) };
  });
}
/** Calculated measures, one per line: `average = [@revenue]/[@orders]`. */
export function parseCalculated(text: string): Calculated[] {
  return text.split("\n").map(l => l.trim()).filter(Boolean).map(line => {
    const i = line.indexOf("=");
    if (i < 1) throw new Error(`calculated measure "${line}": write name = formula`);
    return { name: line.slice(0, i).trim(), formula: "=" + line.slice(i + 1).trim() };
  });
}

/** A chart of the first measure of a summary or report: bars, a line or points (≤100 groups). */
export function drawChart(out: HTMLElement, spec: AnalysisSpec, data: AnalysisResult, kind: ChartKind): void {
  if (spec.kind !== "summary" && spec.kind !== "report") return;
  const measure = spec.groups.length;
  if (!["int", "number"].includes(data.result.columns[measure]?.kind)) { out.append(el("p", {}, "Charts require a numeric measure.")); return; }
  const kindCol = data.result.columns.findIndex(c => c.name === "row_kind");
  const rows = data.result.rows.filter(r => kindCol < 0 || r[kindCol] === "row").slice(0, 100);
  const values = rows.map(r => r[measure] === null ? NaN : Number(r[measure]));
  const label = (r: unknown[]) => r.slice(0, measure).map(v => v === null ? "NULL" : String(v)).join(" / ") || "All rows";
  out.append(el("p", {}, `First measure (${data.result.columns[measure]?.name}) · ${rows.length} groups${data.result.rows.length > 100 || data.result.truncated ? " (partial chart)" : ""}. Totals are not drawn; labels preserve exact values.`));
  const finite = values.filter(Number.isFinite);
  if (kind === "bar") {
    const max = Math.max(...finite.map(Math.abs), 1);
    rows.forEach((r, i) => {
      const v = values[i], bar = el("span", { class: "analysis-bar" });
      if (Number.isFinite(v)) { bar.style.width = `${Math.abs(v) / max * 50}%`; bar.style.left = `${v < 0 ? 50 - Math.abs(v) / max * 50 : 50}%`; }
      out.append(el("div", { class: "analysis-chart-row" }, el("span", {}, label(r)), el("div", { class: "analysis-bar-track", "aria-hidden": "true" }, bar), el("span", {}, r[measure] === null ? "NULL" : String(r[measure]))));
    });
    return;
  }
  // Line or scatter: groups along x in result order, the measure on y.
  const W = 600, H = 200, lo = Math.min(...finite, 0), hi = Math.max(...finite, 1);
  const x = (i: number) => rows.length < 2 ? W / 2 : 20 + i * (W - 40) / (rows.length - 1);
  const y = (v: number) => H - 10 - (v - lo) / (hi - lo || 1) * (H - 20);
  const ns = "http://www.w3.org/2000/svg";
  const svg = document.createElementNS(ns, "svg");
  svg.setAttribute("viewBox", `0 0 ${W} ${H}`); svg.setAttribute("class", "analysis-svg"); svg.setAttribute("aria-hidden", "true");
  const pts = values.map((v, i) => Number.isFinite(v) ? `${x(i)},${y(v)}` : "").filter(Boolean);
  if (kind === "line" && pts.length > 1) { const pl = document.createElementNS(ns, "polyline"); pl.setAttribute("points", pts.join(" ")); pl.setAttribute("class", "analysis-line"); svg.append(pl); }
  values.forEach((v, i) => { if (!Number.isFinite(v)) return; const c = document.createElementNS(ns, "circle"); c.setAttribute("cx", String(x(i))); c.setAttribute("cy", String(y(v))); c.setAttribute("r", "3"); c.setAttribute("class", "analysis-point"); svg.append(c); });
  out.append(svg, el("ol", { class: "analysis-points", "aria-label": "Chart values" }, ...rows.map((r, i) => el("li", {}, `${label(r)}: ${Number.isFinite(values[i]) ? r[measure] : "NULL"}`))));
}
/** Saved typed plans, evaluated on the server when opened or explicitly refreshed. */
export class AnalysisWorkspace {
  private closed = false;
  private modal: { close(): void };
  private status = el("p", { role: "status" });
  private output = el("div");
  private chartOutput = el("div", { role: "figure", "aria-label": "Summary chart", class: "analysis-chart" });
  private schema: TableSchema[] = [];
  private saved?: SavedAnalysis;
  private loadedSpec: AnalysisSpec | null = null;
  private mode = select("Analysis kind", ["summary", "report", "join", "profile"]);
  private measures = el("textarea", { rows: 3, "aria-label": "Measures", placeholder: "revenue = sum(amount)\norders = count()" });
  private calculated = el("textarea", { rows: 2, "aria-label": "Calculated measures", placeholder: "average = [@revenue]/[@orders]" });
  private totals = el("input", { type: "checkbox", "aria-label": "Totals and subtotals", checked: true });
  private chartKind = select("Chart kind", ["bar", "line", "scatter"]);
  private table = select("Source table", []);
  private right = select("Right table", []);
  private groups = el("select", { multiple: true, size: 4, "aria-label": "Group columns" });
  private profile = el("select", { multiple: true, size: 4, "aria-label": "Profile columns" });
  private metric = select("Aggregate", ["count", "sum", "average", "min", "max"]);
  private value = select("Value column", []);
  private pivot = select("Pivot column", []);
  private categories = el("textarea", { rows: 3, "aria-label": "Pivot values", placeholder: "One value per line; (NULL) for missing values" });
  private leftKey = select("Left key", []);
  private rightKey = select("Right key", []);
  private joinKind = select("Join kind", ["inner", "left"]);
  private name = el("input", { value: "My analysis", "aria-label": "Analysis name", maxlength: 200 });
  private chart = el("input", { type: "checkbox", "aria-label": "Show chart", checked: true });
  private list = el("div", { class: "analysis-saved" });
  private buttons: HTMLButtonElement[] = [];
  private sequence = 0;
  private job = 0;
  constructor(private rpc: Rpc, private openTable: (name: string) => Promise<void>, private openView?: (spec: ViewSpec) => Promise<void>) {
    const summary = el("div", {}, field("Group columns (optional; up to 4)", this.groups), field("Aggregate", this.metric), field("Value column", this.value),
      field("Pivot column (optional)", this.pivot), field("Pivot values (up to 24)", this.categories));
    const report = el("div", {}, field("Measures (one per line)", this.measures), field("Calculated measures (optional)", this.calculated), field("Totals and subtotals", this.totals));
    const groupsField = field("Group columns (up to 4)", this.groups);
    const joins = el("div", {}, field("Right table", this.right), field("Left key", this.leftKey), field("Right key", this.rightKey), field("Join kind", this.joinKind));
    const profiles = el("div", {}, field("Profile columns (up to 32)", this.profile));
    const update = () => { this.loadedSpec = null; summary.hidden = this.mode.value !== "summary"; report.hidden = this.mode.value !== "report"; if (this.mode.value === "report") report.prepend(groupsField); else summary.prepend(groupsField); joins.hidden = this.mode.value !== "join"; profiles.hidden = this.mode.value !== "profile"; this.sequence++; this.output.replaceChildren(); this.chartOutput.replaceChildren(); this.status.textContent = "Run the analysis to see current results."; };
    for (const input of [this.groups, this.metric, this.value, this.pivot, this.categories, this.leftKey, this.rightKey, this.joinKind, this.profile, this.chart, this.measures, this.calculated, this.totals, this.chartKind]) input.addEventListener("input", update);
    this.mode.onchange = update;
    this.table.onchange = () => { this.fillColumns(); update(); };
    this.right.onchange = () => { this.fillRight(); update(); };
    const dialog = el("section", { class: "dialog analysis-dialog" }, el("h2", {}, "Analyze data"),
      el("p", {}, "Build an analysis without SQL. Results refresh when opened or when you choose Run / refresh. Previews hold at most 500 rows; charts show at most 100 aggregated groups."),
      this.list, field("Analysis name", this.name), field("Analysis kind", this.mode), field("Source table", this.table), summary, report, joins, profiles, field("Show chart", this.chart), field("Chart kind", this.chartKind),
      el("div", { class: "dialog-actions" },
        this.action("Run / refresh", () => this.run()),
        el("button", { class: "btn", onclick: () => { if (this.job) void rpc.cancel(this.job).catch(e => this.status.textContent = String(e)); } }, "Cancel run"),
        this.action("Save analysis", async () => {
          this.saved = await rpc.call("save_analysis", { saved: this.saved?.id ?? null, name: this.name.value, spec: this.spec(), chart: ["summary", "report"].includes(this.mode.value) && this.chart.checked, chart_kind: this.chartKind.value as ChartKind });
          await this.loadSaved(); this.status.textContent = "Analysis saved. Opening it runs against current data.";
        }),
        this.action("Save as new", async () => { this.saved = await rpc.call("save_analysis", { saved: null, name: this.name.value, spec: this.spec(), chart: ["summary", "report"].includes(this.mode.value) && this.chart.checked, chart_kind: this.chartKind.value as ChartKind }); await this.loadSaved(); this.status.textContent = "New analysis saved."; }),
        this.action("Create result table", async () => { const result = await rpc.call("materialize_analysis", { name: this.name.value, spec: this.spec() }); this.close(); await this.openTable(result.name); }),
        this.action("Delete saved analysis", async () => { if (!this.saved) throw new Error("Select a saved analysis first"); await rpc.call("delete_analysis", { analysis: this.saved.id }); this.saved = undefined; await this.loadSaved(); this.status.textContent = "Saved analysis deleted."; }),
        el("button", { class: "btn", onclick: () => this.close() }, "Close")), this.status, this.chartOutput, this.output);
    this.modal = openModal(dialog, () => this.close()); update();
    void this.load().catch(e => this.status.textContent = String(e));
  }
  private action(label: string, run: () => Promise<void>): HTMLButtonElement {
    const button = el("button", { class: "btn", onclick: async () => {
      this.buttons.forEach(b => b.disabled = true);
      try { await run(); } catch (e) { this.status.textContent = String(e); }
      finally { this.buttons.forEach(b => b.disabled = false); }
    } }, label); this.buttons.push(button); return button;
  }
  private async load() {
    this.schema = await this.rpc.call("schema", {});
    for (const s of [this.table, this.right]) s.replaceChildren(...this.schema.map(t => el("option", { value: t.name }, t.name)));
    this.fillColumns(); this.fillRight(); await this.loadSaved(); this.mode.focus();
  }
  private fillColumns() {
    const columns = this.schema.find(t => t.name === this.table.value)?.columns ?? [];
    for (const s of [this.groups, this.profile, this.value, this.leftKey]) s.replaceChildren(...columns.map(c => el("option", { value: c.name }, c.name)));
    this.pivot.replaceChildren(el("option", { value: "" }, "No pivot"), ...columns.map(c => el("option", { value: c.name }, c.name)));
  }
  private fillRight() { this.rightKey.replaceChildren(...(this.schema.find(t => t.name === this.right.value)?.columns ?? []).map(c => el("option", { value: c.name }, c.name))); }
  private async loadSaved() {
    const items = await this.rpc.call("analyses", {});
    this.list.replaceChildren(...items.map(a => el("button", { class: "btn", onclick: () => { this.loadDefinition(a); void this.run().catch(e => this.status.textContent = String(e)); } }, a.name)));
  }
  private loadDefinition(a: SavedAnalysis) {
    this.saved = a; this.name.value = a.name; this.mode.value = a.spec.kind;
    const single = (s: HTMLSelectElement, value: string) => {
      if (value && ![...s.options].some(o => o.value === value)) s.append(el("option", { value }, `${value} (missing)`));
      s.value = value;
    };
    single(this.table, a.spec.kind === "join" ? a.spec.left : a.spec.table); this.fillColumns();
    this.mode.dispatchEvent(new Event("change")); this.chart.checked = a.chart; this.chartKind.value = a.chart_kind ?? "bar";
    const set = (s: HTMLSelectElement, names: string[]) => {
      for (const value of names) if (![...s.options].some(o => o.value === value)) s.append(el("option", { value }, `${value} (missing)`));
      [...s.options].forEach(o => o.selected = names.includes(o.value));
    };
    const spec = a.spec;
    if (spec.kind === "summary") { set(this.groups, spec.groups); this.metric.value = spec.aggregate; single(this.value, spec.value ?? ""); single(this.pivot, spec.pivot?.column ?? ""); this.categories.value = spec.pivot?.values.map(v => v ?? "(NULL)").join("\n") ?? ""; }
    if (spec.kind === "join") { single(this.right, spec.right); this.fillRight(); single(this.leftKey, spec.left_key); single(this.rightKey, spec.right_key); this.joinKind.value = spec.join; }
    if (spec.kind === "profile") set(this.profile, spec.columns);
    if (spec.kind === "report") { set(this.groups, spec.groups); this.measures.value = spec.measures.map(m => `${m.name} = ${m.aggregate}(${m.value ?? ""})`).join("\n"); this.calculated.value = (spec.calculated ?? []).map(c => `${c.name} = ${c.formula.replace(/^=/, "")}`).join("\n"); this.totals.checked = spec.totals ?? false; }
    this.loadedSpec = a.spec;
  }
  private spec(): AnalysisSpec {
    if (this.loadedSpec) return this.loadedSpec;
    const chosen = (s: HTMLSelectElement) => [...s.selectedOptions].map(o => o.value);
    if (this.mode.value === "join") return { kind: "join", left: this.table.value, right: this.right.value, left_key: this.leftKey.value, right_key: this.rightKey.value, join: this.joinKind.value as JoinKind };
    if (this.mode.value === "profile") return { kind: "profile", table: this.table.value, columns: chosen(this.profile) };
    if (this.mode.value === "report") return { kind: "report", table: this.table.value, groups: chosen(this.groups), measures: parseMeasures(this.measures.value), calculated: parseCalculated(this.calculated.value), totals: this.totals.checked, filters: [] };
    return { kind: "summary", table: this.table.value, groups: chosen(this.groups), aggregate: this.metric.value as Aggregate, value: this.value.value || null,
      pivot: this.pivot.value ? { column: this.pivot.value, values: this.categories.value.split("\n").map(v => v === "(NULL)" ? null : v) } : null };
  }
  private async run() {
    const spec = this.spec(), seq = ++this.sequence;
    this.status.textContent = "Calculating…";
    if (this.job) void this.rpc.cancel(this.job).catch(() => {});
    let data: AnalysisResult;
    try { data = await this.rpc.request("analyze", { spec }, { onSent: id => this.job = id }); }
    finally { if (seq === this.sequence) this.job = 0; }
    if (this.closed || seq !== this.sequence) return;
    const drill = (spec.kind === "summary" || spec.kind === "report") && this.openView
      ? (row: unknown[]) => void this.rpc.call("drill", { spec, row: row as never }).then(v => { this.close(); return this.openView!(v); }).catch(e => this.status.textContent = String(e))
      : undefined;
    this.output.replaceChildren(...(data.diagnostics ? [el("h3", {}, "Join cardinality (NULL keys do not match)"), resultTable(data.diagnostics)] : []), resultTable(data.result, drill));
    this.status.textContent = `${data.result.rows.length} rows · revision ${data.result.revision}${data.result.truncated ? " · truncated: more results exist" : " · complete result"}`;
    this.chartOutput.replaceChildren();
    if ((spec.kind === "summary" || spec.kind === "report") && this.chart.checked) drawChart(this.chartOutput, spec, data, this.chartKind.value as ChartKind);
  }
  onEvent(e: ServerEvent) { if (!this.closed && ["table_changed", "cells_changed", "tables_changed", "resync"].includes(e.event)) { this.sequence++; this.status.textContent = "Workbook changed. Run / refresh to update this analysis."; } }
  close() { if (this.job) void this.rpc.cancel(this.job).catch(() => {}); this.closed = true; this.sequence++; this.modal.close(); }
}

export async function replaceDialog(rpc: Rpc, table: string, workbookId: string): Promise<() => void> {
  const schema = (await rpc.call("schema", {})).find(t => t.name === table);
  const column = select("Replace column", schema?.columns.filter(c => c.sql_type === "VARCHAR" && !c.formula).map(c => c.name) ?? []);
  const find = el("input", { "aria-label": "Find text" }), replacement = el("input", { "aria-label": "Replacement text" });
  const status = el("p", { role: "status" }), output = el("div");
  let preview: ReplacePreview | null = null;
  let previewSpec: ReturnType<typeof spec> | null = null;
  let generation = 0;
  const key = `alephgrid-replace-${workbookId}`;
  let revision = Number(localStorage.getItem(key)) || 0;
  const spec = () => ({ table, column: column.value, find: find.value, replacement: replacement.value });
  const run = async (fn: () => Promise<void>) => { try { await fn(); } catch (e) { status.textContent = String(e); } };
  const apply = el("button", { class: "btn", disabled: true, onclick: () => run(async () => {
    if (!preview || !previewSpec) return; apply.disabled = true;
    const r = await rpc.call("replace_apply", { spec: previewSpec, revision: preview.sample.revision }); revision = r.undo_revision;
    localStorage.setItem(key, String(revision)); preview = null; status.textContent = `Replaced ${r.rows} cells. Undo is available until the next workbook change.`;
  }) }, "Apply replacement");
  for (const input of [column, find, replacement]) input.addEventListener("input", () => { generation++; preview = null; apply.disabled = true; });
  const dialog = el("section", { class: "dialog analysis-dialog" }, el("h2", {}, "Find and replace"),
    el("p", {}, "Literal, case-sensitive replacement across this table, including filtered or hidden rows. NULL cells are unchanged. At most 10,000 affected cells; undo refuses intervening edits."),
    field("Column", column), field("Find", find), field("Replace with", replacement),
    el("div", { class: "dialog-actions" }, el("button", { class: "btn", onclick: () => run(async () => {
      const seq = ++generation, request = spec();
      const response = await rpc.call("replace_preview", { spec: request });
      if (seq !== generation) return;
      preview = response; previewSpec = request; output.replaceChildren(resultTable(preview.sample));
      status.textContent = `${preview.matches} matching cells; preview shows up to 50.`; apply.disabled = preview.matches === 0 || preview.matches > 10_000;
    }) }, "Preview replacement"), apply,
    el("button", { class: "btn", onclick: () => run(async () => {
      if (!revision) throw new Error("No replacement to undo in this browser");
      await rpc.call("replace_undo", { revision }); revision = 0; localStorage.removeItem(key); preview = null; apply.disabled = true; status.textContent = "Replacement undone.";
    }) }, "Undo last replacement"), el("button", { class: "btn", onclick: () => modal.close() }, "Close")), status, output);
  const modal = openModal(dialog, () => modal.close()); find.focus(); return () => modal.close();
}

/** Dashboards (F.11): saved analyses as tiles on a 12-column grid, with shared slicers that
 *  filter every report of their table. Tiles are laid out two per row, at most 12. */
export function dashboardsDialog(rpc: Rpc): () => void {
  const status = el("p", { role: "status" });
  const list = el("div", { class: "analysis-saved" });
  const grid = el("div", { class: "dashboard-grid", role: "region", "aria-label": "Dashboard" });
  const name = el("input", { value: "My dashboard", "aria-label": "Dashboard name", maxlength: 200 });
  const pick = el("select", { multiple: true, size: 5, "aria-label": "Analyses on the dashboard" });
  const sTable = el("input", { "aria-label": "Slicer table", placeholder: "orders" });
  const sColumn = el("input", { "aria-label": "Slicer column", placeholder: "region" });
  const sValues = el("textarea", { rows: 2, "aria-label": "Slicer values", placeholder: "One value per line; (NULL) for blanks" });
  let current: Dashboard | null = null;
  const show = async (id: string) => {
    const out: DashboardResult = await rpc.call("dashboard", { dashboard: id });
    current = out.dashboard;
    name.value = out.dashboard.name;
    const s = out.dashboard.slicers[0];
    sTable.value = s?.table ?? ""; sColumn.value = s?.column ?? ""; sValues.value = s ? s.values.map(v => v ?? "(NULL)").join("\n") : "";
    grid.replaceChildren(...out.tiles.map(t => {
      const tile = el("section", { class: "dashboard-tile", "aria-label": t.analysis?.name ?? "Missing analysis" }, el("h3", {}, t.analysis?.name ?? "Missing analysis"));
      tile.style.gridColumn = `${t.tile.x + 1} / span ${t.tile.w}`;
      tile.style.gridRow = `${t.tile.y + 1} / span ${t.tile.h * 3}`;
      if (t.error) tile.append(el("p", { role: "alert" }, t.error));
      if (t.result && t.analysis) {
        if (t.analysis.chart) { const c = el("div", { class: "analysis-chart" }); drawChart(c, t.analysis.spec, t.result, t.analysis.chart_kind ?? "bar"); tile.append(c); }
        tile.append(resultTable(t.result.result));
      }
      return tile;
    }));
    status.textContent = `${out.tiles.length} tiles${out.dashboard.slicers.length ? ` · sliced by ${out.dashboard.slicers.map(s => `${s.table}.${s.column}`).join(", ")}` : ""}`;
  };
  const load = async () => {
    const [boards, analyses]: [Dashboard[], SavedAnalysis[]] = await Promise.all([rpc.call("dashboards", {}), rpc.call("analyses", {})]);
    list.replaceChildren(...boards.map(d => el("button", { class: "btn", onclick: () => void show(d.id).catch(e => status.textContent = String(e)) }, d.name)));
    pick.replaceChildren(...analyses.map(a => el("option", { value: a.id, selected: current?.tiles.some(t => t.analysis === a.id) ?? false }, a.name)));
  };
  const save = async (asNew: boolean) => {
    const ids = [...pick.selectedOptions].map(o => o.value);
    const tiles = ids.map((analysis, i) => ({ analysis, x: (i % 2) * 6, y: Math.floor(i / 2) * 4, w: 6, h: 4 }));
    const slicers = sTable.value && sColumn.value ? [{ table: sTable.value, column: sColumn.value, values: sValues.value.split("\n").filter(v => v !== "").map(v => v === "(NULL)" ? null : v) }] : [];
    const d: Dashboard = await rpc.call("save_dashboard", { saved: asNew ? null : current?.id ?? null, name: name.value, tiles, slicers });
    await load(); await show(d.id);
  };
  const act = (label: string, f: () => Promise<void>) => el("button", { class: "btn", onclick: () => void f().catch(e => status.textContent = String(e)) }, label);
  const panel = el("section", { class: "dialog analysis-dialog" }, el("h2", {}, "Dashboards"),
    el("p", {}, "Saved analyses side by side; a slicer filters every report of its table. At most 12 tiles and 6 slicers."),
    list, field("Dashboard name", name), field("Analyses (up to 12)", pick), field("Slicer table", sTable), field("Slicer column", sColumn), field("Slicer values", sValues),
    el("div", { class: "dialog-actions" }, act("Save dashboard", () => save(false)), act("Save as new", () => save(true)),
      act("Delete", async () => { if (current) { await rpc.call("delete_dashboard", { dashboard: current.id }); current = null; grid.replaceChildren(); await load(); } }),
      el("button", { class: "btn", onclick: () => modal.close() }, "Close")), status, grid);
  const modal = openModal(panel, () => modal.close());
  void load().catch(e => status.textContent = String(e));
  return () => modal.close();
}
