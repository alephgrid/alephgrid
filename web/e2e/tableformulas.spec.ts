import { expect, test, type Page } from "@playwright/test";
import { Server } from "./server";

const server = new Server();
test.beforeAll(async () => server.start());
test.afterAll(async () => server.dispose());

/** The accessible text of a table cell, moved to with the keyboard. */
async function shown(page: Page, r: number, c: number): Promise<string> {
  await page.locator(".grid-host").focus();
  await page.keyboard.press("ControlOrMeta+Home");
  for (let i = 0; i < r; i++) await page.keyboard.press("ArrowDown");
  for (let i = 0; i < c; i++) await page.keyboard.press("ArrowRight");
  const id = await page.locator(".grid-host").getAttribute("aria-activedescendant");
  return (await page.locator(`#${id}`).textContent()) ?? "";
}

test("a spreadsheet formula column computes over the table and shows errors", async ({ page }) => {
  const wb = await server.createWorkbook("Table formulas"), ws = await server.connect(wb);
  await ws.call("create_table_from_query", { name: "orders", sql: "SELECT * FROM (VALUES (1, 100.0, 4), (2, 50.0, 0)) v(id, amount, units)" });
  await page.goto(`${server.url}/w/${wb}`);
  await expect(page.getByRole("grid", { name: "Table orders" })).toBeVisible();
  await page.getByRole("button", { name: "Add column" }).click();
  const dialog = page.getByRole("dialog", { name: "Add column" });
  await dialog.getByLabel("Name").fill("unit_price");
  await expect(dialog.getByLabel("Type")).toHaveValue("sheet");
  await dialog.getByLabel("Formula (for formula columns)").fill("=[@amount]/[@unit]");
  await dialog.getByRole("button", { name: "Add column" }).click();
  await expect(dialog).toContainText("no column named \"unit\"");
  await expect(dialog).toContainText("character 12");
  await dialog.getByLabel("Formula (for formula columns)").fill("=[@amount]/[@units]");
  await dialog.getByRole("button", { name: "Add column" }).click();
  await expect(dialog).toBeHidden();
  await expect.poll(() => shown(page, 0, 3)).toContain("25");
  await expect.poll(() => shown(page, 1, 3)).toContain("#DIV/0!");
  const cols = await ws.call("open_view", { spec: { table: "orders", sort: [], filters: [] } });
  expect(cols.columns.find((c) => c.name === "unit_price")).toMatchObject({ formula: "=[@amount]/[@units]", formula_language: "sheet", sql_type: "DOUBLE" });
  const block = await ws.call("fetch", { view_id: cols.view_id, row_start: 0, row_count: 2 });
  expect(block.errors).toEqual([{ row: 1, column: 3, code: "#DIV/0!" }]);
  ws.close();
});

test("a table formula reads a sheet cell and follows its edits", async ({ page }) => {
  const wb = await server.createWorkbook("Sheet links"), ws = await server.connect(wb);
  await ws.call("create_table_from_query", { name: "orders", sql: "SELECT * FROM (VALUES (1, 100.0), (2, 50.0)) v(id, amount)" });
  await ws.call("sheet_create", { name: "Settings" });
  await ws.call("sheet_set", { sheet: 0, inputs: [{ row: 0, column: 1, value: "0.1" }] });
  await ws.call("add_formula_column", { table: "orders", name: "gross", formula: "=[@amount]*(1+Settings!$B$1)", language: "sheet" });
  await page.goto(`${server.url}/w/${wb}`);
  await page.getByRole("tab", { name: /orders/ }).click();
  await expect(page.getByRole("grid", { name: "Table orders" })).toBeVisible();
  await expect.poll(() => shown(page, 0, 2)).toContain("110");
  // Another session edits the cell: the open table refreshes by itself.
  await ws.call("sheet_set", { sheet: 0, inputs: [{ row: 0, column: 1, value: "0.5" }] });
  await expect.poll(() => shown(page, 0, 2)).toContain("150");
  // Edited here, in the sheet: back in the table, the new value shows.
  await page.getByRole("tab", { name: /Settings/ }).click();
  await expect(page.getByRole("grid", { name: /^Sheet / })).toHaveAttribute("aria-colcount", /\d+/);
  await page.locator(".grid-host").focus();
  await page.keyboard.press("ControlOrMeta+Home");
  await page.keyboard.press("ArrowRight");
  await page.keyboard.type("1");
  await page.keyboard.press("Enter");
  await page.getByRole("tab", { name: /orders/ }).click();
  await expect.poll(() => shown(page, 1, 2)).toContain("100");
  // A row inserted above the cell: the formula follows it.
  await ws.call("sheet_insert_rows", { sheet: 0, at: 0, count: 1 });
  const view = await ws.call("open_view", { spec: { table: "orders", sort: [], filters: [] } });
  expect(view.columns.find((c) => c.name === "gross")?.formula).toBe("=[@amount]*(1+Settings!$B$2)");
  await expect.poll(() => shown(page, 1, 2)).toContain("100");
  ws.close();
});

test("a sheet cell reads a table and follows its edits", async ({ page }) => {
  const wb = await server.createWorkbook("Table cells"), ws = await server.connect(wb);
  await ws.call("create_table_from_query", { name: "orders", sql: "SELECT * FROM (VALUES (1, 'West', 100.0), (2, 'East', 50.0), (3, 'West', 25.0)) v(id, region, amount)" });
  await ws.call("sheet_create", { name: "Report" });
  await ws.call("sheet_set", { sheet: 0, inputs: [{ row: 0, column: 0, value: "West" }] });
  await page.goto(`${server.url}/w/${wb}`);
  await page.getByRole("tab", { name: /Report/ }).click();
  await expect(page.getByRole("grid", { name: /^Sheet / })).toHaveAttribute("aria-colcount", /\d+/);
  const bar = page.getByRole("textbox", { name: "Cell value or formula", exact: true });
  // B1: typed in the formula bar, reading the criterion in A1.
  await page.locator(".grid-host").focus();
  await page.keyboard.press("ControlOrMeta+Home");
  await page.keyboard.press("ArrowRight");
  await bar.fill("=SUMIFS(orders[amount], orders[region], A1)");
  await bar.press("Enter");
  // 100 + 25.
  await expect.poll(() => shown(page, 0, 1)).toContain("125");
  await page.keyboard.press("ControlOrMeta+Home");
  await page.keyboard.press("ArrowRight");
  await expect(bar).toHaveValue("=SUMIFS(orders[amount], orders[region], A1)");
  // Another session edits the table: the sheet shows the new total by itself.
  const view = await ws.call("open_view", { spec: { table: "orders", sort: [], filters: [] } });
  await ws.call("edit", { view_id: view.view_id, edits: [{ row: 2, column: "amount", value: "75" }] });
  await expect.poll(() => shown(page, 0, 1)).toContain("175");
  // A formula the table engine cannot compute is refused where it goes wrong.
  await page.keyboard.press("ControlOrMeta+Home");
  await page.keyboard.press("ArrowDown");
  await page.keyboard.press("ArrowRight");
  await bar.fill("=SUMIFS(orders[nope], orders[region], A1)");
  await bar.press("Enter");
  await expect(page.locator("body")).toContainText("no column named \"nope\"");
  ws.close();
});

test("a view sorted by a lookup column shows stale stored results while they are recalculated", async ({ page }) => {
  test.setTimeout(120_000);
  const wb = await server.createWorkbook("Stored results"), ws = await server.connect(wb);
  // Large enough for stored results (200,000 rows), and slow enough to cancel.
  await ws.call("create_table_from_query", { name: "customers", sql: "SELECT i AS id, 'c' || lpad(CAST(i AS VARCHAR), 6, '0') AS name FROM range(400000) t(i)" });
  await ws.call("create_table_from_query", { name: "orders", sql: "SELECT i AS id, (i * 7919) % 400000 AS cid FROM range(4000000) t(i)" });
  await ws.call("add_formula_column", { table: "orders", name: "who", formula: "=XLOOKUP([@cid],customers[id],customers[name])", language: "sheet" });
  await ws.call("save_view", { name: "by customer", spec: { table: "orders", sort: [{ column: "who", desc: false }], filters: [], hidden: [] }, widths: {} });
  await page.goto(`${server.url}/w/${wb}`);
  await page.getByRole("tab", { name: /orders/ }).click();
  await page.getByRole("button", { name: "Views", exact: true }).click();
  await page.getByRole("menuitem", { name: /by customer/ }).click();
  await expect.poll(() => shown(page, 0, 2), { timeout: 60_000 }).toContain("c000000");
  const note = page.locator(".stale-note");
  await expect(note).toBeHidden();
  // Another session renames a customer: orders' results are recalculated in the background.
  const customers = await ws.call("open_view", { spec: { table: "customers", sort: [], filters: [] } });
  // A recalculation can finish before the click lands; edit again until a cancel does.
  for (let attempt = 0; attempt < 5; attempt++) {
    await ws.call("edit", { view_id: customers.view_id, edits: [{ row: 0, column: "name", value: `zz${attempt}` }] });
    await expect(note).toContainText("recalculating");
    await note.getByRole("button", { name: "Cancel" }).click({ timeout: 2000 }).catch(() => {});
    if (await expect(note).toContainText("cancelled", { timeout: 5000 }).then(() => true, () => false)) break;
    await expect(note).toBeHidden({ timeout: 60_000 });
  }
  await expect(note).toContainText("recalculation failed: cancelled");
  // Still the last committed order, flagged.
  // Still the last committed order: c000000 unless an earlier attempt's recalculation (which
  // renamed it to zz…) completed first, then c000001.
  await expect.poll(() => shown(page, 0, 2)).toMatch(/c00000[01]/);
  await note.getByRole("button", { name: "Recalculate" }).click();
  await expect(note).toBeHidden({ timeout: 60_000 });
  await expect.poll(() => shown(page, 0, 2)).toContain("c000001");
  ws.close();
});
