import { Rpc } from "./api";
import { builtinFunctions } from "./catalog";
import { el, openModal } from "./ui";
import type { FunctionDefinition, FunctionKind, FunctionTest, WorkbookFunction } from "./protocol";

/** Fields compared to tell whether the editor holds a published version (order-independent). */
function normalize(d: FunctionDefinition): string {
  return JSON.stringify({
    name: d.name, kind: d.kind, arguments: d.arguments, source: d.source, documentation: d.documentation,
    tests: d.tests, returns: d.kind === "python" ? d.returns ?? "number" : null, volatile: d.kind === "python" ? !!d.volatile : null,
  });
}

const KIND_LABEL: Record<string, string> = { formula: "Formula", sql: "SQL", python: "Python" };

/** Workbook-scoped definitions (U.06). Publishing creates an immutable version; pinning is
 *  explicit. The editor says whether it holds a draft, a tested draft or a published version. */
export function functionLibrary(rpc: Rpc, allowSql: boolean): () => void {
  const list = el("ul", { class: "fn-list", "aria-label": "Published functions" });
  const name = el("input", { "aria-label": "Function name", value: "MARGIN", maxlength: 64, spellcheck: false });
  const kind = el("select", { "aria-label": "Function engine" }, el("option", { value: "formula" }, "Sheet formula (IronCalc)"),
    allowSql ? el("option", { value: "sql" }, "SQL expression (DuckDB)") : null,
    el("option", { value: "python" }, "Python (isolated worker)"));
  const returns = el("select", { "aria-label": "Python result type" },
    el("option", { value: "number" }, "Number"), el("option", { value: "text" }, "Text"), el("option", { value: "boolean" }, "Logical"));
  const volatile = el("input", { type: "checkbox", "aria-label": "Volatile: computed again only on Refresh" });
  const pythonFields = el("div", { class: "fn-row fn-python", hidden: true },
    el("label", { class: "function-field" }, "Returns", returns),
    el("label", { class: "fn-check" }, volatile, el("span", {}, "Volatile: reads the clock or randomness; computed again only on Refresh")));
  const sourceLabel = el("span", {}, "Expression");
  const sourceHint = el("small", { class: "muted" });
  const args = el("input", { "aria-label": "Arguments", value: "revenue, cost", spellcheck: false });
  const source = el("textarea", { "aria-label": "Function source", rows: 4, spellcheck: false }, "IF(revenue=0,0,(revenue-cost)/revenue)");
  const documentation = el("textarea", { "aria-label": "Function documentation", rows: 2, class: "prose" }, "Profit as a fraction of revenue.");
  const tests = el("textarea", { "aria-label": "Function tests", rows: 5, spellcheck: false }, JSON.stringify([{ arguments: ["100", "60"], expected: "0.4" }], null, 2));
  const dependencies = el("textarea", { "aria-label": "Dependency versions", rows: 1, spellcheck: false }, "{}");
  const pins = el("textarea", { "aria-label": "Active function versions", rows: 2, spellcheck: false }, "{}");
  const status = el("pre", { role: "status", class: "function-status" });
  const versions = el("select", { "aria-label": "Function version" });
  const signature = el("code", { class: "fn-signature" });
  const state = el("span", { class: "fn-state" });
  let selected: WorkbookFunction | undefined;
  /** The definition (normalized) whose tests last all passed, and whether the last run failed. */
  let tested: string | null = null;
  let failed: string | null = null;

  const field = (label: string | HTMLElement, input: HTMLElement, hint?: HTMLElement) =>
    el("label", { class: "function-field" }, label, input, hint ?? null);
  const definition = (): FunctionDefinition => {
    const parsed: unknown = JSON.parse(tests.value);
    if (!Array.isArray(parsed)) throw new Error("Tests must be a JSON array");
    const d: FunctionDefinition = { name: name.value.trim(), kind: kind.value as FunctionKind, arguments: args.value.split(",").map(x => x.trim()), source: source.value, documentation: documentation.value, tests: parsed as FunctionTest[], dependencies: JSON.parse(dependencies.value) };
    if (d.kind === "python") { d.returns = returns.value as FunctionDefinition["returns"]; d.volatile = volatile.checked; d.dependencies = {}; }
    return d;
  };
  const current = (): string | null => { try { return normalize(definition()); } catch { return null; } };

  /** Signature line and Draft / Tested / Published state of what the editor holds. */
  const render = () => {
    const argList = args.value.split(",").map(x => x.trim()).filter(Boolean);
    signature.textContent = `${name.value.trim().toUpperCase() || "NAME"}(${argList.join(", ")})`;
    const now = current();
    const published = now && selected?.name === name.value.trim()
      ? selected.versions.find(v => normalize(v.definition) === now) : undefined;
    const [label, tone, title] = published
      ? [`Published · v${published.version}${published.version === selected!.pinned ? " · active" : ""}`, "published",
        published.version === selected!.pinned ? "This version is the one the workbook uses." : "Published, not active: select it and choose Activate to use it."]
      : now && now === tested ? ["Tested · ready to publish", "tested", "Every test passed for exactly this definition."]
      : now && now === failed ? ["Draft · tests failing", "failing", "Fix the definition or its tests; publishing needs them to pass."]
      : ["Draft", "draft", "Not published. Run the tests, then publish a version."];
    state.textContent = label;
    state.dataset.state = tone;
    state.title = title;
  };
  const showKind = () => {
    const py = kind.value === "python";
    pythonFields.hidden = !py;
    sourceLabel.textContent = py ? "Python source" : "Expression";
    sourceHint.textContent = py
      ? `Define def ${name.value.trim().toLowerCase() || "name"}(${args.value}): … It runs in an isolated worker with no network or files; results are kept by their exact arguments.`
      : kind.value === "sql" ? "A DuckDB scalar expression over the arguments. Test inputs are strings: cast them explicitly."
      : "A sheet formula over the arguments, without the leading =. Sheet numbers use spreadsheet precision.";
    render();
  };
  for (const input of [name, args, source, documentation, tests, dependencies, returns, volatile]) input.addEventListener("input", () => { showKind(); });
  kind.addEventListener("change", showKind);

  const load = (f: WorkbookFunction, version = f.pinned) => {
    selected = f;
    const v = f.versions.find(v => v.version === version)!;
    name.value = v.definition.name; kind.value = v.definition.kind; args.value = v.definition.arguments.join(", ");
    returns.value = v.definition.returns ?? "number"; volatile.checked = !!v.definition.volatile;
    source.value = v.definition.source; documentation.value = v.definition.documentation; tests.value = JSON.stringify(v.definition.tests, null, 2); dependencies.value = JSON.stringify(v.dependencies);
    versions.replaceChildren(...f.versions.map(v => el("option", { value: v.version }, `v${v.version}${v.version === f.pinned ? " · active" : ""}`)));
    const runtime = v.runtime ? `\nPython ${v.runtime.version} · runtime ${v.runtime.id}${v.runtime.packages.length ? " · " + v.runtime.packages.join(", ") : ""}` : "";
    versions.value = String(version); status.textContent = `${f.name} · workbook scope · pinned v${f.pinned}\n${v.adapter}\nDependencies: ${JSON.stringify(v.dependencies)}${runtime}`;
    for (const li of list.querySelectorAll("button")) li.setAttribute("aria-current", li.dataset.name === f.name ? "true" : "false");
    showKind();
  };
  const blank = () => {
    selected = undefined;
    name.value = ""; args.value = ""; source.value = ""; documentation.value = "";
    tests.value = "[]"; dependencies.value = "{}"; volatile.checked = false;
    versions.replaceChildren();
    status.textContent = "";
    for (const li of list.querySelectorAll("button")) li.setAttribute("aria-current", "false");
    showKind();
    name.focus();
  };
  async function refresh() {
    const items = await rpc.call("functions", {});
    pins.value = JSON.stringify(Object.fromEntries(items.map(f => [f.name, f.pinned])), null, 2);
    list.replaceChildren(...items.map(f => {
      const k = f.versions.find(v => v.version === f.pinned)?.definition.kind ?? "formula";
      return el("li", {}, el("button", { class: "fn-item", "aria-label": `${f.name} · v${f.pinned}`, "data-name": f.name, onclick: () => load(f) },
        el("span", { class: "fn-item-name" }, f.name),
        el("span", { class: `fn-kind fn-kind-${k}` }, KIND_LABEL[k] ?? k),
        el("span", { class: "fn-item-version" }, `v${f.pinned}`)));
    }));
    if (!items.length) list.append(el("li", { class: "fn-empty" }, "No published functions yet. Fill in the form, run its tests and publish."));
    if (selected) { const found = items.find(f => f.name === selected!.name); if (found) load(found); }
    render();
  }
  versions.onchange = () => selected && load(selected, Number(versions.value));
  const controls: HTMLButtonElement[] = [];
  const action = (label: string, run: () => Promise<void>, primary = false) => {
    const button = el("button", { class: `btn${primary ? " btn-primary" : ""}`, onclick: async () => {
      controls.forEach(b => b.disabled = true);
      // Refresh replaces the options after a mutation. Do not accept a new selection
      // that would be silently overwritten before Activate becomes available again.
      versions.disabled = true;
      try { await run(); } catch (e) { status.textContent = String(e); }
      finally { controls.forEach(b => b.disabled = false); versions.disabled = false; render(); }
    } }, label); controls.push(button); return button;
  };
  const test = action("Run tests", async () => {
    const tried = current();
    const results = await rpc.call("test_function", { definition: definition() });
    const ok = results.every(t => t.passed);
    tested = ok ? tried : null;
    failed = ok ? null : tried;
    status.textContent = results.length ? results.map((t, i) => `${i + 1}: ${t.passed ? "Passed" : "FAILED"} · actual ${t.actual} · expected ${t.expected}`).join("\n") : "Definition validated; no test cases supplied.";
  });
  const publish = action("Publish version", async () => {
    selected = await rpc.call("publish_function", { definition: definition() }); await refresh();
    status.textContent += "\nPublished. Existing functions keep their active version until you select a version and activate it.";
  }, true);
  const activate = action("Activate selected version", async () => {
    if (!selected) throw new Error("Select a published function first");
    await rpc.call("pin_functions", { pins: { [selected.name]: Number(versions.value) } }); await refresh();
  });
  const group = action("Activate version map", async () => {
    await rpc.call("pin_functions", { pins: JSON.parse(pins.value) }); await refresh();
    status.textContent += "\nVersion map activated atomically.";
  });
  const section = (title: string, ...children: (HTMLElement | null)[]) =>
    el("section", { class: "fn-section" }, el("h3", {}, title), ...children);
  const dialog = el("section", { class: "dialog function-dialog" },
    el("div", { class: "fn-header" },
      el("h2", {}, "Function library"),
      el("p", {}, "Workbook-scoped functions. Formula functions run in sheets; SQL functions in queries and table columns; Python functions in sheets and as a whole table column (=NAME([column])), in isolated workers. Publishing never updates another workbook.")),
    el("div", { class: "fn-body" },
      el("aside", { class: "fn-side" },
        el("div", { class: "fn-side-head" }, el("h3", {}, "Functions"), el("button", { class: "btn small", onclick: blank }, "New function")),
        list),
      el("div", { class: "fn-editor" },
        el("div", { class: "fn-title" }, signature, state),
        status,
        section("Definition",
          el("div", { class: "fn-row" }, field("Name", name), field("Execution engine", kind)),
          pythonFields,
          field("Arguments (comma separated)", args),
          field(sourceLabel, source, sourceHint),
          field("Documentation and example usage", documentation)),
        section("Tests",
          field("Tests: arguments and expected display values (JSON)", tests),
          el("p", { class: "muted fn-note" }, "NULL binds SQL NULL. XLSX compatibility depends on Excel LAMBDA support; SQL and Python functions are not Excel formulas.")),
        section("Versions",
          el("div", { class: "fn-row fn-versions" }, versions, activate),
          field("Dependency versions (optional JSON overrides)", dependencies),
          field("Active versions (edit together for dependent upgrades)", pins), el("div", {}, group)))),
    el("div", { class: "dialog-actions fn-actions" }, test, publish, el("div", { class: "spacer" }), el("button", { class: "btn", onclick: () => modal.close() }, "Close")));
  const modal = openModal(dialog, () => modal.close());
  showKind();
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
    // Python functions are called from sheet cells and table columns alike.
    const active = functions.filter(f => { const k = f.versions.find(v => v.version === f.pinned)!.definition.kind; return k === engine() || k === "python"; })
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
