import data from "../../crates/rowfinity-engine/data/function_catalog.json";
import { el, openModal } from "./ui";

export const functionCatalog = data;
export const builtinFunctions = data.functions.filter(f => f.sheet_status === "registered" && f.signature !== null);

const plural = (n: number, noun: string) => `${n} ${noun}${n === 1 ? "" : "s"}`;

/** Fixture cells as entered: a leading apostrophe stores text, `=` a formula, nothing an empty cell. */
function fixtureInput(row: number, column: number): string {
  const v = data.fixture.find(x => x.row === row && x.column === column)?.value;
  return v === undefined ? "(empty)" : v;
}

/** Read-only, bounded catalog UI. Recognition never implies certified compatibility. */
export function formulaHelp(): () => void {
  const search = el("input", { type: "search", "aria-label": "Search built-in formulas", placeholder: "Search name or category" });
  const support = el("select", { "aria-label": "Formula availability" },
    el("option", { value: "all" }, "All catalog entries"),
    el("option", { value: "registered" }, "Registered in IronCalc"),
    el("option", { value: "not_registered" }, "Not registered"));
  const list = el("div", { class: "catalog-list", "aria-label": "Matching formulas", role: "region" });
  const details = el("section", { class: "catalog-details", "aria-label": "Formula details" });
  const count = el("p", { role: "status" });
  let offset = 0;
  const size = 40;
  const show = (f: typeof data.functions[number]) => {
    details.replaceChildren(el("h3", {}, f.name), el("p", {}, f.summary),
      el("p", {}, f.sheet_status === "registered" ? "Sheet engine: registered. Full compatibility is not certified." : "Sheet engine: not registered under this name."),
      el("p", {}, "DuckDB table formulas: not supported through this spreadsheet syntax."),
      el("pre", {}, f.signature ?? "Signature not yet reviewed."),
      el("p", {}, f.argument_modes),
      ...f.limitations.map(s => el("p", {}, s)));
    const args = ("arguments" in f ? f.arguments : undefined) ?? [];
    if (args.length) details.append(el("h4", {}, "Arguments"), el("dl", { class: "catalog-arguments" },
      ...args.flatMap(a => [el("dt", {}, el("code", {}, a.name)), el("dd", {}, a.rule, " ", el("span", { class: "muted" }, `Cases: ${a.cases.join(", ")}`))])));
    if (f.examples.length) {
      const fixture = el("table", {}, el("caption", {}, "Example inputs in A1:C4"),
        el("thead", {}, el("tr", {}, ...["Row", "A", "B", "C"].map(s => el("th", { scope: "col" }, s)))));
      const body = el("tbody");
      for (let r = 0; r < 4; r++) body.append(el("tr", {}, el("th", { scope: "row" }, String(r + 1)),
        ...[0, 1, 2].map(c => el("td", {}, data.fixture.find(x => x.row === r && x.column === c)?.value ?? ""))));
      fixture.append(body);
      const typed = el("table", {}, el("caption", {}, "Coercion inputs in F1:G8"),
        el("thead", {}, el("tr", {}, ...["Row", "F", "G"].map(s => el("th", { scope: "col" }, s)))));
      const typedBody = el("tbody");
      for (let r = 0; r < 8; r++) typedBody.append(el("tr", {}, el("th", { scope: "row" }, String(r + 1)),
        ...[5, 6].map(c => el("td", {}, fixtureInput(r, c)))));
      typed.append(typedBody);
      details.append(fixture, typed, ...f.examples.map(e => el("p", {}, el("code", {}, e.formula), " → ", el("strong", {}, e.expected))),
        el("p", {}, "Evidence: tested example, including workbook reopening. This is a baseline, not verification of every argument mode."));
    } else details.append(el("p", {}, "No Rowfinity calculation example is certified by this catalog entry."));
    if (f.verification_cases.length) {
      const verified = f.verification_cases.filter(c => c.status === "verified").length;
      const evidence = el("details", {}, el("summary", {}, `Mode evidence: ${plural(verified, "verified case")}, ${plural(f.verification_cases.length - verified, "known gap")}`),
        el("p", {}, "Uses the example and coercion inputs above. Expected results come from the cited rule, not from this engine; known gaps are not passing evidence. Unlisted inputs and modes remain uncertified."));
      for (const c of f.verification_cases) evidence.append(el("p", {},
        el("strong", {}, `${c.status === "verified" ? "Verified case" : "Known gap"}: ${c.mode}`),
        ` (${c.id}) · `, el("code", {}, c.formula), " → expected ", el("code", {}, JSON.stringify(c.expected)),
        c.note ? ` · ${c.note}` : "", el("br"), el("span", { class: "muted" }, `Source: ${c.oracle}`)));
      details.append(evidence);
    }
    details.append(el("details", {}, el("summary", {}, "Implementation plan"), el("p", {}, f.implementation), el("p", {}, f.upstream)));
  };
  const previous = el("button", { class: "btn", onclick: () => { offset = Math.max(0, offset - size); render(); } }, "Previous formulas");
  const next = el("button", { class: "btn", onclick: () => { offset += size; render(); } }, "Next formulas");
  const render = () => {
    const q = search.value.trim().toLowerCase();
    const matches = data.functions.filter(f => (support.value === "all" || f.sheet_status === support.value)
      && `${f.name} ${f.category} ${f.summary}`.toLowerCase().includes(q));
    const visible = matches.slice(offset, offset + size);
    count.textContent = `${matches.length} matching formulas${visible.length ? ` · showing ${offset + 1}–${offset + visible.length}` : ""}`;
    list.replaceChildren(...visible.map(f => el("button", { class: "btn", onclick: () => show(f) }, f.name)));
    previous.disabled = offset === 0; next.disabled = offset + size >= matches.length;
    if (visible.length) show(visible[0]); else details.replaceChildren(el("p", {}, "No matching formulas."));
  };
  const reset = () => { offset = 0; render(); };
  search.addEventListener("input", reset); support.addEventListener("change", reset);
  const panel = el("section", { class: "dialog catalog-dialog" }, el("h2", {}, "Formula help"),
    el("p", {}, `Catalog ${data.catalog_version} · ${data.engine}. Sheet formulas and SQL table expressions are different languages.`),
    el("label", {}, "Search formulas", search), el("label", {}, "Availability", support), count,
    el("div", { class: "catalog-columns" }, list, details),
    el("div", { class: "dialog-actions" }, previous, next, el("button", { class: "btn", onclick: () => modal.close() }, "Close")));
  const modal = openModal(panel, () => modal.close());
  render(); search.focus();
  return () => modal.close();
}
