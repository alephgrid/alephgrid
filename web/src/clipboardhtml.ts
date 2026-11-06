// Cells copied in other spreadsheets arrive as HTML table clipboard data next to the
// tab-separated text. Read here: each cell's raw value (numbers without their display
// formatting, text that must stay text), its number format, bold, italic, underline,
// strikethrough, text colour and fill. Two conventions are understood: class rules in a
// <style> block (`mso-number-format`, CSS-escaped) with raw numbers in `x:num`, and inline
// styles with typed values in `data-sheets-value` and formats in `data-sheets-numberformat`.
//
// The HTML is untrusted. It is parsed into an inert document (DOMParser runs no scripts and
// loads nothing), never added to the page, and only text and the attributes above are read;
// everything else is dropped. Formulas are not taken over: their values are.

import type { PastedCell, StylePatch } from "./protocol";

/** Clipboard HTML larger than this is not read; the plain text is pasted instead. */
const MAX_HTML = 32 * 1024 * 1024;
const MAX_FORMAT = 255;

export type HtmlCells = { cells: PastedCell[][]; tooMany: boolean };

/** The cells of the first table in `html`, or null when there is none (paste the text then).
 *  Stops after `maxCells` cells and says so (`tooMany`). */
export function parseClipboardHtml(html: string, maxCells: number): HtmlCells | null {
  if (!html || html.length > MAX_HTML || !/<table[\s>]/i.test(html)) return null;
  const doc = new DOMParser().parseFromString(html, "text/html");
  const table = doc.querySelector("table");
  if (!table) return null;
  const classes = classRules([...doc.querySelectorAll("style")].map((s) => s.textContent ?? "").join("\n"));
  const grid: PastedCell[][] = [];
  // Cells still covered by a rowspan from above: row → columns taken.
  const taken = new Map<number, Set<number>>();
  let count = 0;
  const rows = [...table.querySelectorAll("tr")].filter((tr) => tr.closest("table") === table);
  for (let r = 0; r < rows.length; r++) {
    const line: PastedCell[] = grid[r] ?? (grid[r] = []);
    let c = 0;
    for (const td of [...rows[r].children].filter((e) => e.tagName === "TD" || e.tagName === "TH")) {
      while (taken.get(r)?.has(c)) c++;
      const colspan = clamp(Number(td.getAttribute("colspan") ?? 1), 1, 1000);
      const rowspan = clamp(Number(td.getAttribute("rowspan") ?? 1), 1, 1000);
      line[c] = readCell(td, classes);
      for (let dr = 0; dr < rowspan; dr++) {
        for (let dc = 0; dc < colspan; dc++) {
          if (dr === 0 && dc === 0) continue;
          if (dr === 0) line[c + dc] = { value: "" };
          else { const s = taken.get(r + dr) ?? new Set<number>(); s.add(c + dc); taken.set(r + dr, s); }
        }
      }
      c += colspan;
      count += colspan * rowspan;
      if (count > maxCells) return { cells: grid, tooMany: true };
    }
  }
  // Rows covered only by rowspans, and holes, become empty cells.
  let width = 0, height = grid.length;
  for (const l of grid) width = Math.max(width, l?.length ?? 0);
  for (const [r, s] of taken) { height = Math.max(height, r + 1); for (const c of s) width = Math.max(width, c + 1); }
  if (!width || !height) return null;
  if (width * height > maxCells) return { cells: [], tooMany: true };
  const cells: PastedCell[][] = [];
  for (let r = 0; r < height; r++) cells.push(Array.from({ length: width }, (_, c) => grid[r]?.[c] ?? { value: "" }));
  return { cells, tooMany: false };
}

function clamp(n: number, lo: number, hi: number): number {
  return Number.isFinite(n) ? Math.min(hi, Math.max(lo, Math.floor(n))) : lo;
}

function readCell(td: Element, classes: Map<string, Map<string, string>>): PastedCell {
  const decls = new Map<string, string>();
  for (const name of td.classList) for (const [k, v] of classes.get(name) ?? []) decls.set(k, v);
  for (const [k, v] of declarations(td.getAttribute("style") ?? "")) decls.set(k, v);
  const shown = cellText(td);
  let value = shown;
  let text = false;
  let isNumber = false;
  // Typed value as JSON: {"1": type (2 text, 3 number, 4 boolean), "2": text, "3": number, "4": 0/1}.
  const typed = json(td.getAttribute("data-sheets-value"));
  if (typed && typeof typed === "object") {
    const t = (typed as Record<string, unknown>)["1"];
    const v = typed as Record<string, unknown>;
    if (t === 2 && typeof v["2"] === "string") { value = v["2"]; text = true; }
    else if (t === 3 && typeof v["3"] === "number" && Number.isFinite(v["3"])) { value = String(v["3"]); isNumber = true; }
    else if (t === 4) { value = v["4"] ? "TRUE" : "FALSE"; }
  } else if (td.hasAttribute("x:num")) {
    const raw = td.getAttribute("x:num") ?? "";
    if (raw !== "" && Number.isFinite(Number(raw))) { value = String(Number(raw)); isNumber = true; }
  } else if (td.hasAttribute("x:bool")) {
    value = /^(true|1)$/i.test(td.getAttribute("x:bool") ?? shown) ? "TRUE" : "FALSE";
  } else if (td.hasAttribute("x:str")) {
    value = td.getAttribute("x:str") || shown;
    text = true;
  }
  const style: StylePatch = {};
  let format: string | null = null;
  const sheetsFormat = json(td.getAttribute("data-sheets-numberformat"));
  if (sheetsFormat && typeof sheetsFormat === "object" && typeof (sheetsFormat as Record<string, unknown>)["2"] === "string") {
    format = (sheetsFormat as Record<string, string>)["2"];
  } else if (decls.has("mso-number-format")) {
    format = numberFormat(unescapeCss(decls.get("mso-number-format")!));
  }
  if (format === "@") { text = true; format = null; }
  if (format && !text && format.length <= MAX_FORMAT) style.num_fmt = format;
  // Text from other apps never becomes a formula here.
  if (!isNumber && value.startsWith("=")) text = true;
  const marks = inlineMarks(td);
  const weight = (decls.get("font-weight") ?? "").toLowerCase();
  if (weight === "bold" || weight === "bolder" || Number(weight) >= 600 || marks.b) style.b = true;
  if (/^(italic|oblique)/i.test(decls.get("font-style") ?? "") || marks.i) style.i = true;
  const deco = `${decls.get("text-decoration") ?? ""} ${decls.get("text-decoration-line") ?? ""}`.toLowerCase();
  if (deco.includes("underline") || marks.u) style.u = true;
  if (deco.includes("line-through") || marks.s) style.s = true;
  const color = cssColor(decls.get("color") ?? "");
  if (color && color !== "#000000") style.color = color;
  const fill = cssColor(decls.get("background-color") ?? "") ?? firstColor(decls.get("background") ?? "");
  if (fill && fill !== "#FFFFFF") style.fill = fill;
  const cell: PastedCell = { value };
  if (text) cell.text = true;
  if (Object.keys(style).length) cell.style = style;
  return cell;
}

function json(s: string | null): unknown {
  if (!s || s.length > 10_000) return null;
  try { return JSON.parse(s); } catch { return null; }
}

/** The text a cell shows: <br> as a line break, runs of source whitespace as one space,
 *  and nothing from scripts, styles or templates. */
function cellText(td: Element): string {
  let out = "";
  const walk = (n: Node) => {
    if (n.nodeType === Node.TEXT_NODE) { out += (n.nodeValue ?? "").replace(/[ \t\n\r\f]+/g, " "); return; }
    if (n.nodeType !== Node.ELEMENT_NODE) return;
    const tag = (n as Element).tagName;
    if (tag === "BR") { out += "\n"; return; }
    if (tag === "SCRIPT" || tag === "STYLE" || tag === "TEMPLATE" || tag === "NOSCRIPT") return;
    n.childNodes.forEach(walk);
  };
  td.childNodes.forEach(walk);
  return out.split("\n").map((l) => l.trim()).join("\n").replace(/ /g, " ");
}

/** Bold, italic, underline or strikethrough given by tags around all of a cell's text. */
function inlineMarks(td: Element): { b: boolean; i: boolean; u: boolean; s: boolean } {
  const all = { b: true, i: true, u: true, s: true };
  let any = false;
  const visit = (n: Node, b: boolean, i: boolean, u: boolean, s: boolean) => {
    if (n.nodeType === Node.TEXT_NODE) {
      if ((n.nodeValue ?? "").trim()) { any = true; all.b &&= b; all.i &&= i; all.u &&= u; all.s &&= s; }
      return;
    }
    if (n.nodeType !== Node.ELEMENT_NODE) return;
    const e = n as Element;
    const tag = e.tagName;
    if (tag === "SCRIPT" || tag === "STYLE" || tag === "TEMPLATE" || tag === "NOSCRIPT") return;
    const d = declarations(e.getAttribute("style") ?? "");
    const weight = (d.get("font-weight") ?? "").toLowerCase();
    const deco = `${d.get("text-decoration") ?? ""} ${d.get("text-decoration-line") ?? ""}`;
    e.childNodes.forEach((c) => visit(c,
      b || tag === "B" || tag === "STRONG" || weight === "bold" || Number(weight) >= 600,
      i || tag === "I" || tag === "EM" || /^(italic|oblique)/i.test(d.get("font-style") ?? ""),
      u || tag === "U" || deco.includes("underline"),
      s || tag === "S" || tag === "STRIKE" || tag === "DEL" || deco.includes("line-through")));
  };
  td.childNodes.forEach((c) => visit(c, false, false, false, false));
  return any ? all : { b: false, i: false, u: false, s: false };
}

/** `.name { … }` rules of a style sheet, by class name; other selectors are ignored. */
function classRules(css: string): Map<string, Map<string, string>> {
  const out = new Map<string, Map<string, string>>();
  const text = css.replace(/\/\*[\s\S]*?\*\//g, "").replace(/<!--|-->/g, "");
  let i = 0;
  while (i < text.length) {
    const open = text.indexOf("{", i);
    if (open < 0) break;
    const close = blockEnd(text, open + 1);
    const selectors = text.slice(i, open).split(",").map((s) => s.trim());
    const decls = declarations(text.slice(open + 1, close));
    for (const sel of selectors) {
      const m = /^(?:td|th)?\.([\w-]+)$/i.exec(sel);
      if (!m) continue;
      const rule = out.get(m[1]) ?? new Map<string, string>();
      for (const [k, v] of decls) rule.set(k, v);
      out.set(m[1], rule);
    }
    i = close + 1;
  }
  return out;
}

/** Index of the `}` closing a block that starts at `from`, skipping quoted strings. */
function blockEnd(text: string, from: number): number {
  let quote = "";
  for (let i = from; i < text.length; i++) {
    const ch = text[i];
    if (ch === "\\") { i++; continue; }
    if (quote) { if (ch === quote) quote = ""; continue; }
    if (ch === '"' || ch === "'") quote = ch;
    else if (ch === "}") return i;
  }
  return text.length;
}

/** `a: 1; b: "x;y"` → {a → 1, b → "x;y"} (names lower-cased; quotes and escapes respected). */
function declarations(text: string): Map<string, string> {
  const out = new Map<string, string>();
  let start = 0;
  let quote = "";
  const take = (end: number) => {
    const part = text.slice(start, end);
    const colon = part.indexOf(":");
    if (colon > 0) out.set(part.slice(0, colon).trim().toLowerCase(), part.slice(colon + 1).trim().replace(/\s*!important$/i, ""));
    start = end + 1;
  };
  for (let i = 0; i < text.length; i++) {
    const ch = text[i];
    if (ch === "\\") { i++; continue; }
    if (quote) { if (ch === quote) quote = ""; continue; }
    if (ch === '"' || ch === "'") quote = ch;
    else if (ch === ";") take(i);
  }
  take(text.length);
  return out;
}

/** A CSS value with its quotes and escapes resolved: `"\0022$\0022\#,\#\#0"` → `"$"#,##0`. */
function unescapeCss(value: string): string {
  let v = value.trim();
  if (v.length >= 2 && (v[0] === '"' || v[0] === "'") && v[v.length - 1] === v[0]) v = v.slice(1, -1);
  let out = "";
  for (let i = 0; i < v.length; i++) {
    const ch = v[i];
    if (ch !== "\\") { out += ch; continue; }
    const hex = /^[0-9a-fA-F]{1,6}/.exec(v.slice(i + 1));
    if (hex) {
      const code = parseInt(hex[0], 16);
      out += code > 0 && code <= 0x10ffff ? String.fromCodePoint(code) : "";
      i += hex[0].length;
      if (v[i + 1] === " ") i++;
    } else if (i + 1 < v.length) {
      out += v[++i];
    }
  }
  return out;
}

/** A number format code from `mso-number-format`: names of built-in formats are spelled out;
 *  `General` (or anything empty) means no format. */
function numberFormat(f: string): string | null {
  const named: Record<string, string | null> = {
    general: null, percent: "0.00%", fixed: "0.00", standard: "#,##0.00", scientific: "0.00E+00",
  };
  const key = f.trim().toLowerCase();
  if (key in named) return named[key];
  return f.trim() ? f : null;
}

const NAMED_COLORS: Record<string, string> = {
  black: "#000000", white: "#FFFFFF", red: "#FF0000", green: "#008000", lime: "#00FF00", blue: "#0000FF",
  yellow: "#FFFF00", aqua: "#00FFFF", cyan: "#00FFFF", fuchsia: "#FF00FF", magenta: "#FF00FF", silver: "#C0C0C0",
  gray: "#808080", grey: "#808080", maroon: "#800000", olive: "#808000", purple: "#800080", teal: "#008080",
  navy: "#000080", orange: "#FFA500",
};

/** `#rgb`, `#rrggbb`, `rgb(r, g, b)` or a basic colour name as `#RRGGBB`; anything else null. */
function cssColor(value: string): string | null {
  const v = value.trim().toLowerCase();
  if (NAMED_COLORS[v]) return NAMED_COLORS[v];
  let m = /^#([0-9a-f]{3})$/.exec(v);
  if (m) return `#${[...m[1]].map((c) => c + c).join("")}`.toUpperCase();
  m = /^#([0-9a-f]{6})$/.exec(v);
  if (m) return `#${m[1]}`.toUpperCase();
  m = /^rgba?\(\s*(\d{1,3})\s*,\s*(\d{1,3})\s*,\s*(\d{1,3})\s*(?:,\s*([\d.]+)\s*)?\)$/.exec(v);
  if (m) {
    if (m[4] !== undefined && Number(m[4]) === 0) return null;
    const parts = [m[1], m[2], m[3]].map((x) => Math.min(255, Number(x)).toString(16).padStart(2, "0"));
    return `#${parts.join("")}`.toUpperCase();
  }
  return null;
}

/** The first colour in a `background` shorthand. */
function firstColor(value: string): string | null {
  for (const part of value.split(/\s+(?![^(]*\))/)) {
    const c = cssColor(part);
    if (c) return c;
  }
  return null;
}
