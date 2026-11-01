import { Rpc } from "./api";
import { el, openModal } from "./ui";
import { formatValue } from "./format";
import type { CellValue, Column, Presentation, DisplayFormat, DisplayKind, ColorRule, Condition } from "./protocol";
export const emptyPresentation = (): Presentation => ({ hidden: [], frozen: 0, formats: {}, rules: [] });
const numberFormats = new Map<string, Intl.NumberFormat>();
const dateFormats = new Map<string, Intl.DateTimeFormat>();
function numberFormat(locale: string) { let f = numberFormats.get(locale); if (!f) { f = new Intl.NumberFormat(locale); numberFormats.set(locale, f); } return f; }
/** Decimal strings never pass through Number; formatting rounds using integer arithmetic. */
export function displayValue(value: CellValue, column: Column, f?: DisplayFormat): string {
  if (!f || f.kind === "general" || value === null) return formatValue(value, column.kind);
  if (f.kind === "number" || f.kind === "percent") {
    if (typeof value === "number" && Number.isFinite(value)) {
      const key = `${f.locale}:${f.kind}:${f.decimals}`;
      let nf = numberFormats.get(key);
      if (!nf) { nf = new Intl.NumberFormat(f.locale, { style: f.kind === "percent" ? "percent" : "decimal", minimumFractionDigits: f.decimals, maximumFractionDigits: f.decimals }); numberFormats.set(key, nf); }
      return nf.format(value);
    }
    const text = String(value), m = /^(-?)(\d+)(?:\.(\d*))?$/.exec(text);
    if (!m) return formatValue(value, column.kind);
    const decimals = Math.min(f.decimals, 12), fraction = m[3] ?? "";
    let n = BigInt(m[2] + fraction), scale = fraction.length - (f.kind === "percent" ? 2 : 0);
    if (scale > decimals) { const divisor = 10n ** BigInt(scale - decimals); n = (n + divisor / 2n) / divisor; }
    else n *= 10n ** BigInt(decimals - scale);
    const divisor = 10n ** BigInt(decimals), whole = n / divisor;
    const nf = numberFormat(f.locale), separator = nf.formatToParts(1.1).find(p => p.type === "decimal")?.value ?? ".";
    return `${m[1] && n !== 0n ? "-" : ""}${nf.format(whole)}${decimals ? separator + (n % divisor).toString().padStart(decimals, "0") : ""}${f.kind === "percent" ? "%" : ""}`;
  }
  const text = String(value);
  // DATE has no timezone. Naive TIMESTAMP values are interpreted as UTC explicitly.
  const dateOnly = column.kind === "date";
  const iso = dateOnly ? `${text.slice(0, 10)}T00:00:00Z` : text.replace(" ", "T").replace(/([+-]\d{2})$/, "$1:00");
  const date = new Date(dateOnly || /(?:Z|[+-]\d{2}:?\d{2})$/.test(iso) ? iso : iso + "Z");
  if (!Number.isFinite(date.getTime())) return formatValue(value, column.kind);
  const timezone = dateOnly ? "UTC" : f.timezone, key = `${f.locale}:${timezone}:${f.kind}`;
  let df = dateFormats.get(key);
  if (!df) { df = new Intl.DateTimeFormat(f.locale, { timeZone: timezone, year: "numeric", month: "2-digit", day: "2-digit", ...(f.kind === "datetime" && !dateOnly ? { hour: "2-digit", minute: "2-digit", second: "2-digit", hour12: false } as const : {}) }); dateFormats.set(key, df); }
  return df.format(date);
}
export function cellColor(value: CellValue, column: string, rules: ColorRule[]): string | undefined {
  const colors: Record<string, string> = { red: "#d33b3b33", green: "#209a5833", amber: "#c4841233" };
  for (const rule of rules.slice(0, 16)) {
    if (rule.column !== column) continue;
    const n = Number(value);
    // Avoid making rounded comparisons of integers outside the safe range.
    const numeric = value !== null && value !== "" && Number.isFinite(n) && Math.abs(n) <= Number.MAX_SAFE_INTEGER;
    const match = rule.condition === "empty" ? value === null || value === "" : numeric && (rule.condition === "greater" ? n > rule.value : rule.condition === "less" ? n < rule.value : n === rule.value);
    if (match) return colors[rule.color];
  }
}
export async function presentationDialog(rpc: Rpc, table: string, changed: () => Promise<void>): Promise<() => void> {
  const columns = (await rpc.call("schema", {})).find(t => t.name === table)?.columns ?? [];
  const p = await rpc.call("presentation", { table });
  const field = (label: string, input: HTMLElement) => el("label", { class: "analysis-field" }, label, input);
  const choice = (label: string, values: string[], selected: string) => { const e = el("select", { "aria-label": label }, ...values.map(v => el("option", { value: v }, v))); e.value = selected; return e; };
  const first = Object.values(p.formats)[0];
  const locale = choice("Display locale", ["en-US", "en-GB", "de-DE", "fr-FR", "hi-IN", "ja-JP"], first?.locale ?? "en-US");
  const timezone = choice("Display timezone", ["UTC", "Asia/Kolkata", "America/New_York", "Europe/London", "Asia/Tokyo"], first?.timezone ?? "UTC");
  const decimals = el("input", { type: "number", min: 0, max: 12, value: first?.decimals ?? 2, "aria-label": "Decimal places" });
  const frozen = el("input", { type: "number", min: 0, max: 8, value: p.frozen, "aria-label": "Frozen columns" });
  const rows = columns.map(c => {
    const hidden = el("input", { type: "checkbox", checked: p.hidden.includes(c.name), "aria-label": `Hide ${c.name}` });
    const kinds = ["general", ...(["int", "number"].includes(c.kind) ? ["number", "percent"] : []), ...(["date", "datetime"].includes(c.kind) ? ["date", "datetime"] : [])];
    const format = choice(`Format ${c.name}`, kinds, p.formats[c.name]?.kind ?? "general");
    return { c, hidden, format, row: el("div", { class: "presentation-row" }, el("span", {}, c.name), el("label", {}, "Hide ", hidden), format) };
  });
  const ruleColumn = choice("Rule column", columns.map(c => c.name), columns[0]?.name ?? "");
  const condition = choice("Rule condition", ["greater", "less", "equal", "empty"], "greater");
  const value = el("input", { type: "number", step: "any", value: 0, "aria-label": "Rule threshold" });
  const color = choice("Rule color", ["red", "green", "amber"], "green");
  const rules = [...p.rules], ruleList = el("div"), status = el("p", { role: "status" });
  const renderRules = () => ruleList.replaceChildren(...rules.map((r, i) => el("p", {}, `${r.column} ${r.condition} ${r.value} → ${r.color} `,
    el("button", { class: "btn", onclick: () => { rules.splice(i, 1); renderRules(); }, "aria-label": `Remove rule ${i + 1}` }, "Remove")))); renderRules();
  const save = el("button", { class: "btn btn-primary", onclick: async () => {
    save.disabled = true;
    try {
      const formats: Record<string, DisplayFormat> = {};
      for (const r of rows) if (r.format.value !== "general") formats[r.c.name] = { kind: r.format.value as DisplayKind, decimals: Number(decimals.value), locale: locale.value, timezone: timezone.value };
      await rpc.call("save_presentation", { table, presentation: { hidden: rows.filter(r => r.hidden.checked).map(r => r.c.name), frozen: Number(frozen.value), formats, rules } });
      await changed(); modal.close();
    } catch (e) { status.textContent = String(e); } finally { save.disabled = false; }
  } }, "Save presentation");
  const dialog = el("section", { class: "dialog analysis-dialog" }, el("h2", {}, "Table presentation"),
    el("p", { class: "dialog-lede" }, "Display only: raw values remain unchanged; view exports respect hidden columns. DATE values keep their calendar date; timestamps without an offset are treated as UTC. Frozen columns stay on the left while space permits. Conditional rules use safe-range numeric comparisons, at most 16 rules per visible cell."),
    field("Locale", locale), field("Timezone", timezone), field("Decimal places", decimals), field("Freeze first visible columns (0–8)", frozen), ...rows.map(r => r.row),
    el("h3", {}, "Conditional formatting"), field("Column", ruleColumn), field("Condition", condition), field("Threshold", value), field("Color", color),
    el("button", { class: "btn", onclick: () => { if (rules.length >= 16) { status.textContent = "At most 16 rules."; return; } rules.push({ column: ruleColumn.value, condition: condition.value as Condition, value: Number(value.value), color: color.value }); renderRules(); } }, "Add rule"), ruleList,
    el("div", { class: "dialog-actions" }, save, el("button", { class: "btn", onclick: () => modal.close() }, "Cancel")), status);
  const modal = openModal(dialog, () => modal.close()); locale.focus(); return () => modal.close();
}
