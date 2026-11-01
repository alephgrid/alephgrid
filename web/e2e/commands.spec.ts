import AxeBuilder from "@axe-core/playwright";
import { expect, test, type Page } from "@playwright/test";
import { CommandRegistry, matches } from "../src/commands";
import { workbookCommands, type WorkbookActions } from "../src/workbookcommands";
import { Server, menuCommand } from "./server";

const server = new Server();
test.beforeAll(async () => server.start());
test.afterAll(async () => server.dispose());

const noop = () => {};
const stub: WorkbookActions = {
  isSheet: () => true, allowSql: () => true, undo: noop, redo: noop, copy: noop, cut: noop, pasteSpecial: noop, fill: noop,
  selectAll: noop, selectEntire: noop, goTo: noop, names: noop, formulaHelp: noop, sql: noop, palette: noop, shortcuts: noop,
};
const key = (init: Partial<KeyboardEvent>) => ({ key: "", code: "", ctrlKey: false, metaKey: false, shiftKey: false, altKey: false, isComposing: false, keyCode: 0, ...init }) as KeyboardEvent;

test("one registry: no key bound twice in a context; layouts and IME", () => {
  const registry = new CommandRegistry();
  registry.register(...workbookCommands(stub));
  expect(registry.conflicts()).toEqual([]);
  // Reserved browser/OS keys are never bound.
  for (const k of ["t", "w", "n", "q", "l", "Tab"]) expect(registry.find(key({ key: k, ctrlKey: true }), "grid"), k).toBeUndefined();
  expect(registry.find(key({ key: "F5" }), "grid")).toBeUndefined();
  const copy = { key: "c", mod: true };
  expect(matches(copy, key({ key: "c", metaKey: true }))).toBe(true);
  expect(matches(copy, key({ key: "C", ctrlKey: true, shiftKey: true }))).toBe(false);
  // Cyrillic layout: the C key types "с"; the physical key is used.
  expect(matches(copy, key({ key: "с", code: "KeyC", ctrlKey: true }))).toBe(true);
  // AZERTY: the key printed A sits where QWERTY has Q; the printed letter wins.
  expect(matches({ key: "a", mod: true }, key({ key: "a", code: "KeyQ", ctrlKey: true }))).toBe(true);
  expect(matches({ key: "q", mod: true }, key({ key: "a", code: "KeyQ", ctrlKey: true }))).toBe(false);
  expect(matches({ key: "/", mod: true }, key({ key: "?", code: "Slash", ctrlKey: true, shiftKey: true }))).toBe(false);
  expect(matches({ key: "/", mod: true }, key({ key: "÷", code: "Slash", ctrlKey: true }))).toBe(true);
  expect(matches({ key: " ", ctrl: true }, key({ key: " ", metaKey: true }))).toBe(false);
  let ran = 0;
  const r2 = new CommandRegistry();
  r2.register({ id: "x", label: "X", group: "G", shortcuts: [copy], contexts: ["grid"], run: () => { ran++; } });
  expect(r2.handle(key({ key: "c", ctrlKey: true, isComposing: true }), "grid", noop)).toBe(false);
  expect(r2.handle(key({ key: "c", ctrlKey: true, keyCode: 229 }), "grid", noop)).toBe(false);
  expect(r2.handle(key({ key: "c", ctrlKey: true }), "editor", noop)).toBe(false);
  expect(r2.handle(key({ key: "c", ctrlKey: true }), "grid", noop)).toBe(true);
  expect(ran).toBe(1);
  expect(() => r2.register({ id: "x", label: "X", group: "G", shortcuts: [], contexts: ["grid"] })).toThrow();
});

async function sheetPage(page: Page, name: string, inputs: { row: number; column: number; value: string }[]) {
  // Only Chromium has grantable clipboard permissions; Firefox and WebKit allow clipboard
  // access from user actions.
  if (page.context().browser()?.browserType().name() === "chromium") {
    await page.context().grantPermissions(["clipboard-read", "clipboard-write"], { origin: server.url });
  }
  // Remember what the app writes to the clipboard; `pasteClipboard` delivers it.
  await page.addInitScript(() => {
    const write = navigator.clipboard.writeText.bind(navigator.clipboard);
    navigator.clipboard.writeText = async (text: string) => {
      (window as unknown as { __copied: string }).__copied = text;
      return write(text);
    };
  });
  const wb = await server.createWorkbook(name), ws = await server.connect(wb);
  await ws.call("sheet_create", { name: "Data" });
  await ws.call("sheet_set", { sheet: 0, inputs });
  await page.goto(`${server.url}/w/${wb}`);
  await expect(page.locator(".cell-ref")).toHaveText("A1");
  await page.locator(".grid-host").focus();
  const content = async (r: number, c: number) => {
    const cell = (await ws.call("sheet_fetch", { sheet: 0, row_start: r, row_count: 1, column_count: c + 1 })).rows[0]?.[c];
    return cell ? cell[2] ?? cell[0] : "";
  };
  const display = async (r: number, c: number) => (await ws.call("sheet_fetch", { sheet: 0, row_start: r, row_count: 1, column_count: c + 1 })).rows[0]?.[c]?.[0] ?? "";
  return { ws, content, display };
}

/** The system clipboard's text, delivered as a paste event (what Mod+V does). */
async function pasteClipboard(page: Page) {
  // The text the app wrote: reading the system clipboard back is unreliable in headless
  // browsers (Linux has no clipboard owner; Firefox and WebKit need a user action).
  await page.evaluate(async () => {
    const text = (window as unknown as { __copied?: string }).__copied ?? "";
    const data = new DataTransfer();
    data.setData("text/plain", text);
    document.querySelector(".grid-host")!.dispatchEvent(new ClipboardEvent("paste", { clipboardData: data, bubbles: true, cancelable: true }));
  });
}

test("fill down and right translate formulas and undo as one step", async ({ page }) => {
  const { ws, content, display } = await sheetPage(page, "Fill", [
    { row: 0, column: 0, value: "1" }, { row: 0, column: 1, value: "=A1*2" }, { row: 4, column: 0, value: "x" },
  ]);
  await page.keyboard.press("Shift+ArrowRight"); await page.keyboard.press("Shift+ArrowDown"); await page.keyboard.press("Shift+ArrowDown"); await page.keyboard.press("Shift+ArrowDown");
  await expect(page.locator(".cell-ref")).toHaveText("A1:B4");
  await page.keyboard.press("ControlOrMeta+d");
  await expect.poll(() => content(3, 1)).toBe("=A4*2");
  expect(await display(3, 0)).toBe("1");
  expect(await display(3, 1)).toBe("2");
  await page.keyboard.press("ControlOrMeta+z");
  await expect.poll(() => content(3, 1)).toBe("");
  expect(await content(1, 0)).toBe("");
  expect(await content(0, 1)).toBe("=A1*2");
  // Fill right over B1:D1; a single selected cell fills from its neighbour.
  await page.keyboard.press("ControlOrMeta+Home"); await page.keyboard.press("ArrowRight");
  await page.keyboard.press("Shift+ArrowRight"); await page.keyboard.press("Shift+ArrowRight");
  await page.keyboard.press("ControlOrMeta+r");
  await expect.poll(() => content(0, 3)).toBe("=C1*2");
  expect(await display(0, 3)).toBe("8");
  await page.keyboard.press("ControlOrMeta+Home");
  for (let i = 0; i < 5; i++) await page.keyboard.press("ArrowDown");
  await page.keyboard.press("ControlOrMeta+d");
  await expect.poll(() => display(5, 0)).toBe("x");
  ws.close();
});

test("copy pastes formulas, cut moves cells and their references, paste special by keyboard", async ({ page, browserName }) => {
  // Playwright's Firefox drops data attached to script-made paste events and does not turn
  // Mod+V into a paste either, so this cannot be driven there; it is a manual check in
  // docs/ACCESSIBILITY_TESTING.md.
  test.skip(browserName === "firefox", "paste cannot be simulated in Playwright's Firefox");
  const { ws, content, display } = await sheetPage(page, "Clipboard", [
    { row: 0, column: 0, value: "5" }, { row: 1, column: 0, value: "=A1*2" }, { row: 0, column: 1, value: "=A1+A2" },
  ]);
  // Copy A1:A2 and paste at C3: the formula is translated.
  await page.keyboard.press("Shift+ArrowDown");
  await page.keyboard.press("ControlOrMeta+c");
  await expect(page.locator(".toast").last()).toContainText("Copied 2 cells");
  await page.keyboard.press("ControlOrMeta+Home");
  for (const k of ["ArrowDown", "ArrowDown", "ArrowRight", "ArrowRight"]) await page.keyboard.press(k);
  await pasteClipboard(page);
  await expect.poll(() => content(3, 2)).toBe("=C3*2");
  expect(await display(3, 2)).toBe("10");
  await expect(page.locator(".cell-ref")).toHaveText("C3:C4");

  // Paste special, keyboard only: values, transposed, at E1.
  await page.keyboard.press("ControlOrMeta+Home");
  for (let i = 0; i < 4; i++) await page.keyboard.press("ArrowRight");
  await page.keyboard.press("ControlOrMeta+Shift+v");
  const menu = page.getByRole("menu", { name: "Paste special" });
  await expect(menu).toBeVisible();
  const axe = await new AxeBuilder({ page }).include(".menu").analyze();
  expect(axe.violations.filter((v) => v.impact === "serious" || v.impact === "critical")).toEqual([]);
  // Keyboard only: the last item is "Values only, transposed".
  await page.keyboard.press("End");
  await expect(menu.getByRole("menuitem", { name: "Values only, transposed" })).toBeFocused();
  await page.keyboard.press("Enter");
  await expect.poll(() => content(0, 5)).toBe("10");
  expect(await content(0, 4)).toBe("5");

  // Cut A1:A2 and paste at D6: references from B1 follow the moved cells.
  await page.keyboard.press("ControlOrMeta+Home");
  await page.keyboard.press("Shift+ArrowDown");
  await page.keyboard.press("ControlOrMeta+x");
  await expect(page.locator(".toast").last()).toContainText("Cut.");
  await page.keyboard.press("ControlOrMeta+Home");
  for (const k of ["ArrowDown", "ArrowDown", "ArrowDown", "ArrowDown", "ArrowDown", "ArrowRight", "ArrowRight", "ArrowRight"]) await page.keyboard.press(k);
  await pasteClipboard(page);
  await expect.poll(() => content(0, 1)).toBe("=D6+D7");
  expect(await content(0, 0)).toBe("");
  expect(await content(6, 3)).toBe("=D6*2");
  expect(await display(0, 1)).toBe("15");
  await page.keyboard.press("ControlOrMeta+z");
  await expect.poll(() => content(0, 1)).toBe("=A1+A2");
  // Undoing the cut moves A1:A2 back, which the grid learns a moment after the server: wait
  // for the screen to show it, as a person would, before pasting by position. (A paste aimed
  // at the screen from before the undo is refused as "moved", by design.)
  await page.keyboard.press("ControlOrMeta+Home");
  await expect.poll(async () => {
    await page.evaluate(() => new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r))));
    const id = await page.locator(".grid-host").getAttribute("aria-activedescendant");
    return id ? (await page.locator(`#${id}`).textContent()) ?? "" : "";
  }).toContain("5");
  // Text from elsewhere still pastes as values.
  // Text copied in another app.
  await page.evaluate(async () => {
    (window as unknown as { __copied: string }).__copied = "7\t8";
    await navigator.clipboard.writeText("7\t8").catch(() => undefined);
  });
  await page.keyboard.press("ControlOrMeta+Home");
  for (let i = 0; i < 9; i++) await page.keyboard.press("ArrowDown");
  await pasteClipboard(page);
  await expect.poll(() => content(9, 1)).toBe("8");
  ws.close();
});

test("command palette, shortcut help, table context and other keyboard layouts", async ({ page }) => {
  const { ws, content } = await sheetPage(page, "Commands", [{ row: 0, column: 0, value: "1" }]);
  await page.keyboard.press("ControlOrMeta+k");
  const palette = page.getByRole("dialog", { name: "Commands" });
  await expect(palette).toBeVisible();
  await expect(palette.getByRole("combobox", { name: "Search commands" })).toBeFocused();
  let axe = await new AxeBuilder({ page }).include(".command-palette").analyze();
  expect(axe.violations.filter((v) => v.impact === "serious" || v.impact === "critical")).toEqual([]);
  await page.keyboard.type("fill d");
  await expect(palette.getByRole("option")).toHaveCount(1);
  await expect(palette.getByRole("option")).toContainText("Fill down");
  await page.keyboard.press("Escape");
  await expect(palette).toBeHidden();
  await page.keyboard.press("ArrowDown");
  await page.keyboard.press("ControlOrMeta+k");
  await page.keyboard.type("fill down"); await page.keyboard.press("Enter");
  await expect.poll(() => content(1, 0)).toBe("1");

  await page.keyboard.press("ControlOrMeta+/");
  const help = page.getByRole("dialog", { name: "Keyboard shortcuts" });
  await expect(help).toContainText("Fill right");
  await expect(help).toContainText("Replaces the browser's reload shortcut");
  axe = await new AxeBuilder({ page }).include(".shortcut-help").analyze();
  expect(axe.violations.filter((v) => v.impact === "serious" || v.impact === "critical")).toEqual([]);
  await page.keyboard.press("Escape");

  // A Cyrillic layout's C key still copies; composing text does not.
  const send = (init: KeyboardEventInit) => page.locator(".grid-host").evaluate((host, init) => {
    const e = new KeyboardEvent("keydown", { bubbles: true, cancelable: true, ...init });
    host.dispatchEvent(e); return e.defaultPrevented;
  }, init);
  expect(await send({ key: "с", code: "KeyC", ctrlKey: true })).toBe(true);
  await expect(page.locator(".toast").last()).toContainText("Copied");
  expect(await send({ key: "d", code: "KeyD", ctrlKey: true, isComposing: true })).toBe(false);

  // In a table, sheet-only commands explain why they are unavailable.
  await ws.call("create_table_from_query", { name: "t", sql: "SELECT 1 AS n" });
  await page.reload();
  await page.getByRole("tab", { name: /^▦?\s*t$/ }).click();
  await expect(page.getByRole("grid", { name: "Table t" })).toBeVisible();
  await page.locator(".grid-host").focus();
  expect(await send({ key: "d", code: "KeyD", ctrlKey: true })).toBe(true);
  await expect(page.locator(".toast").last()).toContainText("Fill down works in sheets");
  await page.keyboard.press("ControlOrMeta+k");
  await page.keyboard.type("paste special");
  await expect(palette.getByRole("option")).toHaveAttribute("aria-disabled", "true");
  await page.keyboard.press("Enter");
  await expect(palette.getByRole("status")).toContainText("Paste special works in sheets");
  ws.close();
});

test("a dialog keeps Escape and Tab when focus has left it, as after a click in Safari", async ({ page }) => {
  const wb = await server.createWorkbook("Dialog focus");
  await page.goto(`${server.url}/w/${wb}`);
  await menuCommand(page, "Help", "Formula help");
  const dialog = page.getByRole("dialog", { name: "Formula help" });
  await expect(dialog).toBeVisible();
  // Safari leaves focus on the page body after a click on a button.
  const blur = () => page.evaluate(() => (document.activeElement as HTMLElement | null)?.blur());
  await blur();
  await page.keyboard.press("Tab");
  expect(await page.evaluate(() => !!document.activeElement?.closest("[role=dialog]"))).toBe(true);
  await blur();
  await page.keyboard.press("Escape");
  await expect(dialog).toBeHidden();
});
