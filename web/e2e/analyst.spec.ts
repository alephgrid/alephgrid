import { expect, test, type Page } from "@playwright/test";
import { Server, menuCommand } from "./server";

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

async function addColumn(page: Page, name: string, formula: string) {
  await page.getByRole("button", { name: "Add column" }).click();
  const d = page.getByRole("dialog", { name: "Add column" });
  await d.getByLabel("Name").fill(name);
  await d.getByLabel("Formula (for formula columns)").fill(formula);
  await d.getByRole("button", { name: "Add column" }).click();
  await expect(d).toBeHidden();
}

/** F.15 analyst gate, without SQL: import, enrich by XLOOKUP, conditional totals, a report
 *  with totals and a chart, a filtered dashboard, refreshed data, reopening, and an export
 *  that states what it cannot keep. Values by hand from the two files. */
test("F.15: the analyst workflow runs end to end without SQL", async ({ page }) => {
  test.setTimeout(120_000);
  await page.goto(server.url);
  const upload = async (name: string, csv: string) => {
    await page.locator('input[type="file"]').setInputFiles({ name, mimeType: "text/csv", buffer: Buffer.from(csv) });
    await page.getByRole("dialog", { name: `Import ${name}` }).getByRole("button", { name: "Import table", exact: true }).click();
    await expect(page.getByRole("tab", { name: new RegExp(name.replace(".csv", "")) })).toBeVisible();
  };
  await upload("orders.csv", "id,customer,amount\n1,10,100\n2,11,50\n3,10,25\n4,12,10\n");
  await upload("customers.csv", "id,name,region\n10,Ann,West\n11,Bo,East\n12,Cy,West\n");
  // Enrich orders with each customer's region; total each customer's orders.
  await page.getByRole("tab", { name: /orders/ }).click();
  await addColumn(page, "region", '=XLOOKUP([@customer],customers[id],customers[region],"none")');
  await expect.poll(() => shown(page, 0, 3)).toContain("West");
  await page.getByRole("tab", { name: /customers/ }).click();
  await addColumn(page, "total", "=SUMIFS(orders[amount],orders[customer],[@id])");
  await expect.poll(() => shown(page, 0, 3)).toContain("125");
  // A report by region with totals and a line chart: East 50, West 135, total 185.
  await page.getByRole("button", { name: "Analyze", exact: true }).click();
  const d = page.getByRole("dialog", { name: "Analyze data" });
  await d.getByRole("combobox", { name: "Analysis kind", exact: true }).selectOption("report");
  await d.getByRole("combobox", { name: "Source table", exact: true }).selectOption("orders");
  await d.getByRole("listbox", { name: "Group columns" }).selectOption("region");
  await d.getByRole("textbox", { name: "Measures", exact: true }).fill("revenue = sum(amount)");
  await d.getByRole("combobox", { name: "Chart kind", exact: true }).selectOption("line");
  await d.getByRole("button", { name: "Run / refresh", exact: true }).click();
  await expect(d.getByRole("list", { name: "Chart values" })).toContainText("West: 135");
  await expect(d.locator("table.result-table").last()).toContainText("185");
  await d.getByRole("textbox", { name: "Analysis name" }).fill("Revenue by region");
  await d.getByRole("button", { name: "Save analysis", exact: true }).click();
  await expect(d.getByRole("status")).toContainText("Analysis saved");
  await d.getByRole("button", { name: "Close", exact: true }).click();
  // Filtered: a dashboard sliced to East.
  await menuCommand(page, "Data", "Dashboards…");
  const b = page.getByRole("dialog", { name: "Dashboards" });
  await b.getByRole("listbox", { name: "Analyses on the dashboard" }).selectOption({ label: "Revenue by region" });
  await b.getByRole("textbox", { name: "Slicer table" }).fill("orders");
  await b.getByRole("textbox", { name: "Slicer column" }).fill("region");
  await b.getByRole("textbox", { name: "Slicer values" }).fill("East");
  await b.getByRole("button", { name: "Save dashboard", exact: true }).click();
  await expect(b.getByRole("region", { name: "Revenue by region" })).not.toContainText("West");
  await b.getByRole("button", { name: "Close", exact: true }).click();
  // Changed data, from another session: Cy moves East; the open orders view follows.
  const wb = new URL(page.url()).pathname.split("/")[2];
  const ws = await server.connect(wb);
  const view = await ws.call("open_view", { spec: { table: "customers", sort: [], filters: [] } });
  await ws.call("edit", { view_id: view.view_id, edits: [{ row: 2, column: "region", value: "East" }] });
  await page.getByRole("tab", { name: /orders/ }).click();
  await expect.poll(() => shown(page, 3, 3)).toContain("East");
  // A sheet that reads the tables, kept across reopening.
  await page.getByRole("button", { name: "New table or sheet" }).click();
  await page.getByRole("menuitem", { name: /New sheet/ }).click();
  await expect(page.getByRole("grid", { name: /^Sheet / })).toBeVisible();
  const bar = page.getByRole("textbox", { name: "Cell value or formula", exact: true });
  await page.locator(".grid-host").focus();
  await page.keyboard.press("ControlOrMeta+Home");
  await bar.fill('=SUMIFS(orders[amount],orders[region],"West")');
  await bar.press("Enter");
  await expect.poll(() => shown(page, 0, 0)).toContain("125");
  await page.reload();
  await page.getByRole("tab", { name: /Sheet/ }).click();
  await expect.poll(() => shown(page, 0, 0)).toContain("125");
  // Export: the download first says what XLSX cannot keep.
  await page.getByRole("button", { name: "Export", exact: true }).click();
  await page.getByRole("menuitem", { name: /xlsx/i }).first().click();
  const notes = page.locator(".dialog", { hasText: "XLSX compatibility" });
  await expect(notes).toContainText('the table formula =SUMIFS(orders[amount],orders[region],"West") is exported as its value "125"');
  await expect(notes).toContainText("table orders (4 rows) is not in the file");
  const download = page.waitForEvent("download");
  await notes.getByRole("button", { name: "Download anyway" }).click();
  expect((await download).suggestedFilename()).toMatch(/\.xlsx$/);
  ws.close();
});
