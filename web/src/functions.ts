import { Rpc, RpcError } from "./api";
import { codeEditor, lineOf, type Problem } from "./codeeditor";
import { builtinFunctions } from "./catalog";
import { icon } from "./icons";
import { el, pointerDrag, toast } from "./ui";
import { store, stored } from "./storage";
import type { FunctionDefinition, FunctionKind, FunctionTest, FunctionTestResult, FunctionUsage, TestValue, TestValueKind, WorkbookFunction } from "./protocol";

/** U.06a–e: workbook functions in a resizable side panel beside the grid, with Code, Tests,
 *  Docs and Versions tabs and a compact results strip. Running tests never publishes;
 *  publishing a version and activating it in this workbook are separate steps. The panel
 *  keeps its draft while closed. */
export interface FunctionPanel {
  /** Show the panel (again), keeping the draft. */
  open(): void;
  close(): void;
  readonly isOpen: boolean;
}

const WIDTH_KEY = "alephgrid-fn-panel-width";
const RESULTS_KEY = "alephgrid-fn-results-height";
const MIN_WIDTH = 360, GRID_MIN = 560, DEFAULT_WIDTH = 480;
const RESULTS_MIN = 72, RESULTS_DEFAULT = 140;
/** Error values a test can pass or expect (the server's `TEST_ERRORS`). */
const TEST_ERRORS = ["#N/A", "#DIV/0!", "#VALUE!", "#REF!", "#NAME?", "#NUM!", "#NULL!", "#SPILL!", "#CALC!"];
type ValueType = "entered" | "text" | "blank" | "error";

interface Draft {
  name: string; kind: FunctionKind; args: string; source: string; documentation: string;
  tests: FunctionTest[]; returns: NonNullable<FunctionDefinition["returns"]>; volatile: boolean; dependencies: string;
}

const EXAMPLE: Draft = {
  name: "MARGIN", kind: "formula", args: "revenue, cost", source: "IF(revenue=0,0,(revenue-cost)/revenue)",
  documentation: "Profit as a fraction of revenue.", tests: [{ arguments: ["100", "60"], expected: "0.4" }],
  returns: "number", volatile: false, dependencies: "{}",
};
const EMPTY: Draft = { ...EXAMPLE, name: "", args: "", source: "", documentation: "", tests: [] };

function savedWidth(): number {
  const w = Number(stored(WIDTH_KEY)); return Number.isFinite(w) && w > 0 ? w : DEFAULT_WIDTH;
}

/** Fields compared to tell whether the panel holds a published version (order-independent). */
function normalize(d: FunctionDefinition): string {
  return JSON.stringify({
    name: d.name, kind: d.kind, arguments: d.arguments, source: d.source, documentation: d.documentation,
    tests: d.tests.map((t) => ({ arguments: t.arguments, expected: t.expected })),
    returns: d.kind === "python" ? d.returns ?? "number" : null, volatile: d.kind === "python" ? !!d.volatile : null,
  });
}

/** A test value as text (typed values show their text). */
function shown(v: TestValue): string {
  return typeof v === "string" ? v : v.type === "blank" ? "" : v.value;
}
function typeOf(v: TestValue): ValueType {
  return typeof v === "string" ? "entered" : v.type;
}
/** The same text as another type of value. */
function asType(v: TestValue, type: ValueType): TestValue {
  const text = shown(v);
  if (type === "entered") return text;
  if (type === "blank") return { type: "blank" };
  if (type === "error") return { type: "error", value: TEST_ERRORS.includes(text) ? text : "#N/A" };
  return { type: "text", value: text };
}
function isTestValue(v: unknown): v is TestValue {
  if (typeof v === "string") return true;
  if (!v || typeof v !== "object") return false;
  const t = v as { type?: unknown; value?: unknown };
  return t.type === "blank" || (t.type === "text" && typeof t.value === "string") || (t.type === "error" && typeof t.value === "string" && TEST_ERRORS.includes(t.value));
}
/** How a result reads: its text, and its kind where the text alone could mislead. */
function described(text: string, kind: TestValueKind | "entered"): string {
  if (kind === "error") return `${text} (error)`;
  if (kind === "text" && (text === "" || TEST_ERRORS.includes(text) || /^[-+]?\d/.test(text) || /^(TRUE|FALSE)$/i.test(text))) return text === "" ? "empty text" : `${text} (text)`;
  if (kind === "blank") return "blank";
  return text;
}
function describeValue(v: TestValue): string {
  const type = typeOf(v);
  return type === "entered" ? shown(v) : described(shown(v), type);
}
function savedResultsHeight(): number {
  const h = Number(stored(RESULTS_KEY)); return Number.isFinite(h) && h > 0 ? h : RESULTS_DEFAULT;
}

function argList(args: string): string[] {
  return args.split(",").map((x) => x.trim()).filter(Boolean);
}

export function functionPanel(rpc: Rpc, allowSql: boolean, mount: (panel: HTMLElement) => void, onClosed: () => void): FunctionPanel {
  let draft: Draft = { ...EMPTY };
  let functions: WorkbookFunction[] = [];
  let selected: WorkbookFunction | undefined;

  /** Results of the last run, for exactly the definition they ran on. */
  let results: { key: string; list: FunctionTestResult[]; tests: FunctionTest[]; error?: string; cancelled?: boolean } | null = null;
  let running = false, testing = false, cancelling = false, runSeq = 0, runId: number | null = null;
  /** The version whose activation is being confirmed, with what it affects. */
  let impactFor: { version: number; usage: FunctionUsage | null; error?: string } | null = null;
  let width = savedWidth(), expanded = false, isOpen = false;
  const log: string[] = [];

  // ---- header -------------------------------------------------------------------------
  const switcher = el("select", { "aria-label": "Function", class: "fn-switcher" });
  const newBtn = el("button", { class: "btn small", type: "button" }, "New");
  const signature = el("code", { class: "fn-signature" });
  const state = el("span", { class: "fn-state" });
  const expandBtn = el("button", { class: "icon-btn", type: "button" });
  const closeBtn = el("button", { class: "icon-btn", type: "button", "aria-label": "Close functions panel", title: "Close (Esc)" }, icon("x"));

  // ---- code tab -----------------------------------------------------------------------
  const name = el("input", { "aria-label": "Function name", maxlength: 64, spellcheck: false, placeholder: "NAME" });
  const kind = el("select", { "aria-label": "Function engine" }, el("option", { value: "formula" }, "Formula"),
    allowSql ? el("option", { value: "sql" }, "SQL") : null, el("option", { value: "python" }, "Python"));
  const args = el("input", { "aria-label": "Arguments", spellcheck: false, placeholder: "a, b" });
  const returns = el("select", { "aria-label": "Python result type" },
    el("option", { value: "number" }, "Number"), el("option", { value: "text" }, "Text"), el("option", { value: "boolean" }, "Logical"));
  const volatile = el("input", { type: "checkbox", "aria-label": "Volatile: computed again only on Refresh" });
  const pythonRow = el("div", { class: "fn-row", hidden: true },
    el("label", { class: "function-field" }, "Returns", returns),
    el("label", { class: "fn-check", title: "Reads the clock or randomness; computed again only when you choose Refresh" }, volatile, el("span", {}, "Volatile")));
  const editor = codeEditor("Function source", () => { read(); render(); });
  const source = editor.textarea;
  const sourceHint = el("p", { class: "muted fn-hint" });
  const codeTab = el("div", { class: "fn-tab-body" },
    el("div", { class: "fn-row" }, el("label", { class: "function-field grow" }, "Name", name), el("label", { class: "function-field" }, "Language", kind)),
    el("label", { class: "function-field" }, "Arguments", args),
    pythonRow,
    editor.box,
    sourceHint);

  // ---- tests tab ----------------------------------------------------------------------
  const testRows = el("tbody");
  const testHead = el("tr");
  const testNotice = el("p", { class: "notice", role: "note", hidden: true });
  const addTest = el("button", { class: "btn small", type: "button" }, "Add test");
  const testsJson = el("textarea", { "aria-label": "Function tests (JSON)", rows: 5, spellcheck: false, class: "fn-code" });
  const jsonApply = el("button", { class: "btn small", type: "button" }, "Use this JSON");
  const jsonError = el("p", { class: "dialog-error", role: "alert" });
  const testsTab = el("div", { class: "fn-tab-body" },
    testNotice,
    el("table", { class: "fn-tests", "aria-label": "Tests" }, el("thead", {}, testHead), testRows),
    el("div", { class: "fn-row" }, addTest, el("span", { class: "muted fn-hint" }, "“As typed” reads like a cell entry; choose Text, Blank or Error for exact values.")),
    el("details", { class: "fn-advanced" }, el("summary", {}, "Advanced: tests as JSON"), testsJson, el("div", { class: "fn-row" }, jsonApply), jsonError));

  // ---- docs tab -----------------------------------------------------------------------
  const documentation = el("textarea", { "aria-label": "Function documentation", rows: 4, class: "prose" });
  const example = el("code", { class: "fn-example" });
  const copyExample = el("button", { class: "btn small", type: "button" }, "Copy");
  const docsTab = el("div", { class: "fn-tab-body" },
    el("label", { class: "function-field" }, "Description", documentation),
    el("div", { class: "function-field" }, el("span", {}, "Use it in a cell"), el("div", { class: "fn-row" }, example, copyExample)),
    el("p", { class: "muted fn-hint" }, "Published functions appear in cell autocomplete with this description. Formula functions export to XLSX as LAMBDA names; SQL and Python functions are not Excel formulas."));

  // ---- versions tab -------------------------------------------------------------------
  const versionList = el("ul", { class: "fn-versions", "aria-label": "Published versions" });
  const dependencies = el("textarea", { "aria-label": "Dependency versions", rows: 2, spellcheck: false, class: "fn-code" });
  const pins = el("textarea", { "aria-label": "Active function versions", rows: 3, spellcheck: false, class: "fn-code" });
  const pinMap = el("button", { class: "btn small", type: "button" }, "Activate version map");
  const versionsTab = el("div", { class: "fn-tab-body" },
    versionList,
    el("details", { class: "fn-advanced" }, el("summary", {}, "Advanced: dependencies and version map"),
      el("label", { class: "function-field" }, "Dependency versions (JSON overrides)", dependencies),
      el("label", { class: "function-field" }, "Active versions (edit together for dependent upgrades)", pins),
      el("div", { class: "fn-row" }, pinMap)));

  // ---- tabs ---------------------------------------------------------------------------
  const tabNames = ["Code", "Tests", "Docs", "Versions"] as const;
  const bodies = [codeTab, testsTab, docsTab, versionsTab];
  const tabs = tabNames.map((t) => el("button", { type: "button", role: "tab", class: "fn-tab", id: `fn-tab-${t}`, "aria-controls": `fn-panel-${t}` }, t));
  bodies.forEach((b, i) => { b.id = `fn-panel-${tabNames[i]}`; b.setAttribute("role", "tabpanel"); b.setAttribute("aria-labelledby", tabs[i].id); });
  let tab = 0;
  const showTab = (i: number, focus = false) => {
    tab = i;
    tabs.forEach((t, j) => { t.setAttribute("aria-selected", String(i === j)); t.tabIndex = i === j ? 0 : -1; });
    bodies.forEach((b, j) => { b.hidden = i !== j; });
    if (focus) tabs[i].focus();
    if (i === 1) renderTests();
    if (i === 3) renderVersions();
  };
  tabs.forEach((t, i) => t.addEventListener("click", () => showTab(i)));
  const tabList = el("div", { role: "tablist", class: "fn-tabs", "aria-label": "Function parts" }, ...tabs);
  tabList.addEventListener("keydown", (e) => {
    const d = { ArrowRight: 1, ArrowLeft: -1 }[e.key];
    if (d) { e.preventDefault(); showTab((tab + d + tabs.length) % tabs.length, true); }
    else if (e.key === "Home") { e.preventDefault(); showTab(0, true); }
    else if (e.key === "End") { e.preventDefault(); showTab(tabs.length - 1, true); }
  });

  // ---- results strip ------------------------------------------------------------------
  const runBtn = el("button", { class: "btn", type: "button", title: "Run tests (⌘↵ / Ctrl+Enter)" }, "Run tests");
  const cancelBtn = el("button", { class: "btn", type: "button", hidden: true }, "Cancel");
  const publishBtn = el("button", { class: "btn btn-primary", type: "button" }, "Publish version");
  const runState = el("span", { class: "fn-run-state", role: "status" });
  const resultsBody = el("div", { class: "fn-results-body" });
  const consoleBody = el("pre", { class: "fn-console", role: "log", "aria-label": "Console" });
  const resultsTabs = [el("button", { type: "button", class: "fn-rtab", "aria-pressed": "true" }, "Results"), el("button", { type: "button", class: "fn-rtab", "aria-pressed": "false" }, "Console")];
  const collapse = el("button", { class: "icon-btn", type: "button", "aria-expanded": "true", "aria-label": "Hide results" }, icon("chevron"));
  const resultsGrip = el("div", { class: "fn-results-grip", role: "separator", tabindex: "0", "aria-orientation": "horizontal", "aria-label": "Resize results", title: "Drag, or use the arrow keys, to resize" });
  const results$ = el("section", { class: "fn-results", "aria-label": "Test results" },
    resultsGrip,
    el("div", { class: "fn-results-head" }, ...resultsTabs, el("div", { class: "spacer" }), runState, collapse),
    resultsBody, consoleBody);
  let resultsHeight = savedResultsHeight();
  const applyResultsHeight = () => {
    // A short panel (a small window, 200% zoom) keeps most of its height for the code.
    const tall = panel.getBoundingClientRect().height || 800;
    const max = Math.max(RESULTS_MIN, Math.round(tall * (tall < 560 ? 0.3 : 0.6)));
    const h = Math.min(max, Math.max(RESULTS_MIN, resultsHeight));
    results$.style.flexBasis = `${h}px`;
    resultsGrip.setAttribute("aria-valuenow", String(Math.round(h)));
    resultsGrip.setAttribute("aria-valuemin", String(RESULTS_MIN));
    resultsGrip.setAttribute("aria-valuemax", String(max));
  };
  const setResultsHeight = (h: number) => { resultsHeight = Math.max(RESULTS_MIN, h); applyResultsHeight(); store(RESULTS_KEY, String(Math.round(resultsHeight))); };
  resultsGrip.addEventListener("keydown", (e) => {
    const step = { ArrowUp: 24, ArrowDown: -24 }[e.key];
    if (step) { e.preventDefault(); setResultsHeight(results$.getBoundingClientRect().height + step); }
  });
  resultsGrip.addEventListener("pointerdown", (e) => pointerDrag(resultsGrip, e, (m) => setResultsHeight(results$.getBoundingClientRect().bottom - m.clientY)));
  consoleBody.hidden = true;
  resultsTabs.forEach((b, i) => b.addEventListener("click", () => {
    resultsTabs.forEach((x, j) => x.setAttribute("aria-pressed", String(i === j)));
    resultsBody.hidden = i !== 0; consoleBody.hidden = i !== 1;
  }));
  collapse.addEventListener("click", () => {
    const open = collapse.getAttribute("aria-expanded") !== "true";
    collapse.setAttribute("aria-expanded", String(open));
    collapse.setAttribute("aria-label", open ? "Hide results" : "Show results");
    results$.classList.toggle("collapsed", !open);
  });

  const divider = el("div", { class: "fn-divider", role: "separator", tabindex: "0", "aria-orientation": "vertical", "aria-label": "Resize functions panel", title: "Drag, or use the arrow keys, to resize" });
  const heading = el("h2", { id: "fn-panel-title", class: "sr-only" }, "Functions");
  const panel = el("aside", { class: "fn-panel", "aria-labelledby": "fn-panel-title" },
    divider,
    el("div", { class: "fn-panel-inner" },
      heading,
      el("div", { class: "fn-panel-head" }, switcher, newBtn, el("div", { class: "spacer" }), expandBtn, closeBtn),
      el("div", { class: "fn-title" }, signature, state),
      tabList, ...bodies,
      el("div", { class: "fn-actions" }, runBtn, cancelBtn, publishBtn),
      results$));

  // ---- model --------------------------------------------------------------------------
  const read = () => {
    draft = { ...draft, name: name.value.trim(), kind: kind.value as FunctionKind, args: args.value, source: source.value,
      documentation: documentation.value, returns: returns.value as Draft["returns"], volatile: volatile.checked, dependencies: dependencies.value };
  };
  const definition = (): FunctionDefinition => {
    const n = argList(draft.args).length;
    const wrong = draft.tests.findIndex((t) => t.arguments.length !== n);
    if (wrong >= 0) throw new Error(`Test ${wrong + 1} has ${draft.tests[wrong].arguments.length} inputs but the function takes ${n}. Fix it on the Tests tab.`);
    let deps: Record<string, number>;
    try { deps = JSON.parse(draft.dependencies || "{}"); } catch { throw new Error("Dependency versions are not valid JSON (Versions → Advanced)."); }
    const d: FunctionDefinition = { name: draft.name, kind: draft.kind, arguments: argList(draft.args), source: draft.source,
      documentation: draft.documentation, tests: draft.tests, dependencies: deps };
    if (d.kind === "python") { d.returns = draft.returns; d.volatile = draft.volatile; d.dependencies = {}; }
    return d;
  };
  const key = (): string | null => { try { return normalize(definition()); } catch { return null; } };
  const fill = (d: Draft) => {
    draft = { ...d, tests: d.tests.map((t) => ({ arguments: [...t.arguments], expected: t.expected })) };
    impactFor = null;
    name.value = d.name; kind.value = d.kind; args.value = d.args; editor.setValue(d.source); documentation.value = d.documentation;
    returns.value = d.returns; volatile.checked = d.volatile; dependencies.value = d.dependencies;
    results = null;
    render();
  };
  const fromVersion = (f: WorkbookFunction, version: number): Draft => {
    const v = f.versions.find((x) => x.version === version)!;
    const d = v.definition;
    return { name: d.name, kind: d.kind, args: d.arguments.join(", "), source: d.source, documentation: d.documentation,
      tests: d.tests, returns: d.returns ?? "number", volatile: !!d.volatile, dependencies: JSON.stringify(v.dependencies) };
  };

  // ---- rendering ----------------------------------------------------------------------
  const render = () => {
    const names = argList(draft.args);
    signature.textContent = `${draft.name.toUpperCase() || "NAME"}(${names.join(", ")})`;
    example.textContent = `=${draft.name.toUpperCase() || "NAME"}(${names.map(() => "…").join(", ")})`;
    pythonRow.hidden = draft.kind !== "python";
    sourceHint.textContent = draft.kind === "python"
      ? `def ${draft.name.toLowerCase() || "name"}(${names.join(", ")}): … runs in an isolated worker with no network or files. Running tests never runs it in cells.`
      : draft.kind === "sql" ? "A DuckDB scalar expression over the arguments. Test inputs are text: cast them explicitly."
      : "A sheet formula over the arguments, without the leading =.";
    sourceHint.textContent += " Tab indents; Escape, then Tab, leaves the editor.";
    editor.setLanguage(draft.kind, new Set(names.map((n) => n.toUpperCase())));
    const now = key();
    const published = now && selected?.name === draft.name ? selected.versions.find((v) => normalize(v.definition) === now) : undefined;
    // A cancelled run proves nothing: the draft stays a draft.
    const ran = results && results.key === now && !results.cancelled ? results : null;
    const passed = ran && !ran.error && ran.list.every((t) => t.passed);
    const [label, tone, title] = published
      ? [`Published · v${published.version}${published.version === selected!.pinned ? " · active" : ""}`, "published",
        published.version === selected!.pinned ? "The workbook uses this version." : "Published, not active here: activate it on the Versions tab."]
      : passed ? ["Tested · ready to publish", "tested", "Every test passed for exactly this definition."]
      : ran ? ["Draft · tests failing", "failing", "Fix the definition or its tests; publishing needs them to pass."]
      : ["Draft", "draft", "Not published. Run the tests, then publish a version."];
    state.textContent = label; state.dataset.state = tone; state.title = title;
    runState.textContent = cancelling ? "Cancelling…" : testing ? "Running…" : running ? "Working…"
      : results && results.key !== now ? "Results are for an earlier edit"
      : results?.cancelled ? "Cancelled"
      : ran ? (ran.error ? "Failed" : passed ? "Passed" : "Failing") : "";
    // Problems a run found in exactly this source, by line (a Python exception's line).
    const reported: Problem[] = [];
    if (ran?.error && lineOf(ran.error)) reported.push({ line: lineOf(ran.error)!, message: ran.error });
    ran?.list.forEach((t, i) => { const line = lineOf(t.message); if (line && t.message) reported.push({ line, message: `test ${i + 1}: ${t.message}` }); });
    editor.setReported(reported);
    renderResults();
    if (tab === 1) renderTests();
    if (tab === 3) renderVersions();
    runBtn.disabled = running; publishBtn.disabled = running;
    runBtn.hidden = testing; cancelBtn.hidden = !testing; cancelBtn.disabled = cancelling;
    expandBtn.replaceChildren(icon(expanded ? "restore" : "maximize"));
    expandBtn.setAttribute("aria-label", expanded ? "Narrow functions panel" : "Widen functions panel");
    expandBtn.setAttribute("aria-pressed", String(expanded));
  };

  const renderResults = () => {
    const now = key();
    if (!results) { resultsBody.replaceChildren(el("p", { class: "muted" }, "Run the tests to see results here.")); return; }
    const stale = results.key !== now;
    if (results.cancelled) { resultsBody.replaceChildren(el("p", {}, "Cancelled: the run was stopped on the server. Nothing was published.")); return; }
    if (results.error) { resultsBody.replaceChildren(el("p", { class: "dialog-error" }, results.error)); return; }
    const tests = results.tests;
    resultsBody.replaceChildren(
      ...(stale ? [el("p", { class: "notice" }, "Edited since this run: run the tests again.")] : []),
      results.list.length
        ? el("ol", { class: "fn-result-list" }, ...results.list.map((t, i) => el("li", { class: t.passed ? "pass" : "fail" },
          el("strong", {}, t.passed ? "Passed" : "Failed"),
          ` · test ${i + 1} · actual ${described(t.actual, t.kind)} · expected ${tests[i] ? describeValue(tests[i].expected) : t.expected}`,
          t.message ? el("span", { class: "fn-result-message" }, t.message) : null)))
        : el("p", {}, "Definition checked; there are no tests."));
  };

  const renderTests = () => {
    const names = argList(draft.args);
    testHead.replaceChildren(el("th", { scope: "col" }, "#"), ...names.map((a) => el("th", { scope: "col" }, a)),
      el("th", { scope: "col" }, "Expected"), el("th", { scope: "col" }, "Actual"), el("th", { scope: "col" }, el("span", { class: "sr-only" }, "Remove")));
    const ran = results && !results.error && results.key === key() ? results.list : null;
    const mismatch = draft.tests.filter((t) => t.arguments.length !== names.length).length;
    testNotice.hidden = !mismatch;
    testNotice.replaceChildren(`${mismatch} test${mismatch === 1 ? "" : "s"} do not match the ${names.length} argument${names.length === 1 ? "" : "s"}. `,
      el("button", { class: "btn small", type: "button", onclick: () => {
        draft.tests = draft.tests.map((t) => ({ ...t, arguments: names.map((_, i) => t.arguments[i] ?? "") }));
        render(); renderTests();
      } }, "Match the arguments"));
    // Rows are rebuilt on edits: keep focus and caret on the same field.
    const active = document.activeElement instanceof HTMLElement && testRows.contains(document.activeElement) ? document.activeElement : null;
    const focusLabel = active?.getAttribute("aria-label");
    const caret = active instanceof HTMLInputElement ? [active.selectionStart, active.selectionEnd] : null;
    const typeNames: [ValueType, string][] = [["entered", "As typed"], ["text", "Text"], ["blank", "Blank"], ["error", "Error"]];
    /** A test value: its type (as typed, exact text, blank cell, error value) and its text. */
    const valueCell = (label: string, v: TestValue, set: (v: TestValue) => void) => {
      const type = typeOf(v);
      const typeSelect = el("select", { "aria-label": `${label} type`, class: "fn-type", title: "As typed: read like a cell entry. Text: exactly this text. Blank: an empty cell (NULL, None). Error: an error value." },
        ...typeNames.map(([value, text]) => el("option", { value }, text)));
      typeSelect.value = type;
      typeSelect.addEventListener("change", () => { set(asType(v, typeSelect.value as ValueType)); render(); renderTests(); });
      let field: HTMLElement;
      if (type === "error") {
        const codes = el("select", { "aria-label": label }, ...TEST_ERRORS.map((c) => el("option", { value: c }, c)));
        codes.value = shown(v);
        codes.addEventListener("change", () => { set({ type: "error", value: codes.value }); render(); });
        field = codes;
      } else {
        const input = el("input", { "aria-label": label, value: shown(v), spellcheck: false, disabled: type === "blank",
          placeholder: type === "blank" ? "blank cell" : type === "text" ? "empty text" : "" });
        input.addEventListener("input", () => { set(type === "text" ? { type: "text", value: input.value } : input.value); render(); });
        field = input;
      }
      return el("td", { class: `fn-value fn-value-${type}` }, field, typeSelect);
    };
    testRows.replaceChildren(...draft.tests.map((t, i) => {
      const r = ran?.[i];
      return el("tr", { class: r ? (r.passed ? "pass" : "fail") : "" },
        el("th", { scope: "row" }, String(i + 1)),
        ...names.map((a, j) => valueCell(`Test ${i + 1} ${a}`, t.arguments[j] ?? "", (v) => { t.arguments[j] = v; })),
        valueCell(`Test ${i + 1} expected`, t.expected, (v) => { t.expected = v; }),
        el("td", { class: "fn-actual", title: r?.message ?? "" }, r ? `${r.passed ? "✓" : "✗"} ${described(r.actual, r.kind)}` : ""),
        el("td", {}, el("button", { class: "icon-btn", type: "button", "aria-label": `Remove test ${i + 1}`, onclick: () => { draft.tests.splice(i, 1); render(); renderTests(); } }, icon("x"))));
    }));
    if (!draft.tests.length) testRows.append(el("tr", {}, el("td", { colspan: String(names.length + 4), class: "muted" }, "No tests yet.")));
    if (focusLabel) {
      const again = [...testRows.querySelectorAll<HTMLElement>("[aria-label]")].find((x) => x.getAttribute("aria-label") === focusLabel);
      again?.focus();
      if (again instanceof HTMLInputElement && caret) again.setSelectionRange(caret[0], caret[1]);
    }
    if (document.activeElement !== testsJson) testsJson.value = JSON.stringify(draft.tests, null, 2);
  };
  addTest.addEventListener("click", () => {
    draft.tests.push({ arguments: argList(draft.args).map(() => ""), expected: "" });
    render(); renderTests();
    testRows.querySelector<HTMLInputElement>("tr:last-child input")?.focus();
  });
  jsonApply.addEventListener("click", () => {
    try {
      const v: unknown = JSON.parse(testsJson.value);
      if (!Array.isArray(v) || !v.every((t) => t && Array.isArray(t.arguments) && t.arguments.every(isTestValue) && isTestValue(t.expected))) {
        throw new Error(`Expected a list of { "arguments": […], "expected": … } where each value is text or {"type":"blank"}, {"type":"text","value":"…"} or {"type":"error","value":"${TEST_ERRORS[0]}"}.`);
      }
      draft.tests = v as FunctionTest[]; jsonError.textContent = ""; render(); renderTests();
    } catch (e) { jsonError.textContent = (e as Error).message; }
  });

  const renderVersions = () => {
    const f = selected && selected.name === draft.name ? selected : undefined;
    if (!f) { versionList.replaceChildren(el("li", { class: "muted" }, "Not published yet. Run the tests, then Publish version.")); return; }
    versionList.replaceChildren(...[...f.versions].reverse().map((v) => {
      const active = v.version === f.pinned;
      return el("li", { class: active ? "active" : "" },
        el("span", { class: "fn-version-name" }, `v${v.version}`, active ? el("span", { class: "fn-badge" }, "active") : null),
        el("span", { class: "muted" }, v.adapter + (v.runtime ? ` · Python ${v.runtime.version}` : "")),
        el("div", { class: "fn-row" },
          el("button", { class: "btn small", type: "button", "aria-label": `Open v${v.version}`, onclick: () => { fill(fromVersion(f, v.version)); showTab(0); } }, "Open"),
          active ? null : el("button", { class: "btn small", type: "button", "aria-label": `Activate v${v.version} in this workbook`,
            "aria-expanded": String(impactFor?.version === v.version), onclick: () => void showImpact(f, v.version) }, "Activate in this workbook")),
        impactFor?.version === v.version && !active ? impactBox(f, v.version) : null);
    }));
    if (document.activeElement !== pins) pins.value = JSON.stringify(Object.fromEntries(functions.map((x) => [x.name, x.pinned])), null, 2);
  };

  /** U.06e: what activating `version` changes, before anything changes. */
  const showImpact = async (f: WorkbookFunction, version: number) => {
    impactFor = { version, usage: null };
    renderVersions();
    try {
      const usage = await rpc.call("function_usage", { name: f.name });
      if (impactFor?.version === version) impactFor = { version, usage };
    } catch (e) {
      if (impactFor?.version === version) impactFor = { version, usage: null, error: (e as Error).message };
    }
    renderVersions();
    versionList.querySelector<HTMLButtonElement>(".fn-impact .btn-primary")?.focus();
  };
  const impactBox = (f: WorkbookFunction, version: number) => {
    const u = impactFor?.usage;
    const lines: (HTMLElement | string)[] = [el("p", {}, `Replaces v${f.pinned}, the version this workbook uses now. Other workbooks keep their own versions.`)];
    if (impactFor?.error) lines.push(el("p", { class: "dialog-error" }, `Could not find where ${f.name} is used: ${impactFor.error}`));
    else if (!u) lines.push(el("p", { class: "muted" }, `Finding where ${f.name} is used…`));
    else {
      if (u.cell_count) {
        const shownCells = u.cells.slice(0, 8).map((c) => `${c.sheet}!${c.cell}`);
        const more = u.cell_count - shownCells.length;
        lines.push(el("p", {}, `Recalculates ${u.cell_count} cell${u.cell_count === 1 ? "" : "s"}: `, el("span", { class: "fn-impact-list" }, shownCells.join(", ")), more > 0 ? ` and ${more} more` : ""));
      }
      if (u.columns.length) lines.push(el("p", {}, `Recalculates ${u.columns.length} table column${u.columns.length === 1 ? "" : "s"}: `,
        el("span", { class: "fn-impact-list" }, u.columns.map((c) => `${c.table}.${c.column}`).join(", "))));
      if (!u.cell_count && !u.columns.length) lines.push(el("p", {}, "No cell or table column calls it yet."));
      for (const d of u.dependents.filter((d) => d.uses !== version)) {
        lines.push(el("p", { class: "notice" }, `${d.name} v${d.version} calls ${f.name} v${d.uses}. Activating v${version} alone is refused; publish ${d.name} against v${version}, then activate both together (Advanced: version map).`));
      }
    }
    const confirm = el("button", { class: "btn btn-primary small", type: "button", disabled: !u && !impactFor?.error, onclick: () => void act(async () => {
      await rpc.call("pin_functions", { pins: { [f.name]: version } });
      impactFor = null;
      say(`${f.name} v${version} is now active in this workbook.`);
      await refresh();
    }) }, `Activate v${version}`);
    const cancel = el("button", { class: "btn small", type: "button", onclick: () => { impactFor = null; renderVersions(); versionList.querySelector<HTMLButtonElement>(`[aria-label="Activate v${version} in this workbook"]`)?.focus(); } }, "Cancel");
    return el("div", { class: "fn-impact", role: "group", "aria-label": `Activate ${f.name} v${version}` }, ...lines, el("div", { class: "fn-row" }, confirm, cancel));
  };

  const say = (line: string) => { log.push(`${new Date().toLocaleTimeString()}  ${line}`); consoleBody.textContent = log.slice(-200).join("\n"); };

  async function refresh(): Promise<void> {
    functions = await rpc.call("functions", {});
    const current = selected?.name ?? (draft.name || "");
    selected = functions.find((f) => f.name === current);
    switcher.replaceChildren(el("option", { value: "" }, "Draft · not published"),
      ...functions.map((f) => el("option", { value: f.name }, `${f.name} · v${f.pinned}`)));
    switcher.value = selected ? selected.name : "";
    render();
  }

  const act = async (work: () => Promise<void>) => {
    running = true; render();
    try { await work(); } catch (e) { say(String((e as Error).message ?? e)); results = { key: key() ?? "", list: [], tests: [], error: (e as Error).message }; }
    finally { running = false; render(); }
  };

  const runTests = () => act(async () => {
    read();
    const seq = ++runSeq, k = key();
    const d = definition();
    say(`Running ${d.tests.length} test${d.tests.length === 1 ? "" : "s"} for ${d.name || "the draft"}…`);
    testing = true; render();
    try {
      const list = await rpc.request("test_function", { definition: d }, { onSent: (id) => { if (seq === runSeq) runId = id; } });
      if (seq !== runSeq) return; // a newer run replaced this one
      results = { key: k ?? "", list, tests: structuredClone(d.tests) };
      const failedCount = list.filter((t) => !t.passed).length;
      say(list.length ? `${list.length - failedCount} passed, ${failedCount} failed.` : "Definition checked; no tests.");
    } catch (e) {
      // Cancelled: the server answers only once the run has stopped.
      if (e instanceof RpcError && e.code === "cancelled") {
        if (seq === runSeq) { results = { key: k ?? "", list: [], tests: d.tests, cancelled: true }; say("Cancelled; the server stopped the run."); }
        return;
      }
      throw e;
    } finally {
      if (seq === runSeq) { testing = false; cancelling = false; runId = null; }
    }
  });
  cancelBtn.addEventListener("click", () => {
    if (runId === null || cancelling) return;
    cancelling = true; render();
    say("Cancelling…");
    rpc.cancel(runId).catch((e) => { cancelling = false; say(`Could not cancel: ${(e as Error).message}`); render(); });
  });
  runBtn.addEventListener("click", () => void runTests());
  publishBtn.addEventListener("click", () => void act(async () => {
    read();
    const f = await rpc.call("publish_function", { definition: definition() });
    selected = f;
    say(`Published ${f.name} v${f.versions[f.versions.length - 1].version}. Workbooks keep their active version until you activate another (Versions).`);
    await refresh();
  }));
  pinMap.addEventListener("click", () => void act(async () => {
    let map: Record<string, number>;
    try { map = JSON.parse(pins.value); } catch { throw new Error("The version map is not valid JSON."); }
    await rpc.call("pin_functions", { pins: map });
    say("Version map activated together.");
    await refresh();
  }));

  switcher.addEventListener("change", () => {
    const f = functions.find((x) => x.name === switcher.value);
    if (!f) return;
    selected = f; fill(fromVersion(f, f.pinned));
  });
  const start = (d: Draft) => { selected = undefined; fill(d); switcher.value = ""; showTab(0); name.focus(); };
  newBtn.addEventListener("click", () => start({ ...EMPTY }));

  for (const input of [name, args, documentation, dependencies, returns, volatile]) input.addEventListener("input", () => { read(); render(); });
  kind.addEventListener("change", () => { read(); render(); });
  copyExample.addEventListener("click", async () => {
    try { await navigator.clipboard.writeText(example.textContent ?? ""); toast("Copied"); } catch { /* the text stays selectable */ }
  });

  // ---- size and keys ------------------------------------------------------------------
  const maxWidth = () => Math.max(MIN_WIDTH, window.innerWidth - GRID_MIN);
  const applyWidth = () => {
    const w = expanded ? maxWidth() : Math.min(maxWidth(), Math.max(MIN_WIDTH, width));
    panel.style.flexBasis = `${w}px`;
    divider.setAttribute("aria-valuenow", String(Math.round(w)));
    divider.setAttribute("aria-valuemin", String(MIN_WIDTH));
    divider.setAttribute("aria-valuemax", String(Math.round(maxWidth())));
  };
  const setWidth = (w: number) => {
    expanded = false; width = Math.min(maxWidth(), Math.max(MIN_WIDTH, w)); applyWidth(); render();
    store(WIDTH_KEY, String(Math.round(width)));
  };
  divider.addEventListener("keydown", (e) => {
    const step = { ArrowLeft: 24, ArrowRight: -24 }[e.key];
    if (step) { e.preventDefault(); setWidth(width + step); }
    else if (e.key === "Home") { e.preventDefault(); setWidth(MIN_WIDTH); }
    else if (e.key === "End") { e.preventDefault(); setWidth(maxWidth()); }
  });
  divider.addEventListener("pointerdown", (e) => pointerDrag(divider, e, (m) => setWidth(panel.getBoundingClientRect().right - m.clientX)));
  expandBtn.addEventListener("click", () => { expanded = !expanded; applyWidth(); render(); });
  panel.addEventListener("keydown", (e) => {
    if ((e.metaKey || e.ctrlKey) && e.key === "Enter") { e.preventDefault(); e.stopPropagation(); if (!running) void runTests(); }
    else if (e.key === "Escape" && !e.defaultPrevented && !(e.target instanceof HTMLTextAreaElement)) { e.preventDefault(); controller.close(); }
  });

  let opener: HTMLElement | null = null;
  const controller: FunctionPanel = {
    get isOpen() { return isOpen; },
    open() {
      if (!isOpen) {
        opener = document.activeElement instanceof HTMLElement ? document.activeElement : null;
        mount(panel); isOpen = true; applyWidth(); applyResultsHeight();
        window.addEventListener("resize", applyWidth);
        window.addEventListener("resize", applyResultsHeight);
      }
      void refresh().then(() => { if (!functions.length && !draft.name && !draft.source) offerExample(); }).catch((e) => say(String(e)));
      (tabs[tab]).focus();
    },
    close() {
      if (!isOpen) return;
      panel.remove(); isOpen = false; onClosed();
      window.removeEventListener("resize", applyWidth);
      window.removeEventListener("resize", applyResultsHeight);
      if (opener?.isConnected) opener.focus();
    },
  };
  const offerExample = () => {
    resultsBody.replaceChildren(el("p", {}, "No functions in this workbook yet. ",
      el("button", { class: "btn small", type: "button", onclick: () => start({ ...EXAMPLE, tests: EXAMPLE.tests.map((t) => ({ ...t, arguments: [...t.arguments] })) }) }, "Start from an example")));
  };
  closeBtn.addEventListener("click", () => controller.close());
  showTab(0);
  fill({ ...EMPTY });
  return controller;
}

/** Suggest active workbook functions without changing the expression evaluator. */
export function functionCompletion(input: HTMLInputElement, rpc: Rpc, engine: () => FunctionKind): () => void {
  let functions: WorkbookFunction[] = [];
  let disposed = false;
  let matches: { name: string; help: string; example?: string }[] = [];
  const help = el("div", { class: "function-completion", role: "region", "aria-label": "Function suggestions", hidden: true });

  const signature = (f: WorkbookFunction) => {
    const d = f.versions.find(v => v.version === f.pinned)!.definition;
    return `${f.name}(${d.arguments.join(", ")}) — ${d.documentation}`;
  };
  /** The first test of the active version, written as a call. */
  const exampleOf = (f: WorkbookFunction): string | undefined => {
    const d = f.versions.find(v => v.version === f.pinned)!.definition;
    const t = d.tests[0];
    if (!t) return undefined;
    const arg = (v: TestValue) => typeof v === "string" ? v : v.type === "blank" ? "" : v.type === "error" ? v.value : `"${v.value.replace(/"/g, '""')}"`;
    return `Example: ${engine() === "formula" ? "=" : ""}${f.name}(${t.arguments.map(arg).join(", ")}) → ${describeValue(t.expected)}`;
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
      .map((f): { name: string; help: string; example?: string } => ({ name: f.name, help: signature(f), example: exampleOf(f) }));
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
    if (current) help.append(el("p", {}, current.help), ...(current.example ? [el("p", { class: "function-example" }, current.example)] : []));
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
