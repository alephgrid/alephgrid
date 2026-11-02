/**
 * The workbook's single command and shortcut registry. Menus, the command palette, the
 * shortcut help and grid key handling all read it, so a key is bound in one place with its
 * context and enabled state. Keys the browser or OS keeps (new tab, close tab, Spotlight,
 * input-source switching) are never bound; see docs/KEYBOARD.md.
 */
import { el, openModal } from "./ui";

/** Where focus is when the key is pressed. `grid`: the spreadsheet grid; `editor`: a cell
 *  or formula editor (its keys are handled there and listed here for help and audits). */
type CommandContext = "grid" | "editor";

interface Shortcut {
  /** `KeyboardEvent.key`, compared case-insensitively for letters. */
  key: string;
  /** Cmd on macOS, Ctrl elsewhere (either is accepted, as before). */
  mod?: boolean;
  /** The Control key itself, also on macOS. */
  ctrl?: boolean;
  shift?: boolean;
  alt?: boolean;
}

export interface Command {
  id: string;
  label: string;
  group: string;
  shortcuts: Shortcut[];
  contexts: CommandContext[];
  /** Why the command cannot run now (null when it can). */
  unavailable?: () => string | null;
  /** Commands the editor handles itself have no `run` here. */
  run?: () => void | Promise<void>;
  /** Known conflicts on some systems, shown in the shortcut help. */
  note?: string;
}

export const isMac = () => /Mac|iPhone|iPad/.test(navigator.platform);

function shortcutLabel(s: Shortcut, mac = isMac()): string {
  const parts: string[] = [];
  if (s.ctrl) parts.push(mac ? "⌃" : "Ctrl");
  if (s.mod) parts.push(mac ? "⌘" : "Ctrl");
  if (s.alt) parts.push(mac ? "⌥" : "Alt");
  if (s.shift) parts.push(mac ? "⇧" : "Shift");
  const key = s.key === " " ? "Space" : s.key.length === 1 ? s.key.toUpperCase() : s.key;
  parts.push(key);
  return mac ? parts.join("") : parts.join("+");
}

/** The letter a key event stands for: `key` for Latin layouts (AZERTY's A is A), the
 *  physical `code` when the layout produces another script (Cyrillic с on the C key). */
function eventLetter(e: KeyboardEvent): string {
  const k = e.key.length === 1 ? e.key.toLowerCase() : e.key;
  if (k.length === 1 && /[a-z]/.test(k)) return k;
  if (k.length === 1 && /\p{L}/u.test(k) && /^Key[A-Z]$/.test(e.code)) return e.code.slice(3).toLowerCase();
  return k;
}

export function matches(s: Shortcut, e: KeyboardEvent): boolean {
  const mod = e.metaKey || e.ctrlKey;
  // `ctrl` shortcuts need the Control key; `mod` ones accept Cmd or Ctrl.
  if (s.ctrl ? !e.ctrlKey || e.metaKey : !!s.mod !== mod) return false;
  if (!!s.shift !== e.shiftKey || !!s.alt !== e.altKey) return false;
  const want = s.key.length === 1 ? s.key.toLowerCase() : s.key;
  // Shift changes some printed characters (Shift+/ is ?); compare the unshifted key too.
  return eventLetter(e) === want || (s.key === "/" && e.code === "Slash");
}

export class CommandRegistry {
  private commands: Command[] = [];

  register(...commands: Command[]): void {
    for (const c of commands) {
      if (this.commands.some((x) => x.id === c.id)) throw new Error(`duplicate command ${c.id}`);
      this.commands.push(c);
    }
  }

  list(): readonly Command[] {
    return this.commands;
  }

  /** Two commands bound to the same key in the same context. */
  conflicts(): string[] {
    const seen = new Map<string, string>();
    const out: string[] = [];
    for (const c of this.commands) for (const ctx of c.contexts) for (const s of c.shortcuts) {
      const id = `${ctx}:${shortcutLabel(s, false).toLowerCase()}`;
      const other = seen.get(id);
      if (other) out.push(`${id} (${other}, ${c.id})`); else seen.set(id, c.id);
    }
    return out;
  }

  find(e: KeyboardEvent, context: CommandContext): Command | undefined {
    return this.commands.find((c) => c.run && c.contexts.includes(context) && c.shortcuts.some((s) => matches(s, e)));
  }

  /** Run the command bound to this key, if any. Returns whether the key was used. Keys are
   *  ignored while an input method is composing text. */
  handle(e: KeyboardEvent, context: CommandContext, explain: (message: string) => void): boolean {
    if (e.isComposing || e.keyCode === 229) return false;
    const c = this.find(e, context);
    if (!c) return false;
    const reason = c.unavailable?.();
    if (reason) explain(reason);
    else void c.run!();
    return true;
  }
}

/** Searchable list of every command, with shortcuts and the reason a command is unavailable. */
export function commandPalette(registry: CommandRegistry): () => void {
  const search = el("input", { type: "search", "aria-label": "Search commands", placeholder: "Type a command", autocomplete: "off",
    role: "combobox", "aria-expanded": "true", "aria-controls": "command-list", "aria-autocomplete": "list" });
  const list = el("ul", { id: "command-list", role: "listbox", "aria-label": "Commands", class: "command-list" });
  let shown: Command[] = [];
  let active = 0;
  const render = () => {
    const q = search.value.trim().toLowerCase();
    shown = registry.list().filter((c) => c.run && `${c.label} ${c.group}`.toLowerCase().includes(q));
    active = Math.min(active, Math.max(0, shown.length - 1));
    list.replaceChildren(...shown.map((c, i) => {
      const reason = c.unavailable?.() ?? null;
      return el("li", { id: `command-${c.id.replace(/\W/g, "-")}`, role: "option", "aria-selected": i === active ? "true" : "false",
        "aria-disabled": reason ? "true" : "false", class: `command-item${i === active ? " active" : ""}`,
        onmousedown: (e: MouseEvent) => e.preventDefault(), onclick: () => { active = i; void runActive(); } },
        el("span", {}, c.label), el("span", { class: "muted" }, reason ?? c.shortcuts.map((s) => shortcutLabel(s)).join(" or ")));
    }));
    search.setAttribute("aria-activedescendant", shown[active] ? `command-${shown[active].id.replace(/\W/g, "-")}` : "");
  };
  const status = el("p", { role: "status", class: "muted" });
  const runActive = async () => {
    const c = shown[active];
    if (!c) return;
    const reason = c.unavailable?.();
    if (reason) { status.textContent = reason; return; }
    modal.close();
    await c.run!();
  };
  search.addEventListener("input", () => { active = 0; render(); });
  search.addEventListener("keydown", (e) => {
    if (e.key === "ArrowDown") { e.preventDefault(); active = Math.min(shown.length - 1, active + 1); render(); }
    else if (e.key === "ArrowUp") { e.preventDefault(); active = Math.max(0, active - 1); render(); }
    else if (e.key === "Enter") { e.preventDefault(); void runActive(); }
  });
  const panel = el("section", { class: "dialog command-palette" }, el("h2", {}, "Commands"), search, list, status);
  const modal = openModal(panel, () => modal.close());
  render();
  search.focus();
  return () => modal.close();
}

/** Every shortcut, by group, with its context and known conflicts. */
export function shortcutHelp(registry: CommandRegistry): () => void {
  const groups = new Map<string, Command[]>();
  for (const c of registry.list()) groups.set(c.group, [...groups.get(c.group) ?? [], c]);
  const panel = el("section", { class: "dialog shortcut-help" }, el("h2", {}, "Keyboard shortcuts"),
    el("p", { class: "muted" }, "Shortcuts work while the grid has focus; editor keys work while editing a cell or the formula bar. Every command is also in the command palette."),
    ...[...groups].map(([group, cmds]) => el("table", { class: "shortcut-table" }, el("caption", {}, group),
      el("thead", {}, el("tr", {}, el("th", { scope: "col" }, "Command"), el("th", { scope: "col" }, "Keys"), el("th", { scope: "col" }, "Notes"))),
      el("tbody", {}, ...cmds.map((c) => el("tr", {}, el("th", { scope: "row" }, c.label),
        el("td", {}, c.shortcuts.length ? c.shortcuts.map((s) => shortcutLabel(s)).join(" or ") : "Command palette"),
        el("td", {}, [c.contexts.includes("editor") && !c.contexts.includes("grid") ? "While editing" : "", c.note ?? ""].filter(Boolean).join(". "))))))),
    el("div", { class: "dialog-actions" }, el("button", { class: "btn", onclick: () => modal.close() }, "Close")));
  const modal = openModal(panel, () => modal.close());
  panel.querySelector<HTMLButtonElement>("button")?.focus();
  return () => modal.close();
}
