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
