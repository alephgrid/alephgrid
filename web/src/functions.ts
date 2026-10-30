import { Rpc } from "./api";
import { builtinFunctions } from "./catalog";
import { el, openModal } from "./ui";
import type { FunctionDefinition, FunctionKind, FunctionTest, WorkbookFunction } from "./protocol";

/** Workbook-scoped definitions. Publishing creates an immutable version; pinning is explicit. */
export function functionLibrary(rpc: Rpc, allowSql: boolean): () => void {
  const list = el("div", { class: "function-list" });
  const name = el("input", { "aria-label": "Function name", value: "MARGIN", maxlength: 64 });
  const kind = el("select", { "aria-label": "Function engine" }, el("option", { value: "formula" }, "Sheet formula (IronCalc)"),
    allowSql ? el("option", { value: "sql" }, "SQL expression (DuckDB)") : null);
  const args = el("input", { "aria-label": "Arguments", value: "revenue, cost" });
  const source = el("textarea", { "aria-label": "Function source", rows: 4, spellcheck: false }, "IF(revenue=0,0,(revenue-cost)/revenue)");
  const documentation = el("textarea", { "aria-label": "Function documentation", rows: 2 }, "Profit as a fraction of revenue.");
  const tests = el("textarea", { "aria-label": "Function tests", rows: 4, spellcheck: false }, JSON.stringify([{ arguments: ["100", "60"], expected: "0.4" }], null, 2));
  const dependencies = el("textarea", { "aria-label": "Dependency versions", rows: 2 }, "{}");
  const pins = el("textarea", { "aria-label": "Active function versions", rows: 2 }, "{}");
  const status = el("pre", { role: "status", class: "function-status" });
  const versions = el("select", { "aria-label": "Function version" });
  let selected: WorkbookFunction | undefined;
  const field = (label: string, input: HTMLElement) => el("label", { class: "function-field" }, label, input);
  const definition = (): FunctionDefinition => {
    const parsed: unknown = JSON.parse(tests.value);
    if (!Array.isArray(parsed)) throw new Error("Tests must be a JSON array");
    return { name: name.value.trim(), kind: kind.value as FunctionKind, arguments: args.value.split(",").map(x => x.trim()), source: source.value, documentation: documentation.value, tests: parsed as FunctionTest[], dependencies: JSON.parse(dependencies.value) };
  };
  const load = (f: WorkbookFunction, version = f.pinned) => {
    selected = f;
    const v = f.versions.find(v => v.version === version)!;
    name.value = v.definition.name; kind.value = v.definition.kind; args.value = v.definition.arguments.join(", ");
    source.value = v.definition.source; documentation.value = v.definition.documentation; tests.value = JSON.stringify(v.definition.tests, null, 2); dependencies.value = JSON.stringify(v.dependencies);
    versions.replaceChildren(...f.versions.map(v => el("option", { value: v.version }, `v${v.version}${v.version === f.pinned ? " · active" : ""}`)));
    versions.value = String(version); status.textContent = `${f.name} · workbook scope · pinned v${f.pinned}\n${v.adapter}\nDependencies: ${JSON.stringify(v.dependencies)}`;
  };
  async function refresh() {
    const items = await rpc.call("functions", {});
    pins.value = JSON.stringify(Object.fromEntries(items.map(f => [f.name, f.pinned])), null, 2);
    list.replaceChildren(...items.map(f => el("button", { class: "btn", onclick: () => load(f) }, `${f.name} · v${f.pinned}`)));
    if (!items.length) list.append(el("p", {}, "No published functions yet."));
    if (selected) { const found = items.find(f => f.name === selected!.name); if (found) load(found); }
  }
  versions.onchange = () => selected && load(selected, Number(versions.value));
  const controls: HTMLButtonElement[] = [];
  const action = (label: string, run: () => Promise<void>) => {
    const button = el("button", { class: "btn", onclick: async () => {
      controls.forEach(b => b.disabled = true);
      // Refresh replaces the options after a mutation. Do not accept a new selection
      // that would be silently overwritten before Activate becomes available again.
      versions.disabled = true;
      try { await run(); } catch (e) { status.textContent = String(e); }
      finally { controls.forEach(b => b.disabled = false); versions.disabled = false; }
    } }, label); controls.push(button); return button;
  };
  const test = action("Run tests", async () => {
    const results = await rpc.call("test_function", { definition: definition() });
    status.textContent = results.length ? results.map((t, i) => `${i + 1}: ${t.passed ? "Passed" : "FAILED"} · actual ${t.actual} · expected ${t.expected}`).join("\n") : "Definition validated; no test cases supplied.";
  });
  const publish = action("Publish version", async () => {
    selected = await rpc.call("publish_function", { definition: definition() }); await refresh();
    status.textContent += "\nPublished. Existing functions keep their active version until you select a version and activate it.";
  });
  const activate = action("Activate selected version", async () => {
    if (!selected) throw new Error("Select a published function first");
    await rpc.call("pin_functions", { pins: { [selected.name]: Number(versions.value) } }); await refresh();
  });
  const group = action("Activate version map", async () => {
    await rpc.call("pin_functions", { pins: JSON.parse(pins.value) }); await refresh();
    status.textContent += "\nVersion map activated atomically.";
  });
  const dialog = el("section", { class: "dialog function-dialog" }, el("h2", {}, "Function library"),
    el("p", {}, "Workbook-scoped functions. Formula functions run in sheets; SQL functions run in queries and table columns. Publishing never updates another workbook."),
    list, field("Name", name), field("Execution engine", kind), field("Arguments (comma separated)", args), field("Expression", source),
    field("Documentation and example usage", documentation), field("Dependency versions (optional JSON overrides)", dependencies), field("Tests: arguments and expected display values (JSON)", tests),
    el("p", {}, "SQL test inputs are strings; use explicit casts in SQL expressions. NULL binds SQL NULL. Sheet numbers use spreadsheet precision. XLSX compatibility depends on Excel LAMBDA support; SQL functions are not Excel formulas."),
    field("Active versions (edit together for dependent upgrades)", pins), group,
    el("div", { class: "dialog-actions" }, test, publish, versions, activate, el("button", { class: "btn", onclick: () => modal.close() }, "Close")), status);
  const modal = openModal(dialog, () => modal.close());
  void refresh().catch(e => status.textContent = String(e)); name.focus();
  return () => modal.close();
}

/** Suggest active workbook functions without changing the expression evaluator. */
export function functionCompletion(input: HTMLInputElement, rpc: Rpc, engine: () => FunctionKind): () => void {
  let functions: WorkbookFunction[] = [];
  let disposed = false;
  let matches: { name: string; help: string }[] = [];
  const help = el("div", { class: "function-completion", role: "region", "aria-label": "Function suggestions", hidden: true });

  const signature = (f: WorkbookFunction) => {
    const d = f.versions.find(v => v.version === f.pinned)!.definition;
    return `${f.name}(${d.arguments.join(", ")}) — ${d.documentation}`;
  };
  const update = () => {
    const text = input.value.slice(0, input.selectionStart ?? input.value.length);
    // Do not offer functions while entering ordinary text or quoted formula literals.
    let quote = "";
    for (let i = 0; i < text.length; i++) {
      const c = text[i];
      if (quote && c === quote && text[i + 1] === quote) { i++; continue; }
      if (quote && c === quote) quote = "";
      else if (!quote && (c === '"' || c === "'")) quote = c;
    }
    if ((engine() === "formula" && !text.startsWith("=")) || quote) { matches = []; help.hidden = true; return; }
    const prefix = text.match(/([A-Za-z_][A-Za-z_0-9.]*)$/)?.[1]?.toUpperCase();
    const active = functions.filter(f => f.versions.find(v => v.version === f.pinned)!.definition.kind === engine())
      .map(f => ({ name: f.name, help: signature(f) }));
    if (engine() === "formula") {
      const custom = new Set(active.map(f => f.name));
      active.push(...builtinFunctions.filter(f => !custom.has(f.name)).map(f => ({ name: f.name, help: `${f.signature} — ${f.summary}` })));
    }
    matches = prefix ? active.filter(f => f.name.startsWith(prefix)).slice(0, 8) : [];
    const call = text.match(/([A-Za-z_][A-Za-z_0-9]*)\([^()]*$/)?.[1]?.toUpperCase();
    const current = active.find(f => f.name === call);
    const insert = (f: { name: string }) => {
      const end = input.selectionStart ?? input.value.length;
      input.setRangeText(`${f.name}(`, end - (prefix?.length ?? 0), end, "end");
      update(); input.focus();
    };
    help.replaceChildren(...matches.map(f => el("button", { type: "button", class: "btn", onmousedown: (e: MouseEvent) => e.preventDefault(), onclick: () => insert(f) }, f.help)));
    if (current) help.append(el("p", {}, current.help));
    help.hidden = !matches.length && !current;
    // Argument help alone is not interactive: let clicks reach the grid for reference picking.
    help.classList.toggle("help-only", !matches.length);
  };
  const focus = async () => {
    if (!help.isConnected) input.parentElement!.append(help);
    try { functions = await rpc.call("functions", {}); if (!disposed && document.activeElement === input) update(); }
    catch { /* The workbook already displays connection errors. */ }
  };
  const key = (e: KeyboardEvent) => {
    if (e.key === "Tab" && matches.length && !e.shiftKey && !help.hidden) {
      e.preventDefault(); e.stopImmediatePropagation(); help.querySelector<HTMLButtonElement>("button")!.click();
    } else if (e.key === "Escape" && !help.hidden) {
      e.preventDefault(); e.stopImmediatePropagation(); help.hidden = true;
    }
  };
  const blur = () => { help.hidden = true; };
  input.addEventListener("focus", focus); input.addEventListener("input", update);
  input.addEventListener("click", update); input.addEventListener("keydown", key, true); input.addEventListener("blur", blur);
  return () => {
    disposed = true; help.remove(); input.removeEventListener("focus", focus);
    input.removeEventListener("input", update); input.removeEventListener("click", update);
    input.removeEventListener("keydown", key, true); input.removeEventListener("blur", blur);
  };
}
