// S.09: charts placed on a sheet. Each chart floats over the grid at its anchor cell, reads
// its data range from the server (first column categories, first row series names) and is
// redrawn whenever the sheet changes. Charts are moved by their title bar, resized from the
// corner, and edited (type, title, data range) in place; every change to a placed chart is
// saved with the workbook at once. U.04b: Insert chart first shows an unsaved preview with
// Create chart / Cancel; nothing is stored until Create, and Cancel or Escape leaves the sheet
// as it was. Deleting a chart offers Undo.

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
  /** The unsaved preview from Insert chart, if any. */
  private preview: { chart: SheetChart; box: HTMLElement; body: HTMLElement; title: HTMLElement; kind: HTMLSelectElement } | null = null;

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
    this.discardDraft();
    this.set([]);
  }

  /** Remove the unsaved preview without storing anything. */
  discardDraft(): void {
    this.preview?.box.remove();
    this.preview = null;
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
    if (this.preview) {
      const c = this.preview.chart, p = this.host.cellBox(c.row, c.column);
      Object.assign(this.preview.box.style, { left: `${p.x - o.x}px`, top: `${p.y - o.y}px`, width: `${c.width * z}px`, height: `${c.height * z}px` });
    }
  }

  /** Insert chart: preview a chart of `data` to the right of it, unsaved, with Create chart
   *  and Cancel. Resolves true when it was created. */
  draft(data: SheetChart["data"], kind: ChartType = "column"): Promise<boolean> {
    this.discardDraft();
    const chart: SheetChart = {
      chart: `c${Date.now().toString(36)}${Math.random().toString(36).slice(2, 6)}`,
      kind, title: "", data,
      row: data.row, column: data.column + data.columns + 1, width: 480, height: 300,
    };
    const title = el("span", { class: "sheet-chart-title" }, "Chart preview · not saved");
    const kindSel = el("select", { "aria-label": "Chart type" }, ...KINDS.map((k) => el("option", { value: k }, k))) as HTMLSelectElement;
    kindSel.value = kind;
    const create = el("button", { class: "btn btn-primary btn-small", type: "button" }, "Create chart");
    const cancel = el("button", { class: "btn btn-small", type: "button" }, "Cancel");
    const body = el("div", { class: "sheet-chart-body" });
    const box = el("section", { class: "sheet-chart sheet-chart-draft", role: "region", "aria-label": "Chart preview" },
      el("div", { class: "sheet-chart-bar" }, title, kindSel, cancel, create), body);
    this.el.append(box);
    this.preview = { chart, box, body, title, kind: kindSel };
    this.position();
    void this.drawInto(chart, body, null);
    kindSel.addEventListener("change", () => { chart.kind = kindSel.value; void this.drawInto(chart, body, null); });
    return new Promise((resolve) => {
      const finish = async (keep: boolean) => {
        if (this.preview?.box !== box) { resolve(false); return; }
        if (!keep) { this.discardDraft(); resolve(false); return; }
        create.disabled = cancel.disabled = true;
        try {
          await this.host.save([...this.charts, chart]);
          this.discardDraft();
          this.set([...this.charts, chart]);
          resolve(true);
        } catch (e) {
          toast((e as Error).message, "error");
          create.disabled = cancel.disabled = false;
        }
      };
      create.addEventListener("click", () => void finish(true));
      cancel.addEventListener("click", () => void finish(false));
      box.addEventListener("keydown", (e) => { if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); void finish(false); } });
      create.focus();
    });
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
    const bar = el("div", { class: "sheet-chart-bar", title: "Drag to move. Changes to this chart are saved at once." }, title, kind, edit, del);
    const body = el("div", { class: "sheet-chart-body" });
    const grip = el("div", { class: "sheet-chart-grip", title: "Drag to resize", "aria-hidden": "true" });
    const box = el("section", { class: "sheet-chart", role: "region", "aria-label": "Sheet chart" }, bar, body, grip);
    this.el.append(box);
    this.nodes.set(c.chart, { box, body, title, kind });
    const id = c.chart;
    const current = () => this.charts.find((x) => x.chart === id)!;
    kind.addEventListener("change", () => void this.update(id, { kind: kind.value }));
    del.addEventListener("click", async () => {
      const before = this.list();
      await this.commit(this.charts.filter((x) => x.chart !== id));
      if (!this.charts.some((x) => x.chart === id)) toast("Chart deleted", "info", 6000, { label: "Undo", run: () => void this.commit(before) });
    });
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
    await this.drawInto(c, n.body, n.title.textContent);
  }

  private async drawInto(c: SheetChart, body: HTMLElement, heading: string | null): Promise<void> {
    let cells: string[][];
    try { cells = await this.host.values(c.data); } catch (e) { body.replaceChildren(el("p", { class: "chart-empty" }, (e as Error).message)); return; }
    const head = cells[0] ?? [];
    const rows = cells.slice(1);
    const series = [];
    for (let j = 1; j < c.data.columns; j++) {
      series.push({
        name: head[j] || `Column ${columnLetter(c.data.column + j)}`,
        values: rows.map((r) => { const t = (r[j] ?? "").replace(/,/g, ""); const v = Number(t); return t === "" || Number.isNaN(v) ? null : v; }),
      });
    }
    renderChart(body, { labels: rows.map((r) => r[0] ?? ""), series, title: heading ?? `Chart of ${rangeText(c.data)}` }, c.kind as ChartType);
  }
}
