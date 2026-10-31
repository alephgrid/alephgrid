import { expect, test } from "@playwright/test";
import { Server } from "./server";

const server = new Server();
test.beforeAll(async () => server.start());
test.afterAll(async () => server.dispose());

test("F.12: a list rule becomes a dropdown and refuses other values; dedupe makes a new table; formulas can be inspected", async ({ page }) => {
  const wb = await server.createWorkbook("Tools"), ws = await server.connect(wb);
  await ws.call("create_table_from_query", { name: "people", sql: "SELECT * FROM (VALUES (1, 'Ann', 'gold'), (2, 'Bo', 'silver'), (3, 'Bo', 'gold')) v(id, name, tier)" });
  await ws.call("add_formula_column", { table: "people", name: "badge", formula: "=UPPER([@tier])", language: "sheet" });
  await page.goto(`${server.url}/w/${wb}`);
  await expect(page.getByRole("grid", { name: "Table people" })).toBeVisible();
  await page.getByRole("button", { name: "Data tools", exact: true }).click();
  const d = page.getByRole("dialog", { name: /Data tools/ });
  await d.getByRole("combobox", { name: "Column", exact: true }).selectOption("tier");
  await d.getByRole("textbox", { name: "Rule values" }).fill("gold\nsilver");
  await d.getByRole("button", { name: "Add rule", exact: true }).click();
  await expect(d.getByRole("list", { name: "Rules" })).toContainText("tier: one of gold, silver · 0 rows break it now");
  // Dedupe by name: the second Bo goes, into a new table.
  await d.getByRole("combobox", { name: "Transform", exact: true }).selectOption("dedupe");
  await d.getByRole("textbox", { name: "Transform argument" }).fill("name");
  await d.getByRole("button", { name: "Preview", exact: true }).click();
  await expect(d.getByRole("status")).toContainText("2 rows · 1 duplicates left out");
  await d.getByRole("combobox", { name: "Formula column", exact: true }).selectOption("badge");
  await d.getByRole("button", { name: "Inspect", exact: true }).click();
  await expect(d).toContainText("Reads columns");
  await expect(d).toContainText("tier");
  await d.getByRole("textbox", { name: "New table name" }).fill("people_unique");
  await d.getByRole("button", { name: "Create table", exact: true }).click();
  await expect(page.getByRole("tab", { name: /people_unique/ })).toBeVisible();
  // Back in people: the tier editor offers the list, and another value is refused.
  await page.getByRole("tab", { name: "people", exact: true }).click();
  await expect(page.getByRole("grid", { name: "Table people" })).toBeVisible();
  await expect(page.locator(".footer")).toContainText("3 rows");
  await page.locator(".grid-host").focus();
  await page.keyboard.press("ControlOrMeta+Home");
  await page.keyboard.press("ArrowRight"); await page.keyboard.press("ArrowRight");
  // Typing starts the editor, which lists the allowed values.
  await page.keyboard.type("1");
  const editor = page.locator("input[list^='grid-choices-']");
  await expect(editor).toBeVisible();
  expect(await page.locator("datalist[id^='grid-choices-'] option").evaluateAll(o => o.map(x => (x as HTMLOptionElement).value))).toEqual(["gold", "silver"]);
  await editor.fill("platinum");
  await page.keyboard.press("Enter");
  await expect(page.locator("body")).toContainText("is not allowed in column \"tier\"");
  ws.close();
});

test("F.13: an XLSX download first says what it leaves out", async ({ page }) => {
  const wb = await server.createWorkbook("Compat"), ws = await server.connect(wb);
  await ws.call("create_table_from_query", { name: "orders", sql: "SELECT * FROM (VALUES (1, 100.0)) v(id, amount)" });
  await ws.call("sheet_create", { name: "Report" });
  await ws.call("sheet_set", { sheet: 0, inputs: [{ row: 0, column: 0, value: "=SUM(orders[amount])" }] });
  await page.goto(`${server.url}/w/${wb}`);
  await page.getByRole("tab", { name: /Report/ }).click();
  await page.getByRole("button", { name: "Export ▾" }).click();
  await page.getByRole("menuitem", { name: /xlsx/i }).first().click();
  const d = page.locator(".dialog", { hasText: "XLSX compatibility" });
  await expect(d).toContainText("Report!A1: the table formula =SUM(orders[amount]) is exported as its value \"100\"");
  await expect(d).toContainText("table orders (1 rows) is not in the file");
  const download = page.waitForEvent("download");
  await d.getByRole("button", { name: "Download anyway" }).click();
  expect((await download).suggestedFilename()).toBe("Compat.xlsx");
  ws.close();
});
