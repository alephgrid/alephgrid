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

/**
 * Summary numbers with digit grouping. Exact values arrive as strings (big integers,
 * decimals); they are grouped textually so no digit is lost to a double conversion.
 */
export function formatStat(v: CellValue | undefined): string {
  if (v === null || v === undefined || v === "" || (typeof v === "number" && Number.isNaN(v))) return "–";
  if (typeof v === "number") return grouped.format(v);
  if (typeof v === "boolean") return v ? "TRUE" : "FALSE";
  const m = /^(-?)(\d+)(\.\d+)?$/.exec(v);
  if (!m) return v;
  const sep = groupSeparator();
  return m[1] + m[2].replace(/\B(?=(\d{3})+(?!\d))/g, sep) + (m[3] ? decimalSeparator() + m[3].slice(1) : "");
}

function groupSeparator(): string {
  return new Intl.NumberFormat().formatToParts(1000).find((p) => p.type === "group")?.value ?? ",";
}

function decimalSeparator(): string {
  return new Intl.NumberFormat().formatToParts(1.5).find((p) => p.type === "decimal")?.value ?? ".";
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
