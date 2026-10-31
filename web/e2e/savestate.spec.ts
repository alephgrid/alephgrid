// U.11: the top bar says "Saved" only after the server acknowledged every change the page sent
// (it acknowledges after commit). While a reply is outstanding it says "Saving…"; while the
// connection is down with changes kept, "Unsaved changes". The stored value is read back from
// the server through a separate connection, not from the page.

import { expect, test, type Page } from "@playwright/test";
import { Server } from "./server";

const server = new Server();
test.beforeAll(async () => server.start());
test.afterAll(async () => server.dispose());

async function sheetWorkbook(name: string): Promise<string> {
  const wb = await server.createWorkbook(name);
  const ws = await server.connect(wb);
  await ws.call("sheet_create", { name: "Sheet1" });
  ws.close();
  return wb;
}

async function typeA1(page: Page, text: string): Promise<void> {
  await page.locator(".grid-host").focus();
  await page.keyboard.press("ControlOrMeta+Home");
  await page.keyboard.type(text);
  await page.keyboard.press("Enter");
}

async function storedA1(wb: string): Promise<unknown> {
  const ws = await server.connect(wb);
  try {
    const r = await ws.call("sheet_fetch", { sheet: 0, row_start: 0, row_count: 1, column_count: 1 });
    return r.rows[0]?.[0]?.[0] ?? null;
  } finally { ws.close(); }
}

const status = (page: Page) => page.locator(".status-text");

test("Saving… while the reply is outstanding, Saved once acknowledged", async ({ page }) => {
  const wb = await sheetWorkbook("Save state");
  let hold = false;
  const held: (() => void)[] = [];
  await page.routeWebSocket(/\/ws\//, (ws) => {
    const srv = ws.connectToServer();
    ws.onMessage((m) => srv.send(m));
    srv.onMessage((m) => { if (hold) held.push(() => ws.send(m)); else ws.send(m); });
  });
  await page.goto(`${server.url}/w/${wb}`);
  await expect(page.locator(".status-dot.status-open")).toBeVisible();
  await expect(page.getByRole("grid", { name: /^Sheet / })).toHaveAttribute("aria-colcount", /\d+/);
  await expect(status(page)).toHaveText("Saved");

  hold = true;
  await typeA1(page, "41");
  await expect(status(page)).toHaveText("Saving…");
  // The server has it already; the page has not heard back, so it must not claim Saved.
  await expect.poll(() => storedA1(wb)).toBe("41");
  await expect(status(page)).toHaveText("Saving…");

  hold = false;
  for (const send of held.splice(0)) send();
  await expect(status(page)).toHaveText("Saved");
});

test("a change made while the server is down is Unsaved, then Saved after it comes back", async ({ page }) => {
  const wb = await sheetWorkbook("Offline edit");
  await page.goto(`${server.url}/w/${wb}`);
  await expect(page.locator(".status-dot.status-open")).toBeVisible();
  await expect(page.getByRole("grid", { name: /^Sheet / })).toHaveAttribute("aria-colcount", /\d+/);

  await server.stop("SIGTERM");
  await expect(page.locator(".status-dot.status-closed")).toBeVisible();
  await typeA1(page, "7");
  await expect(status(page)).toHaveText("Unsaved changes");

  await server.start();
  await expect(status(page)).toHaveText("Saved", { timeout: 30_000 });
  expect(await storedA1(wb)).toBe("7");
});
