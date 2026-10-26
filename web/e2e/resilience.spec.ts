// Phase 3 failure handling in the browser: a workbook the server cannot open says why and
// can be retried; a server restart is survived without a reload (changes made while it
// was down are sent once it is back); a long SQL query can be cancelled.

import { expect, test, type Page } from "@playwright/test";
import { existsSync, readFileSync, writeFileSync } from "node:fs";
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

async function goTo(page: Page, row: number, col: number): Promise<void> {
  await page.locator(".grid-host").focus();
  await page.keyboard.press("ControlOrMeta+Home");
  for (let i = 0; i < row; i++) await page.keyboard.press("ArrowDown");
  for (let i = 0; i < col; i++) await page.keyboard.press("ArrowRight");
}

async function typeInto(page: Page, row: number, col: number, text: string): Promise<void> {
  await goTo(page, row, col);
  await page.keyboard.type(text);
  await page.keyboard.press("Enter");
}

async function importCsv(wb: string, name: string, csv: string): Promise<void> {
  await server.api(`/api/workbooks/${wb}/import?filename=${name}`, { method: "POST", body: csv });
}

/** Overwrite the workbook's database with garbage (server stopped); returns a restore function. */
function corrupt(wb: string): () => void {
  const file = `${server.dataDir}/${wb}/data.duckdb`;
  const saved = [file, `${file}.wal`].filter(existsSync).map((f) => [f, readFileSync(f)] as const);
  writeFileSync(file, Buffer.alloc(64 * 1024, 0x5a));
  return () => { for (const [f, data] of saved) writeFileSync(f, data); };
}

test("a workbook the server cannot open shows why, and Try again opens it once fixed", async ({ page }) => {
  const wb = await server.createWorkbook("Broken");
  await importCsv(wb, "t.csv", "a,b\n1,x\n2,y\n");
  await server.stop("SIGTERM");
  const restore = corrupt(wb);
  await server.start();

  await page.goto(`${server.url}/w/${wb}`);
  const alert = page.getByRole("alert");
  await expect(alert).toContainText("This workbook can't be opened right now");
  await expect(alert).toContainText("not a valid DuckDB database file");
  // Still broken: trying again shows the same message.
  await alert.getByRole("button", { name: "Try again" }).click();
  await expect(page.getByRole("alert")).toContainText("not a valid DuckDB database file");

  restore();
  await page.getByRole("alert").getByRole("button", { name: "Try again" }).click();
  await expect(page.getByRole("tab", { name: /t/ })).toBeVisible();
  await expect.poll(() => activeCell(page)).toEqual({ ref: "a · 1", value: "1" });
  await expect(page.getByRole("alert")).toHaveCount(0);

  // Broken while open: the reconnect fails, the page says why over the grid, and Try
  // again reconnects once it is fixed.
  await server.stop("SIGTERM");
  const restoreAgain = corrupt(wb);
  await server.start();
  const overlay = page.locator(".workbook-failure");
  await expect(overlay).toBeVisible({ timeout: 30_000 });
  await expect(overlay).toContainText("not a valid DuckDB database file");
  restoreAgain();
  await overlay.getByRole("button", { name: "Try again" }).click();
  await expect(page.locator(".status-dot.status-open")).toBeVisible({ timeout: 30_000 });
  await expect(overlay).toBeHidden();
  await goTo(page, 1, 1);
  await expect.poll(() => activeCell(page)).toEqual({ ref: "b · 2", value: "y" });
});

test("edits survive a server restart without a reload, including one made while it was down", async ({ page }) => {
  const wb = await server.createWorkbook("Restart");
  await importCsv(wb, "people.csv", "id,name\n1,Ada\n2,Linus\n3,Grace\n");
  const edits: { mutation_id?: string; view_id: number; edits: { row: number; record?: number; value: string | null }[] }[] = [];
  page.on("websocket", (socket) => socket.on("framesent", (f) => {
    const msg = JSON.parse(String(f.payload));
    if (msg.op === "edit") edits.push(msg);
  }));
  const csv = () => server.exportCsv(wb, { table: "people" });

  await page.goto(`${server.url}/w/${wb}`);
  await expect.poll(() => activeCell(page)).toEqual({ ref: "id · 1", value: "1" });
  await typeInto(page, 1, 1, "Linus T.");
  await expect.poll(csv).toBe("id,name\n1,Ada\n2,Linus T.\n3,Grace\n");
  // Changes carry a mutation id, and existing rows are addressed by record id.
  expect(edits[0].mutation_id).toMatch(/^[0-9a-f-]{32,36}$/);
  expect(edits[0].edits[0].record).toEqual(expect.any(Number));

  await server.stop("SIGTERM");
  await expect(page.locator(".toast", { hasText: "The server is restarting" })).toBeVisible();
  await expect(page.locator(".status-dot.status-closed, .status-dot.status-connecting")).toBeVisible();
  // Typed while the server is down: kept, and sent once it is back.
  await typeInto(page, 2, 1, "Grace H.");
  await server.start();
  await expect(page.locator(".status-dot.status-open")).toBeVisible({ timeout: 30_000 });
  await expect.poll(csv, { timeout: 30_000 }).toBe("id,name\n1,Ada\n2,Linus T.\n3,Grace H.\n");
  const kept = edits.filter((e) => e.edits[0].value === "Grace H.");
  expect(kept).toHaveLength(1);
  expect(kept[0].mutation_id).not.toBe(edits[0].mutation_id);

  // A further edit on the reconnected page is saved too.
  await typeInto(page, 0, 1, "Ada L.");
  await expect.poll(csv).toBe("id,name\n1,Ada L.\n2,Linus T.\n3,Grace H.\n");
  await goTo(page, 2, 1);
  await expect.poll(() => activeCell(page)).toEqual({ ref: "name · 3", value: "Grace H." });
  await expect(page.locator(".toast-error")).toHaveCount(0);
});

test("a long SQL query can be cancelled", async ({ page }) => {
  const wb = await server.createWorkbook("Long query");
  await importCsv(wb, "t.csv", "a\n1\n");
  await page.goto(`${server.url}/w/${wb}`);
  await expect(page.getByRole("tab", { name: /t/ })).toBeVisible();

  await page.getByRole("button", { name: "SQL", exact: true }).click();
  const editor = page.locator(".sql-editor");
  await editor.fill("SELECT sum(i) FROM range(100000000000) r(i)");
  await page.getByRole("button", { name: "Run", exact: true }).click();
  const status = page.locator(".sql-status");
  await expect(status).toContainText("Running");
  // Progress events from the server show as a percentage.
  await expect(status).toHaveText(/Running… \d+%/);
  const cancel = page.getByRole("dialog").getByRole("button", { name: "Cancel", exact: true });
  await expect(cancel).toBeVisible();
  await cancel.click();
  await expect(status).toHaveText("Cancelled.");
  await expect(cancel).toBeHidden();
  await expect(page.locator(".sql-error")).toHaveCount(0);
  await expect(page.locator(".toast-error")).toHaveCount(0);

  // The connection is fine afterwards.
  await editor.fill("SELECT 42 AS answer");
  await page.getByRole("button", { name: "Run", exact: true }).click();
  await expect(page.locator(".result-table")).toContainText("42");
});
