// L.05: the team screens in a local server: Share on a workbook (U.08), space settings
// (U.09) and server administration (U.10), keyboard reachable and without serious axe
// violations.

import AxeBuilder from "@axe-core/playwright";
import { expect, test, type Page } from "@playwright/test";
import { Server } from "./server";

const server = new Server();
test.beforeAll(async () => server.start());
test.afterAll(async () => server.dispose());

async function serious(page: Page, selector: string): Promise<string[]> {
  const r = await new AxeBuilder({ page }).include(selector).analyze();
  return r.violations.filter((v) => v.impact === "serious" || v.impact === "critical").map((v) => v.id);
}

test("U.08: Share shows the link, who can open it and the next steps", async ({ page }) => {
  const wb = await server.createWorkbook("Shared book");
  await page.addInitScript(() => { try { localStorage.setItem("alephgrid-welcome-done", "1"); } catch { /* */ } });
  await page.goto(`${server.url}/w/${wb}`);
  await page.getByRole("button", { name: "Share" }).click();
  const dialog = page.getByRole("dialog", { name: "Share “Shared book”" });
  await expect(dialog.getByRole("textbox", { name: "Workbook link" })).toHaveValue(`${server.url}/w/${wb}`);
  await expect(dialog).toContainText("General");
  await expect(dialog).toContainText("runs without sign-in");
  await expect(dialog.getByRole("button", { name: "Agent connections…" })).toBeVisible();
  expect(await serious(page, ".share-dialog")).toEqual([]);
  await dialog.getByRole("button", { name: "Change history…" }).click();
  await expect(page.getByRole("dialog", { name: "Change history" })).toBeVisible();
});

test("U.09: space settings gather members, agents, storage and compute", async ({ page }) => {
  await server.createWorkbook("One");
  await page.addInitScript(() => { try { localStorage.setItem("alephgrid-welcome-done", "1"); } catch { /* */ } });
  await page.goto(server.url);
  await page.getByRole("navigation", { name: "Spaces" }).getByRole("button", { name: /^General/ }).first().click({ button: "right" });
  await page.getByRole("menuitem", { name: "Settings…" }).click();
  const dialog = page.getByRole("dialog", { name: "Settings of “General”" });
  for (const s of ["Members", "Agents", "Storage and limits", "Compute"]) await expect(dialog.getByRole("region", { name: s })).toBeVisible();
  await expect(dialog.getByRole("region", { name: "Storage and limits" })).toContainText(/\d+ workbooks?, /);
  expect(await serious(page, ".space-settings")).toEqual([]);
  await dialog.getByRole("button", { name: "Agent connections…" }).click();
  await expect(page.getByRole("dialog", { name: "Agent connections · General" })).toBeVisible();
});

test("U.10: administration shows the server, workbooks, sessions, backups, usage and audit", async ({ page }) => {
  const wb = await server.createWorkbook("Admin book"), ws = await server.connect(wb);
  await ws.call("create_table_from_query", { name: "t", sql: "SELECT 1 AS x" });
  await page.addInitScript(() => { try { localStorage.setItem("alephgrid-welcome-done", "1"); } catch { /* */ } });
  await page.goto(server.url);
  await page.getByRole("button", { name: "Administration" }).click();
  const dialog = page.getByRole("dialog", { name: "Server administration" });
  const panel = dialog.getByRole("tabpanel");
  await expect(panel).toContainText("Local (no sign-in");
  await expect(panel).toContainText("Memory budget");
  // The tabs move with the arrow keys.
  await dialog.getByRole("tab", { name: "Overview" }).press("ArrowRight");
  await expect(dialog.getByRole("tab", { name: "Workbooks and memory" })).toHaveAttribute("aria-selected", "true");
  await expect(panel.getByRole("table", { name: "Open workbooks" })).toContainText(wb);
  await dialog.getByRole("tab", { name: "Sessions" }).click();
  await expect(panel.getByRole("table", { name: "Live sessions" })).toContainText(wb);
  await dialog.getByRole("tab", { name: "Backups" }).click();
  await panel.getByRole("button", { name: "Back up now" }).click();
  await expect(panel.getByRole("table", { name: "Backups" })).toBeVisible();
  await dialog.getByRole("tab", { name: "Usage and limits" }).click();
  await expect(panel.getByRole("table", { name: "Usage per space" })).toContainText("General");
  await dialog.getByRole("tab", { name: "Audit log" }).click();
  await expect(panel.getByRole("table", { name: "Audit log" })).toContainText("backup_created");
  expect(await serious(page, ".admin-dialog")).toEqual([]);
  ws.close();
});
