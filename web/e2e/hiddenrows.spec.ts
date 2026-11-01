// S.05: hiding a row never leaves the active cell on it. Layout replies are held back over the
// WebSocket to widen the moment between the change and the grid learning the new sizes; the
// keyboard is used during that moment. Found on CircleCI (Linux Chromium), where the S.05
// journey in sheetformat.spec.ts landed on the hidden row: the selection moved before the
// layout arrived.

import { expect, test } from "@playwright/test";
import { Server } from "./server";

const server = new Server();
test.beforeAll(async () => server.start());
test.afterAll(async () => server.dispose());

test("hiding a row moves the active cell past it, even when the layout reply is slow", async ({ page }) => {
  const wb = await server.createWorkbook("Hidden rows"), ws = await server.connect(wb);
  await ws.call("sheet_create", { name: "S" });
  await ws.call("sheet_set", { sheet: 0, inputs: [0, 1, 2, 3].map((r) => ({ row: r, column: 0, value: `r${r + 1}` })) });
  // Hold back every sheet_layout reply by 800 ms.
  const layoutIds = new Set<number>();
  await page.routeWebSocket(/\/ws\//, (client) => {
    const server = client.connectToServer();
    client.onMessage((m) => {
      try { const f = JSON.parse(String(m)); if (f.op === "sheet_layout") layoutIds.add(f.id); } catch { /* */ }
      server.send(m);
    });
    server.onMessage((m) => {
      let id: number | undefined;
      try { id = JSON.parse(String(m)).id; } catch { /* */ }
      if (id !== undefined && layoutIds.has(id)) setTimeout(() => client.send(m), 800);
      else client.send(m);
    });
  });
  await page.addInitScript(() => { try { localStorage.setItem("alephgrid-welcome-done", "1"); } catch { /* */ } });
  await page.goto(`${server.url}/w/${wb}`);
  await page.getByRole("tab", { name: /^S/ }).click();
  const ref = page.locator(".cell-ref");
  const box = (await page.locator(".grid-scroller").boundingBox())!;
  const rhw = 54, header = 32, row = 24;
  // Select A2 and hide row 2 from the cell menu.
  await page.mouse.click(box.x + rhw + 30, box.y + header + row + 12);
  await expect(ref).toHaveText("A2");
  await page.mouse.click(box.x + rhw + 30, box.y + header + row + 12, { button: "right" });
  await page.getByRole("menuitem", { name: "Hide row", exact: true }).click();
  // Straight away, while the layout reply is still held back, go to A1 and press ↓.
  await page.locator(".grid-host").focus();
  await page.keyboard.press("ControlOrMeta+Home");
  await page.keyboard.press("ArrowDown");
  // Once the layout has arrived, row 2 is hidden and the active cell is not on it.
  await expect.poll(async () => ((await ws.call("sheet_layout", { sheet: 0 })).rows as { index: number; hidden: boolean }[]).some((l) => l.index === 1 && l.hidden)).toBe(true);
  await expect(ref).toHaveText("A3", { timeout: 5_000 });
  // And the keyboard skips it from now on.
  await page.keyboard.press("ControlOrMeta+Home");
  await page.keyboard.press("ArrowDown");
  await expect(ref).toHaveText("A3");
  ws.close();
});
