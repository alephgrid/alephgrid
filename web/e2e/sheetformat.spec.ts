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
  // The screen-reader cell follows the active cell on the next frame.
  await page.evaluate(() => new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r))));
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
  // Rows are picked as the page shows them: wait until it shows the new row (a change
  // picked from the rows as they were is refused, since they have moved).
  await expect.poll(() => shown(page, 0, 0)).toBe("");
  menu = await cell();
  await menu.getByRole("menuitem", { name: "Delete row" }).click();
  await expect.poll(async () => (await ws.call("sheet_fetch", { sheet: 0, row_start: 0, row_count: 2, column_count: 1 })).rows.map((r: unknown[]) => (r[0] as string[] | null)?.[0] ?? null)).toEqual(["a", "b"]);
  await expect.poll(() => shown(page, 0, 0)).toBe("a");
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

test("S.09: a chart placed on the sheet follows the cells, moves, changes type and is kept", async ({ page }) => {
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
  await page.getByRole("menuitem", { name: "Insert chart" }).click();
  // U.04b: Insert chart shows an unsaved preview; Escape leaves the sheet unchanged.
  const preview = page.getByRole("region", { name: "Chart preview" });
  await expect(preview.getByRole("table", { name: "Chart values" })).toContainText("West125");
  const count = async () => (await ws.call("sheet_charts", { sheet: 0 })).length;
  expect(await count()).toBe(0);
  await page.keyboard.press("Escape");
  await expect(preview).toHaveCount(0);
  expect(await count()).toBe(0);
  // Again, choose a type in the preview, then Create chart stores it.
  await page.mouse.click(box.x + 90, box.y + 44, { button: "right" });
  await page.getByRole("menuitem", { name: "Insert chart" }).click();
  await preview.getByRole("combobox", { name: "Chart type" }).selectOption("bar");
  expect(await count()).toBe(0);
  await preview.getByRole("button", { name: "Create chart" }).click();
  await expect(preview).toHaveCount(0);
  const chart = page.getByRole("region", { name: "Chart of A1:B3" });
  await expect(chart.getByRole("img")).toBeVisible();
  const values = chart.getByRole("table", { name: "Chart values" });
  await expect(values).toContainText("West125");
  const stored = async () => (await ws.call("sheet_charts", { sheet: 0 }))[0];
  await expect.poll(async () => (await stored())?.kind).toBe("bar");
  // Placed right of the data (column D), kept with the workbook.
  await expect.poll(async () => (await stored())?.column).toBe(3);
  // The chart follows a collaborator's edit.
  await ws.call("sheet_set", { sheet: 0, inputs: [{ row: 2, column: 1, value: "200" }] });
  await expect(values).toContainText("West200");
  // L.05c (b): the chart reads computed numbers, so a currency-formatted formula still charts.
  await ws.call("sheet_set", { sheet: 0, inputs: [{ row: 2, column: 1, value: "=B2*5" }] });
  await ws.call("sheet_style", { sheet: 0, area: { row: 1, column: 1, rows: 2, columns: 1 }, style: { num_fmt: "$#,##0.00" } });
  await expect.poll(() => shown(page, 2, 1)).toBe("$250.00");
  await expect(values).toContainText("East50");
  await expect(values).toContainText("West250");
  // Change its type, then move it down by dragging its title bar.
  await chart.getByRole("combobox", { name: "Chart type" }).selectOption("line");
  await expect.poll(async () => (await stored())?.kind).toBe("line");
  const bar = chart.locator(".sheet-chart-bar");
  const b = (await bar.boundingBox())!;
  await page.mouse.move(b.x + 20, b.y + b.height / 2);
  await page.mouse.down();
  await page.mouse.move(b.x + 20, b.y + b.height / 2 + 24 * 3, { steps: 5 });
  await page.mouse.up();
  await expect.poll(async () => (await stored())?.row).toBe(3);
  // Resize from the corner.
  const g = (await chart.locator(".sheet-chart-grip").boundingBox())!;
  await page.mouse.move(g.x + 7, g.y + 7);
  await page.mouse.down();
  await page.mouse.move(g.x + 107, g.y + 47, { steps: 5 });
  await page.mouse.up();
  await expect.poll(async () => { const c = await stored(); return [c?.width, c?.height]; }).toEqual([580, 340]);
  await page.reload();
  await page.getByRole("tab", { name: /^S/ }).click();
  await expect(page.getByRole("region", { name: "Chart of A1:B3" }).getByRole("table", { name: "Chart values" })).toContainText("West250");
  // Deleting removes it from the workbook; Undo in the notice puts it back.
  await page.getByRole("region", { name: "Chart of A1:B3" }).getByRole("button", { name: "Delete chart" }).click();
  await expect.poll(count).toBe(0);
  await page.locator(".toast").getByRole("button", { name: "Undo" }).click();
  await expect.poll(count).toBe(1);
  await expect(page.getByRole("region", { name: "Chart of A1:B3" }).getByRole("table", { name: "Chart values" })).toContainText("West250");
  await page.getByRole("region", { name: "Chart of A1:B3" }).getByRole("button", { name: "Delete chart" }).click();
  await expect.poll(count).toBe(0);
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
  // S.T02: the colour rule, checked on the drawn grid itself: A1 (10 > 5) is filled
  // #FFC7CE, A2 (2) is not. Pixels are read just inside each cell's left edge.
  await ws.call("sheet_set", { sheet: 0, inputs: [{ row: 0, column: 0, value: "10" }, { row: 1, column: 0, value: "2" }] });
  const pixel = (x: number, y: number) => page.evaluate(([px, py]) => {
    const c = document.querySelector<HTMLCanvasElement>(".grid-canvas")!;
    const k = c.width / c.getBoundingClientRect().width;
    return [...c.getContext("2d")!.getImageData(Math.round(px * k), Math.round(py * k), 1, 1).data.slice(0, 3)];
  }, [x, y]);
  await expect.poll(() => pixel(54 + 3, 32 + 4)).toEqual([255, 199, 206]);
  expect(await pixel(54 + 3, 32 + 24 + 4)).not.toEqual([255, 199, 206]);
  // The rules list shows both rules; deleting one removes it from the workbook.
  const menus = page.getByRole("navigation", { name: "Workbook menus" });
  await menus.getByRole("button", { name: "Format", exact: true }).click();
  await page.getByRole("menuitem", { name: "Rules on this sheet…" }).click();
  const dialog = page.getByRole("dialog", { name: "Rules on S" });
  await expect(dialog.getByRole("listitem")).toHaveCount(2);
  await expect(dialog).toContainText("A1:A5 greater than 5 → fill #FFC7CE");
  await expect(dialog).toContainText("B1:B5 allows yes, no");
  await dialog.getByRole("button", { name: "Delete rule on A1:A5" }).click();
  await expect(dialog.getByRole("listitem")).toHaveCount(1);
  await expect.poll(async () => (await ws.call("sheet_rules", { sheet: 0 })).colors.length).toBe(0);
  await dialog.getByRole("button", { name: "Done" }).click();
  ws.close();
});

test("S.05: rows and columns resize by dragging, hide and unhide, freeze, and are kept with the sheet", async ({ page }) => {
  const wb = await server.createWorkbook("Layout"), ws = await server.connect(wb);
  await ws.call("sheet_create", { name: "S" });
  await ws.call("sheet_set", { sheet: 0, inputs: [0, 1, 2, 3].map((r) => ({ row: r, column: 0, value: `r${r + 1}` })) });
  await page.addInitScript(() => { try { localStorage.setItem("alephgrid-welcome-done", "1"); } catch { /* */ } });
  await page.goto(`${server.url}/w/${wb}`);
  await page.getByRole("tab", { name: /^S/ }).click();
  await expect.poll(() => shown(page, 0, 0)).toBe("r1");
  const box = (await page.locator(".grid-scroller").boundingBox())!;
  // 1000 shown rows: the row header is 54 px wide; columns are 104, rows 24, the header 32.
  const rhw = 54, header = 32, row = 24, col = 104;
  // Drag the bottom edge of row 1 down by 24 px and the right edge of column A by 46 px.
  await page.mouse.move(box.x + 20, box.y + header + row);
  await page.mouse.down();
  await page.mouse.move(box.x + 20, box.y + header + row + 24, { steps: 4 });
  await page.mouse.up();
  await page.mouse.move(box.x + rhw + col, box.y + 16);
  await page.mouse.down();
  await page.mouse.move(box.x + rhw + col + 46, box.y + 16, { steps: 4 });
  await page.mouse.up();
  const line = async (axis: "rows" | "columns", index: number) => ((await ws.call("sheet_layout", { sheet: 0 }))[axis] as { index: number; size: number; hidden: boolean }[]).find((l) => l.index === index);
  await expect.poll(() => line("rows", 0)).toEqual({ index: 0, size: 48, hidden: false });
  await expect.poll(() => line("columns", 0)).toEqual({ index: 0, size: 150, hidden: false });
  // Row 2 is now 48 px lower: a click in it lands on A2.
  await page.mouse.click(box.x + rhw + 30, box.y + header + 48 + 12);
  await expect(page.locator(".cell-ref")).toContainText("A2");
  // Hide row 2 from the cell menu: the keyboard skips it.
  await page.mouse.click(box.x + rhw + 30, box.y + header + 48 + 12, { button: "right" });
  await page.getByRole("menuitem", { name: "Hide row", exact: true }).click();
  await expect.poll(() => line("rows", 1)).toEqual({ index: 1, size: 24, hidden: true });
  await expect.poll(() => shown(page, 1, 0)).toBe("r3");
  // Freeze two rows from the View menu.
  const menus = page.getByRole("navigation", { name: "Workbook menus" });
  await page.keyboard.press("ControlOrMeta+Home");
  await page.keyboard.press("ArrowDown");
  await menus.getByRole("button", { name: "View", exact: true }).click();
  await page.getByRole("menuitem", { name: /^Freeze rows 1–3/ }).click();
  await expect.poll(async () => (await ws.call("sheet_layout", { sheet: 0 })).frozen_rows).toBe(3);
  // Everything is kept with the workbook: reload and the sizes are back.
  await page.reload();
  await page.getByRole("tab", { name: /^S/ }).click();
  await expect.poll(() => shown(page, 0, 0)).toBe("r1");
  await page.mouse.click(box.x + rhw + col + 30, box.y + header + 10);
  await expect(page.locator(".cell-ref")).toContainText("A1");
  // Unhide around the selection brings row 2 back.
  await page.locator(".grid-host").focus();
  await page.keyboard.press("ControlOrMeta+Home");
  await page.keyboard.press("Shift+ArrowDown");
  await page.mouse.click(box.x + rhw + 30, box.y + header + 10, { button: "right" });
  await page.getByRole("menuitem", { name: /^Unhide rows/ }).click();
  await expect.poll(async () => (await ws.call("sheet_layout", { sheet: 0 })).rows.some((r: { hidden: boolean }) => r.hidden)).toBe(false);
  ws.close();
});

test("S.06: a range filter hides the rows whose value is not kept, and clears", async ({ page }) => {
  const wb = await server.createWorkbook("Range filter"), ws = await server.connect(wb);
  await ws.call("sheet_create", { name: "S" });
  const fruit = ["Fruit", "apple", "pear", "apple", "fig", "pear"];
  await ws.call("sheet_set", { sheet: 0, inputs: fruit.map((value, row) => ({ row, column: 0, value })) });
  await page.addInitScript(() => { try { localStorage.setItem("alephgrid-welcome-done", "1"); } catch { /* */ } });
  await page.goto(`${server.url}/w/${wb}`);
  await page.getByRole("tab", { name: /^S/ }).click();
  await expect.poll(() => shown(page, 0, 0)).toBe("Fruit");
  await page.keyboard.press("ControlOrMeta+Home");
  for (let i = 0; i < 5; i++) await page.keyboard.press("Shift+ArrowDown");
  const box = (await page.locator(".grid-scroller").boundingBox())!;
  await page.mouse.click(box.x + 90, box.y + 44, { button: "right" });
  await page.getByRole("menuitem", { name: /^Filter by column A/ }).click();
  const pear = page.locator(".fp-item", { hasText: "pear" }).locator("input");
  await expect(pear).toBeChecked();
  await pear.uncheck();
  await page.locator(".fp-item", { hasText: "fig" }).locator("input").uncheck();
  await page.getByRole("button", { name: "Apply" }).click();
  await expect(page.getByText("3 rows hidden by the filter")).toBeVisible();
  const hidden = async () => (await ws.call("sheet_layout", { sheet: 0 })).rows.filter((r: { hidden: boolean }) => r.hidden).map((r: { index: number }) => r.index);
  await expect.poll(hidden).toEqual([2, 4, 5]);
  // Values are kept: the hidden cells are still there.
  const cells = await ws.call("sheet_fetch", { sheet: 0, row_start: 2, row_count: 1, column_count: 1 });
  expect(JSON.stringify(cells.rows)).toContain("pear");
  // The keyboard walks only the kept rows.
  await expect.poll(() => shown(page, 2, 0)).toBe("apple");
  await page.mouse.click(box.x + 90, box.y + 44, { button: "right" });
  await page.getByRole("menuitem", { name: /^Filter by column A/ }).click();
  await expect(page.locator(".fp-item", { hasText: "pear" }).locator("input")).not.toBeChecked();
  await page.getByRole("button", { name: "Clear", exact: true }).click();
  await expect.poll(hidden).toEqual([]);
  ws.close();
});

test("S.05: merge and centre a range; a click inside selects the merged cell", async ({ page }) => {
  const wb = await server.createWorkbook("Merge"), ws = await server.connect(wb);
  await ws.call("sheet_create", { name: "S" });
  await ws.call("sheet_set", { sheet: 0, inputs: [{ row: 0, column: 0, value: "Title" }] });
  await page.addInitScript(() => { try { localStorage.setItem("alephgrid-welcome-done", "1"); } catch { /* */ } });
  await page.goto(`${server.url}/w/${wb}`);
  await page.getByRole("tab", { name: /^S/ }).click();
  await expect.poll(() => shown(page, 0, 0)).toBe("Title");
  await page.keyboard.press("ControlOrMeta+Home");
  await page.keyboard.press("Shift+ArrowRight");
  await page.keyboard.press("Shift+ArrowRight");
  const menus = page.getByRole("navigation", { name: "Workbook menus" });
  await menus.getByRole("button", { name: "Format", exact: true }).click();
  await page.getByRole("menuitem", { name: "Merge and centre" }).click();
  await expect.poll(async () => (await ws.call("sheet_layout", { sheet: 0 })).merged).toEqual([{ row: 0, column: 0, rows: 1, columns: 3 }]);
  // The page knows the merge once the keyboard leaves A1 from the merge's far edge.
  await expect.poll(async () => {
    await page.locator(".grid-host").focus();
    await page.keyboard.press("ControlOrMeta+Home");
    await page.keyboard.press("ArrowRight");
    return page.locator(".cell-ref").textContent();
  }).toContain("D1");
  const box = (await page.locator(".grid-scroller").boundingBox())!;
  await page.mouse.click(box.x + 54 + 104 + 50, box.y + 32 + 12);
  await expect(page.locator(".cell-ref")).toContainText("A1");
  // The keyboard leaves the merged cell from its far edge.
  await page.keyboard.press("ArrowRight");
  await expect(page.locator(".cell-ref")).toContainText("D1");
  await menus.getByRole("button", { name: "Format", exact: true }).click();
  await page.keyboard.press("Escape");
  ws.close();
});

test("S.04: find and replace across all sheets", async ({ page }) => {
  const wb = await server.createWorkbook("All sheets find"), ws = await server.connect(wb);
  await ws.call("sheet_create", { name: "One" });
  await ws.call("sheet_create", { name: "Two" });
  await ws.call("sheet_set", { sheet: 0, inputs: [{ row: 0, column: 0, value: "plum" }] });
  await ws.call("sheet_set", { sheet: 1, inputs: [{ row: 4, column: 2, value: "plum jam" }] });
  await page.addInitScript(() => { try { localStorage.setItem("alephgrid-welcome-done", "1"); } catch { /* */ } });
  await page.goto(`${server.url}/w/${wb}`);
  await page.getByRole("tab", { name: /^One/ }).click();
  await expect.poll(() => shown(page, 0, 0)).toBe("plum");
  await page.keyboard.press("ControlOrMeta+f");
  const bar = page.getByRole("search", { name: "Find in One" });
  await bar.getByRole("searchbox", { name: "Find" }).fill("plum");
  await expect(bar.getByRole("status")).toHaveText("1 match");
  await bar.getByRole("button", { name: "All sheets" }).click();
  await expect(bar.getByRole("status")).toHaveText("2 matches");
  // Next goes on to the other sheet's match.
  await bar.getByRole("searchbox", { name: "Find" }).press("Enter");
  await expect(page.getByRole("tab", { name: /^Two/ })).toHaveAttribute("aria-selected", "true");
  await expect(page.locator(".cell-ref")).toContainText("C5");
  await bar.getByRole("button", { name: "Show replace" }).click();
  await bar.getByRole("textbox", { name: "Replace with" }).fill("pear");
  await bar.getByRole("button", { name: "Preview" }).click();
  await bar.getByRole("button", { name: "Replace all" }).click();
  await expect.poll(async () => (await ws.call("sheet_find_all", { search: { text: "pear" } })).map((s: { name: string }) => s.name)).toEqual(["One", "Two"]);
  // One undo takes back both sheets.
  await page.locator(".grid-host").focus();
  await page.keyboard.press("ControlOrMeta+z");
  await expect.poll(async () => ws.call("sheet_find_all", { search: { text: "pear" } })).toEqual([]);
  ws.close();
});
