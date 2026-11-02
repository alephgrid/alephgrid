// Light, dark or the system's choice, remembered in this browser. The chosen theme is set as
// `data-theme` on <html>; CSS tokens and the canvas grid follow it.

import { el } from "./ui";
import { icon } from "./icons";

type ThemePreference = "system" | "light" | "dark";
const KEY = "alephgrid-theme";
const system = matchMedia("(prefers-color-scheme: dark)");
let preference: ThemePreference = "system";
try {
  const saved = localStorage.getItem(KEY);
  if (saved === "light" || saved === "dark") preference = saved;
} catch { /* Storage can be unavailable (private windows); the default applies. */ }

export const THEME_EVENT = "alephgrid-theme";

function apply(): void {
  document.documentElement.dataset.theme = preference === "system" ? (system.matches ? "dark" : "light") : preference;
  window.dispatchEvent(new Event(THEME_EVENT));
}
system.addEventListener("change", () => { if (preference === "system") apply(); });
apply();
// The canvas grid draws text itself: repaint once the brand font has loaded.
void document.fonts?.ready.then(() => window.dispatchEvent(new Event(THEME_EVENT)));

export function themePreference(): ThemePreference { return preference; }

export function setTheme(next: ThemePreference): void {
  preference = next;
  try { localStorage.setItem(KEY, next); } catch { /* Kept for this page only. */ }
  apply();
}

const LABEL: Record<ThemePreference, string> = { system: "System theme", light: "Light theme", dark: "Dark theme" };
const ORDER: ThemePreference[] = ["system", "light", "dark"];

/** A button that cycles system → light → dark. */
export function themeButton(): HTMLButtonElement {
  const btn = el("button", { class: "icon-btn theme-btn" });
  const render = () => {
    btn.replaceChildren(icon(preference === "dark" ? "moon" : preference === "light" ? "sun" : "monitor"));
    btn.title = `${LABEL[preference]} (click to change)`;
    btn.setAttribute("aria-label", `Appearance: ${LABEL[preference]}`);
  };
  btn.addEventListener("click", () => {
    setTheme(ORDER[(ORDER.indexOf(preference) + 1) % ORDER.length]);
    render();
  });
  render();
  return btn;
}

export const THEME_CHOICES = ORDER.map((value) => ({ value, label: LABEL[value] }));
