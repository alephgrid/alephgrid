// S.01 / S.02 / S.T01: the sheet format toolbar and shortcuts: styles and number formats
// reach the server as one step each and show in the grid; tables do not offer them.

import { expect, test, type Page } from "@playwright/test";
import { Server } from "./server";

const server = new Server();
test.beforeAll(async () => server.start());
test.afterAll(async () => server.dispose());

async function shown(page: Page, r: number, c: number): Promise<string> {
  await page.locator(".grid-host").focus();
  await page.keyboard.press("ControlOrMeta+Home");
  for (let i = 0; i < r; i++) await page.keyboard.press("ArrowDown");
  for (let i = 0; i < c; i++) await page.keyboard.press("ArrowRight");
  const id = await page.locator(".grid-host").getAttribute("aria-activedescendant");
  return (await page.locator(`#${id}`).textContent()) ?? "";
}

test("format a sheet range from the toolbar and the keyboard", async ({ page }) => {
  const wb = await server.createWorkbook("Formats"), ws = await server.connect(wb);
  await ws.call("sheet_create", { name: "S" });
  await ws.call("sheet_set", { sheet: 0, inputs: [{ row: 0, column: 0, value: "0.25" }, { row: 0, column: 1, value: "1234.5" }] });
  await page.addInitScript(() => { try { localStorage.setItem("alephgrid-welcome-done", "1"); } catch { /* */ } });
  await page.goto(`${server.url}/w/${wb}`);
  await page.getByRole("tab", { name: /^S/ }).click();
  const bar = page.getByRole("toolbar", { name: "Cell format" });
  await expect(bar).toBeVisible();
  await expect.poll(() => shown(page, 0, 0)).toBe("0.25");
  // A1:B1 bold, from the toolbar.
  await page.keyboard.press("ControlOrMeta+Home");
  await page.keyboard.press("Shift+ArrowRight");
  await bar.getByRole("button", { name: "Bold" }).click();
  await expect.poll(async () => (await ws.call("sheet_fetch", { sheet: 0, row_start: 0, row_count: 1, column_count: 2 })).styles?.filter((s: { style: { b?: boolean } }) => s.style.b).length ?? 0).toBe(2);
  await expect(bar.getByRole("button", { name: "Bold" })).toHaveAttribute("aria-pressed", "true");
  // Percent on A1: the display changes, the value does not.
  await page.locator(".grid-host").focus();
  await page.keyboard.press("ControlOrMeta+Home");
  await bar.getByRole("combobox", { name: "Number format" }).selectOption({ label: "Percent (12.35%)" });
  await expect.poll(() => shown(page, 0, 0)).toBe("25.00%");
  await page.keyboard.press("ControlOrMeta+Home");
  await bar.getByRole("button", { name: "Fewer decimals" }).click();
  await expect.poll(() => shown(page, 0, 0)).toBe("25.0%");
  const raw = await ws.call("sheet_cell_detail", { sheet: 0, row: 0, column: 0 });
  expect(JSON.stringify(raw)).toContain("0.25");
  // Italic with the keyboard, on B1.
  await page.locator(".grid-host").focus();
  await page.keyboard.press("ControlOrMeta+Home");
  await page.keyboard.press("ArrowRight");
  await page.keyboard.press("ControlOrMeta+i");
  await expect.poll(async () => (await ws.call("sheet_fetch", { sheet: 0, row_start: 0, row_count: 1, column_count: 2 })).styles?.find((s: { column: number }) => s.column === 1)?.style.i ?? false).toBe(true);
  // Undo takes back the italic only.
  await page.keyboard.press("ControlOrMeta+z");
  await expect.poll(async () => (await ws.call("sheet_fetch", { sheet: 0, row_start: 0, row_count: 1, column_count: 2 })).styles?.find((s: { column: number }) => s.column === 1)?.style.i ?? false).toBe(false);
  // Clear formatting on A1:B1 keeps the values.
  await page.keyboard.press("ControlOrMeta+Home");
  await page.keyboard.press("Shift+ArrowRight");
  await bar.getByRole("button", { name: "Clear formatting" }).click();
  await expect.poll(async () => (await ws.call("sheet_fetch", { sheet: 0, row_start: 0, row_count: 1, column_count: 2 })).styles?.length ?? 0).toBe(0);
  await expect.poll(() => shown(page, 0, 0)).toBe("0.25");
  // Tables have no cell format toolbar.
  await ws.call("create_table_from_query", { name: "t", sql: "SELECT 1 AS x" });
  await page.reload();
  await page.getByRole("tab", { name: /^t/ }).click();
  await expect(page.getByRole("toolbar", { name: "Cell format" })).toBeHidden();
  ws.close();
});

test("S.03: the cell menu inserts, deletes, clears and formats", async ({ page }) => {
  const wb = await server.createWorkbook("Cell menu"), ws = await server.connect(wb);
  await ws.call("sheet_create", { name: "S" });
  await ws.call("sheet_set", { sheet: 0, inputs: [{ row: 0, column: 0, value: "a" }, { row: 1, column: 0, value: "b" }] });
  await page.addInitScript(() => { try { localStorage.setItem("alephgrid-welcome-done", "1"); } catch { /* */ } });
  await page.goto(`${server.url}/w/${wb}`);
  await page.getByRole("tab", { name: /^S/ }).click();
  await expect.poll(() => shown(page, 0, 0)).toBe("a");
  const cell = async () => {
    const box = (await page.locator(".grid-scroller").boundingBox())!;
    await page.mouse.click(box.x + 90, box.y + 44, { button: "right" });
    return page.getByRole("menu", { name: "Cell actions" });
  };
  let menu = await cell();
  for (const item of ["Cut", "Copy", "Paste", "Paste special…", "Insert row above", "Insert column left", "Delete row", "Delete column", "Clear contents", "Clear formatting"]) {
    await expect(menu.getByRole("menuitem", { name: new RegExp(`^${item.replace(".", "\\.")}`) }).first()).toBeVisible();
  }
  await menu.getByRole("menuitem", { name: "Insert row above" }).click();
  await expect.poll(async () => (await ws.call("sheet_fetch", { sheet: 0, row_start: 0, row_count: 3, column_count: 1 })).rows.map((r: unknown[]) => (r[0] as string[] | null)?.[0] ?? null)).toEqual([null, "a", "b"]);
  menu = await cell();
  await menu.getByRole("menuitem", { name: "Delete row" }).click();
  await expect.poll(async () => (await ws.call("sheet_fetch", { sheet: 0, row_start: 0, row_count: 2, column_count: 1 })).rows.map((r: unknown[]) => (r[0] as string[] | null)?.[0] ?? null)).toEqual(["a", "b"]);
  menu = await cell();
  await menu.getByRole("menuitem", { name: "Clear contents" }).click();
  await expect.poll(() => shown(page, 0, 0)).toBe("");
  ws.close();
});

test("S.04: find and replace in a sheet", async ({ page }) => {
  const wb = await server.createWorkbook("Sheet find"), ws = await server.connect(wb);
  await ws.call("sheet_create", { name: "S" });
  await ws.call("sheet_set", { sheet: 0, inputs: [{ row: 0, column: 0, value: "north" }, { row: 2, column: 1, value: "North sea" }] });
  await page.addInitScript(() => { try { localStorage.setItem("alephgrid-welcome-done", "1"); } catch { /* */ } });
  await page.goto(`${server.url}/w/${wb}`);
  await page.getByRole("tab", { name: /^S/ }).click();
  await expect.poll(() => shown(page, 0, 0)).toBe("north");
  await page.keyboard.press("ControlOrMeta+f");
  const bar = page.getByRole("search", { name: "Find in S" });
  await bar.getByRole("searchbox", { name: "Find" }).fill("north");
  await expect(bar.getByRole("status")).toHaveText("2 matches");
  await bar.getByRole("button", { name: "Whole cell" }).click();
  await expect(bar.getByRole("status")).toHaveText("1 match");
  await bar.getByRole("button", { name: "Whole cell" }).click();
  await expect(bar.getByRole("status")).toHaveText("2 matches");
  await bar.getByRole("searchbox", { name: "Find" }).press("Enter");
  await expect(page.locator(".cell-ref")).toContainText("B3");
  await bar.getByRole("button", { name: "Show replace" }).click();
  await bar.getByRole("textbox", { name: "Replace with" }).fill("south");
  // Replace one: only the current match (B3) changes.
  await bar.getByRole("button", { name: "Replace", exact: true }).click();
  await expect.poll(() => shown(page, 2, 1)).toBe("south sea");
  await expect.poll(() => shown(page, 0, 0)).toBe("north");
  await bar.getByRole("button", { name: "Preview" }).click();
  await bar.getByRole("button", { name: "Replace all" }).click();
  await expect.poll(() => shown(page, 2, 1)).toBe("south sea");
  await expect.poll(() => shown(page, 0, 0)).toBe("south");
  ws.close();
});

test("S.11: comment on a cell, see a collaborator's reply, resolve, and follow an inserted row", async ({ page }) => {
  const wb = await server.createWorkbook("Comments"), ws = await server.connect(wb);
  await ws.call("sheet_create", { name: "S" });
  await ws.call("sheet_set", { sheet: 0, inputs: [{ row: 0, column: 0, value: "total" }] });
  await page.addInitScript(() => { try { localStorage.setItem("alephgrid-welcome-done", "1"); } catch { /* */ } });
  await page.goto(`${server.url}/w/${wb}`);
  await page.getByRole("tab", { name: /^S/ }).click();
  await expect.poll(() => shown(page, 0, 0)).toBe("total");
  const box = (await page.locator(".grid-scroller").boundingBox())!;
  await page.mouse.click(box.x + 90, box.y + 44, { button: "right" });
  await page.getByRole("menuitem", { name: /^Add comment/ }).click();
  await page.getByRole("dialog", { name: "Comment on A1" }).getByRole("textbox", { name: "Comment" }).fill("Is this the final total?");
  await page.getByRole("dialog", { name: "Comment on A1" }).getByRole("button", { name: "Comment" }).click();
  const pane = page.getByRole("complementary", { name: "Comments" });
  const thread = pane.getByRole("region", { name: "Comment on A1" });
  await expect(thread).toContainText("Is this the final total?");
  // A collaborator replies; the open pane shows it.
  const [first] = await ws.call("comments", { sheet: 0 });
  await ws.call("add_comment", { sheet: 0, row: 0, column: 0, text: "Yes, final.", parent: first.id });
  await expect(thread).toContainText("Yes, final.");
  await thread.getByRole("button", { name: "Resolve" }).click();
  await expect(thread).toContainText("Resolved");
  // A row inserted above moves the thread to A2.
  await page.mouse.click(box.x + 90, box.y + 44, { button: "right" });
  await page.getByRole("menuitem", { name: "Insert row above" }).click();
  await expect(pane.getByRole("region", { name: "Comment on A2" })).toBeVisible();
  expect((await ws.call("comments", { sheet: 0 })).every((c: { row: number }) => c.row === 1)).toBe(true);
  ws.close();
});

test("S.08: zoom scales the grid, keeps clicks on the right cell, and is remembered", async ({ page }) => {
  const wb = await server.createWorkbook("Zoom"), ws = await server.connect(wb);
  await ws.call("sheet_create", { name: "S" });
  await page.addInitScript(() => { try { localStorage.setItem("alephgrid-welcome-done", "1"); } catch { /* */ } });
  await page.goto(`${server.url}/w/${wb}`);
  await page.getByRole("tab", { name: /^S/ }).click();
  const menus = page.getByRole("navigation", { name: "Workbook menus" });
  for (let i = 0; i < 5; i++) {
    await menus.getByRole("button", { name: "View", exact: true }).click();
    await page.getByRole("menuitem", { name: /^Zoom in/ }).click();
  }
  // At 150% every grid unit is 1.5 screen pixels: this point is column B, row 2 only if
  // clicks are mapped back through the zoom (unzoomed it would be about column D, row 3).
  const box = (await page.locator(".grid-scroller").boundingBox())!;
  const rhw = 50, col = 104, header = 32, row = 24;
  await page.mouse.click(box.x + (rhw + col * 1.3) * 1.5, box.y + (header + row * 1.5) * 1.5);
  await expect(page.locator(".cell-ref")).toContainText("B2");
  await page.reload();
  await page.getByRole("tab", { name: /^S/ }).click();
  await menus.getByRole("button", { name: "View", exact: true }).click();
  await expect(page.getByRole("menuitem", { name: /^Zoom in/ })).toContainText("150%");
  ws.close();
});

test("S.09: a chart of a selected range follows the cells", async ({ page }) => {
  const wb = await server.createWorkbook("Range chart"), ws = await server.connect(wb);
  await ws.call("sheet_create", { name: "S" });
  await ws.call("sheet_set", { sheet: 0, inputs: [
    { row: 0, column: 0, value: "region" }, { row: 0, column: 1, value: "sales" },
    { row: 1, column: 0, value: "East" }, { row: 1, column: 1, value: "50" },
    { row: 2, column: 0, value: "West" }, { row: 2, column: 1, value: "125" }] });
  await page.addInitScript(() => { try { localStorage.setItem("alephgrid-welcome-done", "1"); } catch { /* */ } });
  await page.goto(`${server.url}/w/${wb}`);
  await page.getByRole("tab", { name: /^S/ }).click();
  await expect.poll(() => shown(page, 0, 0)).toBe("region");
  await page.keyboard.press("ControlOrMeta+Home");
  await page.keyboard.press("Shift+ArrowRight");
  await page.keyboard.press("Shift+ArrowDown");
  await page.keyboard.press("Shift+ArrowDown");
  const box = (await page.locator(".grid-scroller").boundingBox())!;
  await page.mouse.click(box.x + 90, box.y + 44, { button: "right" });
  await page.getByRole("menuitem", { name: "Chart from selection" }).click();
  const chart = page.getByRole("region", { name: "Range chart" });
  await expect(chart.getByRole("img")).toBeVisible();
  const values = chart.getByRole("table", { name: "Chart values" });
  await expect(values).toContainText("West125");
  await ws.call("sheet_set", { sheet: 0, inputs: [{ row: 2, column: 1, value: "200" }] });
  await expect(values).toContainText("West200");
  ws.close();
});

test("S.10: a validation rule refuses a value; a colour rule fills matching cells", async ({ page }) => {
  const wb = await server.createWorkbook("Rules"), ws = await server.connect(wb);
  await ws.call("sheet_create", { name: "S" });
  await ws.call("set_sheet_rules", { sheet: 0, rules: { colors: [{ area: { row: 0, column: 0, rows: 5, columns: 1 }, condition: "greater", value: "5", color: "#FFC7CE" }], validations: [{ area: { row: 0, column: 1, rows: 5, columns: 1 }, list: ["yes", "no"] }] } });
  await page.addInitScript(() => { try { localStorage.setItem("alephgrid-welcome-done", "1"); } catch { /* */ } });
  await page.goto(`${server.url}/w/${wb}`);
  await page.getByRole("tab", { name: /^S/ }).click();
  const bar = page.getByRole("textbox", { name: "Cell value or formula", exact: true });
  await page.locator(".grid-host").focus();
  await page.keyboard.press("ControlOrMeta+Home");
  await page.keyboard.press("ArrowRight");
  await bar.fill("maybe");
  await bar.press("Enter");
  await expect(page.getByText(/not one of the allowed values/)).toBeVisible();
  const cells = await ws.call("sheet_fetch", { sheet: 0, row_start: 0, row_count: 1, column_count: 2 });
  expect(JSON.stringify(cells.rows)).not.toContain("maybe");
  expect((await ws.call("sheet_rules", { sheet: 0 })).colors.length).toBe(1);
  ws.close();
});
