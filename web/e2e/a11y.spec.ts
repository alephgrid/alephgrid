// 2.09: the canvas grid is exposed as an ARIA grid with an active cell, focus is visible,
// keyboard users can reach and leave every control, and axe finds no serious problems.

import AxeBuilder from "@axe-core/playwright";
import { expect, test, type Page } from "@playwright/test";
import { Server } from "./server";

const server = new Server();
test.beforeAll(async () => server.start());
test.afterAll(async () => server.dispose());

/** A fresh workbook with a small `people` table, so tests do not affect each other. */
async function workbook(): Promise<string> {
  const id = (await server.api<{ id: string }>("/api/workbooks", {
    method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ name: "Accessible" }),
  })).id;
  await server.api(`/api/workbooks/${id}/import?filename=people.csv`, { method: "POST", body: "id,name\n1,Ada\n2,Linus\n" });
  return id;
}

async function seriousViolations(page: Page): Promise<string[]> {
  const results = await new AxeBuilder({ page }).analyze();
  return results.violations
    .filter((v) => v.impact === "serious" || v.impact === "critical")
    .map((v) => `${v.id}: ${v.help} (${v.nodes.map((n) => n.target.join(" ")).join(", ")})`);
}

test("home and workbook pages have no serious axe violations", async ({ page }) => {
  const wb = await workbook();
  await page.goto(server.url);
  await expect(page.locator(".wb-row", { hasText: "Accessible" }).first()).toBeVisible();
  expect(await seriousViolations(page)).toEqual([]);
  await page.goto(`${server.url}/w/${wb}`);
  await expect(page.locator(".status-dot.status-open")).toBeVisible();
  await expect(page.getByRole("grid", { name: "Table people" })).toBeVisible();
  expect(await seriousViolations(page)).toEqual([]);
  // A dialog.
  await page.getByRole("button", { name: "Filter", exact: true }).click();
  await expect(page.getByRole("dialog", { name: "Filter rows" })).toBeVisible();
  expect(await seriousViolations(page)).toEqual([]);
  await page.keyboard.press("Escape");
  await expect(page.getByRole("dialog")).toHaveCount(0);
  // A sheet, and the SQL panel.
  await page.getByRole("button", { name: "New table or sheet" }).click();
  await page.getByRole("menuitem", { name: /New sheet/ }).click();
  await expect(page.getByRole("grid", { name: /Sheet/ })).toBeVisible();
  expect(await seriousViolations(page)).toEqual([]);
});

test("grid exposes the active cell and announces moves", async ({ page }) => {
  const wb = await workbook();
  await page.goto(`${server.url}/w/${wb}`);
  const grid = page.getByRole("grid", { name: "Table people" });
  await expect(grid).toHaveAttribute("aria-rowcount", "3");
  await expect(grid).toHaveAttribute("aria-colcount", "2");
  await grid.focus();
  await page.keyboard.press("ArrowDown");
  await page.keyboard.press("ArrowRight");
  const active = page.locator(`#${await grid.getAttribute("aria-activedescendant")}`);
  await expect(active).toHaveAttribute("role", "gridcell");
  await expect(active).toHaveAttribute("aria-label", "name, row 2: Linus");
  await expect(page.locator("body > [aria-live]")).toHaveText("name, row 2: Linus");
  await page.keyboard.press("Shift+ArrowUp");
  await expect(page.locator("body > [aria-live]")).toHaveText("name, row 1: Ada. 2 by 1 selected");
});

test("keyboard focus is visible and Tab can leave the grid", async ({ page }) => {
  const wb = await workbook();
  await page.goto(`${server.url}/w/${wb}`);
  await expect(page.locator(".status-dot.status-open")).toBeVisible();
  // Reach the grid with the keyboard from the formula bar.
  await expect(page.getByRole("grid", { name: "Table people" })).toHaveAttribute("aria-colcount", "2");
  await page.locator(".formula-input").focus();
  await page.keyboard.press("Tab");
  const grid = page.locator(".grid-host");
  await expect(grid).toBeFocused();
  const ring = await grid.evaluate((el) => getComputedStyle(el, "::after").borderTopStyle);
  expect(ring).toBe("solid");
  // Tab moves between cells, and leaves the grid at the last column.
  await page.keyboard.press("Tab");
  await expect(grid).toBeFocused();
  await page.keyboard.press("Tab");
  await expect(grid).not.toBeFocused();
});

test("tabs use arrow keys and menus are keyboard operable", async ({ page }) => {
  const wb = await workbook();
  await page.goto(`${server.url}/w/${wb}`);
  await expect(page.locator(".status-dot.status-open")).toBeVisible();
  // Add a sheet from the keyboard: the "+" menu opens with its first item focused.
  const add = page.getByRole("button", { name: "New table or sheet" });
  await add.focus();
  await page.keyboard.press("Enter");
  const items = page.getByRole("menuitem");
  await expect(items.first()).toBeFocused();
  await page.keyboard.press("Escape");
  await expect(page.getByRole("menu")).toHaveCount(0);
  await expect(add).toBeFocused();
  await page.keyboard.press("Enter");
  await page.getByRole("menuitem", { name: /New sheet/ }).focus();
  await page.keyboard.press("Enter");
  await expect(page.getByRole("tab", { name: /Sheet/ })).toHaveAttribute("aria-selected", "true");

  // Arrow keys move between tabs and select them.
  await page.getByRole("tab", { name: /Sheet/ }).focus();
  await page.keyboard.press("ArrowLeft");
  const people = page.getByRole("tab", { name: /people/ });
  await expect(people).toHaveAttribute("aria-selected", "true");
  await expect(people).toBeFocused();
  await expect(page.getByRole("grid", { name: "Table people" })).toBeVisible();
});
