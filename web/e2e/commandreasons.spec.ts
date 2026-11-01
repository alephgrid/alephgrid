// U.04a / U.T05: a command that does not apply here is disabled and says why, in place of
// its shortcut; the same command works where it applies.

import { expect, test } from "@playwright/test";
import { Server } from "./server";

const server = new Server();
test.beforeAll(async () => server.start());
test.afterAll(async () => server.dispose());

test("disabled menu items say why; they work where they apply", async ({ page }) => {
  const wb = await server.createWorkbook("Reasons"), ws = await server.connect(wb);
  await ws.call("create_table_from_query", { name: "t", sql: "SELECT 1 AS x" });
  await ws.call("sheet_create", { name: "S" });
  await page.addInitScript(() => { try { localStorage.setItem("alephgrid-welcome-done", "1"); } catch { /* */ } });
  await page.goto(`${server.url}/w/${wb}`);
  const bar = page.getByRole("navigation", { name: "Workbook menus" });

  // In a table, sheet-only commands are disabled with "Sheets only".
  await page.getByRole("tab", { name: /^t/ }).click();
  await bar.getByRole("button", { name: "Edit", exact: true }).click();
  const edit = page.getByRole("menu", { name: "Edit" });
  const pasteSpecial = edit.getByRole("menuitem", { name: /^Paste special…/ });
  await expect(pasteSpecial).toBeDisabled();
  await expect(pasteSpecial).toContainText("Sheets only");
  await expect(pasteSpecial).toHaveAttribute("title", "Sheets only");
  // Table commands are enabled and show their shortcut, not a reason.
  const find = edit.getByRole("menuitem", { name: /^Find…/ });
  await expect(find).toBeEnabled();
  await expect(find).not.toContainText("only");
  await page.keyboard.press("Escape");

  // In a sheet it is the other way round.
  await page.getByRole("tab", { name: /^S/ }).click();
  await bar.getByRole("button", { name: "Edit", exact: true }).click();
  await expect(edit.getByRole("menuitem", { name: /^Paste special…/ })).toBeEnabled();
  // Find works in sheets too (it was wrongly marked tables-only).
  await edit.getByRole("menuitem", { name: /^Find…/ }).click();
  await expect(page.getByRole("search")).toBeVisible();
  ws.close();
});
