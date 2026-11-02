// A panel docked under the grid (U.05): resizable by dragging or with the arrow keys,
// maximizable, closed with its button or Escape. Its height is remembered in this browser.

import { el, pointerDrag } from "./ui";
import { icon } from "./icons";

const KEY = "alephgrid-dock";
const MIN = 20, MAX = 85, STEP = 5;

function saved(): { height: number; max: boolean } {
  try {
    const v = JSON.parse(localStorage.getItem(KEY) ?? "{}");
    const height = typeof v.height === "number" ? Math.min(MAX, Math.max(MIN, v.height)) : 45;
    return { height, max: v.max === true };
  } catch { return { height: 45, max: false }; }
}

let seq = 0;

/** Dock `panel` at the bottom of `host` (a column flex box); `label` names the region. */
export function dockPanel(host: HTMLElement, panel: HTMLElement, label: string, onClose: () => void): { close: () => void } {
  const opener = document.activeElement instanceof HTMLElement ? document.activeElement : null;
  const state = saved();
  const heading = panel.querySelector("h2");
  if (heading) heading.id ||= `dock-title-${++seq}`;
  const handle = el("div", {
    class: "dock-resize", role: "separator", tabindex: "0", "aria-orientation": "horizontal",
    "aria-label": `Resize ${label} panel`, "aria-valuemin": String(MIN), "aria-valuemax": String(MAX),
    title: "Drag, or use the arrow keys, to resize",
  });
  const maxBtn = el("button", { class: "icon-btn dock-max-btn" });
  const closeBtn = el("button", { class: "icon-btn", "aria-label": `Close ${label} panel`, title: "Close (Esc)", onclick: () => onClose() }, icon("x"));
  const dock = el("section", { class: "dock", role: "region", "aria-labelledby": heading?.id ?? false, "aria-label": heading ? false : label },
    handle, el("div", { class: "dock-controls" }, maxBtn, closeBtn), panel);

  const persist = () => { try { localStorage.setItem(KEY, JSON.stringify(state)); } catch { /* this page only */ } };
  const render = () => {
    dock.style.height = state.max ? "" : `${state.height}%`;
    host.classList.toggle("dock-maximized", state.max);
    handle.setAttribute("aria-valuenow", String(state.max ? MAX : Math.round(state.height)));
    handle.setAttribute("aria-valuetext", state.max ? "maximized" : `${Math.round(state.height)}% of the height`);
    maxBtn.replaceChildren(icon(state.max ? "restore" : "maximize"));
    maxBtn.setAttribute("aria-label", state.max ? `Restore ${label} panel` : `Maximize ${label} panel`);
    maxBtn.setAttribute("aria-pressed", state.max ? "true" : "false");
    maxBtn.title = state.max ? "Restore" : "Maximize";
  };
  const setHeight = (h: number) => { state.max = false; state.height = Math.min(MAX, Math.max(MIN, h)); render(); persist(); };
  maxBtn.addEventListener("click", () => { state.max = !state.max; render(); persist(); });
  handle.addEventListener("keydown", (e) => {
    const step = { ArrowUp: STEP, ArrowDown: -STEP }[e.key];
    if (step) { e.preventDefault(); setHeight(state.height + step); }
    else if (e.key === "Home") { e.preventDefault(); setHeight(MIN); }
    else if (e.key === "End") { e.preventDefault(); setHeight(MAX); }
  });
  handle.addEventListener("pointerdown", (e) => pointerDrag(handle, e, (m) => {
    const r = host.getBoundingClientRect();
    setHeight(((r.bottom - m.clientY) / r.height) * 100);
  }));
  dock.addEventListener("keydown", (e) => {
    if (e.key === "Escape" && !e.defaultPrevented) { e.preventDefault(); onClose(); }
  });
  // Safari does not focus a clicked button, so focus can fall to the page body after a click in
  // the panel: bring it back to the panel so Escape and Tab keep working there.
  dock.tabIndex = -1;
  dock.addEventListener("mousedown", () => setTimeout(() => {
    if (dock.isConnected && (document.activeElement === document.body || !document.activeElement)) dock.focus({ preventScroll: true });
  }));
  // Firefox puts scrollable boxes in the Tab order; inside the panel only controls belong there.
  for (const box of panel.querySelectorAll<HTMLElement>(".sql-side, .sql-main, .sql-results")) {
    if (!box.hasAttribute("tabindex")) box.tabIndex = -1;
  }
  render();
  host.append(dock);
  return {
    close: () => {
      dock.remove();
      host.classList.remove("dock-maximized");
      if (opener?.isConnected) opener.focus({ preventScroll: true });
    },
  };
}
