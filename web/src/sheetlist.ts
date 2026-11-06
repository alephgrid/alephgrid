// U.04a: the list of every table and sheet, opened from the tab bar: type to narrow it, arrow
// keys to move, Enter to open. It finds a sheet without scrolling through the tabs.

import { icon } from "./icons";
import { el } from "./ui";

export interface SheetListItem {
  kind: "table" | "sheet";
  name: string;
  /** Shown after the name (row count, for instance). */
  detail?: string;
  active: boolean;
  open: () => void;
}

let seq = 0;

/** Open the list above `anchor`; it closes on Escape, an outside click or opening an item. */
export function sheetList(anchor: HTMLElement, items: SheetListItem[]): void {
  document.querySelector(".sheet-list")?.remove();
  const id = `sheet-list-${++seq}`;
  const search = el("input", {
    type: "search", class: "sheet-list-search", placeholder: "Find a sheet or table",
    role: "combobox", "aria-label": "Find a sheet or table", "aria-expanded": "true",
    "aria-controls": `${id}-options`, "aria-autocomplete": "list", autocomplete: "off", spellcheck: false,
  });
  const listbox = el("ul", { class: "sheet-list-options", role: "listbox", id: `${id}-options`, "aria-label": "Sheets and tables" });
  const empty = el("p", { class: "sheet-list-empty", role: "status" });
  const box = el("div", { class: "sheet-list", role: "dialog", "aria-label": "All sheets and tables" }, search, listbox, empty);
  let shown: SheetListItem[] = [];
  let highlight = 0;

  const close = (refocus: boolean) => {
    box.remove();
    document.removeEventListener("mousedown", outside, true);
    anchor.setAttribute("aria-expanded", "false");
    if (refocus && anchor.isConnected) anchor.focus();
  };
  const outside = (e: Event) => { if (!box.contains(e.target as Node) && e.target !== anchor && !anchor.contains(e.target as Node)) close(false); };
  const choose = (item: SheetListItem | undefined) => {
    if (!item) return;
    close(false);
    item.open();
  };
  const paint = () => {
    const q = search.value.trim().toLowerCase();
    shown = items.filter((i) => !q || i.name.toLowerCase().includes(q));
    highlight = Math.min(highlight, Math.max(0, shown.length - 1));
    listbox.replaceChildren(...shown.map((item, i) => el("li", {
      id: `${id}-${i}`, role: "option", class: `sheet-list-option${i === highlight ? " highlighted" : ""}`,
      "aria-selected": item.active ? "true" : "false",
      onmousedown: (e: MouseEvent) => e.preventDefault(),
      onclick: () => choose(item),
    }, el("span", { class: "tab-icon" }, icon(item.kind === "table" ? "table" : "sheet", 13)),
      el("span", { class: "sheet-list-name" }, item.name),
      item.detail ? el("span", { class: "sheet-list-detail" }, item.detail) : null)));
    empty.textContent = shown.length ? "" : `No sheets or tables match “${search.value.trim()}”.`;
    empty.hidden = !!shown.length;
    if (shown.length) {
      search.setAttribute("aria-activedescendant", `${id}-${highlight}`);
      listbox.children[highlight]?.scrollIntoView({ block: "nearest" });
    } else search.removeAttribute("aria-activedescendant");
  };
  search.addEventListener("input", () => { highlight = 0; paint(); });
  search.addEventListener("keydown", (e) => {
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      if (!shown.length) return;
      highlight = (highlight + (e.key === "ArrowDown" ? 1 : -1) + shown.length) % shown.length;
      paint();
    } else if (e.key === "Enter") {
      e.preventDefault();
      choose(shown[highlight]);
    } else if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      close(true);
    } else if (e.key === "Tab") {
      close(false);
    }
  });

  document.body.append(box);
  // Start on the open sheet or table.
  highlight = Math.max(0, items.findIndex((i) => i.active));
  paint();
  const r = anchor.getBoundingClientRect();
  const b = box.getBoundingClientRect();
  box.style.left = `${Math.max(8, Math.min(r.left, innerWidth - b.width - 8))}px`;
  box.style.top = `${Math.max(8, r.top - b.height - 6)}px`;
  anchor.setAttribute("aria-expanded", "true");
  search.focus();
  setTimeout(() => document.addEventListener("mousedown", outside, true));
}
