// Switching tables while one is still opening: the table whose tab is selected is the one
// shown, whatever order the server's replies arrive in.

import { expect, test } from "@playwright/test";
import { Server } from "./server";

const server = new Server();
test.beforeAll(async () => server.start());
test.afterAll(async () => server.dispose());

test("a table that finishes opening after another tab was chosen does not replace it", async ({ page }) => {
  const wb = await server.createWorkbook("Tabs"), ws = await server.connect(wb);
  await ws.call("create_table_from_query", { name: "people", sql: "SELECT * FROM range(3) t(id)" });
  await ws.call("create_table_from_query", { name: "pets", sql: "SELECT * FROM range(2) t(id)" });
  // Hold the reply that opens "pets", and count the replies that open "people".
  let releasePets: (() => void) | undefined;
  let petsId = -1, peopleOpened = 0;
  const peopleIds = new Set<number>();
  await page.routeWebSocket(/\/ws\//, (client) => {
    const srv = client.connectToServer();
    client.onMessage((m) => {
      const msg = JSON.parse(String(m));
      if (msg.op === "open_view" && msg.spec?.table === "pets" && petsId === -1) petsId = msg.id;
      if (msg.op === "open_view" && msg.spec?.table === "people") peopleIds.add(msg.id);
      srv.send(m);
    });
    srv.onMessage((m) => {
      const msg = JSON.parse(String(m));
      if (msg.id === petsId && !releasePets) { releasePets = () => client.send(m); return; }
      if (peopleIds.has(msg.id)) peopleOpened++;
      client.send(m);
    });
  });
  await page.goto(`${server.url}/w/${wb}`);
  await page.getByRole("tab", { name: "people", exact: true }).click();
  await expect(page.locator(".footer")).toContainText("3 rows");
  const before = peopleOpened;
  // Open pets, and go back to people before pets has opened.
  await page.getByRole("tab", { name: "pets", exact: true }).click();
  await expect.poll(() => !!releasePets).toBe(true);
  await page.getByRole("tab", { name: "people", exact: true }).click();
  await expect.poll(() => peopleOpened).toBeGreaterThan(before);
  releasePets!();
  // The late reply for pets changes nothing: people stays, with its own rows.
  await page.waitForTimeout(500);
  await expect(page.getByRole("grid", { name: "Table people" })).toBeVisible();
  await expect(page.getByRole("tab", { name: "people", exact: true })).toHaveAttribute("aria-selected", "true");
  await expect(page.locator(".footer")).toContainText("3 rows");
  await expect(page.locator(".footer")).not.toContainText("2 rows");
});
