import AxeBuilder from "@axe-core/playwright";
import { expect, test } from "@playwright/test";
import { Server } from "./server";

const server = new Server();
test.beforeAll(async () => server.start());
test.afterAll(async () => server.dispose());

test("formula help searches a bounded catalog and distinguishes registry evidence", async ({ page }) => {
  const wb = await server.createWorkbook("Catalog");
  await page.goto(`${server.url}/w/${wb}`);
  await page.getByRole("button", { name: "Formula help", exact: true }).click();
  const dialog = page.getByRole("dialog", { name: "Formula help" });
  const search = dialog.getByRole("searchbox", { name: "Search built-in formulas" });
  await expect(search).toBeFocused();
  await expect(dialog.getByRole("status")).toContainText("562 matching formulas");
  await expect(dialog.getByRole("region", { name: "Matching formulas" }).getByRole("button")).toHaveCount(40);
  await dialog.getByRole("button", { name: "Next formulas" }).click();
  await expect(dialog.getByRole("status")).toContainText("showing 41–80");
  await search.fill("XLOOKUP");
  const details = dialog.getByRole("region", { name: "Formula details" });
  await expect(details).toContainText("Sheet engine: registered");
  await expect(details).toContainText("DuckDB table formulas: not supported");
  await expect(details).toContainText("=XLOOKUP(2,A1:A4,B1:B4)");
  await expect(details).toContainText("tested example");
  await details.getByText("Mode evidence: 9 verified cases, 1 known gaps", { exact: true }).click();
  await expect(details.getByText("Known gap: inline arrays", { exact: true })).toBeVisible();
  await expect(details.getByText("Verified case: last duplicate", { exact: true })).toBeVisible();
  await expect(details).toContainText("IronCalc 0.9.1 returns #N/A; desired result retained");
  const axe = await new AxeBuilder({ page }).include('.catalog-dialog').analyze();
  expect(axe.violations.filter(v => v.impact === "serious" || v.impact === "critical")).toEqual([]);
  await search.fill("SUMIFS");
  await details.getByText("Mode evidence: 2 verified cases, 1 known gaps", { exact: true }).click();
  await expect(details.getByText("Known gap: unequal ranges", { exact: true })).toBeVisible();
  await expect(details).toContainText("IronCalc 0.9.1 returns 50");
  await expect(details).toContainText('expected "#VALUE!"');
  await search.fill("REGEXMATCH");
  await dialog.getByLabel("Formula availability").selectOption("not_registered");
  await expect(details).toContainText("not registered under this name");
  await expect(details).toContainText("No Rowfinity calculation example");
  await search.fill("does-not-exist");
  await expect(dialog.getByRole("status")).toHaveText("0 matching formulas");
  await expect(details).toHaveText("No matching formulas.");
  await search.press("Escape");
  await expect(dialog).toHaveCount(0);
});

test("built-in completion inserts a real sheet formula without suggesting inside text", async ({ page }) => {
  const wb = await server.createWorkbook("Built-ins");
  const ws = await server.connect(wb);
  await ws.call("sheet_create", { name: "Examples" });
  await ws.call("sheet_set", { sheet: 0, inputs: [
    { row: 0, column: 0, value: "1" }, { row: 1, column: 0, value: "2" }, { row: 2, column: 0, value: "2" },
    { row: 0, column: 1, value: "10" }, { row: 1, column: 1, value: "20" }, { row: 2, column: 1, value: "30" },
  ] });
  await page.goto(`${server.url}/w/${wb}`);
  await expect(page.getByRole("grid", { name: /^Sheet/ })).toBeVisible();
  await expect(page.locator(".cell-ref")).not.toHaveText("");
  await page.locator(".grid-host").focus();
  await page.keyboard.press("ControlOrMeta+Home");
  await page.keyboard.press("ArrowRight");
  await page.keyboard.press("ArrowRight");
  const editor = page.getByRole("textbox", { name: "Cell value or formula", exact: true });
  const suggestions = page.getByRole("region", { name: "Function suggestions" });
  for (const text of ['SUM', '="SUM', '="a""SUM', "='SUM"]) {
    await editor.fill(text);
    await expect(suggestions).toBeHidden();
  }
  await editor.fill("=XLO");
  await expect(suggestions).toContainText("XLOOKUP(lookup_value");
  await editor.press("Tab");
  await expect(editor).toHaveValue("=XLOOKUP(");
  await expect(suggestions).toContainText("return_array");
  await editor.fill("=XLOOKUP(2,A1:A3,B1:B3)");
  await editor.press("Enter");
  const cell = async () => (await ws.call("sheet_fetch", { sheet: 0, row_start: 0, row_count: 1, column_count: 3 })).rows[0]?.[2]?.[0];
  await expect.poll(cell).toBe("20");
  await page.reload();
  await expect.poll(cell).toBe("20");
  ws.close();
});

test("sheet built-ins are not offered as DuckDB column formulas", async ({ page }) => {
  const wb = await server.createWorkbook("Table catalog boundary");
  await server.api(`/api/workbooks/${wb}/import?filename=data.csv`, { method: "POST", body: "id,value\n1,10\n" });
  await page.goto(`${server.url}/w/${wb}`);
  await expect(page.locator(".row-count")).toHaveText("1 rows");
  const editor = page.getByRole("textbox", { name: "Cell value or formula", exact: true });
  await editor.fill("=XLO");
  await expect(page.getByRole("region", { name: "Function suggestions" })).toBeHidden();
});
