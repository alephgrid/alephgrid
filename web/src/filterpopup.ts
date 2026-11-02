// U.16: the column header filter popup for tables. Lists the column's distinct values with
// their counts (bounded, counted on the server over the whole table, honouring the other
// columns' filters), with search, select all, sort and clear. Condition filters ("greater
// than", "contains", …) stay available as the advanced option.

import type { Column, ColumnValues, Filter, ViewSpec } from "./protocol";
import { el } from "./ui";
import { formatCount } from "./format";

export interface FilterPopupHost {
  values(spec: ViewSpec, column: string, search: string): Promise<ColumnValues>;
  apply(spec: ViewSpec): Promise<void>;
  /** Condition filters ("greater than", …); the button is left out without it. */
  condition?(): void;
}

const EMPTY = "\u0000empty";

let open: { close: () => void } | null = null;

export function closeFilterPopup(): void {
  open?.close();
}

/** Open the popup for `column` of `spec` at (x, y). */
export function filterPopup(column: Column, spec: ViewSpec, x: number, y: number, host: FilterPopupHost, returnFocus?: { focus(): void } | null): void {
  closeFilterPopup();
  const current = spec.filters.find((f) => f.column === column.name && f.op === "in");
  const others = spec.filters.filter((f) => !(f.column === column.name && f.op === "in"));
  const chosen = new Set<string>(Array.isArray(current?.value) ? (current!.value as (string | null)[]).map((v) => (v === null ? EMPTY : String(v))) : []);
  let allChosen = !current;
  let loaded: ColumnValues | null = null;
  let seq = 0;

  const search = el("input", { type: "search", class: "fp-search", placeholder: "Search values", "aria-label": `Search values of ${column.name}` }) as HTMLInputElement;
  const all = el("input", { type: "checkbox", "aria-label": "Select all" }) as HTMLInputElement;
  const list = el("div", { class: "fp-list", role: "group", "aria-label": `Values of ${column.name}` });
  const note = el("p", { class: "fp-note", "aria-live": "polite" });
  const sortAsc = el("button", { class: "btn btn-ghost fp-sort", type: "button" }, "Sort A → Z");
  const sortDesc = el("button", { class: "btn btn-ghost fp-sort", type: "button" }, "Sort Z → A");
  const condition = el("button", { class: "btn btn-ghost", type: "button" }, "Condition…");
  const clear = el("button", { class: "btn", type: "button", disabled: !spec.filters.some((f) => f.column === column.name) }, "Clear");
  const apply = el("button", { class: "btn btn-primary", type: "button" }, "Apply");

  const popup = el("div", { class: "filter-popup", role: "dialog", "aria-label": `Filter ${column.name}`, "aria-modal": "false" },
    el("div", { class: "fp-sorts" }, sortAsc, sortDesc),
    search,
    el("label", { class: "fp-all" }, all, el("span", {}, "Select all")),
    list,
    note,
    el("div", { class: "fp-actions" }, condition, el("span", { class: "fp-spacer" }), clear, apply),
  );

  const isChosen = (key: string) => allChosen || chosen.has(key);
  const syncAll = () => {
    const keys = (loaded?.values ?? []).map((v) => (v.value === null ? EMPTY : v.value));
    const n = keys.filter(isChosen).length;
    all.checked = keys.length > 0 && n === keys.length;
    all.indeterminate = n > 0 && n < keys.length;
  };
  const render = () => {
    const values = loaded?.values ?? [];
    list.replaceChildren(...values.map((v) => {
      const key = v.value === null ? EMPTY : v.value;
      const box = el("input", { type: "checkbox", checked: isChosen(key) }) as HTMLInputElement;
      box.addEventListener("change", () => {
        if (allChosen) {
          allChosen = false;
          for (const w of values) chosen.add(w.value === null ? EMPTY : w.value);
        }
        if (box.checked) chosen.add(key); else chosen.delete(key);
        syncAll();
      });
      return el("label", { class: "fp-item" }, box,
        el("span", { class: v.value === null ? "fp-value fp-empty" : "fp-value" }, v.value === null ? "(Empty)" : v.value === "" ? "(Blank text)" : v.value),
        el("span", { class: "fp-count" }, formatCount(v.count)));
    }));
    if (!loaded) note.textContent = "Loading values…";
    else if (!values.length) note.textContent = search.value ? "No value matches." : "No values.";
    else note.textContent = loaded.truncated
      ? `Showing ${formatCount(values.length)} of ${formatCount(loaded.distinct)} values; search to find others.`
      : `${formatCount(loaded.distinct)} values in ${formatCount(loaded.rows)} rows.`;
    syncAll();
  };
  const load = async () => {
    const mine = ++seq;
    try {
      const out = await host.values({ ...spec, filters: others }, column.name, search.value);
      if (mine !== seq) return;
      loaded = out;
    } catch (e) {
      if (mine !== seq) return;
      loaded = { values: [], distinct: 0, rows: 0, truncated: false };
      note.textContent = (e as Error).message;
      return;
    }
    render();
  };

  let timer = 0;
  search.addEventListener("input", () => { window.clearTimeout(timer); timer = window.setTimeout(load, 200); });
  all.addEventListener("change", () => {
    const keys = (loaded?.values ?? []).map((v) => (v.value === null ? EMPTY : v.value));
    if (!search.value) {
      allChosen = all.checked;
      chosen.clear();
    } else {
      if (allChosen) { allChosen = false; }
      for (const k of keys) if (all.checked) chosen.add(k); else chosen.delete(k);
    }
    render();
  });

  const finish = async (next: ViewSpec | null) => {
    close();
    if (next) await host.apply(next);
  };
  sortAsc.addEventListener("click", () => finish({ ...spec, sort: [{ column: column.name, desc: false }] }));
  sortDesc.addEventListener("click", () => finish({ ...spec, sort: [{ column: column.name, desc: true }] }));
  clear.addEventListener("click", () => finish({ ...spec, filters: spec.filters.filter((f) => f.column !== column.name) }));
  condition.addEventListener("click", () => { close(); host.condition?.(); });
  if (!host.condition) condition.hidden = true;
  apply.addEventListener("click", () => {
    if (allChosen && !search.value) return finish({ ...spec, filters: others });
    const keys = allChosen ? (loaded?.values ?? []).map((v) => (v.value === null ? EMPTY : v.value)) : [...chosen];
    const value = keys.map((k) => (k === EMPTY ? null : k));
    const filter: Filter = { column: column.name, op: "in", value: value as unknown as Filter["value"] };
    return finish({ ...spec, filters: [...others, filter] });
  });

  const onKey = (e: KeyboardEvent) => {
    if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); close(); }
    if (e.key === "Enter" && e.target === search) { e.preventDefault(); apply.click(); }
    if (e.key === "Tab") {
      const f = [...popup.querySelectorAll<HTMLElement>("button:not([disabled]), input")];
      const i = f.indexOf(document.activeElement as HTMLElement);
      if (e.shiftKey && i <= 0) { e.preventDefault(); f[f.length - 1].focus(); }
      else if (!e.shiftKey && i === f.length - 1) { e.preventDefault(); f[0].focus(); }
    }
  };
  const onDown = (e: MouseEvent) => { if (!popup.contains(e.target as Node)) close(); };
  popup.addEventListener("keydown", onKey);
  let listenTimer = 0;
  const close = () => {
    if (!popup.isConnected) return;
    popup.remove();
    clearTimeout(listenTimer);
    document.removeEventListener("mousedown", onDown, true);
    open = null;
    returnFocus?.focus();
  };
  open = { close };
  document.body.append(popup);
  const w = Math.min(320, window.innerWidth - 16);
  popup.style.width = `${w}px`;
  popup.style.left = `${Math.max(8, Math.min(x, window.innerWidth - w - 8))}px`;
  popup.style.top = `${Math.max(8, Math.min(y, window.innerHeight - popup.offsetHeight - 8))}px`;
  listenTimer = window.setTimeout(() => document.addEventListener("mousedown", onDown, true));
  render();
  search.focus();
  void load();
}
