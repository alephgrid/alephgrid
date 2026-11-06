// U.04a: the searchable list of every sheet and table, opened from the tab bar.

import { expect, test } from "@playwright/test";
import { Server } from "./server";

const server = new Server();
test.beforeAll(async () => server.start());
test.afterAll(async () => server.dispose());

test.beforeEach(async ({ page }) => {
  await page.addInitScript(() => { try { localStorage.setItem("alephgrid-welcome-done", "1"); } catch { /* */ } });
});

test("U.04a: the sheet list finds a sheet or table by name and opens it", async ({ page }) => {
  const wb = await server.createWorkbook("Many sheets"), ws = await server.connect(wb);
  await ws.call("create_table_from_query", { name: "orders", sql: "SELECT 1 AS id" });
  const sheetNames = ["January", "February", "March", "April", "Q1 summary", "Q2 plan", "Q3 plan", "Notes"];
  for (const name of sheetNames) await ws.call("sheet_create", { name });
  ws.close();
  await page.goto(`${server.url}/w/${wb}`);
  await expect(page.getByRole("grid", { name: "Table orders" })).toBeVisible();

  const opener = page.getByRole("button", { name: "All sheets and tables" });
  await opener.click();
  const list = page.getByRole("dialog", { name: "All sheets and tables" });
  const search = list.getByRole("combobox", { name: "Find a sheet or table" });
  await expect(search).toBeFocused();
  const options = list.getByRole("option");
  await expect(options).toHaveCount(9);
  await expect(list.getByRole("option", { name: /orders/ })).toHaveAttribute("aria-selected", "true");

  // Typing narrows the list; nothing matching says so.
  await search.fill("zzz");
  await expect(options).toHaveCount(0);
  await expect(list).toContainText("No sheets or tables match “zzz”.");
  await search.fill("plan");
  await expect(options).toHaveText([/Q2 plan/, /Q3 plan/]);
  // Arrow keys move the highlight; Enter opens it.
  await page.keyboard.press("ArrowDown");
  await expect(search).toHaveAttribute("aria-activedescendant", (await options.nth(1).getAttribute("id"))!);
  await page.keyboard.press("Enter");
  await expect(list).toHaveCount(0);
  await expect(page.getByRole("tab", { name: /Q3 plan/ })).toHaveAttribute("aria-selected", "true");

  // Escape closes it and returns focus to its button.
  await opener.click();
  await expect(search).toBeFocused();
  await page.keyboard.press("Escape");
  await expect(list).toHaveCount(0);
  await expect(opener).toBeFocused();
});
