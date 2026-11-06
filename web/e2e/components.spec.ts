// U.11a: the shared component contract: disabled controls say why (menu items stay reachable
// by keyboard), refused form values are linked error text, and buttons are busy while waiting.

import { expect, test } from "@playwright/test";
import { Server } from "./server";

const server = new Server();
test.beforeAll(async () => server.start());
test.afterAll(async () => server.dispose());

test.beforeEach(async ({ page }) => {
  await page.addInitScript(() => { try { localStorage.setItem("alephgrid-welcome-done", "1"); } catch { /* */ } });
});

test("U.11a: disabled menu items are reachable by keyboard and say why; activating one does nothing", async ({ page }) => {
  const wb = await server.createWorkbook("Menus"), ws = await server.connect(wb);
  await ws.call("create_table_from_query", { name: "t", sql: "SELECT 1 AS x" });
  await page.goto(`${server.url}/w/${wb}`);
  await expect(page.getByRole("grid", { name: "Table t" })).toBeVisible();
  const edit = page.getByRole("navigation", { name: "Workbook menus" }).getByRole("button", { name: "Edit", exact: true });
  await edit.focus();
  await page.keyboard.press("ArrowDown");
  const menu = page.getByRole("menu", { name: "Edit" });
  await expect(menu).toBeVisible();
  // Home goes to the first item even though it does not apply to tables.
  await page.keyboard.press("Home");
  const undo = menu.getByRole("menuitem", { name: /^Undo/ });
  await expect(undo).toBeFocused();
  await expect(undo).toBeDisabled();
  await expect(undo).toHaveAccessibleDescription("Sheets only");
  // Enter on it runs nothing: the menu stays open and no toast appears.
  await page.keyboard.press("Enter");
  await expect(menu).toBeVisible();
  await expect(page.locator(".toast")).toHaveCount(0);
  await page.keyboard.press("Escape");
  await expect(edit).toBeFocused();
  ws.close();
});

test("U.11a: a form shows a refused value as linked error text and is busy while it waits", async ({ page }) => {
  await page.goto(server.url);
  let release!: () => void;
  const held = new Promise<void>((r) => (release = r));
  let refuse = true;
  await page.route("**/api/spaces", async (route) => {
    if (route.request().method() !== "POST" || !refuse) return route.continue();
    refuse = false;
    await held;
    await route.fulfill({ status: 400, contentType: "application/json", body: JSON.stringify({ error: "A space needs a name" }) });
  });
  await page.getByRole("button", { name: "New space" }).click();
  const dialog = page.getByRole("dialog", { name: "New space" });
  const create = dialog.getByRole("button", { name: "Create" });
  await create.click();
  // While the server decides, Create is busy and cannot be pressed twice.
  await expect(create).toHaveAttribute("aria-busy", "true");
  await expect(create).toBeDisabled();
  release();
  const name = dialog.getByRole("textbox", { name: "Name" });
  await expect(dialog.getByRole("alert")).toHaveText("A space needs a name");
  await expect(name).toHaveAttribute("aria-invalid", "true");
  await expect(name).toHaveAccessibleDescription(/A space needs a name/);
  await expect(name).toBeFocused();
  await expect(create).toBeEnabled();
  await expect(create).not.toHaveAttribute("aria-busy", "true");
  // Fixing it clears the error state and the form goes through.
  await name.fill("Finance");
  await expect(name).not.toHaveAttribute("aria-invalid", "true");
  await create.click();
  await expect(dialog).toHaveCount(0);
  await expect(page.getByRole("heading", { name: "Finance" })).toBeVisible();
});

test("U.11a / U.04a: toolbar controls that cannot run here say why", async ({ page }) => {
  const wb = await server.createWorkbook("Empty toolbar");
  await page.goto(`${server.url}/w/${wb}`);
  await expect(page.locator(".status-dot.status-open")).toBeVisible({ timeout: 60_000 });
  const tools = page.getByRole("toolbar", { name: "Workbook tools" });
  const exportBtn = tools.getByRole("button", { name: "Export" });
  await expect(exportBtn).toBeDisabled();
  await expect(exportBtn).toHaveAccessibleDescription("Open a table or sheet to export it");
});
