import { isMac, type Command } from "./commands";

/** What the workbook commands act on; the workbook screen supplies these. */
export interface WorkbookActions {
  isSheet(): boolean;
  allowSql(): boolean;
  undo(): void; redo(): void;
  copy(): void; cut(): void; pasteSpecial(): void;
  fill(direction: "down" | "right"): void;
  selectAll(): void; selectEntire(what: "rows" | "columns"): void;
  goTo(): void; names(): void | Promise<void>; formulaHelp(): void; sql(): void;
  palette(): void; shortcuts(): void; history(): void;
}

/** Every workbook command with its keys. Grid keys without `run` are handled by the grid;
 *  editor keys by the cell editor and formula bar. See docs/KEYBOARD.md. */
export function workbookCommands(a: WorkbookActions): Command[] {
  const sheetOnly = (what: string) => () => a.isSheet() ? null : `${what} works in sheets. Tables use formula columns and Find / replace.`;
  return [
    { id: "edit.undo", label: "Undo", group: "Edit", shortcuts: [{ key: "z", mod: true }], contexts: ["grid"], run: () => a.undo() },
    { id: "edit.redo", label: "Redo", group: "Edit", shortcuts: [{ key: "z", mod: true, shift: true }, { key: "y", mod: true }], contexts: ["grid"], run: () => a.redo() },
    { id: "edit.copy", label: "Copy", group: "Edit", shortcuts: [{ key: "c", mod: true }], contexts: ["grid"], run: () => a.copy() },
    { id: "edit.cut", label: "Cut", group: "Edit", shortcuts: [{ key: "x", mod: true }], contexts: ["grid"], unavailable: sheetOnly("Cut"), run: () => a.cut() },
    { id: "edit.paste", label: "Paste", group: "Edit", shortcuts: [{ key: "v", mod: true }], contexts: ["grid"],
      note: "Uses the system clipboard. Cells copied in this workbook keep their formulas and formats" },
    { id: "edit.pasteSpecial", label: "Paste special…", group: "Edit", shortcuts: [{ key: "v", mod: true, shift: true }], contexts: ["grid"], unavailable: sheetOnly("Paste special"), run: () => a.pasteSpecial() },
    { id: "edit.fillDown", label: "Fill down", group: "Edit", shortcuts: [{ key: "d", mod: true }], contexts: ["grid"], unavailable: sheetOnly("Fill down"), run: () => a.fill("down"),
      note: "Replaces the browser's bookmark shortcut while the grid has focus" },
    { id: "edit.fillRight", label: "Fill right", group: "Edit", shortcuts: [{ key: "r", mod: true }], contexts: ["grid"], unavailable: sheetOnly("Fill right"), run: () => a.fill("right"),
      note: "Replaces the browser's reload shortcut while the grid has focus" },
    { id: "edit.start", label: "Edit cell", group: "Edit", shortcuts: [{ key: "F2" }, { key: "Enter" }], contexts: ["grid"] },
    { id: "edit.clear", label: "Clear cells", group: "Edit", shortcuts: [{ key: "Delete" }, { key: "Backspace" }], contexts: ["grid"] },
    { id: "select.all", label: "Select all", group: "Select", shortcuts: [{ key: "a", mod: true }], contexts: ["grid"], run: () => a.selectAll() },
    { id: "select.rows", label: "Select whole rows", group: "Select", shortcuts: [{ key: " ", shift: true }], contexts: ["grid"], run: () => a.selectEntire("rows") },
    { id: "select.columns", label: "Select whole columns", group: "Select", shortcuts: [{ key: " ", ctrl: true }], contexts: ["grid"], run: () => a.selectEntire("columns"),
      note: isMac() ? "macOS switches input sources with Ctrl+Space by default; use the command palette if the key does nothing" : undefined },
    { id: "nav.goTo", label: "Go to…", group: "Navigate", shortcuts: [{ key: "g", mod: true }], contexts: ["grid"], unavailable: sheetOnly("Go to"), run: () => a.goTo() },
    { id: "names.manage", label: "Named ranges…", group: "Formulas", shortcuts: [], contexts: ["grid"], unavailable: sheetOnly("Named ranges"), run: () => a.names() },
    { id: "help.formulas", label: "Formula help", group: "Formulas", shortcuts: [{ key: "F3", shift: true }], contexts: ["grid"], run: () => a.formulaHelp() },
    { id: "app.history", label: "Change history…", group: "Workbook", shortcuts: [], contexts: ["grid"], run: () => a.history() },
    { id: "app.sql", label: "Open SQL workspace", group: "Workbook", shortcuts: [], contexts: ["grid"], unavailable: () => a.allowSql() ? null : "SQL is turned off on this server", run: () => a.sql() },
    { id: "app.commands", label: "Command palette", group: "Help", shortcuts: [{ key: "k", mod: true }], contexts: ["grid"], run: () => a.palette() },
    { id: "help.shortcuts", label: "Keyboard shortcuts", group: "Help", shortcuts: [{ key: "/", mod: true }], contexts: ["grid"], run: () => a.shortcuts() },
    { id: "editor.commit", label: "Save the edit and move down", group: "Editing", shortcuts: [{ key: "Enter" }], contexts: ["editor"] },
    { id: "editor.commitRight", label: "Save the edit and move right", group: "Editing", shortcuts: [{ key: "Tab" }], contexts: ["editor"], note: "Accepts a function suggestion first when one is shown" },
    { id: "editor.cancel", label: "Discard the edit", group: "Editing", shortcuts: [{ key: "Escape" }], contexts: ["editor"] },
    { id: "editor.anchor", label: "Cycle $ anchors of a reference", group: "Editing", shortcuts: [{ key: "F4" }], contexts: ["editor"] },
  ];
}
