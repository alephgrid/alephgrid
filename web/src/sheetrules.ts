// S.10: a sheet's colour rules (conditional fills and colour scales) and allowed-value
// rules. The matching is a pure function of the rule and the cell's displayed text.

import type { Rpc } from "./api";
import type { CellArea, CellStyle, SheetColorRule, SheetRules } from "./protocol";
import { rangeText } from "./sheetcharts";
import { el, openModal, toast } from "./ui";

/** Whether a colour rule's condition holds for a cell showing `t`. */
export function ruleMatches(x: SheetColorRule, t: string): boolean {
  const n = Number(t.replace(/,/g, "")), v = Number(x.value);
  switch (x.condition) {
    case "greater": return t !== "" && n > v;
    case "less": return t !== "" && n < v;
    case "equal": return t === x.value;
    case "contains": return t.toLowerCase().includes(x.value.toLowerCase());
    case "empty": return t === "";
    case "not_empty": return t !== "";
    case "scale": return t !== "" && !Number.isNaN(Number(t.replace(/,/g, "")));
    default: return false;
  }
}

/** Cell (r, c)'s own style with the fill of the first matching colour rule; its own fill wins.
 *  `text` is the cell's displayed text, read only when a rule covers the cell. */
export function ruleFill(rules: SheetRules, own: CellStyle | undefined, text: () => string, r: number, c: number): CellStyle | undefined {
  const rule = rules.colors.find((x) => r >= x.area.row && r < x.area.row + x.area.rows && c >= x.area.column && c < x.area.column + x.area.columns && ruleMatches(x, text()));
  if (rule?.condition === "scale" && !own?.fill) {
    // Colour scale: the rule's colour, stronger the closer the value is to the top of "low,high".
    const [lo, hi] = rule.value.split(",").map(Number);
    const n = Number(text().replace(/,/g, ""));
    const t = Math.max(0, Math.min(1, (n - lo) / ((hi - lo) || 1)));
    const alpha = Math.round(40 + t * 215).toString(16).padStart(2, "0");
    return { ...(own ?? {}), fill: `${rule.color}${alpha}` };
  }
  return rule && !own?.fill ? { ...(own ?? {}), fill: rule.color } : own;
}

export interface RulesDialogHost {
  /** Show and focus these cells (the dialog has closed). */
  select(area: CellArea): void;
  /** The sheet's rules were saved as `rules`. */
  changed(rules: SheetRules): void;
}

/** Every colour and allowed-value rule of sheet `sheet` (named `name`), to review, visit or delete. */
export async function rulesDialog(rpc: Rpc, sheet: number, name: string, host: RulesDialogHost): Promise<void> {
  let rules = await rpc.call("sheet_rules", { sheet });
  const CONDITION: Record<string, string> = { greater: "greater than", less: "less than", equal: "equal to", contains: "contains", empty: "is empty", not_empty: "is not empty", scale: "shade by value" };
  const list = el("ul", { class: "rules-list", "aria-label": "Rules" });
  const done = el("button", { class: "btn btn-primary", type: "button" }, "Done");
  const dialog = el("div", { class: "dialog rules-dialog" }, el("h2", {}, `Rules on ${name}`),
    el("p", { class: "dialog-hint" }, "Colour rules fill matching cells; allowed-value rules refuse other values. Neither is written to XLSX files."), list,
    el("div", { class: "dialog-actions" }, done));
  const save = async (next: typeof rules) => {
    try { await rpc.call("set_sheet_rules", { sheet, rules: next }); rules = next; host.changed(next); render(); }
    catch (e) { toast((e as Error).message, "error"); }
  };
  const goTo = (a: CellArea) => {
    modal.close();
    host.select(a);
  };
  const item = (label: string, swatch: string | null, area: CellArea, remove: () => void) =>
    el("li", { class: "rules-item" },
      swatch ? el("span", { class: "rules-swatch", style: `background:${swatch}`, "aria-hidden": "true" }) : el("span", { class: "rules-swatch rules-swatch-none", "aria-hidden": "true" }, "✓"),
      el("span", { class: "rules-text" }, el("strong", {}, rangeText(area)), ` ${label}`),
      el("button", { class: "btn btn-ghost btn-small", type: "button", "aria-label": `Go to ${rangeText(area)}`, onclick: () => goTo(area) }, "Go to"),
      el("button", { class: "btn btn-ghost btn-small", type: "button", "aria-label": `Delete rule on ${rangeText(area)}`, onclick: remove }, "Delete"));
  const render = () => {
    const items = [
      ...rules.colors.map((r, i) => item(r.condition === "scale" ? `shade by value from ${r.value || "low"}` : `${CONDITION[r.condition] ?? r.condition}${r.value ? ` ${r.value}` : ""} → fill ${r.color}`, r.color, r.area,
        () => void save({ ...rules, colors: rules.colors.filter((_, j) => j !== i) }))),
      ...rules.validations.map((v, i) => item(v.list?.length ? `allows ${v.list.join(", ")}` : `allows numbers${v.min !== undefined ? ` from ${v.min}` : ""}${v.max !== undefined ? ` to ${v.max}` : ""}`, null, v.area,
        () => void save({ ...rules, validations: rules.validations.filter((_, j) => j !== i) }))),
    ];
    list.replaceChildren(...(items.length ? items : [el("li", { class: "rules-empty" }, "No rules yet: select cells, then use Conditional colour… or Allowed values… in the cell menu.")]));
  };
  render();
  const modal = openModal(dialog, () => modal.close());
  done.addEventListener("click", () => modal.close());
  done.focus();
}
