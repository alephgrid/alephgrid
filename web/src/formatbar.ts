// S.01 / S.02: the cell format toolbar for sheets: bold, italic, underline, strikethrough,
// text and fill colour, alignment, wrap, number format, more or fewer decimals and clear
// formatting. Each change is one undo step on the server.

import { el, formDialog } from "./ui";
import type { CellStyle, StylePatch } from "./protocol";

export interface FormatHost {
  /** The style of the active cell, to show and toggle. */
  current(): CellStyle | undefined;
  apply(patch: StylePatch): Promise<void>;
  clear(): Promise<void>;
}

export const NUMBER_FORMATS: { label: string; code: string }[] = [
  { label: "General", code: "" },
  { label: "Number (1,234.56)", code: "#,##0.00" },
  { label: "Whole number (1,235)", code: "#,##0" },
  { label: "Currency ($1,234.56)", code: "$#,##0.00" },
  { label: "Percent (12.35%)", code: "0.00%" },
  { label: "Date (2026-11-01)", code: "yyyy-mm-dd" },
  { label: "Time (13:45:00)", code: "hh:mm:ss" },
  { label: "Date and time", code: "yyyy-mm-dd hh:mm" },
  { label: "Scientific (1.23E+03)", code: "0.00E+00" },
  { label: "Text", code: "@" },
];

/** A number format with one more (or fewer) decimal place. */
export function stepDecimals(code: string, more: boolean): string {
  const base = code === "" || code.toLowerCase() === "general" ? "0" : code;
  const m = /0(\.0*)?/.exec(base);
  if (!m) return more ? `${base}.0` : base;
  const decimals = m[1] ? m[1].length - 1 : 0;
  const next = Math.max(0, Math.min(10, decimals + (more ? 1 : -1)));
  const replacement = next ? `0.${"0".repeat(next)}` : "0";
  return base.slice(0, m.index) + replacement + base.slice(m.index + m[0].length);
}

export function formatBar(host: FormatHost): HTMLElement {
  const toggle = (label: string, key: "b" | "i" | "u" | "s", text: string, keys?: string) => {
    const b = el("button", { class: "fmt-btn", type: "button", "aria-label": label, title: keys ? `${label} (${keys})` : label, "aria-pressed": "false" }, text);
    b.classList.add(`fmt-${key}`);
    b.addEventListener("click", () => void host.apply({ [key]: !(host.current()?.[key] ?? false) }));
    return b;
  };
  const colour = (label: string, key: "color" | "fill", value: string) => {
    const input = el("input", { type: "color", value, "aria-label": label, title: label, class: "fmt-color" }) as HTMLInputElement;
    input.addEventListener("change", () => void host.apply({ [key]: input.value.toUpperCase() }));
    return input;
  };
  const align = (h: "left" | "center" | "right", text: string) => {
    const b = el("button", { class: "fmt-btn", type: "button", "aria-label": `Align ${h}`, title: `Align ${h}` }, text);
    b.addEventListener("click", () => void host.apply({ h: host.current()?.h === h ? "" : h }));
    return b;
  };
  const wrap = el("button", { class: "fmt-btn", type: "button", "aria-label": "Wrap text", title: "Wrap text", "aria-pressed": "false" }, "↵");
  wrap.addEventListener("click", () => void host.apply({ wrap: !(host.current()?.wrap ?? false) }));
  const fmt = el("select", { "aria-label": "Number format", class: "fmt-select" },
    ...NUMBER_FORMATS.map((f) => el("option", { value: f.code }, f.label)), el("option", { value: "__custom" }, "Custom…")) as HTMLSelectElement;
  fmt.addEventListener("change", async () => {
    let code = fmt.value;
    if (code === "__custom") {
      const v = await formDialog("Custom number format", [{ name: "code", label: "Format code", value: host.current()?.num_fmt ?? "", mono: true, hint: "For example #,##0.0, 0.00% or yyyy-mm-dd" }], "Apply");
      if (!v) return;
      code = v.code;
    }
    void host.apply({ num_fmt: code });
  });
  const dec = (more: boolean) => {
    const b = el("button", { class: "fmt-btn", type: "button", "aria-label": more ? "More decimals" : "Fewer decimals", title: more ? "More decimals" : "Fewer decimals" }, more ? ".0+" : ".0−");
    b.addEventListener("click", () => void host.apply({ num_fmt: stepDecimals(host.current()?.num_fmt ?? "", more) }));
    return b;
  };
  const clear = el("button", { class: "fmt-btn", type: "button", "aria-label": "Clear formatting", title: "Clear formatting" }, "⌫");
  clear.addEventListener("click", () => void host.clear());
  const bar = el("div", { class: "format-bar", role: "toolbar", "aria-label": "Cell format" },
    toggle("Bold", "b", "B", "⌘B"), toggle("Italic", "i", "I", "⌘I"), toggle("Underline", "u", "U", "⌘U"), toggle("Strikethrough", "s", "S"),
    el("span", { class: "fmt-sep" }), colour("Text colour", "color", "#102D25"), colour("Fill colour", "fill", "#FFF2CC"),
    el("span", { class: "fmt-sep" }), align("left", "⇤"), align("center", "↔"), align("right", "⇥"), wrap,
    el("span", { class: "fmt-sep" }), fmt, dec(false), dec(true), el("span", { class: "fmt-sep" }), clear);
  /** Show the active cell's format on the controls. */
  (bar as HTMLElement & { sync?: () => void }).sync = () => {
    const st = host.current();
    for (const k of ["b", "i", "u", "s"] as const) bar.querySelector(`.fmt-${k}`)?.setAttribute("aria-pressed", String(!!st?.[k]));
    wrap.setAttribute("aria-pressed", String(!!st?.wrap));
    const code = st?.num_fmt ?? "";
    fmt.value = NUMBER_FORMATS.some((f) => f.code === code) ? code : "__custom";
  };
  return bar;
}
