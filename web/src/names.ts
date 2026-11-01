import type { NamedRange, SheetInfo } from "./protocol";
import type { Rpc } from "./api";
import { el, openModal, toast, confirmDialog } from "./ui";
import { columnLetter } from "./format";

export interface Address { sheet: string | null; r0: number; c0: number; r1: number; c1: number }

const columnIndex = (letters: string) => [...letters.toUpperCase()].reduce((n, ch) => n * 26 + ch.charCodeAt(0) - 64, 0) - 1;

/** A sheet name as written in a formula: quoted unless it is a plain identifier that cannot be read as a cell. */
export function quoteSheet(name: string): string {
  return /^[A-Za-z_][A-Za-z0-9_.]*$/.test(name) && !/^[A-Za-z]{1,3}[0-9]+$/.test(name) ? name : `'${name.replace(/'/g, "''")}'`;
}

/** `A1`, `$B$2:C9`, `Sheet1!A1`, `'Q1 sales'!A1:B4`; null for anything else (including names). */
export function parseAddress(text: string): Address | null {
  const m = /^\s*(?:(?:'((?:[^']|'')+)'|([A-Za-z0-9_.]+))!)?\$?([A-Za-z]{1,3})\$?([0-9]+)(?::\$?([A-Za-z]{1,3})\$?([0-9]+))?\s*$/.exec(text);
  if (!m) return null;
  const sheet = m[1] !== undefined ? m[1].replace(/''/g, "'") : m[2] ?? null;
  const c0 = columnIndex(m[3]), r0 = Number(m[4]) - 1;
  const c1 = m[5] ? columnIndex(m[5]) : c0, r1 = m[6] ? Number(m[6]) - 1 : r0;
  if (r0 < 0 || r1 < 0 || Math.max(c0, c1) >= 16384 || Math.max(r0, r1) >= 1048576) return null;
  return { sheet, r0: Math.min(r0, r1), c0: Math.min(c0, c1), r1: Math.max(r0, r1), c1: Math.max(c0, c1) };
}

/** The absolute reference to a selection, as a name's "refers to". */
export function absoluteReference(sheet: string, r: { r0: number; c0: number; r1: number; c1: number }): string {
  const a = `$${columnLetter(r.c0)}$${r.r0 + 1}`;
  const b = `$${columnLetter(r.c1)}$${r.r1 + 1}`;
  return `=${quoteSheet(sheet)}!${a}${a === b ? "" : `:${b}`}`;
}

/** Resolve Go to input: an address, or a name visible from `current` (sheet names shadow workbook names). */
export function resolveTarget(text: string, names: NamedRange[], current: number): Address | null {
  const direct = parseAddress(text);
  if (direct) return direct;
  const wanted = text.trim().toLowerCase();
  const hit = names.find((n) => n.name.toLowerCase() === wanted && n.scope === current) ?? names.find((n) => n.name.toLowerCase() === wanted && n.scope === null);
  return hit ? parseAddress(hit.refers_to.replace(/^=/, "")) : null;
}

export interface NamesContext {
  rpc: Rpc;
  sheets: () => SheetInfo[];
  current: () => { index: number; name: string; selection: { r0: number; c0: number; r1: number; c1: number } } | null;
  goTo: (target: Address) => Promise<void>;
}

/** Go to (Ctrl/Cmd+G): a cell, range or name, with the workbook's names listed. */
export async function goToDialog(ctx: NamesContext): Promise<void> {
  const here = ctx.current();
  if (!here) { toast("Open a sheet to go to a cell, range or name"); return; }
  const names = await ctx.rpc.call("sheet_names");
  const input = el("input", { type: "text", "aria-label": "Cell, range or name", list: "goto-names", placeholder: "A1, Sheet1!B2:C9 or a name", autocomplete: "off" });
  const list = el("datalist", { id: "goto-names" }, ...names.map((n) => el("option", { value: n.name })));
  const problem = el("p", { class: "form-error", role: "alert" });
  const go = async () => {
    const target = resolveTarget(input.value, names, here.index);
    if (!target) { problem.textContent = `“${input.value}” is not a cell, range or name in this workbook.`; input.focus(); return; }
    modal.close();
    await ctx.goTo(target);
  };
  input.addEventListener("keydown", (e) => { if (e.key === "Enter") { e.preventDefault(); void go(); } });
  const panel = el("section", { class: "dialog", "aria-label": "Go to" }, el("h2", {}, "Go to"),
    el("label", {}, "Go to", input), list, problem,
    names.length ? el("p", { class: "muted" }, `Names: ${names.map((n) => n.name).join(", ")}`) : el("p", { class: "muted" }, "No named ranges yet."),
    el("div", { class: "dialog-actions" }, el("button", { class: "btn", onclick: () => modal.close() }, "Cancel"), el("button", { class: "btn btn-primary", onclick: () => void go() }, "Go")));
  const modal = openModal(panel, () => modal.close());
  input.focus();
}

/** Named ranges: list, define from the selection, rename/repoint and delete. */
export async function namesDialog(ctx: NamesContext): Promise<() => void> {
  let names = await ctx.rpc.call("sheet_names");
  const here = ctx.current();
  const scopeLabel = (s: number | null) => s === null ? "Workbook" : ctx.sheets().find((x) => x.index === s)?.name ?? `Sheet ${s + 1}`;
  const name = el("input", { type: "text", "aria-label": "Name", autocomplete: "off", spellcheck: false });
  const refersTo = el("input", { type: "text", "aria-label": "Refers to", spellcheck: false });
  const scope = el("select", { "aria-label": "Scope" }, el("option", { value: "" }, "Workbook"),
    ...ctx.sheets().map((s) => el("option", { value: String(s.index) }, `Sheet: ${s.name}`)));
  const problem = el("p", { class: "form-error", role: "alert" });
  const status = el("p", { role: "status", class: "muted" });
  const table = el("table", { class: "names-table" });
  let editing: NamedRange | null = null;
  const submit = el("button", { class: "btn btn-primary" }, "Define name");
  const resetForm = () => {
    editing = null;
    name.value = "";
    scope.value = "";
    refersTo.value = here ? absoluteReference(here.name, here.selection) : "";
    submit.textContent = "Define name";
  };
  const render = () => {
    table.replaceChildren(el("caption", {}, names.length ? `${names.length} named range${names.length === 1 ? "" : "s"}` : "No named ranges"),
      el("thead", {}, el("tr", {}, ...["Name", "Scope", "Refers to", "Actions"].map((h) => el("th", { scope: "col" }, h)))),
      el("tbody", {}, ...names.map((n) => el("tr", {},
        el("th", { scope: "row" }, n.name), el("td", {}, scopeLabel(n.scope)), el("td", {}, el("code", {}, n.refers_to)),
        el("td", {},
          el("button", { class: "btn", "aria-label": `Go to ${n.name}`, onclick: async () => {
            const target = parseAddress(n.refers_to.replace(/^=/, ""));
            if (!target) { toast(`${n.name} does not refer to a valid range (${n.refers_to})`, "error"); return; }
            modal.close(); await ctx.goTo(target);
          } }, "Go to"),
          el("button", { class: "btn", "aria-label": `Edit ${n.name}`, onclick: () => {
            editing = n; name.value = n.name; scope.value = n.scope === null ? "" : String(n.scope); refersTo.value = n.refers_to;
            submit.textContent = "Save changes"; name.focus();
          } }, "Edit"),
          el("button", { class: "btn btn-danger", "aria-label": `Delete ${n.name}`, onclick: async () => {
            if (!await confirmDialog(`Delete ${n.name}?`, "Formulas that use this name will show #NAME?. Undo restores it.")) return;
            try { names = await ctx.rpc.call("sheet_delete_name", { name: n.name, ...(n.scope === null ? {} : { scope: n.scope }) }); status.textContent = `Deleted ${n.name}`; render(); }
            catch (e) { problem.textContent = (e as Error).message; }
          } }, "Delete"))))));
  };
  submit.addEventListener("click", async () => {
    problem.textContent = "";
    const s = scope.value === "" ? {} : { scope: Number(scope.value) };
    try {
      if (editing) {
        names = await ctx.rpc.call("sheet_update_name", { name: editing.name, ...(editing.scope === null ? {} : { scope: editing.scope }),
          new_name: name.value, ...(scope.value === "" ? {} : { new_scope: Number(scope.value) }), refers_to: refersTo.value });
        status.textContent = `Saved ${name.value}`;
      } else {
        names = await ctx.rpc.call("sheet_define_name", { name: name.value, ...s, refers_to: refersTo.value });
        status.textContent = `Defined ${name.value}`;
      }
      resetForm(); render(); name.focus();
    } catch (e) {
      problem.textContent = (e as Error).message;
      name.focus();
    }
  });
  const panel = el("section", { class: "dialog names-dialog", "aria-label": "Named ranges" }, el("h2", {}, "Named ranges"),
    el("p", { class: "dialog-lede" }, "Names refer to cells or ranges and can be used in sheet formulas, e.g. =SUM(Sales). Workbook functions are managed in Functions."),
    table,
    el("h3", {}, "Define or edit"),
    el("label", {}, "Name", name), el("label", {}, "Scope", scope), el("label", {}, "Refers to", refersTo),
    problem, status,
    el("div", { class: "dialog-actions" }, el("button", { class: "btn", onclick: () => resetForm() }, "New"), el("button", { class: "btn", onclick: () => modal.close() }, "Close"), submit));
  const modal = openModal(panel, () => modal.close());
  resetForm(); render(); name.focus();
  return () => modal.close();
}
