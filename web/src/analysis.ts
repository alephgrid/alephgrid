import { Rpc } from "./api";
import { el, openModal } from "./ui";
import { renderChart, type ChartType } from "./charts";
import type { Aggregate, AnalysisSpec, AnalysisResult, SavedAnalysis, TableSchema, QueryResult, ReplacePreview, ChartKind, Measure, Calculated, Dashboard, DashboardResult, ColumnValues } from "./protocol";

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

/** A chart of a summary or report (U.18): every numeric measure is a series, groups are
 *  the categories (≤100); totals are left out. Values come from the bounded result. */
export function drawChart(out: HTMLElement, spec: AnalysisSpec, data: AnalysisResult, kind: ChartKind): void {
  if (spec.kind !== "summary" && spec.kind !== "report") return;
  const groups = spec.groups.length;
  const cols = data.result.columns;
  const measures = cols.map((c, i) => ({ c, i })).filter(({ c, i }) => i >= groups && ["int", "number"].includes(c.kind) && c.name !== "row_kind");
  if (!measures.length) { out.append(el("p", {}, "Charts need a numeric value.")); return; }
  const kindCol = cols.findIndex(c => c.name === "row_kind");
  const rows = data.result.rows.filter(r => kindCol < 0 || r[kindCol] === "row");
  const label = (r: unknown[]) => r.slice(0, groups).map(v => v === null ? "(Empty)" : String(v)).join(" / ") || "All rows";
  const series = (kind === "pie" ? measures.slice(0, 1) : measures.slice(0, 6)).map(({ c, i }) => ({
    name: c.name, values: rows.map(r => r[i] === null || r[i] === undefined ? null : Number(r[i])),
  }));
  const box = el("div", { class: "chart-box" });
  renderChart(box, { labels: rows.map(label), series, title: spec.kind === "summary" ? "Summary" : "Report" }, kind as ChartType);
  const partial = rows.length > 100 || data.result.truncated;
  out.append(box, el("p", { class: "chart-note" }, `${rows.length} ${rows.length === 1 ? "group" : "groups"}${partial ? "; the chart shows the first 100" : ""}. Totals are not drawn.`));
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
  const status = el("p", { role: "status", class: "dialog-status" });
  const boards = el("select", { "aria-label": "Open a dashboard" }) as HTMLSelectElement;
  const grid = el("div", { class: "dashboard-grid", role: "region", "aria-label": "Dashboard" });
  const name = el("input", { value: "My dashboard", "aria-label": "Dashboard name", maxlength: 200 }) as HTMLInputElement;
  const picks = el("div", { class: "pick-list", role: "group", "aria-label": "Analyses on the dashboard" });
  const sTable = el("select", { "aria-label": "Slicer table" }) as HTMLSelectElement;
  const sColumn = el("select", { "aria-label": "Slicer column" }) as HTMLSelectElement;
  const sValues = el("div", { class: "pick-list", role: "group", "aria-label": "Slicer values" });
  let current: Dashboard | null = null;
  let schema: TableSchema[] = [];
  let chosenValues = new Set<string>();
  const NULL = "\u0000";
  const fillColumns = () => {
    const cols = schema.find((t) => t.name === sTable.value)?.columns ?? [];
    sColumn.replaceChildren(...cols.map((c) => el("option", { value: c.name }, c.name)));
  };
  const loadValues = async () => {
    sValues.replaceChildren();
    if (!sTable.value || !sColumn.value) return;
    const out: ColumnValues = await rpc.call("column_values", { spec: { table: sTable.value, sort: [], filters: [], hidden: [] }, column: sColumn.value, limit: 200 });
    sValues.replaceChildren(...out.values.map((v) => {
      const key = v.value ?? NULL;
      const box = el("input", { type: "checkbox", checked: chosenValues.has(key) }) as HTMLInputElement;
      box.addEventListener("change", () => { if (box.checked) chosenValues.add(key); else chosenValues.delete(key); });
      return el("label", { class: "pick-item" }, box, el("span", {}, v.value === null ? "(Empty)" : v.value), el("span", { class: "muted" }, String(v.count)));
    }));
  };
  const show = async (id: string) => {
    const out: DashboardResult = await rpc.call("dashboard", { dashboard: id });
    current = out.dashboard;
    name.value = out.dashboard.name;
    boards.value = id;
    const sl = out.dashboard.slicers[0];
    if (sl) { sTable.value = sl.table; fillColumns(); sColumn.value = sl.column; chosenValues = new Set(sl.values.map((v) => v ?? NULL)); } else { sTable.value = ""; sColumn.replaceChildren(); chosenValues = new Set(); }
    void loadValues().catch((e) => (status.textContent = String(e)));
    for (const box of picks.querySelectorAll<HTMLInputElement>("input[type=checkbox]")) box.checked = out.dashboard.tiles.some((t) => t.analysis === box.value);
    grid.replaceChildren(...out.tiles.map((t) => {
      const tile = el("section", { class: "dashboard-tile", "aria-label": t.analysis?.name ?? "Missing analysis" }, el("h3", {}, t.analysis?.name ?? "Missing analysis"));
      tile.style.gridColumn = `${t.tile.x + 1} / span ${t.tile.w}`;
      tile.style.gridRow = `${t.tile.y + 1} / span ${t.tile.h * 3}`;
      if (t.error) tile.append(el("p", { role: "alert" }, t.error));
      if (t.result && t.analysis) {
        if (t.analysis.chart) { const c = el("div", { class: "analysis-chart" }); drawChart(c, t.analysis.spec, t.result, t.analysis.chart_kind ?? "column"); tile.append(c); }
        tile.append(resultTable(t.result.result));
      }
      return tile;
    }));
    status.textContent = `${out.tiles.length} tiles${out.dashboard.slicers.length ? ` · sliced by ${out.dashboard.slicers.map((x) => `${x.table}.${x.column}`).join(", ")}` : ""}`;
  };
  const load = async () => {
    const [list, analyses, sch]: [Dashboard[], SavedAnalysis[], TableSchema[]] = await Promise.all([rpc.call("dashboards", {}), rpc.call("analyses", {}), rpc.call("schema", {})]);
    schema = sch;
    boards.replaceChildren(el("option", { value: "" }, list.length ? "Open a dashboard…" : "No dashboards yet"), ...list.map((d) => el("option", { value: d.id, selected: d.id === current?.id }, d.name)));
    picks.replaceChildren(...(analyses.length ? analyses.map((a) => el("label", { class: "pick-item" },
      el("input", { type: "checkbox", value: a.id, checked: current?.tiles.some((t) => t.analysis === a.id) ?? false }), el("span", {}, a.name))) : [el("p", { class: "muted" }, "Save an analysis in Analyze first.")]));
    const keep = sTable.value;
    sTable.replaceChildren(el("option", { value: "" }, "No slicer"), ...schema.map((t) => el("option", { value: t.name }, t.name)));
    sTable.value = keep;
  };
  boards.addEventListener("change", () => { if (boards.value) void show(boards.value).catch((e) => (status.textContent = String(e))); });
  sTable.addEventListener("change", () => { chosenValues.clear(); fillColumns(); void loadValues().catch((e) => (status.textContent = String(e))); });
  sColumn.addEventListener("change", () => { chosenValues.clear(); void loadValues().catch((e) => (status.textContent = String(e))); });
  const save = async (asNew: boolean) => {
    const ids = [...picks.querySelectorAll<HTMLInputElement>("input[type=checkbox]:checked")].map((b) => b.value);
    if (ids.length > 12) throw new Error("A dashboard holds at most 12 analyses");
    const tiles = ids.map((analysis, i) => ({ analysis, x: (i % 2) * 6, y: Math.floor(i / 2) * 4, w: 6, h: 4 }));
    const slicers = sTable.value && sColumn.value && chosenValues.size ? [{ table: sTable.value, column: sColumn.value, values: [...chosenValues].map((v) => (v === NULL ? null : v)) }] : [];
    const d: Dashboard = await rpc.call("save_dashboard", { saved: asNew ? null : current?.id ?? null, name: name.value, tiles, slicers });
    current = d;
    await load(); await show(d.id);
  };
  const act = (label: string, f: () => Promise<void>, cls = "btn") => el("button", { class: cls, onclick: () => void f().catch((e) => (status.textContent = String(e))) }, label);
  const panel = el("section", { class: "dialog analysis-dialog dashboards-dialog" },
    el("header", { class: "dialog-head" }, el("h2", {}, "Dashboards"), boards),
    el("p", { class: "dialog-lede" }, "Saved analyses side by side; a slicer keeps only the chosen values in every report of its table. At most 12 tiles."),
    el("div", { class: "dialog-columns" },
      el("div", {}, field("Name", name), el("div", { class: "analysis-field" }, el("span", {}, "Analyses"), picks)),
      el("div", {}, field("Slicer table", sTable), field("Slicer column", sColumn), el("div", { class: "analysis-field" }, el("span", {}, "Keep these values"), sValues))),
    el("div", { class: "dialog-actions" }, act("Save", () => save(false), "btn btn-primary"), act("Save as new", () => save(true)),
      act("Delete", async () => { if (current) { await rpc.call("delete_dashboard", { dashboard: current.id }); current = null; grid.replaceChildren(); await load(); } }, "btn btn-ghost"),
      el("button", { class: "btn", onclick: () => modal.close() }, "Close")), status, grid);
  const modal = openModal(panel, () => modal.close());
  void load().catch((e) => (status.textContent = String(e)));
  return () => modal.close();
}
