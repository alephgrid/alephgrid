// 2.07: saved views (live) with filters and hidden columns survive a reload; snapshots
// copy rows into a new table.

import { expect, test } from "@playwright/test";
import { Server } from "./server";

const server = new Server();
test.beforeAll(async () => server.start());
test.afterAll(async () => server.dispose());

test("save a view, reload, re-apply it, and snapshot it", async ({ page }) => {
  const wb = (await server.api<{ id: string }>("/api/workbooks", {
    method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ name: "Views" }),
  })).id;
  await server.api(`/api/workbooks/${wb}/import?filename=people.csv`, { method: "POST", body: "id,name,amount\n1,Ada,10\n2,Linus,-3\n3,Grace,7\n" });
  // Keep opening in flight so toolbar readiness is tested independently of speed.
  let releasePresentation: (() => void) | undefined;
  let presentationId = -1;
  await page.routeWebSocket(/\/ws\//, ws => {
    const srv = ws.connectToServer();
    ws.onMessage(m => {
      const msg = JSON.parse(String(m));
      if (msg.op === "presentation" && presentationId === -1) presentationId = msg.id;
      srv.send(m);
    });
    srv.onMessage(m => {
      const msg = JSON.parse(String(m));
      if (msg.id === presentationId && !releasePresentation) releasePresentation = () => ws.send(m);
      else ws.send(m);
    });
  });
  await page.goto(`${server.url}/w/${wb}`);
  await expect(page.getByRole("grid", { name: "Table people" })).toBeVisible();
  await expect.poll(() => !!releasePresentation).toBe(true);
  await expect(page.getByRole("button", { name: "Filter", exact: true })).toBeDisabled();
  releasePresentation!();

  // Filter amount >= 0 and hide the id column.
  await page.getByRole("button", { name: "Filter", exact: true }).click();
  const f = page.getByRole("dialog", { name: "Filter rows" });
  await f.locator('select[name="column"]').selectOption("amount");
  await f.locator('select[name="op"]').selectOption("gte");
  await f.locator('input[name="value"]').fill("0");
  await f.getByRole("button", { name: "Apply filter" }).click();
  await page.locator(".grid-scroller").click({ button: "right", position: { x: 70, y: 16 } });
  await page.getByRole("menuitem", { name: "Hide column" }).click();
  await expect(page.locator(".chips")).toContainText("1 hidden column");

  // Save it.
  await page.getByRole("toolbar", { name: "Workbook tools" }).getByRole("button", { name: "Views", exact: true }).click();
  await page.getByRole("menuitem", { name: "Save view as…" }).click();
  await page.getByRole("dialog", { name: "Save view" }).getByLabel("View name").fill("Positive");
  await page.getByRole("dialog", { name: "Save view" }).getByRole("button", { name: "Save view" }).click();
  await expect(page.locator(".chip-view")).toHaveText("View: Positive");

  // Reload: the table opens plain; the saved view brings everything back.
  await page.reload();
  await expect(page.locator(".grid-host")).toBeVisible();
  await expect(page.locator(".chip-view")).toHaveCount(0);
  await page.getByRole("toolbar", { name: "Workbook tools" }).getByRole("button", { name: "Views", exact: true }).click();
  await page.getByRole("menuitem", { name: "Positive" }).click();
  await expect(page.locator(".chip-view")).toHaveText("View: Positive");
  await expect(page.locator(".chips")).toContainText("amount");
  await expect(page.locator(".chips")).toContainText("1 hidden column");
  await page.locator(".grid-host").focus();
  await page.keyboard.press("ControlOrMeta+Home");
  await expect(page.locator(".cell-ref")).toHaveText("name · 1");
  await expect(page.locator(".formula-input")).toHaveValue("Ada");
  await expect(page.locator(".row-count")).toHaveText("2 rows");

  // Snapshot.
  await page.getByRole("toolbar", { name: "Workbook tools" }).getByRole("button", { name: "Views", exact: true }).click();
  await page.getByRole("menuitem", { name: "Save rows as a new table (snapshot)…" }).click();
  const snap = page.getByRole("dialog", { name: "Save rows as a new table" });
  await expect(snap).toContainText("snapshot");
  await snap.getByRole("button", { name: "Create table" }).click();
  await expect(page.getByRole("tab", { name: /people_snapshot/ })).toBeVisible();
  expect(await server.exportCsv(wb, { table: "people_snapshot", sort: [], filters: [], hidden: [] })).toBe("name,amount\nAda,10\nGrace,7\n");
});
