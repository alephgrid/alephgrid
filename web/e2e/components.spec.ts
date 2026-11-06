// U.11a: the shared component contract: disabled controls say why (menu items stay reachable
// by keyboard), refused form values are linked error text, and buttons are busy while waiting.

import { expect, test } from "@playwright/test";
import { Server, menuCommand } from "./server";

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

test("U.11a: a cancel stays pending until the server acknowledges it", async ({ page }) => {
  const wb = await server.createWorkbook("Cancelling");
  // Hold the server's answer to the long query, so the moment between asking to cancel and
  // the server confirming it can be seen.
  let queryId = -1;
  let release: (() => void) | undefined;
  await page.routeWebSocket(/\/ws\//, (ws) => {
    const srv = ws.connectToServer();
    ws.onMessage((m) => {
      const msg = JSON.parse(String(m));
      if (msg.op === "query" && String(msg.sql).includes("range(100000000000)")) queryId = msg.id;
      srv.send(m);
    });
    srv.onMessage((m) => {
      const msg = JSON.parse(String(m));
      if (queryId !== -1 && msg.id === queryId) release = () => ws.send(m);
      else ws.send(m);
    });
  });
  await page.goto(`${server.url}/w/${wb}`);
  await expect(page.locator(".status-dot.status-open")).toBeVisible({ timeout: 60_000 });
  await menuCommand(page, "Tools", "SQL workspace");
  const sql = page.getByRole("region", { name: "SQL" });
  await sql.getByRole("combobox", { name: "SQL query" }).fill("SELECT sum(i) FROM range(100000000000) r(i)");
  await sql.getByRole("button", { name: "Run", exact: true }).click();
  await expect(sql.locator(".sql-status")).toContainText("Running");
  const cancel = sql.getByRole("button", { name: "Cancel", exact: true });
  await cancel.click();
  await expect.poll(() => !!release, { timeout: 30_000 }).toBe(true);
  // The server stopped it; until its answer arrives the cancel is pending, not done.
  await expect(sql.locator(".sql-status")).toHaveText("Cancelling…");
  await expect(cancel).toBeDisabled();
  await expect(cancel).toHaveAttribute("aria-busy", "true");
  release!();
  await expect(sql.locator(".sql-status")).toHaveText("Cancelled.");
  await expect(cancel).toBeHidden();
});

test("U.11a: dialogs and panels return focus to what opened them", async ({ page }) => {
  const wb = await server.createWorkbook("Focus"), ws = await server.connect(wb);
  await ws.call("sheet_create", { name: "S" });
  ws.close();
  await page.goto(`${server.url}/w/${wb}`);
  await page.getByRole("tab", { name: /^S/ }).click();
  const tools = page.getByRole("toolbar", { name: "Workbook tools" });
  // A dialog from the toolbar: Escape closes it and focus is back on its button.
  for (const name of ["Go to", "Names"]) {
    const button = tools.getByRole("button", { name, exact: true });
    await button.click();
    await expect(page.getByRole("dialog")).toBeVisible();
    await page.keyboard.press("Escape");
    await expect(page.getByRole("dialog")).toHaveCount(0);
    await expect(button).toBeFocused();
  }
  // A dialog from a menu returns focus to the menu bar button that led there.
  const help = page.getByRole("navigation", { name: "Workbook menus" }).getByRole("button", { name: "Help", exact: true });
  await help.click();
  await page.getByRole("menuitem", { name: /^Keyboard shortcuts/ }).click();
  await expect(page.getByRole("dialog")).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await expect(help).toBeFocused();
});
