import { test, expect } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";
import { Server } from "./server";
import { displayValue, cellColor } from "../src/presentation";
import type { Column, DisplayFormat } from "../src/protocol";
const server = new Server();
test.beforeAll(async () => server.start());
test.afterAll(async () => server.dispose());
const sales = "id,region,amount\n1,north,10\n1,north,20\n2,south,30\n3,,\n";

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
  const d = page.getByRole("dialog", { name: "Analyze data" });
  const select = (name: string, value: string) => d.getByRole("combobox", { name, exact: true }).selectOption(value);
  await expect(d.getByRole("combobox", { name: "Source table", exact: true })).toContainText("sales");
  await select("Analysis kind", "join"); await select("Source table", "sales"); await select("Right table", "people");
  await select("Left key", "id"); await select("Right key", "id");
  await d.getByRole("button", { name: "Run / refresh", exact: true }).click();
  await expect(d.getByRole("status")).toContainText("5 rows");
  await expect(d.locator("table").first()).toContainText("left_duplicate_keys");
  await d.getByRole("textbox", { name: "Analysis name" }).fill("joined");
  await d.getByRole("button", { name: "Create result table", exact: true }).click();
  await expect(page.getByRole("tab", { name: /joined/ })).toBeVisible();
  await page.getByRole("button", { name: "Analyze", exact: true }).click();
  await expect(d.getByRole("combobox", { name: "Source table", exact: true })).toContainText("joined");
  await select("Source table", "joined"); await d.getByRole("listbox", { name: "Group columns" }).selectOption("right_name");
  await select("Aggregate", "sum"); await select("Value column", "left_amount");
  await d.getByRole("button", { name: "Run / refresh", exact: true }).click();
  await expect(d.getByRole("status")).toContainText("3 rows");
  await expect(d.getByRole("figure", { name: "Summary chart" })).toContainText("Ada");
  // Each summary row ends with its drill-down button (F.11).
  expect(await d.locator("tbody tr").allTextContents()).toEqual(["Ada30Show rows", "Grace30Show rows", "Lin30Show rows"]);
  await d.getByRole("textbox", { name: "Analysis name" }).fill("Revenue by person");
  await d.getByRole("button", { name: "Save analysis", exact: true }).click();
  await expect(d.getByRole("status")).toContainText("Analysis saved");
  await page.reload(); await page.getByRole("button", { name: "Analyze", exact: true }).click();
  await d.getByRole("button", { name: "Revenue by person", exact: true }).click();
  await expect(d.getByRole("status")).toContainText("3 rows");
  const ws = await server.connect(wb); await ws.call("sql_change", { sql: "UPDATE joined SET left_amount=left_amount*2" }); ws.close();
  await expect(d.getByRole("status")).toContainText("Workbook changed");
  await d.getByRole("button", { name: "Run / refresh", exact: true }).click();
  await expect(d.locator("tbody tr").first()).toHaveText("Ada60Show rows");
  expect((await new AxeBuilder({ page }).include('.analysis-dialog').analyze()).violations).toEqual([]);
  const schemaWriter = await server.connect(wb); await schemaWriter.call("drop_column", { table: "joined", name: "right_name" }); schemaWriter.close();
  await page.reload(); await page.getByRole("button", { name: "Analyze", exact: true }).click();
  await d.getByRole("button", { name: "Revenue by person", exact: true }).click();
  await expect(d.getByRole("status")).toContainText("unknown column");
  await d.getByRole("checkbox", { name: "Show chart" }).uncheck();
  await d.getByRole("button", { name: "Run / refresh", exact: true }).click();
  await expect(d.getByRole("status")).toContainText("unknown column");
});

test("profile, pivot and bounded charts return only aggregate rows", async ({ page }) => {
  const wb = await server.createWorkbook("Bounded analysis"); const ws = await server.connect(wb);
  await ws.call("create_table_from_query", { name: "many", sql: "SELECT i, CASE WHEN i%2=0 THEN 'even' ELSE 'odd' END AS category FROM range(600) t(i)" });
  await page.goto(`${server.url}/w/${wb}`); await page.getByRole("button", { name: "Analyze", exact: true }).click();
  const d = page.getByRole("dialog", { name: "Analyze data" });
  await expect(d.getByRole("listbox", { name: "Group columns" })).toContainText("category");
  await d.getByRole("listbox", { name: "Group columns" }).selectOption("i");
  await d.getByRole("button", { name: "Run / refresh", exact: true }).click();
  await expect(d.getByRole("status")).toContainText("500 rows"); await expect(d.getByRole("status")).toContainText("truncated");
  await expect(d.locator(".analysis-chart-row")).toHaveCount(100);
  await d.getByRole("listbox", { name: "Group columns" }).selectOption([]);
  await d.getByRole("combobox", { name: "Pivot column" }).selectOption("category");
  await d.getByRole("textbox", { name: "Pivot values" }).fill("even\nodd");
  await d.getByRole("button", { name: "Run / refresh", exact: true }).click();
  await expect(d.locator("tbody tr")).toHaveText(["300300Show rows"]);
  await d.getByRole("combobox", { name: "Analysis kind" }).selectOption("profile");
  await d.getByRole("listbox", { name: "Profile columns" }).selectOption("category");
  await d.getByRole("button", { name: "Run / refresh", exact: true }).click();
  await expect(d.locator("tbody tr")).toHaveText(["categoryVARCHAR60002598"]); ws.close();
});

test("replacement preview, undo after reopen and stale-preview protection", async ({ page }) => {
  const wb = await server.createWorkbook("Replacement");
  await server.api(`/api/workbooks/${wb}/import?filename=sales.csv`, { method: "POST", body: sales });
  await page.goto(`${server.url}/w/${wb}`); await page.getByRole("button", { name: "Find / replace", exact: true }).click();
  const d = page.getByRole("dialog", { name: "Find and replace" });
  const preview = async () => {
    await d.getByRole("textbox", { name: "Find text" }).fill("north"); await d.getByRole("textbox", { name: "Replacement text" }).fill("east");
    await d.getByRole("button", { name: "Preview replacement" }).click(); await expect(d.getByRole("status")).toContainText("2 matching cells");
  };
  await preview(); await d.getByRole("button", { name: "Apply replacement" }).click(); await expect(d.getByRole("status")).toContainText("Replaced 2 cells");
  await page.reload(); await page.getByRole("button", { name: "Find / replace", exact: true }).click();
  await d.getByRole("button", { name: "Undo last replacement" }).click(); await expect(d.getByRole("status")).toContainText("Replacement undone");
  expect(await server.exportCsv(wb, { table: "sales" })).toContain("north"); await preview();
  const ws = await server.connect(wb); await ws.call("sql_change", { sql: "UPDATE sales SET region='latest' WHERE id=2" }); ws.close();
  await d.getByRole("button", { name: "Apply replacement" }).click(); await expect(d.getByRole("status")).toContainText("preview replacement again");
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
  const d = page.getByRole("dialog", { name: "Analyze data" });
  await d.getByRole("combobox", { name: "Analysis kind", exact: true }).selectOption("report");
  await d.getByRole("combobox", { name: "Source table", exact: true }).selectOption("orders");
  await d.getByRole("listbox", { name: "Group columns" }).selectOption("region");
  await d.getByRole("textbox", { name: "Measures", exact: true }).fill("revenue = sum(amount)\norders = count()");
  await d.getByRole("textbox", { name: "Calculated measures", exact: true }).fill("average = [@revenue]/[@orders]");
  await d.getByRole("combobox", { name: "Chart kind", exact: true }).selectOption("line");
  await d.getByRole("button", { name: "Run / refresh", exact: true }).click();
  // East 50, West 125 (62.5 each), total 175.
  const table = d.locator("table.result-table").last();
  await expect(table).toContainText("West");
  await expect(table).toContainText("62.5");
  await expect(table).toContainText("total");
  await expect(d.getByRole("list", { name: "Chart values" })).toContainText("West: 125");
  await d.getByRole("textbox", { name: "Analysis name" }).fill("Revenue by region");
  await d.getByRole("button", { name: "Save analysis", exact: true }).click();
  await expect(d.getByRole("status")).toContainText("Analysis saved");
  // Drill into West: the orders view, filtered to its two rows.
  await table.getByRole("row", { name: /West/ }).getByRole("button", { name: "Show rows" }).click();
  await expect(d).toBeHidden();
  await expect(page.locator(".footer")).toContainText("2 rows");
  // A dashboard sliced to East.
  await page.getByRole("button", { name: "Dashboards", exact: true }).click();
  const b = page.getByRole("dialog", { name: "Dashboards" });
  await b.getByRole("listbox", { name: "Analyses on the dashboard" }).selectOption({ label: "Revenue by region" });
  await b.getByRole("textbox", { name: "Slicer table" }).fill("orders");
  await b.getByRole("textbox", { name: "Slicer column" }).fill("region");
  await b.getByRole("textbox", { name: "Slicer values" }).fill("East");
  await b.getByRole("button", { name: "Save dashboard", exact: true }).click();
  const tile = b.getByRole("region", { name: "Revenue by region" });
  await expect(tile).toContainText("East");
  await expect(tile).not.toContainText("West");
  ws.close();
});
