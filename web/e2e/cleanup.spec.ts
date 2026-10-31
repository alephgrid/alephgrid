// U.T04: the header filter popup (value counts against an independent count, keyboard path,
// apply over the whole table), the first-run guide, and accessibility of the new panes in
// both themes. The find bar and the Analyze pane are covered in analysis.spec.ts.

import { expect, test, type Page } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";
import { Server } from "./server";

const server = new Server();
test.beforeAll(async () => server.start());
test.afterAll(async () => server.dispose());

const N = 3000;
// Region of row i: north for i % 3 == 0, south for 1, empty for 2; independent of the server.
const region = (i: number) => (i % 3 === 0 ? "north" : i % 3 === 1 ? "south" : "");
const csv = "id,region\n" + Array.from({ length: N }, (_, i) => `${i},${region(i)}`).join("\n") + "\n";
const expected = (r: string) => Array.from({ length: N }, (_, i) => region(i)).filter((x) => x === r).length;

async function open(page: Page, name: string): Promise<string> {
  const wb = await server.createWorkbook(name);
  await server.api(`/api/workbooks/${wb}/import?filename=t.csv`, { method: "POST", body: csv });
  await page.goto(`${server.url}/w/${wb}`);
  await expect(page.locator(".footer")).toContainText(`${N.toLocaleString()} rows`);
  return wb;
}

test("the header filter lists every value with its count and filters the whole table", async ({ page }) => {
  await open(page, "Filter popup");
  const grid = page.locator(".grid-host");
  await grid.focus();
  await page.keyboard.press("ArrowRight");
  await page.keyboard.press("Alt+ArrowDown");
  const pop = page.getByRole("dialog", { name: "Filter region" });
  await expect(pop).toBeVisible();
  await expect(pop.getByRole("searchbox", { name: "Search values of region" })).toBeFocused();
  const list = pop.getByRole("group", { name: "Values of region" });
  // Counts from the server over all rows, against the counts computed here.
  await expect(list.locator(".fp-item")).toHaveCount(3);
  await expect(list.locator(".fp-item", { hasText: "north" }).locator(".fp-count")).toHaveText(expected("north").toLocaleString());
  await expect(list.locator(".fp-item", { hasText: "south" }).locator(".fp-count")).toHaveText(expected("south").toLocaleString());
  await expect(list.locator(".fp-item", { hasText: "(Empty)" }).locator(".fp-count")).toHaveText(expected("").toLocaleString());
  // Search narrows the list.
  await pop.getByRole("searchbox").fill("nor");
  await expect(list.locator(".fp-item")).toHaveCount(1);
  await pop.getByRole("searchbox").fill("");
  await expect(list.locator(".fp-item")).toHaveCount(3);
  // Keep only south and empty.
  await list.locator(".fp-item", { hasText: "north" }).getByRole("checkbox").uncheck();
  await expect(pop.getByRole("checkbox", { name: "Select all" })).not.toBeChecked();
  await pop.getByRole("button", { name: "Apply" }).click();
  await expect(pop).toBeHidden();
  await expect(grid).toBeFocused();
  await expect(page.locator(".footer")).toContainText(`${(expected("south") + expected("")).toLocaleString()} rows`);
  await expect(page.locator(".chips")).toContainText("region is");
  // Reopened, it remembers the choice; Clear removes the filter.
  await page.keyboard.press("Alt+ArrowDown");
  await expect(list.locator(".fp-item", { hasText: "north" }).getByRole("checkbox")).not.toBeChecked();
  await expect(list.locator(".fp-item", { hasText: "south" }).getByRole("checkbox")).toBeChecked();
  expect((await new AxeBuilder({ page }).include(".filter-popup").analyze()).violations).toEqual([]);
  await pop.getByRole("button", { name: "Clear" }).click();
  await expect(page.locator(".footer")).toContainText(`${N.toLocaleString()} rows`);
  // Escape closes without changes.
  await page.keyboard.press("Alt+ArrowDown");
  await expect(pop).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(pop).toBeHidden();
  await expect(page.locator(".footer")).toContainText(`${N.toLocaleString()} rows`);
});

test("first run: a dismissible guide, sample data in the empty state, and nothing the server turned off", async ({ page }) => {
  const wb = await server.createWorkbook("First run");
  await page.goto(`${server.url}/w/${wb}`);
  const guide = page.getByRole("region", { name: "Getting started" });
  await expect(guide).toBeVisible();
  await expect(guide).toContainText("Alt");
  await expect(page.getByRole("button", { name: "Try sample data (1M flights)" })).toBeVisible();
  await guide.getByRole("button", { name: "Got it" }).click();
  await expect(guide).toBeHidden();
  await page.reload();
  await expect(page.locator(".status-dot.status-open")).toBeVisible();
  await expect(page.getByRole("region", { name: "Getting started" })).toHaveCount(0);
});

test("with SQL turned off, no SQL control is shown", async ({ page }) => {
  const off = new Server(["--disable-sql"]);
  await off.start();
  try {
    const wb = await off.createWorkbook("No SQL");
    await page.goto(`${off.url}/w/${wb}`);
    await expect(page.locator(".status-dot.status-open")).toBeVisible();
    await expect(page.getByRole("toolbar", { name: "Workbook tools" }).getByRole("button", { name: "SQL", exact: true })).toBeHidden();
    await expect(page.getByRole("button", { name: "Start with SQL" })).toHaveCount(0);
    // Hidden, not merely disabled: the visible text never mentions SQL.
    expect(await page.getByRole("region", { name: "Getting started" }).innerText()).not.toContain("SQL");
  } finally {
    await off.dispose();
  }
});

for (const theme of ["light", "dark"] as const) {
  test(`the Analyze pane, its chart and the find bar pass axe in the ${theme} theme`, async ({ page }) => {
    await page.addInitScript((t) => { try { localStorage.setItem("alephgrid-theme", t); localStorage.setItem("alephgrid-welcome-done", "1"); } catch { /* */ } }, theme);
    await open(page, `Axe ${theme}`);
    await page.getByRole("button", { name: "Analyze", exact: true }).click();
    const pane = page.getByRole("complementary", { name: "Analyze" });
    await pane.getByRole("checkbox", { name: "Totals and subtotals" }).uncheck();
    await pane.getByRole("button", { name: "Run", exact: true }).click();
    await expect(page.getByRole("region", { name: "Analysis result" }).getByRole("img")).toBeVisible();
    // Chart values match the result table.
    const res = page.getByRole("region", { name: "Analysis result" });
    const chartRows = await res.getByRole("table", { name: "Chart values" }).locator("tbody tr").allTextContents();
    const resultRows = await res.locator("table.result-table tbody tr").allTextContents();
    expect(chartRows.length).toBe(resultRows.length);
    expect((await new AxeBuilder({ page }).include(".analyze-pane").include(".analyze-results").analyze()).violations).toEqual([]);
    await page.locator(".grid-host").focus();
    await page.keyboard.press("ControlOrMeta+f");
    await page.getByRole("search").getByRole("searchbox", { name: "Find" }).fill("north");
    await expect(page.getByRole("search").getByRole("status")).toHaveText(`${expected("north").toLocaleString()} matches`);
    expect((await new AxeBuilder({ page }).include(".find-bar").analyze()).violations).toEqual([]);
  });
}
