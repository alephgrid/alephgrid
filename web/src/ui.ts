// Tiny DOM helpers: element builder, toasts, popup menus and form dialogs.

type Child = Node | string | null | undefined | false;

export function el<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  attrs: Record<string, any> = {},
  ...children: Child[]
): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) {
    if (v === undefined || v === null || v === false) continue;
    if (k.startsWith("on") && typeof v === "function") node.addEventListener(k.slice(2), v);
    else if (k === "class") node.className = v;
    else if (k === "style" && typeof v === "object") Object.assign(node.style, v);
    else if (k in node && typeof v !== "string") (node as any)[k] = v;
    else node.setAttribute(k, v === true ? "" : String(v));
  }
  for (const c of children) if (c !== null && c !== undefined && c !== false) node.append(c);
  return node;
}

let toastHost: HTMLElement | null = null;

export function toast(message: string, kind: "info" | "error" | "success" = "info", ms = 4000): void {
  if (!toastHost) {
    toastHost = el("div", { class: "toasts", role: "status", "aria-live": "polite" });
    document.body.append(toastHost);
  }
  const t = el("div", { class: `toast toast-${kind}` }, message);
  toastHost.append(t);
  setTimeout(() => t.remove(), kind === "error" ? ms * 2 : ms);
}

export type MenuItem =
  | { label: string; action: () => void; danger?: boolean; disabled?: boolean; hint?: string }
  | "sep";

let openMenu: HTMLElement | null = null;

export function closeMenu(): void {
  openMenu?.remove();
  openMenu = null;
}

export function showMenu(items: MenuItem[], x: number, y: number): void {
  closeMenu();
  const menu = el("div", { class: "menu", role: "menu" });
  for (const item of items) {
    if (item === "sep") {
      menu.append(el("div", { class: "menu-sep" }));
      continue;
    }
    menu.append(
      el(
        "button",
        {
          class: `menu-item${item.danger ? " danger" : ""}`,
          role: "menuitem",
          disabled: item.disabled,
          onclick: () => { closeMenu(); item.action(); },
        },
        el("span", {}, item.label),
        item.hint ? el("span", { class: "menu-hint" }, item.hint) : null,
      ),
    );
  }
  document.body.append(menu);
  const r = menu.getBoundingClientRect();
  menu.style.left = `${Math.max(8, Math.min(x, innerWidth - r.width - 8))}px`;
  menu.style.top = `${Math.max(8, Math.min(y, innerHeight - r.height - 8))}px`;
  openMenu = menu;
  setTimeout(() => {
    const close = (e: Event) => {
      if (!menu.contains(e.target as Node)) {
        closeMenu();
        document.removeEventListener("mousedown", close, true);
        document.removeEventListener("keydown", esc, true);
      }
    };
    const esc = (e: KeyboardEvent) => e.key === "Escape" && close(e);
    document.addEventListener("mousedown", close, true);
    document.addEventListener("keydown", esc, true);
  });
}

export interface Field {
  name: string;
  label: string;
  value?: string;
  placeholder?: string;
  type?: "text" | "textarea" | "select" | "number";
  options?: { value: string; label: string }[];
  hint?: string;
  mono?: boolean;
}

/** Modal form. Resolves with field values, or null when cancelled. */
export function formDialog(
  title: string,
  fields: Field[],
  submitLabel = "Save",
  validate?: (v: Record<string, string>) => Promise<string | null> | string | null,
): Promise<Record<string, string> | null> {
  return new Promise((resolve) => {
    const inputs = new Map<string, HTMLInputElement | HTMLTextAreaElement | HTMLSelectElement>();
    const error = el("div", { class: "dialog-error", role: "alert" });
    const form = el("form", { class: "dialog", onsubmit: submit },
      el("h2", {}, title),
      ...fields.map((f) => {
        let input: HTMLInputElement | HTMLTextAreaElement | HTMLSelectElement;
        if (f.type === "textarea") {
          input = el("textarea", { name: f.name, rows: 6, placeholder: f.placeholder ?? "", class: f.mono ? "mono" : "" });
          input.value = f.value ?? "";
        } else if (f.type === "select") {
          input = el("select", { name: f.name }, ...(f.options ?? []).map((o) => el("option", { value: o.value }, o.label)));
          input.value = f.value ?? f.options?.[0]?.value ?? "";
        } else {
          input = el("input", { name: f.name, type: f.type ?? "text", placeholder: f.placeholder ?? "", class: f.mono ? "mono" : "" });
          input.value = f.value ?? "";
        }
        inputs.set(f.name, input);
        return el("label", { class: "field" }, el("span", {}, f.label), input, f.hint ? el("small", {}, f.hint) : null);
      }),
      error,
      el("div", { class: "dialog-actions" },
        el("button", { type: "button", class: "btn", onclick: () => finish(null) }, "Cancel"),
        el("button", { type: "submit", class: "btn btn-primary" }, submitLabel),
      ),
    );
    const backdrop = el("div", { class: "backdrop", onmousedown: (e: Event) => e.target === backdrop && finish(null) }, form);
    backdrop.addEventListener("keydown", (e) => e.key === "Escape" && finish(null));
    document.body.append(backdrop);
    (inputs.values().next().value as HTMLElement | undefined)?.focus();

    function values(): Record<string, string> {
      const out: Record<string, string> = {};
      for (const [k, i] of inputs) out[k] = i.value;
      return out;
    }
    async function submit(e: Event) {
      e.preventDefault();
      const v = values();
      if (validate) {
        error.textContent = "";
        const msg = await validate(v);
        if (msg) { error.textContent = msg; return; }
      }
      finish(v);
    }
    function finish(v: Record<string, string> | null) {
      backdrop.remove();
      resolve(v);
    }
  });
}

export function confirmDialog(title: string, message: string, confirmLabel = "Delete"): Promise<boolean> {
  return new Promise((resolve) => {
    const done = (v: boolean) => { backdrop.remove(); resolve(v); };
    const ok = el("button", { class: "btn btn-danger", onclick: () => done(true) }, confirmLabel);
    const backdrop = el("div", { class: "backdrop", onmousedown: (e: Event) => e.target === backdrop && done(false) },
      el("div", { class: "dialog", role: "alertdialog" },
        el("h2", {}, title),
        el("p", {}, message),
        el("div", { class: "dialog-actions" },
          el("button", { class: "btn", onclick: () => done(false) }, "Cancel"),
          ok,
        ),
      ),
    );
    backdrop.addEventListener("keydown", (e) => e.key === "Escape" && done(false));
    document.body.append(backdrop);
    ok.focus();
  });
}
