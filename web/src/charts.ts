// U.18: charts drawn from bounded, aggregated results: bar, column, line, area, scatter and
// pie, with axes, value labels, a legend and theme colours. Every chart also carries its
// values as a table for screen readers and for checking against the result.

import { el } from "./ui";

export type ChartType = "bar" | "column" | "line" | "area" | "scatter" | "pie";

interface ChartData {
  /** One label per category (x axis, bar or slice). */
  labels: string[];
  series: { name: string; values: (number | null)[] }[];
  title?: string;
}

const NS = "http://www.w3.org/2000/svg";
const MAX_CATEGORIES = 100;

function svgEl<K extends keyof SVGElementTagNameMap>(tag: K, attrs: Record<string, string | number>, text?: string): SVGElementTagNameMap[K] {
  const e = document.createElementNS(NS, tag);
  for (const [k, v] of Object.entries(attrs)) e.setAttribute(k, String(v));
  if (text !== undefined) e.textContent = text;
  return e;
}

/** A readable number for an axis or label: up to 3 significant digits with k/M/B. */
function shortNumber(v: number): string {
  const a = Math.abs(v);
  const fmt = (n: number, s: string) => `${Number(n.toPrecision(3))}${s}`;
  if (a >= 1e9) return fmt(v / 1e9, "B");
  if (a >= 1e6) return fmt(v / 1e6, "M");
  if (a >= 1e4) return fmt(v / 1e3, "k");
  return Number.isInteger(v) ? String(v) : String(Number(v.toPrecision(4)));
}

/** "Nice" ticks covering [lo, hi]. */
function ticks(lo: number, hi: number, count = 5): number[] {
  if (!(hi > lo)) { hi = lo + 1; }
  const raw = (hi - lo) / count;
  const mag = 10 ** Math.floor(Math.log10(raw));
  const step = [1, 2, 2.5, 5, 10].map((m) => m * mag).find((s) => s >= raw) ?? raw;
  const start = Math.floor(lo / step) * step;
  const out: number[] = [];
  for (let t = start; t <= hi + step * 1e-9; t += step) out.push(Number(t.toPrecision(12)));
  return out;
}

const truncate = (s: string, n: number) => (s.length > n ? s.slice(0, n - 1) + "…" : s);

/** Draw `data` as a `type` chart into `out` (replacing its content). */
export function renderChart(out: HTMLElement, data: ChartData, type: ChartType): void {
  out.replaceChildren();
  const labels = data.labels.slice(0, MAX_CATEGORIES);
  const series = data.series.map((s) => ({ ...s, values: s.values.slice(0, MAX_CATEGORIES) }));
  if (!labels.length || !series.length) { out.append(el("p", { class: "chart-empty" }, "Nothing to chart.")); return; }
  const W = 640, H = type === "bar" ? Math.max(160, 28 * labels.length + 40) : 280;
  const svg = svgEl("svg", { viewBox: `0 0 ${W} ${H}`, class: `chart chart-${type}`, role: "img", "aria-label": `${data.title ?? "Chart"}: ${type} chart of ${series.map((s) => s.name).join(", ")} by category` });
  const color = (i: number) => `chart-c${i % 6}`;

  if (type === "pie") {
    const s = series[0];
    const vals = s.values.map((v) => (v !== null && Number.isFinite(v) && v > 0 ? v : 0));
    const total = vals.reduce((a, b) => a + b, 0);
    const cx = 150, cy = H / 2, r = Math.min(120, H / 2 - 16);
    let angle = -Math.PI / 2;
    vals.forEach((v, i) => {
      if (!total || !v) return;
      const a = (v / total) * Math.PI * 2;
      const x1 = cx + r * Math.cos(angle), y1 = cy + r * Math.sin(angle);
      const x2 = cx + r * Math.cos(angle + a), y2 = cy + r * Math.sin(angle + a);
      const d = a >= Math.PI * 2 - 1e-9
        ? `M ${cx - r} ${cy} a ${r} ${r} 0 1 0 ${2 * r} 0 a ${r} ${r} 0 1 0 ${-2 * r} 0`
        : `M ${cx} ${cy} L ${x1} ${y1} A ${r} ${r} 0 ${a > Math.PI ? 1 : 0} 1 ${x2} ${y2} Z`;
      const p = svgEl("path", { d, class: `chart-slice ${color(i)}` });
      p.append(svgEl("title", {}, `${labels[i]}: ${s.values[i]} (${((v / total) * 100).toFixed(1)}%)`));
      svg.append(p);
      angle += a;
    });
    // Legend with percentages.
    labels.slice(0, 14).forEach((l, i) => {
      const y = 24 + i * 18;
      svg.append(svgEl("rect", { x: 300, y: y - 9, width: 10, height: 10, rx: 2, class: color(i) }));
      const pct = total && vals[i] ? ` · ${((vals[i] / total) * 100).toFixed(1)}%` : "";
      svg.append(svgEl("text", { x: 316, y, class: "chart-legend-text" }, `${truncate(l, 28)}${pct}`));
    });
    if (labels.length > 14) svg.append(svgEl("text", { x: 316, y: 24 + 14 * 18, class: "chart-legend-text" }, `+${labels.length - 14} more`));
    out.append(svg, valueTable(labels, series));
    return;
  }

  const all = series.flatMap((s) => s.values).filter((v): v is number => v !== null && Number.isFinite(v));
  const lo = Math.min(0, ...all), hi = Math.max(0, ...all, lo === 0 && all.every((v) => v === 0) ? 1 : 0);
  const tickVals = ticks(lo, hi);
  const vmin = Math.min(lo, tickVals[0]), vmax = Math.max(hi, tickVals[tickVals.length - 1]);
  const legend = series.length > 1;
  const top = legend ? 34 : 14;

  if (type === "bar") {
    const left = 140, right = 60, bottom = 26;
    const plotW = W - left - right;
    const x = (v: number) => left + ((v - vmin) / (vmax - vmin || 1)) * plotW;
    const band = (H - top - bottom) / labels.length;
    for (const t of tickVals) {
      svg.append(svgEl("line", { x1: x(t), x2: x(t), y1: top, y2: H - bottom, class: t === 0 ? "chart-axis" : "chart-grid" }));
      svg.append(svgEl("text", { x: x(t), y: H - bottom + 16, class: "chart-tick", "text-anchor": "middle" }, shortNumber(t)));
    }
    labels.forEach((l, i) => {
      const y0 = top + i * band;
      svg.append(svgEl("text", { x: left - 8, y: y0 + band / 2 + 4, class: "chart-tick", "text-anchor": "end" }, truncate(l, 20)));
      const bh = Math.max(2, (band * 0.7) / series.length);
      series.forEach((s, k) => {
        const v = s.values[i];
        if (v === null || !Number.isFinite(v)) return;
        const xa = x(Math.min(0, v)), xb = x(Math.max(0, v));
        const r = svgEl("rect", { x: xa, y: y0 + band * 0.15 + k * bh, width: Math.max(1, xb - xa), height: bh, rx: 2, class: color(k) });
        r.append(svgEl("title", {}, `${l} · ${s.name}: ${v}`));
        svg.append(r);
        if (series.length === 1) svg.append(svgEl("text", { x: xb + 4, y: y0 + band / 2 + 4, class: "chart-value" }, shortNumber(v)));
      });
    });
  } else {
    const left = 56, right = 16, bottom = 54;
    const plotW = W - left - right, plotH = H - top - bottom;
    const y = (v: number) => top + plotH - ((v - vmin) / (vmax - vmin || 1)) * plotH;
    const n = labels.length;
    const band = plotW / n;
    const cxOf = (i: number) => left + band * (i + 0.5);
    for (const t of tickVals) {
      svg.append(svgEl("line", { x1: left, x2: W - right, y1: y(t), y2: y(t), class: t === 0 ? "chart-axis" : "chart-grid" }));
      svg.append(svgEl("text", { x: left - 6, y: y(t) + 4, class: "chart-tick", "text-anchor": "end" }, shortNumber(t)));
    }
    const every = Math.max(1, Math.ceil(n / 16));
    labels.forEach((l, i) => {
      if (i % every) return;
      const t = svgEl("text", { x: cxOf(i), y: H - bottom + 16, class: "chart-tick", "text-anchor": n > 8 ? "end" : "middle" }, truncate(l, 14));
      if (n > 8) t.setAttribute("transform", `rotate(-35 ${cxOf(i)} ${H - bottom + 16})`);
      svg.append(t);
    });
    series.forEach((s, k) => {
      if (type === "column") {
        const bw = Math.max(1, (band * 0.7) / series.length);
        s.values.forEach((v, i) => {
          if (v === null || !Number.isFinite(v)) return;
          const ya = y(Math.max(0, v)), yb = y(Math.min(0, v));
          const r = svgEl("rect", { x: left + band * i + band * 0.15 + k * bw, y: ya, width: bw, height: Math.max(1, yb - ya), rx: 2, class: color(k) });
          r.append(svgEl("title", {}, `${labels[i]} · ${s.name}: ${v}`));
          svg.append(r);
        });
        return;
      }
      const pts = s.values.map((v, i) => (v === null || !Number.isFinite(v) ? null : [cxOf(i), y(v)] as const));
      if (type === "line" || type === "area") {
        // Gaps (empty values) break the line.
        let run: (readonly [number, number])[] = [];
        const flush = () => {
          if (run.length > 1 || (run.length === 1 && type === "line")) {
            if (type === "area") {
              const d = `M ${run[0][0]} ${y(0)} ` + run.map(([px, py]) => `L ${px} ${py}`).join(" ") + ` L ${run[run.length - 1][0]} ${y(0)} Z`;
              svg.append(svgEl("path", { d, class: `chart-area ${color(k)}` }));
            }
            svg.append(svgEl("polyline", { points: run.map(([px, py]) => `${px},${py}`).join(" "), class: `chart-line ${color(k)}` }));
          }
          run = [];
        };
        for (const p of pts) { if (p) run.push(p); else flush(); }
        flush();
      }
      pts.forEach((p, i) => {
        if (!p) return;
        const c = svgEl("circle", { cx: p[0], cy: p[1], r: type === "scatter" ? 4 : 3, class: `chart-point ${color(k)}` });
        c.append(svgEl("title", {}, `${labels[i]} · ${s.name}: ${s.values[i]}`));
        svg.append(c);
      });
    });
  }
  if (legend) {
    let lx = 12;
    series.forEach((s, k) => {
      svg.append(svgEl("rect", { x: lx, y: 10, width: 10, height: 10, rx: 2, class: color(k) }));
      const name = truncate(s.name, 24);
      svg.append(svgEl("text", { x: lx + 14, y: 19, class: "chart-legend-text" }, name));
      lx += 30 + name.length * 7;
    });
  }
  out.append(svg, valueTable(labels, series));
}

/** The charted values as a table (visually hidden unless asked for). */
function valueTable(labels: string[], series: ChartData["series"]): HTMLElement {
  const table = el("table", { class: "chart-values sr-only", "aria-label": "Chart values" },
    el("thead", {}, el("tr", {}, el("th", {}, "Category"), ...series.map((s) => el("th", {}, s.name)))),
    el("tbody", {}, ...labels.map((l, i) => el("tr", {}, el("th", {}, l), ...series.map((s) => el("td", {}, s.values[i] === null ? "" : String(s.values[i])))))));
  return table;
}
