// 2.05 and 2.03 in the browser: preview, type overrides, actionable errors with a one-click
// fix, cancelling before and during an import, and xlsx compatibility notes.

import { mkdtempSync, readdirSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, test, type Page } from "@playwright/test";
import { Server } from "./server";

const server = new Server();
test.beforeAll(async () => server.start());
test.afterAll(async () => server.dispose());

/** Upload directories left behind: `.uploads/<workbook>/<upload>/` (each workbook keeps its own folder). */
function uploadsLeft(): number {
  try {
    const root = join(server.dataDir, ".uploads");
    return readdirSync(root).reduce((n, wb) => n + readdirSync(join(root, wb)).length, 0);
  } catch {
    return 0;
  }
}

async function newWorkbook(page: Page, name: string): Promise<string> {
  const wb = await server.api<{ id: string }>("/api/workbooks", {
    method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ name }),
  });
  await page.goto(`${server.url}/w/${wb.id}`);
  await expect(page.locator(".status-dot.status-open")).toBeVisible();
  return wb.id;
}

function pick(page: Page, name: string, text: string | Buffer, mimeType = "text/csv") {
  return page.locator('.workbook input[type="file"]').setInputFiles({ name, mimeType, buffer: Buffer.isBuffer(text) ? text : Buffer.from(text) });
}

test("preview shows detected types, overrides change the preview and the import", async ({ page }) => {
  const wb = await newWorkbook(page, "Preview");
  await pick(page, "prices.csv", "code,amount\n007,1.5\n042,2.25\n");
  const dialog = page.getByRole("dialog", { name: "Import prices.csv" });
  await expect(dialog).toContainText("CSV");
  const preview = dialog.locator(".import-preview");
  await expect(preview.locator("tbody tr").first()).toContainText("007");
  const amount = dialog.getByLabel("Type for column amount");
  await expect(amount).toHaveValue("DOUBLE");
  await amount.selectOption("DECIMAL(18,2)");
  await expect(preview.locator("tbody tr").first()).toContainText("1.50");
  await dialog.getByLabel("Table name").fill("price list");
  await dialog.getByRole("button", { name: "Import table" }).click();
  await expect(page.getByRole("tab", { name: /price_list/ })).toBeVisible();
  await expect(page.locator(".toast-success")).toContainText("2 rows");
  const csv = await server.exportCsv(wb, { table: "price_list" });
  expect(csv).toBe("code,amount\n007,1.50\n042,2.25\n");
  expect(uploadsLeft()).toBe(0);
});

test("a failing import explains the problem and the suggested fix imports it", async ({ page }) => {
  await newWorkbook(page, "Fix");
  let text = "n,label\n";
  for (let i = 0; i < 120_000; i++) text += `${i},x\n`;
  text += "oops,last\n";
  await pick(page, "late.csv", text);
  const dialog = page.getByRole("dialog", { name: "Import late.csv" });
  await dialog.getByRole("button", { name: "Import table" }).click();
  await expect(dialog.getByRole("alert")).toContainText('Line 120002: "oops" in column "n" is not a valid BIGINT');
  await dialog.getByRole("button", { name: "Import “n” as Text and try again" }).click();
  await expect(page.getByRole("tab", { name: /late/ })).toBeVisible();
  await expect(page.locator(".toast-success")).toContainText("120,001 rows");
  expect(uploadsLeft()).toBe(0);
});

test("cancelling the preview leaves nothing behind", async ({ page }) => {
  const wb = await newWorkbook(page, "Cancel preview");
  await pick(page, "t.csv", "a\n1\n");
  const dialog = page.getByRole("dialog", { name: "Import t.csv" });
  await dialog.getByRole("button", { name: "Cancel" }).click();
  await expect(dialog).toHaveCount(0);
  await expect.poll(uploadsLeft).toBe(0);
  const info = await server.api<{ tables: unknown[] }>(`/api/workbooks/${wb}`);
  expect(info.tables).toEqual([]);
});

test("a running import can be stopped and retried", async ({ page }) => {
  const wb = await newWorkbook(page, "Stop");
  // Large enough that the import is still running when Stop is pressed; written to disk
  // because Playwright limits in-memory files to 50 MB.
  const lines = ["a,b,c,d"];
  for (let i = 0; i < 4_000_000; i++) lines.push(`${i},${(i * 7) % 1000},${i % 13},text ${i % 101}`);
  const file = join(mkdtempSync(join(tmpdir(), "rowfinity-big-")), "big.csv");
  writeFileSync(file, lines.join("\n") + "\n");
  await page.locator('.workbook input[type="file"]').setInputFiles(file);
  const dialog = page.getByRole("dialog", { name: "Import big.csv" });
  await dialog.getByRole("button", { name: "Import table" }).click();
  await expect(dialog.locator(".import-progress progress")).toBeVisible();
  await dialog.getByRole("button", { name: "Stop import" }).click();
  await expect(dialog.getByRole("alert")).toHaveText("Cancelled.");
  let info = await server.api<{ tables: unknown[] }>(`/api/workbooks/${wb}`);
  expect(info.tables).toEqual([]);
  // The same upload imports when tried again.
  await dialog.getByRole("button", { name: "Import table" }).click();
  await expect(page.getByRole("tab", { name: /big/ })).toBeVisible({ timeout: 60_000 });
  info = await server.api<{ tables: { row_count: number }[] }>(`/api/workbooks/${wb}`);
  expect(info.tables).toEqual([expect.objectContaining({ row_count: 4_000_000 })]);
});

test("xlsx import lists sheets and compatibility notes before importing", async ({ page }) => {
  // Make an xlsx with an unsupported function through the server itself.
  const src = await server.api<{ id: string }>("/api/workbooks", {
    method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ name: "src" }),
  });
  await page.goto(`${server.url}/w/${src.id}`);
  await page.getByRole("button", { name: "New table or sheet" }).click();
  await page.getByRole("menuitem", { name: /New sheet/ }).click();
  await expect(page.getByRole("grid", { name: /Sheet/ })).toBeVisible();
  await page.locator(".grid-host").focus();
  await page.keyboard.type("=NOTREAL(1)");
  await page.keyboard.press("Enter");
  // Wait until the server has evaluated and saved it.
  await page.keyboard.press("ArrowUp");
  await expect(page.locator(".formula-input")).toHaveValue(/^=notreal\(1\)$/i);
  const xlsx = Buffer.from(await (await fetch(`${server.url}/api/workbooks/${src.id}/export.xlsx`)).arrayBuffer());

  await newWorkbook(page, "Excel");
  await pick(page, "model.xlsx", xlsx, "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet");
  const dialog = page.getByRole("dialog", { name: "Import model.xlsx" });
  await expect(dialog).toContainText("has 1 sheet");
  await expect(dialog.getByRole("note")).toContainText("NOTREAL");
  await dialog.getByRole("button", { name: "Import sheets" }).click();
  await expect(page.getByRole("tab", { name: /Sheet/ })).toBeVisible();
});
