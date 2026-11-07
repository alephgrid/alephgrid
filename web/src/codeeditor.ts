import { el } from "./ui";
import type { FunctionKind } from "./protocol";

/** U.06b: the function source editor. A plain textarea for input, selection, IME and
 *  accessibility, over a highlighted copy of the same text drawn from text nodes (source is
 *  user input: it never becomes markup). A small hand-written scanner per language gives the
 *  colours and the problems it can see without running anything: brackets that are never
 *  closed or closed by the wrong bracket, unterminated text and mixed indentation. Problems
 *  reported by a test run (a Python exception's line) are shown the same way. Indentation
 *  and undo are the editor's own, so both work the same in every browser and Undo never
 *  reaches the grid. */

export type TokenType = "keyword" | "string" | "number" | "comment" | "function" | "name" | "error" | "plain";
export interface Token { type: TokenType; text: string }
export interface Problem { line: number; message: string }

const PY_KEYWORDS = new Set(("False None True and as assert async await break class continue def del elif else except finally for from global if " +
  "import in is lambda nonlocal not or pass raise return try while with yield match case").split(" "));
const SQL_KEYWORDS = new Set(("select from where and or not null is in like ilike glob between case when then else end cast try_cast as true false " +
  "distinct all any some exists interval date timestamp time integer bigint double decimal varchar boolean asc desc nulls first last over " +
  "partition by order group having limit offset union intersect except filter within collate escape similar to").split(" "));
const FORMULA_ERRORS = ["#NULL!", "#DIV/0!", "#VALUE!", "#REF!", "#NAME?", "#NUM!", "#N/A", "#SPILL!", "#CALC!"];
const OPEN: Record<string, string> = { "(": ")", "[": "]", "{": "}" };
const CLOSE: Record<string, string> = { ")": "(", "]": "[", "}": "{" };

/** Tokens of `text` and the problems seen while scanning it. `names` are the declared
 *  arguments, shown as names. Concatenating the tokens' text gives `text` back exactly. */
export function scan(kind: FunctionKind, text: string, names: ReadonlySet<string> = new Set()): { tokens: Token[]; problems: Problem[] } {
  const tokens: Token[] = [];
  const problems: Problem[] = [];
  const stack: { ch: string; line: number }[] = [];
  let i = 0, line = 1;
  let afterDef = false;
  const push = (type: TokenType, end: number) => {
    const t = text.slice(i, end);
    for (const c of t) if (c === "\n") line++;
    const last = tokens[tokens.length - 1];
    if (last && last.type === type && type === "plain") last.text += t; else tokens.push({ type, text: t });
    i = end;
  };
  const upper = (s: string) => s.toUpperCase();
  const isNames = (s: string) => names.has(upper(s));
  const wordEnd = (from: number, extra = "") => {
    let j = from;
    while (j < text.length && (/[A-Za-z0-9_]/.test(text[j]) || extra.includes(text[j]))) j++;
    return j;
  };
  const callFollows = (from: number) => { let j = from; while (text[j] === " " || text[j] === "\t") j++; return text[j] === "("; };
  const quoted = (quote: string, doubled: boolean, multiline: boolean, what: string): number => {
    const start = line;
    let j = i + 1;
    while (j < text.length) {
      if (text[j] === quote) { if (doubled && text[j + 1] === quote) { j += 2; continue; } return j + 1; }
      if (!doubled && text[j] === "\\") { j += 2; continue; }
      if (text[j] === "\n" && !multiline) break;
      j++;
    }
    problems.push({ line: start, message: `${what} is not closed with ${quote}` });
    return Math.min(j, text.length);
  };
  // Python indentation that mixes tabs and spaces.
  if (kind === "python") {
    text.split("\n").forEach((l, n) => { const ws = /^[ \t]*/.exec(l)![0]; if (ws.includes(" ") && ws.includes("\t")) problems.push({ line: n + 1, message: "indentation mixes tabs and spaces" }); });
  }
  while (i < text.length) {
    const c = text[i];
    if (c === "\n" || c === " " || c === "\t" || c === "\r") { push("plain", i + 1); continue; }
    if (kind === "python" && c === "#") { const e = text.indexOf("\n", i); push("comment", e < 0 ? text.length : e); continue; }
    if (kind === "sql" && c === "-" && text[i + 1] === "-") { const e = text.indexOf("\n", i); push("comment", e < 0 ? text.length : e); continue; }
    if (kind === "sql" && c === "/" && text[i + 1] === "*") {
      const e = text.indexOf("*/", i + 2);
      if (e < 0) problems.push({ line, message: "comment is not closed with */" });
      push("comment", e < 0 ? text.length : e + 2); continue;
    }
    if (kind === "python") {
      const m = /^[rRbBuUfF]{0,2}('''|"""|'|")/.exec(text.slice(i, i + 5));
      if (m) {
        const q = m[1], prefix = m[0].length - q.length;
        if (q.length === 3) {
          const e = text.indexOf(q, i + prefix + 3);
          if (e < 0) problems.push({ line, message: `string is not closed with ${q}` });
          push("string", e < 0 ? text.length : e + 3);
        } else {
          const start = i; i += prefix; const e = quoted(q, false, false, "string"); i = start; push("string", e);
        }
        afterDef = false; continue;
      }
    }
    if (kind === "formula" && c === '"') { push("string", quoted('"', true, true, "text")); continue; }
    if (kind === "sql" && c === "'") { push("string", quoted("'", true, true, "string")); continue; }
    if (kind === "sql" && c === '"') { push("name", quoted('"', true, true, "name")); continue; }
    if (kind === "formula" && c === "#") {
      const err = FORMULA_ERRORS.find((x) => text.startsWith(x, i) || upper(text.slice(i, i + x.length)) === x);
      if (err) { push("error", i + err.length); continue; }
    }
    if (/[0-9]/.test(c) || (c === "." && /[0-9]/.test(text[i + 1] ?? ""))) {
      const m = /^(0[xX][0-9a-fA-F_]+|[0-9_]*\.?[0-9_]+(?:[eE][+-]?[0-9]+)?[jJ%]?)/.exec(text.slice(i));
      push("number", i + Math.max(1, m ? m[0].length : 1)); continue;
    }
    if (/[A-Za-z_]/.test(c) || (kind === "python" && c === "@")) {
      const e = wordEnd(i + 1, kind === "formula" ? "." : "");
      const word = text.slice(i, e);
      let type: TokenType = "plain";
      if (c === "@") type = "function";
      else if (kind === "python" && PY_KEYWORDS.has(word)) type = "keyword";
      else if (kind === "sql" && SQL_KEYWORDS.has(word.toLowerCase()) && !callFollows(e)) type = "keyword";
      else if (kind === "formula" && (upper(word) === "TRUE" || upper(word) === "FALSE")) type = "keyword";
      else if (afterDef || callFollows(e)) type = "function";
      else if (isNames(word)) type = "name";
      afterDef = kind === "python" && (word === "def" || word === "class");
      push(type, e); continue;
    }
    if (OPEN[c]) stack.push({ ch: c, line });
    else if (CLOSE[c]) {
      const top = stack[stack.length - 1];
      if (!top) problems.push({ line, message: `${c} has no matching ${CLOSE[c]}` });
      else if (top.ch !== CLOSE[c]) { problems.push({ line, message: `${c} closes the ${top.ch} opened on line ${top.line}` }); stack.pop(); }
      else stack.pop();
    }
    afterDef = false;
    push("plain", i + 1);
  }
  for (const open of stack) problems.push({ line: open.line, message: `${open.ch} is not closed` });
  problems.sort((a, b) => a.line - b.line);
  return { tokens, problems };
}

/** The line a run's message points at ("… line 3 …"), if any. */
export function lineOf(message: string | undefined): number | null {
  const m = /\bline (\d+)\b/.exec(message ?? "");
  return m ? Number(m[1]) : null;
}

export interface CodeEditor {
  /** The editor box: gutter, code and, at its foot, the problems. */
  readonly box: HTMLElement;
  readonly textarea: HTMLTextAreaElement;
  get value(): string;
  /** Replace the text (another function or version): its undo history starts afresh. */
  setValue(value: string): void;
  setLanguage(kind: FunctionKind, names: ReadonlySet<string>): void;
  /** Problems a run reported for exactly this text (cleared by the next edit). */
  setReported(problems: Problem[]): void;
}

type Snapshot = { value: string; start: number; end: number };

export function codeEditor(label: string, onInput: () => void): CodeEditor {
  const textarea = el("textarea", { "aria-label": label, spellcheck: false, class: "fn-code", wrap: "off", autocapitalize: "off", autocomplete: "off" });
  const code = el("code");
  const highlight = el("pre", { class: "fn-highlight", "aria-hidden": "true" }, code);
  const gutter = el("pre", { class: "fn-gutter", "aria-hidden": "true" });
  const problemsId = `fn-problems-${Math.random().toString(36).slice(2)}`;
  const problemsList = el("ul", { class: "fn-problems", id: problemsId, "aria-label": "Problems", hidden: true });
  // The problems sit inside the editor's frame, so they stay in view with the code.
  const box = el("div", { class: "fn-editor-box" }, el("div", { class: "fn-editor-main" }, gutter, el("div", { class: "fn-code-wrap" }, highlight, textarea)), problemsList);
  textarea.setAttribute("aria-describedby", problemsId);
  let kind: FunctionKind = "formula";
  let names: ReadonlySet<string> = new Set();
  let reported: Problem[] = [];

  const draw = () => {
    const { tokens, problems: found } = scan(kind, textarea.value, names);
    code.replaceChildren(...tokens.map((t) => t.type === "plain" ? document.createTextNode(t.text) : el("span", { class: `tok-${t.type}` }, t.text)),
      // A last empty line still takes its height.
      document.createTextNode("\n"));
    const problems = [...found, ...reported].sort((a, b) => a.line - b.line);
    const lines = Math.max(1, textarea.value.split("\n").length);
    const byLine = new Map<number, string[]>();
    for (const p of problems) byLine.set(p.line, [...(byLine.get(p.line) ?? []), p.message]);
    const nums: (Node | string)[] = [];
    for (let n = 1; n <= lines; n++) {
      const msgs = byLine.get(n);
      nums.push(el("span", { class: msgs ? "fn-ln has-problem" : "fn-ln", "data-line": String(n), title: msgs ? msgs.join("; ") : "" }, String(n)));
      if (n < lines) nums.push("\n");
    }
    gutter.replaceChildren(...nums);
    problemsList.replaceChildren(...problems.map((p) => el("li", {}, el("button", {
      type: "button", class: "fn-problem",
      onclick: () => goToLine(p.line),
    }, `Line ${p.line}: ${p.message}`))));
    problemsList.hidden = !problems.length;
    textarea.setAttribute("aria-invalid", String(problems.length > 0));
    sync();
  };
  const sync = () => {
    highlight.scrollTop = textarea.scrollTop; highlight.scrollLeft = textarea.scrollLeft;
    gutter.scrollTop = textarea.scrollTop;
  };
  const goToLine = (n: number) => {
    const lines = textarea.value.split("\n");
    const at = lines.slice(0, n - 1).reduce((s, l) => s + l.length + 1, 0);
    textarea.focus();
    textarea.setSelectionRange(at, at + (lines[n - 1]?.length ?? 0));
  };

  // ---- undo -------------------------------------------------------------------------------
  let undo: Snapshot[] = [], redo: Snapshot[] = [];
  let lastKind = "", lastAt = 0;
  const snap = (): Snapshot => ({ value: textarea.value, start: textarea.selectionStart, end: textarea.selectionEnd });
  const record = (what: string) => {
    const now = Date.now();
    // Typing (or deleting) in one run is one step.
    if (!(what === lastKind && (what === "type" || what === "delete") && now - lastAt < 1000)) {
      undo.push(snap());
      if (undo.length > 200) undo.shift();
    }
    redo = [];
    lastKind = what; lastAt = now;
  };
  const restore = (from: Snapshot[], to: Snapshot[]) => {
    const s = from.pop();
    if (!s) return;
    to.push(snap());
    textarea.value = s.value;
    textarea.setSelectionRange(s.start, s.end);
    lastKind = "";
    changed();
  };
  const changed = () => { reported = []; draw(); onInput(); };
  /** Replace `from`–`to` with `text`, as one undo step. */
  const edit = (text: string, from: number, to: number, select: "end" | "preserve" = "end") => {
    record("edit");
    textarea.setRangeText(text, from, to, select);
    lastKind = "";
    changed();
  };

  textarea.addEventListener("beforeinput", (e) => {
    if (e.inputType === "historyUndo") { e.preventDefault(); restore(undo, redo); return; }
    if (e.inputType === "historyRedo") { e.preventDefault(); restore(redo, undo); return; }
    record(e.inputType.startsWith("insertText") || e.inputType === "insertCompositionText" ? "type" : e.inputType.startsWith("delete") ? "delete" : "other");
  });
  textarea.addEventListener("input", (e) => { if ((e as InputEvent).inputType !== "historyUndo" && (e as InputEvent).inputType !== "historyRedo") changed(); });
  textarea.addEventListener("scroll", sync);

  // ---- keys -------------------------------------------------------------------------------
  const unit = () => kind === "python" ? "    " : "  ";
  const lineStart = (at: number) => textarea.value.lastIndexOf("\n", at - 1) + 1;
  let escaped = false;
  textarea.addEventListener("keydown", (e) => {
    const mod = e.metaKey || e.ctrlKey;
    if (e.key === "Escape") { escaped = true; e.stopPropagation(); return; }
    const wasEscaped = escaped;
    if (e.key !== "Shift") escaped = false;
    if (e.isComposing) return;
    // Read-only (a viewer, U.T06): Tab moves on and nothing below may change the text.
    if (textarea.readOnly) return;
    if (mod && !e.altKey && (e.key.toLowerCase() === "z" || e.key.toLowerCase() === "y")) {
      e.preventDefault(); e.stopPropagation();
      if (e.key.toLowerCase() === "y" || e.shiftKey) restore(redo, undo); else restore(undo, redo);
      return;
    }
    const { selectionStart: start, selectionEnd: end, value } = textarea;
    // Tab indents and Shift+Tab outdents; Escape first, then Tab, moves on (WCAG 2.1.2).
    if (e.key === "Tab" && !mod && !e.altKey && !wasEscaped) {
      e.preventDefault();
      const first = lineStart(start);
      const multiline = value.slice(start, end).includes("\n");
      if (!e.shiftKey && !multiline) { edit(unit(), start, end); return; }
      const last = end > start && value[end - 1] === "\n" ? end - 1 : end;
      const block = value.slice(first, last);
      const lines = block.split("\n");
      const out = e.shiftKey
        ? lines.map((l) => l.replace(new RegExp(`^( {1,${unit().length}}|\\t)`), ""))
        : lines.map((l) => (l.length ? unit() + l : l));
      const replaced = out.join("\n");
      record("edit");
      textarea.setRangeText(replaced, first, last, "preserve");
      const removedBefore = e.shiftKey ? lines[0].length - out[0].length : -(out[0].length - lines[0].length);
      const newStart = Math.max(first, start - removedBefore);
      textarea.setSelectionRange(multiline ? first : newStart, multiline ? first + replaced.length : Math.max(newStart, end - (block.length - replaced.length)));
      lastKind = "";
      changed();
      return;
    }
    // Enter keeps the line's indentation, one level deeper after `:` (Python) or an open bracket.
    if (e.key === "Enter" && !mod && !e.shiftKey && !e.altKey) {
      e.preventDefault();
      const current = value.slice(lineStart(start), start);
      let indent = /^[ \t]*/.exec(current)![0];
      const code = kind === "python" ? current.replace(/#.*$/, "") : current;
      if ((kind === "python" && /:\s*$/.test(code)) || /[([{]\s*$/.test(code)) indent += unit();
      edit("\n" + indent, start, end);
    }
  });

  draw();
  return {
    box, textarea,
    get value() { return textarea.value; },
    setValue(value: string) {
      if (value === textarea.value) return;
      textarea.value = value; undo = []; redo = []; lastKind = ""; reported = []; draw();
    },
    setLanguage(k: FunctionKind, n: ReadonlySet<string>) {
      const same = k === kind && n.size === names.size && [...n].every((x) => names.has(x));
      kind = k; names = n;
      if (!same) draw();
    },
    setReported(problems: Problem[]) {
      const key = (ps: Problem[]) => ps.map((p) => `${p.line}:${p.message}`).join("|");
      if (key(problems) === key(reported)) return;
      reported = problems; draw();
    },
  };
}
