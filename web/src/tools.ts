import { Rpc } from "./api";
import { el, openModal } from "./ui";
import { resultTable } from "./analysis";
import type { Column, Rule, Transform, Validation } from "./protocol";

const field = (label: string, input: HTMLElement) => el("label", { class: "analysis-field" }, label, input);
const select = (label: string, values: string[]) => el("select", { "aria-label": label }, ...values.map(value => el("option", { value }, value)));

/** Data tools (F.12): validation rules (lists become dropdowns), transforms previewed and
 *  applied as new tables, and formula inspection. Nothing here rewrites the open table. */
export function dataToolsDialog(rpc: Rpc, table: string, columns: Column[], openTable: (name: string) => Promise<void>): () => void {
  const status = el("p", { role: "status" });
  const output = el("div");
  const stored = columns.filter(c => !c.formula).map(c => c.name);
  const column = select("Column", stored);
  const ruleKind = select("Rule", ["one_of", "between", "required"]);
  const ruleValues = el("textarea", { rows: 3, "aria-label": "Rule values", placeholder: "one_of: one value per line · between: min and max on two lines" });
  const transform = select("Transform", ["dedupe", "trim", "split", "convert"]);
  const arg = el("input", { "aria-label": "Transform argument", placeholder: "dedupe: key columns, comma-separated · split: delimiter · convert: BIGINT, DOUBLE, DATE, BOOLEAN or VARCHAR" });
  const parts = el("input", { type: "number", min: 1, max: 16, value: 2, "aria-label": "Split parts" });
  const name = el("input", { value: `${table}_clean`, "aria-label": "New table name" });
  const formula = select("Formula column", columns.filter(c => c.formula).map(c => c.name));
  const rule = (): Rule => {
    const lines = ruleValues.value.split("\n").map(s => s.trim()).filter(Boolean);
    if (ruleKind.value === "one_of") return { kind: "one_of", values: lines };
    if (ruleKind.value === "between") return { kind: "between", ...(lines[0] ? { min: Number(lines[0]) } : {}), ...(lines[1] ? { max: Number(lines[1]) } : {}) };
    return { kind: "required" };
  };
  const spec = (): Transform => {
    switch (transform.value) {
      case "dedupe": return { kind: "dedupe", keys: (arg.value || column.value).split(",").map(s => s.trim()).filter(Boolean) };
      case "split": return { kind: "split", column: column.value, delimiter: arg.value || ",", parts: Number(parts.value) || 2 };
      case "convert": return { kind: "convert", column: column.value, to: arg.value || "DOUBLE" };
      default: return { kind: "trim", column: column.value, inner: true };
    }
  };
  const act = (label: string, f: () => Promise<void>) => el("button", { class: "btn", onclick: () => void f().catch(e => status.textContent = String(e)) }, label);
  const rulesList = el("ul", { "aria-label": "Rules" });
  let rules: Validation[] = [];
  const showRules = (counts?: bigint[] | number[]) => rulesList.replaceChildren(...rules.map((r, i) => el("li", {}, `${r.column}: ${r.rule.kind === "one_of" ? `one of ${r.rule.values.join(", ")}` : r.rule.kind === "between" ? `between ${r.rule.min ?? "−∞"} and ${r.rule.max ?? "∞"}` : "required"}${counts ? ` · ${counts[i]} rows break it now` : ""}`)));
  void rpc.call("validations", { table }).then(r => { rules = r; showRules(); }).catch(() => {});
  const panel = el("section", { class: "dialog analysis-dialog" }, el("h2", {}, `Data tools · ${table}`),
    el("p", { class: "dialog-lede" }, "Check values against rules, and preview transforms before they make a new table. The table itself is never changed here."),
    el("h3", {}, "Validation"), field("Column", column), field("Rule", ruleKind), field("Rule values", ruleValues), rulesList,
    el("div", { class: "dialog-actions" },
      act("Add rule", async () => { const report = await rpc.call("set_validations", { table, rules: [...rules.filter(r => r.column !== column.value || r.rule.kind !== ruleKind.value), { column: column.value, rule: rule() }] }); rules = report.rules; showRules(report.violations); status.textContent = "Rule saved; edits and SQL changes must keep it."; }),
      act("Remove rules of this column", async () => { const report = await rpc.call("set_validations", { table, rules: rules.filter(r => r.column !== column.value) }); rules = report.rules; showRules(report.violations); status.textContent = "Rules removed."; })),
    el("h3", {}, "Clean up into a new table"), field("Transform", transform), field("Argument", arg), field("Split parts", parts), field("New table name", name),
    el("div", { class: "dialog-actions" },
      act("Preview", async () => { const p = await rpc.call("preview_transform", { table, transform: spec() }); status.textContent = `${p.rows} rows · ${p.affected} ${transform.value === "dedupe" ? "duplicates left out" : transform.value === "convert" ? "values that do not convert (blank)" : "values changed"}`; output.replaceChildren(resultTable({ columns: p.sample.columns, rows: p.sample.rows, truncated: p.sample.truncated, revision: 0, sources: [] } as never)); }),
      act("Create table", async () => { const info = await rpc.call("apply_transform", { table, transform: spec(), name: name.value }); modal.close(); await openTable(info.name); })),
    el("h3", {}, "Inspect a formula"), field("Formula column", formula),
    el("div", { class: "dialog-actions" },
      act("Inspect", async () => { const i = await rpc.call("inspect_formula", { table, column: formula.value }); output.replaceChildren(el("dl", { class: "catalog-arguments" },
        ...([["Formula", i.formula], ["Reads columns", i.reads_columns.join(", ") || "—"], ["Reads tables", i.reads_tables.join(", ") || "—"], ["Reads sheet cells", i.reads_cells.join(", ") || "—"],
          ["Read by", i.read_by.join(", ") || "—"], ["Runs as", i.plan.join(", ")], ["Errors", i.errors.map(([c, n]) => `${c} × ${n}`).join(", ") || "none"]] as [string, string][])
          .flatMap(([k, v]) => [el("dt", {}, k), el("dd", {}, v)]))); }),
      el("button", { class: "btn", onclick: () => modal.close() }, "Close")),
    status, output);
  const modal = openModal(panel, () => modal.close());
  return () => modal.close();
}
