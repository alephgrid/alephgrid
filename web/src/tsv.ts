// The clipboard's tab-separated text: cells with a tab, newline or quote are quoted, and
// quotes inside are doubled (as spreadsheets write it).

import { rawText } from "./format";
import type { CellValue } from "./types";

export function parseTsv(text: string): string[][] {
  const rows: string[][] = [];
  let row: string[] = [];
  let cell = "";
  let quoted = false;
  for (let i = 0; i < text.length; i++) {
    const ch = text[i];
    if (quoted) {
      if (ch === '"' && text[i + 1] === '"') { cell += '"'; i++; }
      else if (ch === '"') quoted = false;
      else cell += ch;
    } else if (ch === '"' && cell === "") quoted = true;
    else if (ch === "\t") { row.push(cell); cell = ""; }
    else if (ch === "\n" || ch === "\r") {
      if (ch === "\r" && text[i + 1] === "\n") i++;
      row.push(cell); rows.push(row); row = []; cell = "";
    } else cell += ch;
  }
  if (cell !== "" || row.length) { row.push(cell); rows.push(row); }
  return rows;
}

export function tsvCell(v: CellValue): string {
  const s = rawText(v);
  return /[\t\n"]/.test(s) ? `"${s.replace(/"/g, '""')}"` : s;
}
