// U.01 / U.12 / U.T01: light, dark and system themes. Expected colors are the tokens published in
// docs/DESIGN.md (the design contract), not values read back from the stylesheet under test.

import AxeBuilder from "@axe-core/playwright";
import { expect, test, type Page } from "@playwright/test";
import { Server } from "./server";

const server = new Server();
test.beforeAll(async () => server.start());
test.afterAll(async () => server.dispose());

const LIGHT_BG = "rgb(255, 255, 255)"; // --bg light, #FFFFFF
const DARK_BG = "rgb(24, 24, 27)"; // --bg dark, #18181B
const DARK_GRID = [17, 17, 19]; // --grid-bg dark, #111113
const LIGHT_GRID = [255, 255, 255]; // --grid-bg light, #FFFFFF

async function workbook(): Promise<string> {
  const id = (await server.api<{ id: string }>("/api/workbooks", {
    method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ name: "Themed" }),
  })).id;
  await server.api(`/api/workbooks/${id}/import?filename=people.csv`, { method: "POST", body: "id,name\n1,Ada\n2,Linus\n" });
  return id;
}

const bodyBg = (page: Page) => page.evaluate(() => getComputedStyle(document.body).backgroundColor);

/** A pixel of the canvas grid's empty area (bottom right), as drawn. */
async function gridPixel(page: Page): Promise<number[]> {
  return page.evaluate(() => {
    const c = document.querySelector<HTMLCanvasElement>(".grid-canvas")!;
    const d = c.getContext("2d")!.getImageData(c.width - 4, c.height - 4, 1, 1).data;
    return [d[0], d[1], d[2]];
  });
}

async function seriousViolations(page: Page): Promise<string[]> {
  const results = await new AxeBuilder({ page }).analyze();
  return results.violations
    .filter((v) => v.impact === "serious" || v.impact === "critical")
    .map((v) => `${v.id}: ${v.help} (${v.nodes.map((n) => n.target.join(" ")).join(", ")})`);
}

test("the first paint follows a dark system before any script runs (no theme flash)", async ({ browser }) => {
  const context = await browser.newContext({ colorScheme: "dark", javaScriptEnabled: false });
  const page = await context.newPage();
  await page.goto(server.url);
  expect(await bodyBg(page)).toBe(DARK_BG);
  await context.close();
});

test("system theme follows changes; a chosen theme persists across reloads and repaints the grid", async ({ page }) => {
  const wb = await workbook();
  await page.emulateMedia({ colorScheme: "light" });
  await page.goto(`${server.url}/w/${wb}`);
  await expect(page.locator(".status-dot.status-open")).toBeVisible();
  await expect(page.locator("html")).toHaveAttribute("data-theme", "light");
  expect(await bodyBg(page)).toBe(LIGHT_BG);
  await expect.poll(() => gridPixel(page)).toEqual(LIGHT_GRID);

  // The system switches to dark: so does the page, grid included.
  await page.emulateMedia({ colorScheme: "dark" });
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
  expect(await bodyBg(page)).toBe(DARK_BG);
  await expect.poll(() => gridPixel(page)).toEqual(DARK_GRID);

  // Choosing Light from the View menu wins over the dark system, and survives a reload.
  await page.getByRole("navigation", { name: "Workbook menus" }).getByRole("button", { name: "View", exact: true }).click();
  await page.getByRole("menuitem", { name: "Light theme" }).click();
  await expect(page.locator("html")).toHaveAttribute("data-theme", "light");
  await expect.poll(() => gridPixel(page)).toEqual(LIGHT_GRID);
  await page.reload();
  await expect(page.locator(".status-dot.status-open")).toBeVisible();
  await expect(page.locator("html")).toHaveAttribute("data-theme", "light");
  await expect.poll(() => gridPixel(page)).toEqual(LIGHT_GRID);
  await expect(page.getByRole("button", { name: "Appearance: Light theme" })).toBeVisible();

  // The top bar button cycles light → dark, and the hub shares the choice.
  await page.getByRole("button", { name: "Appearance: Light theme" }).click();
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
  await page.goto(server.url);
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
  await expect(page.getByRole("button", { name: "Appearance: Dark theme" })).toBeVisible();
});

test("dark theme pages have no serious axe violations (contrast included)", async ({ page }) => {
  const wb = await workbook();
  await page.emulateMedia({ colorScheme: "dark" });
  await page.goto(server.url);
  await expect(page.locator(".wb-row", { hasText: "Themed" }).first()).toBeVisible();
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
  expect(await seriousViolations(page)).toEqual([]);
  await page.goto(`${server.url}/w/${wb}`);
  await expect(page.getByRole("grid", { name: "Table people" })).toBeVisible();
  expect(await seriousViolations(page)).toEqual([]);
  await page.getByRole("navigation", { name: "Workbook menus" }).getByRole("button", { name: "Data", exact: true }).click();
  await expect(page.getByRole("menu")).toBeVisible();
  expect(await seriousViolations(page)).toEqual([]);
  await page.keyboard.press("Escape");
  await page.getByRole("button", { name: "Filter", exact: true }).click();
  await expect(page.getByRole("dialog", { name: "Filter rows" })).toBeVisible();
  expect(await seriousViolations(page)).toEqual([]);
});
