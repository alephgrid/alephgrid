// U.06a–d / U.T06: the functions side panel keeps the grid visible, resizes by keyboard and
// remembers its width, keeps its draft while closed, shares the right side with Analyze,
// runs tests only when asked (Mod+Enter in the panel) and marks results stale after edits.

import AxeBuilder from "@axe-core/playwright";
import { expect, test } from "@playwright/test";
import { Server } from "./server";

const server = new Server();
test.beforeAll(async () => server.start());
test.afterAll(async () => server.dispose());

test("the functions panel docks beside the grid, resizes, keeps its draft and runs only on request", async ({ page }) => {
  const wb = await server.createWorkbook("Function panel");
  const ops: string[] = [];
  await page.routeWebSocket(/\/ws\//, (ws) => {
    const srv = ws.connectToServer();
    ws.onMessage((m) => { try { ops.push(JSON.parse(String(m)).op); } catch { /* */ } srv.send(m); });
    srv.onMessage((m) => ws.send(m));
  });
  await page.addInitScript(() => { try { localStorage.setItem("alephgrid-welcome-done", "1"); } catch { /* */ } });
  await page.goto(`${server.url}/w/${wb}`);
  await expect(page.locator(".status-dot.status-open")).toBeVisible();
  await page.getByRole("button", { name: "Functions", exact: true }).click();
  const panel = page.getByRole("complementary", { name: "Functions" });
  await expect(panel).toBeVisible();
  await expect(page.locator(".grid-host")).toBeVisible();
  expect((await new AxeBuilder({ page }).include(".fn-panel").analyze()).violations.filter((v) => v.impact === "serious" || v.impact === "critical")).toEqual([]);

  // Opening the panel, or an example, never runs code.
  await panel.getByRole("button", { name: "Start from an example" }).click();
  expect(ops.filter((o) => o === "test_function")).toEqual([]);

  // Resize with the keyboard; the width is remembered after a reload.
  const width = async () => (await panel.boundingBox())!.width;
  const before = await width();
  const divider = panel.getByRole("separator", { name: "Resize functions panel" });
  await divider.focus();
  await page.keyboard.press("ArrowLeft"); await page.keyboard.press("ArrowLeft");
  await expect.poll(width).toBeGreaterThan(before + 40);
  const wider = await width();

  // Mod+Enter in the panel runs the tests.
  await panel.getByRole("textbox", { name: "Function source" }).focus();
  await page.keyboard.press("ControlOrMeta+Enter");
  await expect(panel.getByRole("region", { name: "Test results" })).toContainText("Passed · test 1 · actual 0.4");
  await expect(panel.locator(".fn-state")).toHaveText("Tested · ready to publish");
  // An edit makes those results stale.
  await panel.getByRole("textbox", { name: "Function source" }).fill("revenue-cost");
  await expect(panel.getByRole("region", { name: "Test results" })).toContainText("Edited since this run");
  await expect(panel.locator(".fn-state")).toHaveText("Draft");

  // Opening Analyze closes the panel; reopening it brings the draft back.
  await page.getByRole("button", { name: "Analyze", exact: true }).click();
  await expect(page.getByRole("complementary", { name: "Analyze" })).toBeVisible();
  await expect(panel).toHaveCount(0);
  await page.getByRole("button", { name: "Functions", exact: true }).click();
  await expect(page.getByRole("complementary", { name: "Analyze" })).toHaveCount(0);
  await expect(panel.getByRole("textbox", { name: "Function source" })).toHaveValue("revenue-cost");
  expect(ops.filter((o) => o === "test_function").length).toBe(1);

  // Escape closes the panel (outside the code editor) and nothing was published.
  await panel.getByRole("tab", { name: "Docs" }).focus();
  await page.keyboard.press("Escape");
  await expect(panel).toHaveCount(0);
  expect(ops).not.toContain("publish_function");

  await page.reload();
  await page.getByRole("button", { name: "Functions", exact: true }).click();
  await expect.poll(width).toBeGreaterThan(wider - 2);
});
