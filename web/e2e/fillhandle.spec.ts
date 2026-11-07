// F.04a–c: the fill handle at the selection's corner, the copied-range outline and Ctrl/Cmd+A
// that selects the filled block first. Values are checked on the server against numbers
// worked out by hand, not against the grid.

import { expect, test, type Page } from "@playwright/test";
import { Server } from "./server";

const server = new Server();
test.beforeAll(async () => server.start());
test.afterAll(async () => server.dispose());

const HEADER_H = 32, ROW_H = 24;

/** The fill handle of the current selection, found where the pointer turns into a crosshair
 *  along the bottom edge of row `r`. */
async function handleAt(page: Page, r: number): Promise<{ x: number; y: number }> {
  const box = (await page.locator(".grid-scroller").boundingBox())!;
  const y = box.y + HEADER_H + ROW_H * (r + 1);
  for (let x = box.x + 40; x < box.x + box.width - 20; x += 5) {
    await page.mouse.move(x, y);
    const cursor = await page.locator(".grid-scroller").evaluate((e) => (e as HTMLElement).style.cursor);
    if (cursor === "crosshair") return { x, y };
  }
  throw new Error("no fill handle on that row");
}

async function column(ws: Awaited<ReturnType<Server["connect"]>>, c: number, rows: number): Promise<string[]> {
  const block = await ws.call("sheet_fetch", { sheet: 0, row_start: 0, row_count: rows, column_count: c + 1 });
  return Array.from({ length: rows }, (_, r) => {
    const cell = block.rows?.[r]?.[c];
    return cell == null ? "" : String(Array.isArray(cell) ? cell[0] : cell);
  });
}

async function open(page: Page, wb: string): Promise<void> {
  await page.addInitScript(() => { try { localStorage.setItem("alephgrid-welcome-done", "1"); } catch { /* */ } });
  await page.goto(`${server.url}/w/${wb}`);
  await page.getByRole("tab", { name: /^S/ }).click();
  await expect(page.locator(".cell-ref")).toHaveText("A1");
}

test("dragging the fill handle continues a series and counts on text", async ({ page }) => {
  const wb = await server.createWorkbook("Fill"), ws = await server.connect(wb);
  await ws.call("sheet_create", { name: "S" });
  await ws.call("sheet_set", { sheet: 0, inputs: [
    { row: 0, column: 0, value: "2" }, { row: 1, column: 0, value: "4" },
    { row: 0, column: 1, value: "Week 1" },
  ] });
  await open(page, wb);
  const grid = page.locator(".grid-host");
  // A1:A2 selected; drag the handle three rows down: 2, 4 → 6, 8, 10.
  await grid.focus();
  await page.keyboard.press("Shift+ArrowDown");
  const h = await handleAt(page, 1);
  await page.mouse.down();
  await page.mouse.move(h.x, h.y + ROW_H * 3, { steps: 6 });
  await page.mouse.up();
  await expect.poll(() => column(ws, 0, 5)).toEqual(["2", "4", "6", "8", "10"]);
  await expect(page.locator(".cell-ref")).toHaveText("A1:A5");
  // One undo takes the whole drag back.
  await page.keyboard.press("ControlOrMeta+z");
  await expect.poll(() => column(ws, 0, 5)).toEqual(["2", "4", "", "", ""]);

  // B1 alone: "Week 1" counts on.
  await grid.focus();
  await page.keyboard.press("ControlOrMeta+Home");
  await page.keyboard.press("ArrowRight");
  const b = await handleAt(page, 0);
  await page.mouse.down();
  await page.mouse.move(b.x, b.y + ROW_H * 2, { steps: 4 });
  await page.mouse.up();
  await expect.poll(() => column(ws, 1, 3)).toEqual(["Week 1", "Week 2", "Week 3"]);
  ws.close();
});

test("Ctrl/Cmd+A selects the filled block, then everything; copy outlines its range", async ({ page }) => {
  const wb = await server.createWorkbook("Select"), ws = await server.connect(wb);
  await ws.call("sheet_create", { name: "S" });
  await ws.call("sheet_set", { sheet: 0, inputs: [
    { row: 1, column: 1, value: "a" }, { row: 1, column: 2, value: "b" }, { row: 2, column: 1, value: "c" }, { row: 2, column: 2, value: "d" },
    { row: 6, column: 5, value: "far" },
  ] });
  if (page.context().browser()?.browserType().name() === "chromium") {
    await page.context().grantPermissions(["clipboard-read", "clipboard-write"], { origin: server.url });
  }
  // A browser that does not let the page read the clipboard from a menu; copies are recorded
  // instead of reaching the shared system clipboard.
  await page.addInitScript(() => {
    navigator.clipboard.writeText = async () => undefined;
    navigator.clipboard.write = async () => undefined;
    navigator.clipboard.read = async () => { throw new DOMException("Read permission denied.", "NotAllowedError"); };
    navigator.clipboard.readText = async () => { throw new DOMException("Read permission denied.", "NotAllowedError"); };
  });
  await open(page, wb);
  const grid = page.locator(".grid-host");
  await grid.focus();
  await page.keyboard.press("ArrowDown"); await page.keyboard.press("ArrowRight"); // B2
  await expect(page.locator(".cell-ref")).toHaveText("B2");
  await page.keyboard.press("ControlOrMeta+a");
  await expect(page.locator(".cell-ref")).toHaveText("B2:C3");
  // Copy, then Escape: the copied state is gone, so paste special has no cells of this
  // workbook to paste and, unable to read the clipboard, waits for Mod+V instead.
  await page.keyboard.press("ControlOrMeta+c");
  await expect(page.locator(".toast").last()).toContainText("Copied 4 cells");
  await page.keyboard.press("Escape");
  await page.keyboard.press("ControlOrMeta+Shift+v");
  await page.getByRole("menu", { name: "Paste special" }).getByRole("menuitem", { name: "Values only", exact: true }).click();
  await expect(page.locator(".grid-note")).toContainText(/Press (⌘V|Ctrl\+V) to paste values only/);
  await grid.focus();
  await page.keyboard.press("Escape");
  await expect(page.locator(".grid-note")).toHaveCount(0);
  expect((await ws.call("sheet_fetch", { sheet: 0, row_start: 1, row_count: 1, column_count: 3 })).rows[0][1][0]).toBe("a");
  // Escape kept B2 active: Ctrl/Cmd+A twice selects the block, then everything.
  await page.keyboard.press("ControlOrMeta+a");
  await expect(page.locator(".cell-ref")).toHaveText("B2:C3");
  await page.keyboard.press("ControlOrMeta+a");
  await expect(page.locator(".cell-ref")).not.toHaveText("B2:C3");
  await expect(page.locator(".cell-ref")).toHaveText(/^A1:/);
  ws.close();
});
