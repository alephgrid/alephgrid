import { test, expect } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";
import { Server, menuCommand } from "./server";
import { displayValue, cellColor } from "../src/presentation";
import type { Column, DisplayFormat } from "../src/protocol";
const server = new Server();
test.beforeAll(async () => server.start());
test.afterAll(async () => server.dispose());
const sales = "id,region,amount\n1,north,10\n1,north,20\n2,south,30\n3,,\n";

const pane = (page: import("@playwright/test").Page) => page.getByRole("complementary", { name: "Analyze" });
const results = (page: import("@playwright/test").Page) => page.getByRole("region", { name: "Analysis result" });

test("no-SQL gate: import, join, summarize, chart, save and reopen", async ({ page }) => {
  await page.goto(server.url);
  await page.locator('input[type="file"]').setInputFiles({ name: "sales.csv", mimeType: "text/csv", buffer: Buffer.from(sales) });
  await page.getByRole("dialog", { name: "Import sales.csv" }).getByRole("button", { name: "Import table", exact: true }).click();
  await expect(page.getByRole("tab", { name: /sales/ })).toBeVisible();
  const wb = new URL(page.url()).pathname.split("/")[2];
  await page.locator('input[type="file"]').setInputFiles({ name: "people.csv", mimeType: "text/csv", buffer: Buffer.from("id,name\n1,Ada\n1,Grace\n2,Lin\n") });
  await page.getByRole("dialog", { name: "Import people.csv" }).getByRole("button", { name: "Import table", exact: true }).click();
  await expect(page.getByRole("tab", { name: /people/ })).toBeVisible();
  await page.getByRole("button", { name: "Analyze", exact: true }).click();
  const d = pane(page);
  // A side pane, not a modal: the grid stays usable beside it.
  await expect(d).toBeVisible();
  await expect(page.locator('[aria-modal="true"]')).toHaveCount(0);
  const select = (name: string, value: string) => d.getByRole("combobox", { name, exact: true }).selectOption(value);
  await expect(d.getByRole("combobox", { name: "Source table", exact: true })).toContainText("sales");
  await select("Analysis kind", "join"); await select("Source table", "sales"); await select("Right table", "people");
  await select("Left key", "id"); await select("Right key", "id");
  await d.getByRole("button", { name: "Run", exact: true }).click();
  await expect(d.getByRole("status")).toContainText("5 rows");
  await expect(results(page).locator("table").first()).toContainText("left_duplicate_keys");
  await d.getByRole("textbox", { name: "Analysis name" }).fill("joined");
  await d.getByRole("button", { name: "Result as table", exact: true }).click();
  await expect(page.getByRole("tab", { name: /joined/ })).toBeVisible();
  // Pivot: rows by person, the sum of the amount, built from pickers.
  await select("Analysis kind", "pivot");
  await select("Source table", "joined");
  const rows = d.getByRole("region", { name: "Rows" });
  while (await rows.getByRole("button", { name: /^Remove / }).count()) await rows.getByRole("button", { name: /^Remove / }).first().click();
  await d.getByRole("combobox", { name: "Add field to Rows" }).selectOption("right_name");
  await expect(d.getByRole("region", { name: "Rows" })).toContainText("right_name");
  await select("Aggregate of value 1", "sum"); await select("Column of value 1", "left_amount");
  await d.getByRole("checkbox", { name: "Totals and subtotals" }).uncheck();
  await d.getByRole("button", { name: "Run", exact: true }).click();
  await expect(d.getByRole("status")).toContainText("3 rows");
  await expect(results(page).getByRole("figure", { name: "Analysis chart" }).getByRole("img")).toBeVisible();
  await expect(results(page).getByRole("table", { name: "Chart values" })).toContainText("Ada");
  // Each row ends with its drill-down button (F.11).
  const body = results(page).locator("table.result-table tbody tr");
  await expect(body).toHaveCount(3);
  await expect(body.first()).toContainText("Ada");
  await expect(body.first()).toContainText("30");
  await d.getByRole("textbox", { name: "Analysis name" }).fill("Revenue by person");
  await d.getByRole("button", { name: "Save", exact: true }).click();
  await expect(d.getByRole("status")).toContainText("Saved");
  await page.reload(); await page.getByRole("button", { name: "Analyze", exact: true }).click();
  await pane(page).getByRole("combobox", { name: "Open a saved analysis" }).selectOption({ label: "Revenue by person" });
  await expect(pane(page).getByRole("status")).toContainText("3 rows");
  const ws = await server.connect(wb); await ws.call("sql_change", { sql: "UPDATE joined SET left_amount=left_amount*2" }); ws.close();
  await expect(pane(page).getByRole("status")).toContainText("workbook changed");
  await pane(page).getByRole("button", { name: "Run", exact: true }).click();
  await expect(results(page).locator("table.result-table tbody tr").first()).toContainText("60");
  expect((await new AxeBuilder({ page }).include(".analyze-pane").analyze()).violations).toEqual([]);
  const schemaWriter = await server.connect(wb); await schemaWriter.call("drop_column", { table: "joined", name: "right_name" }); schemaWriter.close();
  await page.reload(); await page.getByRole("button", { name: "Analyze", exact: true }).click();
  await pane(page).getByRole("combobox", { name: "Open a saved analysis" }).selectOption({ label: "Revenue by person" });
  await expect(pane(page).getByRole("status")).toContainText("unknown column");
});

test("pivot values are picked from the column, and charts stay bounded", async ({ page }) => {
  const wb = await server.createWorkbook("Bounded analysis"); const ws = await server.connect(wb);
  await ws.call("create_table_from_query", { name: "many", sql: "SELECT i, CASE WHEN i%2=0 THEN 'even' ELSE 'odd' END AS category FROM range(600) t(i)" });
  await page.goto(`${server.url}/w/${wb}`); await page.getByRole("button", { name: "Analyze", exact: true }).click();
  const d = pane(page);
  const rows = d.getByRole("region", { name: "Rows" });
  await expect(rows).toContainText("category");
  await rows.getByRole("button", { name: "Remove category from Rows" }).click();
  await d.getByRole("combobox", { name: "Add field to Rows" }).selectOption("i");
  await d.getByRole("checkbox", { name: "Totals and subtotals" }).uncheck();
  await d.getByRole("button", { name: "Run", exact: true }).click();
  await expect(d.getByRole("status")).toContainText("500 rows"); await expect(d.getByRole("status")).toContainText("more results exist");
  // At most 100 groups are drawn.
  await expect(results(page).locator("svg.chart rect.chart-c0")).toHaveCount(100);
  await d.getByRole("region", { name: "Rows" }).getByRole("button", { name: "Remove i from Rows" }).click();
  await d.getByRole("combobox", { name: "Column of value 1" }).selectOption("");
  await d.getByRole("combobox", { name: "Pivot column" }).selectOption("category");
  const values = d.getByRole("group", { name: "Pivot values" });
  // The column's own values, with their counts, picked rather than typed.
  await expect(values.getByRole("checkbox")).toHaveCount(2);
  await expect(values).toContainText("even");
  await expect(values).toContainText("300");
  await d.getByRole("button", { name: "Run", exact: true }).click();
  const body = results(page).locator("table.result-table tbody tr");
  await expect(body).toHaveCount(1);
  await expect(body.first()).toContainText("300");
  await d.getByRole("combobox", { name: "Analysis kind" }).selectOption("profile");
  const chips = d.getByRole("region", { name: "Columns" }).getByRole("button", { name: /^Remove / });
  while (await chips.count()) await chips.first().click();
  await d.getByRole("combobox", { name: "Add field to Columns" }).selectOption("category");
  await d.getByRole("button", { name: "Run", exact: true }).click();
  await expect(results(page).locator("table.result-table tbody tr")).toHaveText(["categoryVARCHAR60002598"]); ws.close();
});

test("find bar: match count on the server, next and previous, replace with preview and undo", async ({ page }) => {
  const wb = await server.createWorkbook("Replacement");
  await server.api(`/api/workbooks/${wb}/import?filename=sales.csv`, { method: "POST", body: sales });
  await page.goto(`${server.url}/w/${wb}`);
  await expect(page.locator(".footer")).toContainText("4 rows");
  await page.locator(".grid-host").focus();
  await page.keyboard.press("ControlOrMeta+f");
  const bar = page.getByRole("search", { name: "Find in sales" });
  await expect(bar).toBeVisible();
  await bar.getByRole("searchbox", { name: "Find" }).fill("north");
  // Two cells match; the first is selected.
  await expect(bar.getByRole("status")).toHaveText("2 matches");
  await expect(page.locator(".cell-ref")).toHaveText("region · 1");
  await bar.getByRole("searchbox", { name: "Find" }).press("Enter");
  await expect(page.locator(".cell-ref")).toHaveText("region · 2");
  await bar.getByRole("searchbox", { name: "Find" }).press("Enter");
  await expect(page.locator(".cell-ref")).toHaveText("region · 1");
  await bar.getByRole("button", { name: "Previous match" }).click();
  await expect(page.locator(".cell-ref")).toHaveText("region · 2");
  // Match case.
  await bar.getByRole("button", { name: "Match case" }).click();
  await bar.getByRole("searchbox", { name: "Find" }).fill("North");
  await expect(bar.getByRole("status")).toHaveText("No matches");
  await bar.getByRole("button", { name: "Match case" }).click();
  await expect(bar.getByRole("status")).toHaveText("2 matches");
  // Replace: preview, apply, undo after a reload.
  await bar.getByRole("searchbox", { name: "Find" }).fill("north");
  await bar.getByRole("button", { name: "Show replace" }).click();
  await bar.getByRole("textbox", { name: "Replace with" }).fill("east");
  await bar.getByRole("button", { name: "Preview" }).click();
  await expect(bar.getByRole("status")).toContainText("2 cells will change");
  await bar.getByRole("button", { name: "Replace all" }).click();
  await expect(page.getByText("Replaced 2 cells")).toBeVisible();
  expect(await server.exportCsv(wb, { table: "sales" })).not.toContain("north");
  await page.reload();
  await expect(page.locator(".footer")).toContainText("4 rows");
  await page.locator(".grid-host").focus();
  await page.keyboard.press("ControlOrMeta+Shift+h");
  await page.getByRole("search", { name: "Find in sales" }).getByRole("button", { name: "Undo replace" }).click();
  await expect(page.getByText("Replacement undone")).toBeVisible();
  expect(await server.exportCsv(wb, { table: "sales" })).toContain("north");
  // Escape closes it and returns to the grid.
  await page.getByRole("search", { name: "Find in sales" }).getByRole("searchbox", { name: "Find" }).press("Escape");
  await expect(page.getByRole("search", { name: "Find in sales" })).toBeHidden();
  await expect(page.locator(".grid-host")).toBeFocused();
});

test("presentation persists, preserves exact display and freezes hit testing", async ({ page }) => {
  const wb = await server.createWorkbook("Presentation"); const ws = await server.connect(wb);
  await ws.call("create_table_from_query", { name: "data", sql: "SELECT 9007199254740993.145::DECIMAL(22,3) AS amount, DATE '2024-02-29' AS day, TIMESTAMP '2024-01-01 23:30:00' AS stamp, 'text' AS label, 1 AS a, 2 AS b, 3 AS c, 4 AS d, 5 AS e, 6 AS f, 7 AS g" });
  await page.goto(`${server.url}/w/${wb}`); await page.getByRole("button", { name: "Format table", exact: true }).click();
  const d = page.getByRole("dialog", { name: "Table presentation" });
  await d.getByRole("combobox", { name: "Format amount", exact: true }).selectOption("number");
  await d.getByRole("combobox", { name: "Display locale" }).selectOption("de-DE"); await d.getByRole("combobox", { name: "Display timezone" }).selectOption("Asia/Kolkata");
  await d.getByRole("combobox", { name: "Format day", exact: true }).selectOption("date"); await d.getByRole("combobox", { name: "Format stamp", exact: true }).selectOption("datetime");
  await d.getByRole("checkbox", { name: "Hide label", exact: true }).check(); await d.getByRole("spinbutton", { name: "Frozen columns" }).fill("1");
  await d.getByRole("button", { name: "Save presentation" }).click(); await expect(d).not.toBeVisible();
  await expect(page.getByRole("gridcell")).toHaveText("9.007.199.254.740.993,15"); await page.reload();
  await expect(page.getByRole("gridcell")).toHaveText("9.007.199.254.740.993,15");
  const grid = page.locator(".grid-host"); await grid.focus(); await page.keyboard.press("ArrowRight"); await expect(page.getByRole("gridcell")).toHaveText("29.02.2024");
  await page.keyboard.press("ArrowRight"); await expect(page.getByRole("gridcell")).toContainText("02.01.2024"); await expect(page.getByRole("gridcell")).toContainText("05:00:00");
  const box = (await grid.boundingBox())!; await page.mouse.move(box.x + 400, box.y + 70); await page.mouse.wheel(900, 0);
  await grid.click({ position: { x: 90, y: 44 } }); await expect(page.locator(".cell-ref")).toHaveText("amount · 1");
  const p = await ws.call("presentation", { table: "data" }); expect(p.hidden).toEqual(["label"]); expect(p.frozen).toBe(1); ws.close();
});

test("formatting preserves exact decimals, percentages and dates", () => {
  const col: Column = { name: "v", kind: "number", sql_type: "DECIMAL(30,4)" };
  const f: DisplayFormat = { kind: "number", decimals: 2, locale: "en-US", timezone: "UTC" };
  for (const [input, expected] of [["9007199254740993.145", "9,007,199,254,740,993.15"], ["-1.235", "-1.24"], ["0.005", "0.01"]]) expect(displayValue(input, col, f)).toBe(expected);
  expect(displayValue(1e-7, col, f)).toBe("0.00");
  expect(displayValue("0.125", col, { ...f, kind: "percent" })).toBe("12.50%");
  expect(displayValue("2024-02-29", { ...col, kind: "date" }, { ...f, kind: "date", timezone: "America/New_York" })).toBe("02/29/2024");
  expect(cellColor(null, "v", [{ column: "v", condition: "empty", value: 0, color: "red" }])).toBe("#d33b3b33");
  expect(cellColor("9007199254740993", "v", [{ column: "v", condition: "greater", value: 0, color: "red" }])).toBeUndefined();
});

test("F.11: a report with totals, a line chart, drill-down and a sliced dashboard", async ({ page }) => {
  const wb = await server.createWorkbook("Reports"), ws = await server.connect(wb);
  await ws.call("create_table_from_query", { name: "orders", sql: "SELECT * FROM (VALUES (1, 'West', 100), (2, 'East', 50), (3, 'West', 25)) v(id, region, amount)" });
  await page.goto(`${server.url}/w/${wb}`);
  await page.getByRole("button", { name: "Analyze", exact: true }).click();
  const d = pane(page);
  await d.getByRole("combobox", { name: "Source table", exact: true }).selectOption("orders");
  await expect(d.getByRole("region", { name: "Rows" })).toContainText("region");
  await d.getByRole("combobox", { name: "Aggregate of value 1" }).selectOption("sum");
  await d.getByRole("combobox", { name: "Column of value 1" }).selectOption("amount");
  await d.getByRole("textbox", { name: "Name of value 1" }).fill("revenue");
  await d.getByRole("button", { name: "Add value" }).click();
  await d.getByRole("combobox", { name: "Column of value 2" }).selectOption("");
  await d.getByRole("textbox", { name: "Name of value 2" }).fill("orders");
  await d.getByRole("button", { name: "Add calculated value" }).click();
  await d.getByRole("textbox", { name: "Name of calculated value 1" }).fill("average");
  await d.getByRole("textbox", { name: "Formula of calculated value 1" }).fill("=[@revenue]/[@orders]");
  await d.getByRole("combobox", { name: "Chart kind", exact: true }).selectOption("line");
  await d.getByRole("button", { name: "Run", exact: true }).click();
  // East 50, West 125 (62.5 each), total 175.
  const table = results(page).locator("table.result-table").last();
  await expect(table).toContainText("West");
  await expect(table).toContainText("62.5");
  await expect(table).toContainText("total");
  await expect(results(page).getByRole("table", { name: "Chart values" })).toContainText("125");
  await d.getByRole("textbox", { name: "Analysis name" }).fill("Revenue by region");
  await d.getByRole("button", { name: "Save", exact: true }).click();
  await expect(d.getByRole("status")).toContainText("Saved");
  // Drill into West: the orders view, filtered to its two rows.
  await table.getByRole("row", { name: /West/ }).getByRole("button", { name: "Show rows" }).click();
  await expect(page.locator(".footer")).toContainText("2 rows");
  await d.getByRole("button", { name: "Close Analyze" }).click();
  await expect(d).toBeHidden();
  // A dashboard sliced to East, from lists.
  await menuCommand(page, "Data", "Dashboards…");
  const b = page.getByRole("dialog", { name: "Dashboards" });
  await b.getByRole("group", { name: "Analyses on the dashboard" }).getByRole("checkbox", { name: "Revenue by region" }).check();
  await b.getByRole("combobox", { name: "Slicer table" }).selectOption("orders");
  await b.getByRole("combobox", { name: "Slicer column" }).selectOption("region");
  await b.getByRole("group", { name: "Slicer values" }).getByRole("checkbox", { name: /East/ }).check();
  await b.getByRole("button", { name: "Save", exact: true }).click();
  const tile = b.getByRole("region", { name: "Revenue by region" });
  await expect(tile).toContainText("East");
  await expect(tile).not.toContainText("West");
  ws.close();
});
