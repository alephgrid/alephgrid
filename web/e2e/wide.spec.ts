// 3.05 in the browser: a 300-column table only fetches the columns on screen. The far-right
// cell is reached with the keyboard and read back from the formula bar; the WebSocket
// frames show that every fetch asks for a narrow column range.

import { expect, test, type Page } from "@playwright/test";
import { Server } from "./server";

const server = new Server();
test.beforeAll(async () => server.start());
test.afterAll(async () => server.dispose());

async function activeCell(page: Page): Promise<{ ref: string; value: string }> {
  return {
    ref: (await page.locator(".cell-ref").textContent()) ?? "",
    value: await page.locator(".formula-input").inputValue(),
  };
}

test("a 300-column table fetches only the visible column range", async ({ page }) => {
  const wb = await server.createWorkbook("Wide");
  const ws = await server.connect(wb);
  // Cell (row i, column c<k>) holds i * 1000 + k.
  const cols = Array.from({ length: 300 }, (_, k) => `i * 1000 + ${k} AS c${k}`).join(", ");
  await ws.call("create_table_from_query", { name: "wide", sql: `SELECT ${cols} FROM range(500) t(i)` });
  ws.close();

  const fetches: { column_start?: number; column_count?: number; row_start: number }[] = [];
  page.on("websocket", (socket) => socket.on("framesent", (f) => {
    const msg = JSON.parse(String(f.payload));
    if (msg.op === "fetch") fetches.push(msg);
  }));

  await page.goto(`${server.url}/w/${wb}`);
  await expect.poll(() => activeCell(page)).toEqual({ ref: "c0 · 1", value: "0" });
  await page.locator(".grid-host").focus();
  await page.keyboard.press("End");
  for (let i = 0; i < 3; i++) await page.keyboard.press("ArrowDown");
  await expect.poll(() => activeCell(page)).toEqual({ ref: "c299 · 4", value: "3299" });
  // A little to the left, still far from the start.
  for (let i = 0; i < 5; i++) await page.keyboard.press("ArrowLeft");
  await expect.poll(() => activeCell(page)).toEqual({ ref: "c294 · 4", value: "3294" });

  expect(fetches.length).toBeGreaterThan(0);
  for (const f of fetches) {
    expect(typeof f.column_start).toBe("number");
    expect(f.column_count).toBeLessThanOrEqual(32);
  }
  // The far right was fetched; the middle, never on screen, was not.
  expect(fetches.some((f) => f.column_start! >= 256)).toBe(true);
  expect(fetches.some((f) => f.column_start! >= 96 && f.column_start! < 192)).toBe(false);
});
