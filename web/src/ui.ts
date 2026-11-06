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

/** A pointer drag that started with `e` on `target`: the pointer is captured, `move` sees
 *  each move and `up` the release, and both listeners go away on release. */
export function pointerDrag(target: HTMLElement, e: PointerEvent, move: (m: PointerEvent) => void, up?: (u: PointerEvent) => void): void {
  e.preventDefault();
  target.setPointerCapture(e.pointerId);
  const end = (u: PointerEvent) => {
    target.removeEventListener("pointermove", move);
    target.removeEventListener("pointerup", end);
    up?.(u);
  };
  target.addEventListener("pointermove", move);
  target.addEventListener("pointerup", end);
}

/** Show an error's message as an error toast. */
export function toastError(e: unknown): void {
  toast((e as Error).message, "error");
}

export function toast(message: string, kind: "info" | "error" | "success" = "info", ms = 4000,
  action?: { label: string; run: () => void }): void {
  if (!toastHost) {
    toastHost = el("div", { class: "toasts", role: "status", "aria-live": "polite" });
    document.body.append(toastHost);
  }
  const t = el("div", { class: `toast toast-${kind}` }, message);
  if (action) {
    t.append(" ", el("button", { class: "btn btn-small toast-action", onclick: () => { t.remove(); action.run(); } }, action.label));
  }
  toastHost.append(t);
  // A toast with an action stays long enough to use it.
  setTimeout(() => t.remove(), (kind === "error" ? ms * 2 : ms) * (action ? 2 : 1));
}

/**
 * U.11a: mark `button` busy while work it started is in flight: disabled (so it cannot be
 * pressed twice) and `aria-busy`. Returns the function that restores it.
 */
export function busy(button: HTMLButtonElement): () => void {
  const wasDisabled = button.disabled;
  button.disabled = true;
  button.setAttribute("aria-busy", "true");
  return () => {
    button.removeAttribute("aria-busy");
    button.disabled = wasDisabled;
  };
}

/**
 * U.11a: disable `button` with the reason it cannot be used here (its tooltip and accessible
 * description), or enable it again with `reason` false. `title` is the tooltip it has when enabled.
 */
export function disabledFor(button: HTMLButtonElement, reason: string | false, title: string): void {
  button.disabled = !!reason;
  button.title = reason || title;
}

export type MenuItem =
  /** `disabled` as text disables the item and shows why in place of its shortcut (U.04a). */
  | { label: string; action: () => void; danger?: boolean; disabled?: boolean | string; hint?: string }
  | "sep";

let openMenu: HTMLElement | null = null;

let menuOpener: HTMLElement | null = null;
let menuCleanup: (() => void) | null = null;

export function closeMenu(): void {
  menuCleanup?.();
  menuCleanup = null;
  if (!openMenu) return;
  const hadFocus = openMenu.contains(document.activeElement);
  openMenu.remove();
  openMenu = null;
  // Return focus to whatever opened the menu, as keyboard users expect.
  if (hadFocus && menuOpener?.isConnected) menuOpener.focus();
  menuOpener = null;
}

interface MenuOptions {
  /** ArrowLeft / ArrowRight in the menu: move to the neighbouring menu of a menu bar. */
  onArrow?: (direction: -1 | 1) => void;
  /** Called however the menu closes. */
  onClose?: () => void;
  /** Accessible name of the menu (the menu bar item that opened it). */
  label?: string;
}

export function showMenu(items: MenuItem[], x: number, y: number, opts: MenuOptions = {}): void {
  closeMenu();
  menuOpener = document.activeElement instanceof HTMLElement ? document.activeElement : null;
  const menu = el("div", { class: "menu", role: "menu", "aria-label": opts.label ?? false });
  menu.addEventListener("keydown", (e) => {
    // Disabled items stay reachable (U.11a): keyboard and screen-reader users hear the reason.
    const buttons = [...menu.querySelectorAll<HTMLButtonElement>(".menu-item")];
    const i = buttons.indexOf(document.activeElement as HTMLButtonElement);
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      const next = e.key === "ArrowDown" ? (i + 1) % buttons.length : (i - 1 + buttons.length) % buttons.length;
      buttons[next]?.focus();
    } else if (e.key === "Home" || e.key === "End") {
      e.preventDefault();
      buttons[e.key === "Home" ? 0 : buttons.length - 1]?.focus();
    } else if ((e.key === "ArrowLeft" || e.key === "ArrowRight") && opts.onArrow) {
      e.preventDefault();
      e.stopPropagation();
      opts.onArrow(e.key === "ArrowLeft" ? -1 : 1);
    } else if (e.key === "Tab" || e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      closeMenu();
    }
  });
  for (const item of items) {
    if (item === "sep") {
      menu.append(el("div", { class: "menu-sep" }));
      continue;
    }
    // A disabled item is aria-disabled rather than disabled, so it keeps focus and its reason
    // (the title, which becomes its accessible description) is announced; activating it does nothing.
    menu.append(
      el(
        "button",
        {
          class: `menu-item${item.danger ? " danger" : ""}`,
          role: "menuitem",
          "aria-disabled": item.disabled ? "true" : false,
          title: typeof item.disabled === "string" ? item.disabled : false,
          onclick: () => { if (item.disabled) return; closeMenu(); item.action(); },
        },
        el("span", {}, item.label),
        typeof item.disabled === "string" ? el("span", { class: "menu-hint menu-reason" }, item.disabled)
          : item.hint ? el("span", { class: "menu-hint" }, item.hint) : null,
      ),
    );
  }
  document.body.append(menu);
  const r = menu.getBoundingClientRect();
  menu.style.left = `${Math.max(8, Math.min(x, innerWidth - r.width - 8))}px`;
  menu.style.top = `${Math.max(8, Math.min(y, innerHeight - r.height - 8))}px`;
  openMenu = menu;
  (menu.querySelector<HTMLButtonElement>(".menu-item:not([aria-disabled])") ?? menu.querySelector<HTMLButtonElement>(".menu-item"))?.focus({ preventScroll: true });
  // Outside clicks and Escape close the menu; the listeners go away with it however it closes.
  const outside = (e: Event) => !menu.contains(e.target as Node) && closeMenu();
  const esc = (e: KeyboardEvent) => e.key === "Escape" && closeMenu();
  const timer = setTimeout(() => {
    document.addEventListener("mousedown", outside, true);
    document.addEventListener("keydown", esc, true);
  });
  menuCleanup = () => {
    opts.onClose?.();
    clearTimeout(timer);
    document.removeEventListener("mousedown", outside, true);
    document.removeEventListener("keydown", esc, true);
  };
}

interface Field {
  name: string;
  label: string;
  value?: string;
  placeholder?: string;
  type?: "text" | "textarea" | "select" | "number" | "password";
  options?: { value: string; label: string }[];
  hint?: string;
  mono?: boolean;
}

let modalSeq = 0;

/**
 * Show `dialog` as a modal: labelled by its heading, focus kept inside while open and
 * returned to the opener afterwards, Escape and outside clicks call `onCancel`.
 */
export function openModal(dialog: HTMLElement, onCancel: () => void, role: "dialog" | "alertdialog" = "dialog"): { close: () => void } {
  const opener = document.activeElement instanceof HTMLElement ? document.activeElement : null;
  dialog.setAttribute("role", role);
  dialog.setAttribute("aria-modal", "true");
  const heading = dialog.querySelector("h2");
  if (heading) {
    heading.id ||= `dialog-title-${++modalSeq}`;
    dialog.setAttribute("aria-labelledby", heading.id);
  }
  const backdrop = el("div", { class: "backdrop", onmousedown: (e: Event) => e.target === backdrop && onCancel() }, dialog);
  backdrop.addEventListener("keydown", (e) => {
    if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      onCancel();
    } else if (e.key === "Tab") {
      const focusable = [...dialog.querySelectorAll<HTMLElement>("button, input, select, textarea, a[href], [tabindex]:not([tabindex='-1'])")]
        .filter((x) => !(x as HTMLButtonElement).disabled && x.offsetParent !== null);
      if (!focusable.length) return;
      const first = focusable[0], last = focusable[focusable.length - 1];
      if (e.shiftKey && document.activeElement === first) { e.preventDefault(); last.focus(); }
      else if (!e.shiftKey && document.activeElement === last) { e.preventDefault(); first.focus(); }
    }
  });
  // Safari does not focus a clicked button, so focus can leave the dialog for the page body:
  // keep Escape and Tab working for the topmost dialog wherever focus is, and bring focus
  // back to the dialog when a click inside it leaves focus nowhere.
  if (!dialog.hasAttribute("tabindex")) dialog.tabIndex = -1;
  const outside = (e: KeyboardEvent) => {
    const top = [...document.querySelectorAll(".backdrop")].at(-1) === backdrop;
    if (!top || e.defaultPrevented || backdrop.contains(document.activeElement)) return;
    if (e.key === "Escape") {
      e.preventDefault();
      onCancel();
    } else if (e.key === "Tab") {
      e.preventDefault();
      dialog.focus({ preventScroll: true });
    }
  };
  const refocus = () => setTimeout(() => {
    if (backdrop.isConnected && !backdrop.contains(document.activeElement)) dialog.focus({ preventScroll: true });
  });
  document.addEventListener("keydown", outside);
  dialog.addEventListener("mousedown", refocus);
  // A dialog opened over another: the one beneath is inert (and out of the accessibility
  // tree) until this one closes, so only the topmost modal is reachable.
  const beneath = [...document.querySelectorAll<HTMLElement>(".backdrop")].filter((b) => !b.inert);
  for (const b of beneath) b.inert = true;
  document.body.append(backdrop);
  return {
    close: () => {
      document.removeEventListener("keydown", outside);
      backdrop.remove();
      for (const b of beneath) b.inert = false;
      if (opener?.isConnected) opener.focus({ preventScroll: true });
    },
  };
}

/** A wide dialog with a Close button whose body `render` fills (and may fill again); an
 *  error it throws is shown in the body. `lede` is a line under the title. */
export function panelDialog(title: string, render: (body: HTMLElement, close: () => void) => Promise<void>, opts: { lede?: string } = {}): void {
  const body = el("div", { class: "panel-body" });
  const box = el("div", { class: "dialog wide" }, el("h2", {}, title),
    opts.lede ? el("p", { class: "dialog-lede" }, opts.lede) : null, body,
    el("div", { class: "dialog-actions" }, el("button", { class: "btn", onclick: () => modal.close() }, "Close")));
  const modal = openModal(box, () => modal.close());
  render(body, () => modal.close()).catch((e) => body.replaceChildren(el("p", { role: "alert" }, (e as Error).message)));
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
    const error = el("div", { class: "dialog-error", role: "alert", id: `dialog-error-${++modalSeq}` });
    const submitButton = el("button", { type: "submit", class: "btn btn-primary" }, submitLabel);
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
        submitButton,
      ),
    );
    // Editing after a refusal clears the invalid state; the message stays until the next try.
    form.addEventListener("input", () => markInvalid(false));
    const modal = openModal(form, () => finish(null));
    (inputs.values().next().value as HTMLElement | undefined)?.focus();

    function values(): Record<string, string> {
      const out: Record<string, string> = {};
      for (const [k, i] of inputs) out[k] = i.value;
      return out;
    }
    /** U.11a: a refused value marks the first field invalid, linked to the error text. */
    function markInvalid(on: boolean): void {
      const first = inputs.values().next().value as HTMLElement | undefined;
      if (!first) return;
      if (on) {
        first.setAttribute("aria-invalid", "true");
        first.setAttribute("aria-describedby", error.id);
      } else {
        first.removeAttribute("aria-invalid");
      }
    }
    async function submit(e: Event) {
      e.preventDefault();
      if (submitButton.disabled) return;
      const v = values();
      if (validate) {
        error.textContent = "";
        markInvalid(false);
        // Busy while the check runs: it cannot be sent twice.
        const done = busy(submitButton);
        let msg: string | null;
        try { msg = await validate(v); } finally { done(); }
        if (msg) {
          error.textContent = msg;
          markInvalid(true);
          (inputs.values().next().value as HTMLElement | undefined)?.focus();
          return;
        }
      }
      finish(v);
    }
    function finish(v: Record<string, string> | null) {
      modal.close();
      resolve(v);
    }
  });
}

export function confirmDialog(title: string, message: string, confirmLabel = "Delete"): Promise<boolean> {
  return new Promise((resolve) => {
    const done = (v: boolean) => { modal.close(); resolve(v); };
    const ok = el("button", { class: "btn btn-danger", onclick: () => done(true) }, confirmLabel);
    const modal = openModal(
      el("div", { class: "dialog" },
        el("h2", {}, title),
        el("p", {}, message),
        el("div", { class: "dialog-actions" },
          el("button", { class: "btn", onclick: () => done(false) }, "Cancel"),
          ok,
        ),
      ),
      () => done(false),
      "alertdialog",
    );
    ok.focus();
  });
}

/**
 * A question with several answers (the first is the default and gets focus). Resolves
 * with the chosen answer's `value`, or null when dismissed.
 */
export function choiceDialog(
  title: string,
  message: string,
  choices: { value: string; label: string; primary?: boolean }[],
): Promise<string | null> {
  return new Promise((resolve) => {
    const done = (v: string | null) => { modal.close(); resolve(v); };
    const buttons = choices.map((c) =>
      el("button", { class: `btn${c.primary ? " btn-primary" : ""}`, onclick: () => done(c.value) }, c.label));
    const modal = openModal(
      el("div", { class: "dialog" },
        el("h2", {}, title),
        el("p", {}, message),
        el("div", { class: "dialog-actions" },
          el("button", { class: "btn", onclick: () => done(null) }, "Cancel"),
          ...buttons,
        ),
      ),
      () => done(null),
      "alertdialog",
    );
    (buttons.find((_, i) => choices[i].primary) ?? buttons[0])?.focus();
  });
}
