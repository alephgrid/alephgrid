// S.09: charts placed on a sheet. Each chart floats over the grid at its anchor cell, reads
// its data range from the server (first column categories, first row series names) and is
// redrawn whenever the sheet changes. Charts are moved by their title bar, resized from the
// corner, and edited (type, title, data range) in place; every change is saved with the
// workbook.

import { renderChart, type ChartType } from "./charts";
import type { SheetChart } from "./protocol";
import { columnLetter } from "./format";
import { referenceRanges } from "./references";
import { el, formDialog, toast } from "./ui";

export interface ChartsHost {
  /** Top-left of a cell in CSS pixels relative to the grid, and the zoom. */
  cellBox(r: number, c: number): { x: number; y: number };
  zoom(): number;
  /** The body's origin (below the column header, right of the row header). */
  bodyOrigin(): { x: number; y: number };
  /** The cell under a viewport point, or null outside the body. */
  cellAt(clientX: number, clientY: number): { r: number; c: number } | null;
  /** The shown text of the data range's cells (row-major), from the server. */
  values(area: SheetChart["data"]): Promise<string[][]>;
  save(charts: SheetChart[]): Promise<void>;
}

const KINDS: ChartType[] = ["column", "bar", "line", "area", "scatter", "pie"];

export function rangeText(a: SheetChart["data"]): string {
  return `${columnLetter(a.column)}${a.row + 1}:${columnLetter(a.column + a.columns - 1)}${a.row + a.rows}`;
}

export class SheetChartsLayer {
  readonly el = el("div", { class: "sheet-charts" });
  private charts: SheetChart[] = [];
  private nodes = new Map<string, { box: HTMLElement; body: HTMLElement; title: HTMLElement; kind: HTMLSelectElement }>();

  constructor(private host: ChartsHost) {}

  list(): SheetChart[] {
    return this.charts.map((c) => ({ ...c, data: { ...c.data } }));
  }

  /** Show these charts (from the server) and read their values again. */
  set(charts: SheetChart[]): void {
    this.charts = charts.map((c) => ({ ...c, data: { ...c.data } }));
    for (const [id, n] of this.nodes) if (!this.charts.some((c) => c.chart === id)) { n.box.remove(); this.nodes.delete(id); }
    for (const c of this.charts) if (!this.nodes.has(c.chart)) this.build(c);
    this.position();
    void this.refresh();
  }

  clear(): void {
    this.set([]);
  }

  /** Read every chart's values again (after the sheet changed). */
  async refresh(): Promise<void> {
    await Promise.all(this.charts.map((c) => this.draw(c)));
  }

  /** Follow the grid's scroll and zoom. */
  position(): void {
    const o = this.host.bodyOrigin(), z = this.host.zoom();
    Object.assign(this.el.style, { left: `${o.x}px`, top: `${o.y}px` });
    for (const c of this.charts) {
      const n = this.nodes.get(c.chart);
      if (!n) continue;
      const p = this.host.cellBox(c.row, c.column);
      Object.assign(n.box.style, { left: `${p.x - o.x}px`, top: `${p.y - o.y}px`, width: `${c.width * z}px`, height: `${c.height * z}px` });
    }
  }

  /** Add a chart of `data` at the cell to the right of it. */
  async add(data: SheetChart["data"], kind: ChartType = "column"): Promise<void> {
    const chart: SheetChart = {
      chart: `c${Date.now().toString(36)}${Math.random().toString(36).slice(2, 6)}`,
      kind, title: "", data,
      row: data.row, column: data.column + data.columns + 1, width: 480, height: 300,
    };
    await this.commit([...this.charts, chart]);
  }

  private async commit(next: SheetChart[]): Promise<void> {
    try {
      await this.host.save(next);
      this.set(next);
    } catch (e) {
      toast((e as Error).message, "error");
      this.set(this.charts);
    }
  }

  private update(id: string, patch: Partial<SheetChart>): Promise<void> {
    return this.commit(this.charts.map((c) => (c.chart === id ? { ...c, ...patch } : c)));
  }

  private build(c: SheetChart): void {
    const title = el("span", { class: "sheet-chart-title" });
    const kind = el("select", { "aria-label": "Chart type" }, ...KINDS.map((k) => el("option", { value: k }, k))) as HTMLSelectElement;
    const edit = el("button", { class: "icon-btn", type: "button", title: "Edit chart", "aria-label": "Edit chart" }, "✎");
    const del = el("button", { class: "icon-btn", type: "button", title: "Delete chart", "aria-label": "Delete chart" }, "×");
    const bar = el("div", { class: "sheet-chart-bar", title: "Drag to move" }, title, kind, edit, del);
    const body = el("div", { class: "sheet-chart-body" });
    const grip = el("div", { class: "sheet-chart-grip", title: "Drag to resize", "aria-hidden": "true" });
    const box = el("section", { class: "sheet-chart", role: "region", "aria-label": "Sheet chart" }, bar, body, grip);
    this.el.append(box);
    this.nodes.set(c.chart, { box, body, title, kind });
    const id = c.chart;
    const current = () => this.charts.find((x) => x.chart === id)!;
    kind.addEventListener("change", () => void this.update(id, { kind: kind.value }));
    del.addEventListener("click", () => void this.commit(this.charts.filter((x) => x.chart !== id)));
    edit.addEventListener("click", async () => {
      const now = current();
      const v = await formDialog("Edit chart", [
        { name: "title", label: "Title", value: now.title },
        { name: "range", label: "Data range (first column: categories, first row: series names)", value: rangeText(now.data), mono: true },
      ], "Save", (x) => {
        const r = referenceRanges(x.range.trim())[0];
        if (!r) return "Write the range as A1:C10";
        if (r.r1 - r.r0 < 1 || r.c1 - r.c0 < 1) return "A chart reads at least two rows and two columns";
        return null;
      });
      if (!v) return;
      const r = referenceRanges(v.range.trim())[0];
      await this.update(id, { title: v.title.trim(), data: { row: r.r0, column: r.c0, rows: r.r1 - r.r0 + 1, columns: r.c1 - r.c0 + 1 } });
    });
    // Move by the title bar; drop anchors the chart at the cell under its top-left corner.
    bar.addEventListener("pointerdown", (e) => {
      if ((e.target as HTMLElement).closest("button, select")) return;
      e.preventDefault();
      bar.setPointerCapture(e.pointerId);
      const start = { x: e.clientX, y: e.clientY, left: box.offsetLeft, top: box.offsetTop };
      const move = (m: PointerEvent) => { box.style.left = `${start.left + m.clientX - start.x}px`; box.style.top = `${start.top + m.clientY - start.y}px`; };
      const up = (u: PointerEvent) => {
        bar.removeEventListener("pointermove", move);
        bar.removeEventListener("pointerup", up);
        const r = box.getBoundingClientRect();
        const cell = this.host.cellAt(r.left + 2, r.top + 2);
        if (cell && (u.clientX !== start.x || u.clientY !== start.y)) void this.update(id, { row: cell.r, column: cell.c });
        else this.position();
      };
      bar.addEventListener("pointermove", move);
      bar.addEventListener("pointerup", up);
    });
    grip.addEventListener("pointerdown", (e) => {
      e.preventDefault();
      grip.setPointerCapture(e.pointerId);
      const z = this.host.zoom(), now = current();
      const start = { x: e.clientX, y: e.clientY, w: now.width, h: now.height };
      const size = (m: PointerEvent) => ({ width: Math.round(Math.max(160, Math.min(2000, start.w + (m.clientX - start.x) / z))), height: Math.round(Math.max(120, Math.min(2000, start.h + (m.clientY - start.y) / z))) });
      const move = (m: PointerEvent) => { const s = size(m); box.style.width = `${s.width * z}px`; box.style.height = `${s.height * z}px`; };
      const up = (u: PointerEvent) => {
        grip.removeEventListener("pointermove", move);
        grip.removeEventListener("pointerup", up);
        void this.update(id, size(u));
      };
      grip.addEventListener("pointermove", move);
      grip.addEventListener("pointerup", up);
    });
  }

  private async draw(c: SheetChart): Promise<void> {
    const n = this.nodes.get(c.chart);
    if (!n) return;
    n.kind.value = c.kind;
    n.title.textContent = c.title || `Chart of ${rangeText(c.data)}`;
    n.box.setAttribute("aria-label", n.title.textContent);
    let cells: string[][];
    try { cells = await this.host.values(c.data); } catch (e) { n.body.replaceChildren(el("p", { class: "chart-empty" }, (e as Error).message)); return; }
    const head = cells[0] ?? [];
    const rows = cells.slice(1);
    const series = [];
    for (let j = 1; j < c.data.columns; j++) {
      series.push({
        name: head[j] || `Column ${columnLetter(c.data.column + j)}`,
        values: rows.map((r) => { const t = (r[j] ?? "").replace(/,/g, ""); const v = Number(t); return t === "" || Number.isNaN(v) ? null : v; }),
      });
    }
    renderChart(n.body, { labels: rows.map((r) => r[0] ?? ""), series, title: n.title.textContent }, c.kind as ChartType);
  }
}
