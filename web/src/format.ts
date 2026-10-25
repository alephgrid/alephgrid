import type { CellValue, Kind } from "./types";

const grouped = new Intl.NumberFormat(undefined, { maximumFractionDigits: 2 });

/** Excel "General"-style display: no thousands separators, at most ~10 significant digits. */
export function formatValue(v: CellValue, kind: Kind): string {
  if (v === null || v === undefined) return "";
  if (typeof v === "boolean") return v ? "TRUE" : "FALSE";
  if (typeof v === "number") {
    if (kind === "int" || Number.isInteger(v)) return String(v);
    const abs = Math.abs(v);
    if (abs !== 0 && (abs >= 1e15 || abs < 1e-6)) return v.toExponential(4);
    return String(parseFloat(v.toPrecision(10)));
  }
  if (kind === "datetime") return v.replace("T", " ").replace(/\.\d+$/, "");
  return v;
}

export function formatStat(v: number | undefined | null): string {
  if (v === null || v === undefined || Number.isNaN(v)) return "–";
  return grouped.format(v);
}

export function isNumericKind(kind: Kind): boolean {
  return kind === "int" || kind === "number";
}

/** Raw text for editing / copying. */
export function rawText(v: CellValue): string {
  if (v === null || v === undefined) return "";
  if (typeof v === "boolean") return v ? "true" : "false";
  return String(v);
}

export function columnLetter(index: number): string {
  let s = "";
  let n = index + 1;
  while (n > 0) {
    const r = (n - 1) % 26;
    s = String.fromCharCode(65 + r) + s;
    n = Math.floor((n - 1) / 26);
  }
  return s;
}

export function formatCount(n: number): string {
  return n.toLocaleString();
}
