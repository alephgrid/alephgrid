// U.04 / U.05 / U.T01: the collapsible workbook explorer and the SQL workspace docked under the
// grid (resizable, collapsible, maximizable, closable), with their state kept across reloads.

import { expect, test, type Page } from "@playwright/test";
import { Server } from "./server";

const server = new Server();
test.beforeAll(async () => server.start());
test.afterAll(async () => server.dispose());

async function workbook(name: string): Promise<string> {
  const wb = await server.createWorkbook(name);
  await server.api(`/api/workbooks/${wb}/import?filename=sales.csv`, { method: "POST", body: "id,amount\n1,10\n2,20\n" });
  const ws = await server.connect(wb);
  await ws.call("sheet_create", { name: "Plan" });
  await ws.call("save_query", { name: "Big sales", sql: "SELECT * FROM sales WHERE amount > 15" });
  ws.close();
  return wb;
}

async function open(page: Page, wb: string): Promise<void> {
  await page.goto(`${server.url}/w/${wb}`);
  await expect(page.locator(".status-dot.status-open")).toBeVisible();
  await expect(page.getByRole("tab", { name: /sales/ })).toBeVisible();
}

const explorer = (page: Page) => page.getByRole("navigation", { name: "Workbook explorer" });
const explorerToggle = (page: Page) => page.getByRole("toolbar", { name: "Workbook tools" }).getByRole("button", { name: "Explorer", exact: true });

test("the explorer lists the workbook's objects, opens them, and stays open across reloads", async ({ page }) => {
  const wb = await workbook("Explorer");
  await open(page, wb);
  await expect(explorer(page)).toBeHidden();
  await expect(explorerToggle(page)).toHaveAttribute("aria-pressed", "false");

  await explorerToggle(page).click();
  await expect(explorerToggle(page)).toHaveAttribute("aria-pressed", "true");
  await expect(explorer(page).getByRole("button", { name: /^sales/ })).toBeVisible();
  await expect(explorer(page).getByRole("button", { name: /^Plan/ })).toBeVisible();
  await expect(explorer(page).getByRole("button", { name: /^Big sales/ })).toBeVisible();

  // A sheet opens from the explorer, and the explorer marks it as current.
  await explorer(page).getByRole("button", { name: /^Plan/ }).click();
  await expect(page.getByRole("tab", { name: /Plan/ })).toHaveAttribute("aria-selected", "true");
  await expect(explorer(page).getByRole("button", { name: /^Plan/ })).toHaveAttribute("aria-current", "true");

  // Kept open after a reload.
  await page.reload();
  await expect(page.locator(".status-dot.status-open")).toBeVisible();
  await expect(explorer(page)).toBeVisible();

  // A saved query opens in the SQL workspace with its SQL loaded.
  await explorer(page).getByRole("button", { name: /^Big sales/ }).click();
  const sql = page.getByRole("region", { name: "SQL" });
  await expect(sql).toBeVisible();
  await expect(page.locator(".sql-editor")).toHaveValue("SELECT * FROM sales WHERE amount > 15");
  await expect(sql.locator(".sql-loaded")).toContainText("Big sales");

  // Closed again from the View menu.
  await page.getByRole("navigation", { name: "Workbook menus" }).getByRole("button", { name: "View", exact: true }).click();
  await page.getByRole("menuitem", { name: /Explorer/ }).click();
  await expect(explorer(page)).toBeHidden();
});

test("the SQL workspace docks under the grid: resize, maximize, restore and close", async ({ page }) => {
  const wb = await workbook("Dock");
  await open(page, wb);
  const grid = page.locator(".grid-wrap");
  const gridBefore = (await grid.boundingBox())!;

  await page.getByRole("button", { name: "SQL", exact: true }).click();
  const sql = page.getByRole("region", { name: "SQL" });
  await expect(sql).toBeVisible();
  // Docked, not modal: the grid stays visible above it and nothing is aria-modal.
  await expect(page.locator('[aria-modal="true"]')).toHaveCount(0);
  const dock = page.locator(".dock");
  const d1 = (await dock.boundingBox())!;
  const g1 = (await grid.boundingBox())!;
  expect(g1.height).toBeLessThan(gridBefore.height);
  expect(g1.height).toBeGreaterThan(50);
  expect(Math.abs(g1.y + g1.height - d1.y)).toBeLessThan(3);

  // The separator resizes it with the arrow keys and reports its size.
  const handle = page.getByRole("separator", { name: "Resize SQL panel" });
  const before = Number(await handle.getAttribute("aria-valuenow"));
  await handle.focus();
  await page.keyboard.press("ArrowUp");
  await page.keyboard.press("ArrowUp");
  await expect(handle).toHaveAttribute("aria-valuenow", String(before + 10));
  expect((await dock.boundingBox())!.height).toBeGreaterThan(d1.height);

  // Maximize hides the grid area; restore brings it back at the chosen size.
  await sql.getByRole("button", { name: "Maximize SQL panel" }).click();
  await expect.poll(async () => (await grid.boundingBox())?.height ?? 0).toBeLessThan(2);
  await sql.getByRole("button", { name: "Restore SQL panel" }).click();
  await expect.poll(async () => (await grid.boundingBox())?.height ?? 0).toBeGreaterThan(50);

  // Close with its button; reopened, it keeps the size.
  await sql.getByRole("button", { name: "Close SQL panel" }).click();
  await expect(sql).toBeHidden();
  expect((await grid.boundingBox())!.height).toBeCloseTo(gridBefore.height, 0);
  await page.getByRole("button", { name: "SQL", exact: true }).click();
  await expect(page.getByRole("separator", { name: "Resize SQL panel" })).toHaveAttribute("aria-valuenow", String(before + 10));
  // Escape inside the panel closes it too.
  await page.locator(".sql-editor").focus();
  await page.keyboard.press("Escape");
  await expect(sql).toBeHidden();
});

test("the SQL panel collapses to its title and keeps its draft and result; the editor resizes from the keyboard", async ({ page }) => {
  const wb = await workbook("Collapse");
  await open(page, wb);
  const grid = page.locator(".grid-wrap");
  const full = (await grid.boundingBox())!.height;
  await page.getByRole("button", { name: "SQL", exact: true }).click();
  const sql = page.getByRole("region", { name: "SQL" });
  const editor = page.locator(".sql-editor");
  await editor.fill("SELECT sum(amount) AS total FROM sales");
  await sql.getByRole("button", { name: "Run", exact: true }).click();
  // 10 + 20 from the fixture.
  await expect(page.locator(".result-table tbody td")).toHaveText(["30"]);
  const docked = (await grid.boundingBox())!.height;

  // The editor and the results share the panel: a separator under the editor resizes it.
  const grip = sql.getByRole("separator", { name: "Resize SQL editor" });
  const h0 = (await editor.boundingBox())!.height;
  const v0 = Number(await grip.getAttribute("aria-valuenow"));
  expect(Math.abs(v0 - h0)).toBeLessThan(2);
  await grip.focus();
  await page.keyboard.press("ArrowDown");
  await page.keyboard.press("ArrowDown");
  await expect(grip).toHaveAttribute("aria-valuenow", String(v0 + 32));
  await expect.poll(async () => Math.round((await editor.boundingBox())!.height - h0)).toBe(32);
  await page.keyboard.press("ArrowUp");
  await expect(grip).toHaveAttribute("aria-valuenow", String(v0 + 16));

  // Collapsed, only its title bar stays and the grid gets the room back.
  const collapse = sql.getByRole("button", { name: "Collapse SQL panel" });
  await expect(collapse).toHaveAttribute("aria-expanded", "true");
  await collapse.click();
  const expand = sql.getByRole("button", { name: "Expand SQL panel" });
  await expect(expand).toHaveAttribute("aria-expanded", "false");
  await expect(editor).toBeHidden();
  await expect(page.locator(".result-table")).toBeHidden();
  await expect(sql.getByRole("heading", { name: "SQL" })).toBeVisible();
  await expect(page.getByRole("separator", { name: "Resize SQL panel" })).toBeHidden();
  await expect.poll(async () => (await grid.boundingBox())!.height).toBeGreaterThan(docked + 80);
  expect((await grid.boundingBox())!.height).toBeLessThan(full);

  // Expanded from the keyboard: the draft, the result and the editor's size are kept.
  await expand.focus();
  await page.keyboard.press("Enter");
  await expect(sql.getByRole("button", { name: "Collapse SQL panel" })).toBeFocused();
  await expect(editor).toHaveValue("SELECT sum(amount) AS total FROM sales");
  await expect(page.locator(".result-table tbody td")).toHaveText(["30"]);
  await expect(grip).toHaveAttribute("aria-valuenow", String(v0 + 16));
  await expect.poll(async () => Math.round((await grid.boundingBox())!.height)).toBe(Math.round(docked));

  // Opening SQL again while it is collapsed expands it.
  await sql.getByRole("button", { name: "Collapse SQL panel" }).click();
  await expect(editor).toBeHidden();
  await page.getByRole("button", { name: "SQL", exact: true }).click();
  await expect(editor).toBeVisible();
  await expect(page.locator(".result-table tbody td")).toHaveText(["30"]);
});

test("the menu bar works from the keyboard: arrows move between menus, Escape returns focus", async ({ page }) => {
  const wb = await workbook("Menu keys");
  await open(page, wb);
  const bar = page.getByRole("navigation", { name: "Workbook menus" });
  const file = bar.getByRole("button", { name: "File", exact: true });
  await file.focus();
  await page.keyboard.press("ArrowRight");
  await expect(bar.getByRole("button", { name: "Edit", exact: true })).toBeFocused();
  await page.keyboard.press("ArrowDown");
  await expect(page.getByRole("menu", { name: "Edit" })).toBeVisible();
  await expect(bar.getByRole("button", { name: "Edit", exact: true })).toHaveAttribute("aria-expanded", "true");
  // Focus moves into the menu, on its first enabled item (Undo is disabled for tables).
  await expect(page.getByRole("menu", { name: "Edit" }).getByRole("menuitem", { disabled: false }).first()).toBeFocused();
  // ArrowRight in an open menu opens the next one.
  await page.keyboard.press("ArrowRight");
  await expect(page.getByRole("menu", { name: "View" })).toBeVisible();
  await expect(bar.getByRole("button", { name: "Edit", exact: true })).toHaveAttribute("aria-expanded", "false");
  await expect(bar.getByRole("button", { name: "View", exact: true })).toHaveAttribute("aria-expanded", "true");
  await page.keyboard.press("ArrowLeft");
  await page.keyboard.press("ArrowLeft");
  await expect(page.getByRole("menu", { name: "File" })).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(page.getByRole("menu")).toHaveCount(0);
  await expect(file).toBeFocused();
  await expect(file).toHaveAttribute("aria-expanded", "false");
});

test("at phone width every menu stays reachable and the page does not scroll sideways", async ({ page }) => {
  const wb = await workbook("Narrow");
  await page.setViewportSize({ width: 390, height: 800 });
  await open(page, wb);
  const help = page.getByRole("navigation", { name: "Workbook menus" }).getByRole("button", { name: "Help", exact: true });
  await help.scrollIntoViewIfNeeded();
  await help.click();
  await expect(page.getByRole("menuitem", { name: /Keyboard shortcuts/ })).toBeVisible();
  await page.keyboard.press("Escape");
  expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(390);
  // The theme and history stay available from the menus when their top-bar buttons hide.
  await page.getByRole("navigation", { name: "Workbook menus" }).getByRole("button", { name: "View", exact: true }).click();
  await expect(page.getByRole("menuitem", { name: /Dark theme/ })).toBeVisible();
});

test("the hub shows each workbook's compute, with sleeping as a normal state", async ({ page }) => {
  const wb = await workbook("Compute state");
  // After a restart nothing is open: the workbook sleeps.
  await server.stop();
  await server.start();
  await page.goto(server.url);
  const row = page.locator(".wb-row", { hasText: "Compute state" });
  await expect(row.locator(".wb-compute")).toHaveText("Sleeping");
  await expect(row.locator(".wb-compute")).toHaveAttribute("title", /Opening the workbook starts it/);
  // Opening it starts its compute; back on the hub it is ready.
  await open(page, wb);
  await page.getByRole("button", { name: "Alephgrid — all workbooks" }).click();
  await expect(page.locator(".wb-row", { hasText: "Compute state" }).locator(".wb-compute")).toHaveText("Ready");
});
