// U.03a / U.03b: the hub's recent cards (at most three), sorting, search with Clear search,
// and the list state kept when coming back from a workbook.

import { expect, test } from "@playwright/test";
import { Server } from "./server";

const server = new Server();
test.beforeAll(async () => server.start());
test.afterAll(async () => server.dispose());

test("hub: three recent cards, sort by name, search and Clear search, state kept on return", async ({ page }) => {
  test.setTimeout(180_000);
  const names = ["Delta", "alpha", "Charlie", "Bravo"];
  const ids: string[] = [];
  for (const n of names) ids.push(await server.createWorkbook(n));
  await page.addInitScript(() => { try { localStorage.setItem("alephgrid-welcome-done", "1"); } catch { /* */ } });
  // Open each once so it counts as recent.
  // Workbooks start one at a time; under a loaded test run a start can queue for a while.
  for (const id of ids) {
    await page.goto(`${server.url}/w/${id}`);
    await expect(page.locator(".status-dot.status-open")).toBeVisible({ timeout: 60_000 });
  }
  await page.goto(server.url);
  const cards = page.getByRole("region", { name: "Recently opened" }).locator(".wb-card");
  await expect(cards).toHaveCount(3);

  // Sort by name (case-insensitive).
  await page.getByRole("combobox", { name: "Sort by" }).selectOption("name");
  await expect(page.locator(".wb-row .wb-name strong")).toHaveText(["alpha", "Bravo", "Charlie", "Delta"]);

  // A search with no match says so and offers Clear search.
  await page.getByRole("searchbox", { name: "Search workbooks" }).fill("zulu");
  await expect(page.locator(".wb-empty")).toContainText("No workbooks match “zulu”.");
  await page.getByRole("button", { name: "Clear search" }).click();
  await expect(page.locator(".wb-row .wb-name strong")).toHaveCount(4);

  // Open one and come back: the sort is kept.
  await page.locator(".wb-name", { hasText: "Charlie" }).click();
  await expect(page.locator(".status-dot.status-open")).toBeVisible({ timeout: 60_000 });
  await page.goBack();
  await expect(page.getByRole("combobox", { name: "Sort by" })).toHaveValue("name");
  await expect(page.locator(".wb-row .wb-name strong")).toHaveText(["alpha", "Bravo", "Charlie", "Delta"]);
});
