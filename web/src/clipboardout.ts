// Copying cells out to other spreadsheets (F.04b): next to the tab-separated text, the
// clipboard gets an HTML table whose cells show their displayed (number-formatted) values
// and carry their raw values and basic formats, in the attributes and inline styles other
// spreadsheets read (and that `clipboardhtml.ts` reads back): `data-sheets-value` and
// `x:num` / `x:bool` for raw values, `data-sheets-numberformat` and `mso-number-format` for
// number formats, and CSS for bold, italic, underline, strikethrough, text colour and fill.
//
// The table is built with DOM methods in a detached document, never from strings: every
// value is set as text or as an attribute value, so the serializer escapes it and no cell
// can add markup.

import type { CellStyle } from "./protocol";

/** One copied cell. `number` is its computed number when it is one (formulas included). */
export type OutCell = {
  display: string;
  kind: "number" | "text" | "boolean" | "error" | "empty";
  number?: number;
  style?: CellStyle;
};

/** At most this many cells get the HTML form; larger copies are text only. */
export const MAX_HTML_CELLS = 100_000;

/** The clipboard HTML for `rows` (a rectangle of cells). */
export function clipboardHtml(rows: OutCell[][]): string {
  const doc = document.implementation.createHTMLDocument("");
  const table = doc.createElement("table");
  table.setAttribute("cellspacing", "0");
  table.setAttribute("cellpadding", "0");
  table.setAttribute("style", "border-collapse: collapse");
  const body = doc.createElement("tbody");
  table.append(body);
  for (const row of rows) {
    const tr = doc.createElement("tr");
    for (const cell of row) tr.append(td(doc, cell));
    body.append(tr);
  }
  const meta = doc.createElement("meta");
  meta.setAttribute("charset", "utf-8");
  return meta.outerHTML + table.outerHTML;
}

function td(doc: Document, cell: OutCell): HTMLTableCellElement {
  const td = doc.createElement("td");
  // Line breaks inside a cell are <br>, as other spreadsheets write them.
  cell.display.split("\n").forEach((line, i) => {
    if (i) td.append(doc.createElement("br"));
    if (line) td.append(doc.createTextNode(line));
  });
  const css: string[] = [];
  const s = cell.style;
  const fmt = s?.num_fmt;
  if (cell.kind === "number" && cell.number !== undefined && Number.isFinite(cell.number)) {
    td.setAttribute("data-sheets-value", JSON.stringify({ 1: 3, 3: cell.number }));
    td.setAttribute("x:num", String(cell.number));
  } else if (cell.kind === "boolean") {
    td.setAttribute("data-sheets-value", JSON.stringify({ 1: 4, 4: cell.display.toUpperCase() === "TRUE" ? 1 : 0 }));
    td.setAttribute("x:bool", cell.display.toUpperCase());
  } else if (cell.kind === "text") {
    td.setAttribute("data-sheets-value", JSON.stringify({ 1: 2, 2: cell.display }));
    // Text that reads like a number (007, 1-2) stays text in the other spreadsheet.
    if (!fmt) css.push(`mso-number-format:${cssString("@")}`);
  }
  if (fmt && cell.kind !== "text") {
    td.setAttribute("data-sheets-numberformat", JSON.stringify({ 1: 2, 2: fmt, 3: 1 }));
    css.push(`mso-number-format:${cssString(fmt)}`);
  }
  if (s?.b) css.push("font-weight:bold");
  if (s?.i) css.push("font-style:italic");
  const lines = [s?.u ? "underline" : "", s?.s ? "line-through" : ""].filter(Boolean);
  if (lines.length) css.push(`text-decoration:${lines.join(" ")}`);
  if (s?.color && /^#[0-9A-Fa-f]{6}$/.test(s.color)) css.push(`color:${s.color}`);
  if (s?.fill && /^#[0-9A-Fa-f]{6}$/.test(s.fill)) css.push(`background-color:${s.fill}`);
  if (s?.h && /^(left|center|right)$/.test(s.h)) css.push(`text-align:${s.h}`);
  if (css.length) td.setAttribute("style", css.join(";"));
  return td;
}

/** `value` as a quoted CSS string: quotes and backslashes escaped, line breaks as escapes. */
function cssString(value: string): string {
  return `"${value.replace(/[\\"]/g, (c) => `\\${c}`).replace(/[\n\r\f]/g, (c) => `\\${c.charCodeAt(0).toString(16)} `)}"`;
}

/** Put `text` (and `html`, when given) on the system clipboard. Falls back to the text alone
 *  where the browser cannot write HTML. */
export async function writeClipboard(text: string, html?: string): Promise<void> {
  if (html && typeof ClipboardItem !== "undefined" && navigator.clipboard?.write) {
    try {
      await navigator.clipboard.write([new ClipboardItem({
        "text/plain": new Blob([text], { type: "text/plain" }),
        "text/html": new Blob([html], { type: "text/html" }),
      })]);
      return;
    } catch { /* the text alone, below */ }
  }
  await navigator.clipboard.writeText(text);
}

/** What the system clipboard holds, read from a menu (paste special); null when the browser
 *  does not let the page read it (or the person did not allow it in time). */
export async function readClipboard(waitMs = 1500): Promise<{ text: string; html?: string } | null> {
  const read = (async () => {
    try {
      if (navigator.clipboard?.read) {
        const items = await navigator.clipboard.read();
        let text = "", html: string | undefined;
        for (const item of items) {
          if (item.types.includes("text/html") && html === undefined) html = await (await item.getType("text/html")).text();
          if (item.types.includes("text/plain") && !text) text = await (await item.getType("text/plain")).text();
        }
        if (text || html) return { text, html };
      }
      if (navigator.clipboard?.readText) {
        const text = await navigator.clipboard.readText();
        return text ? { text } : null;
      }
    } catch { /* not allowed */ }
    return null;
  })();
  const timeout = new Promise<null>((ok) => setTimeout(() => ok(null), waitMs));
  return Promise.race([read, timeout]);
}
