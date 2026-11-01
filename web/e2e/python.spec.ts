// Phase 8 gate in the browser (8.12): define a Python function, run its tests, publish it, call
// it from sheet cells, see it recalculate when its input changes, and reopen the workbook with
// the result still there. The runtime is made offline with `alephgrid python-runtime create`.

import { spawnSync } from "node:child_process";
import { existsSync, mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { expect, test, type Page } from "@playwright/test";
import { Server } from "./server";

const BIN = process.env.ALEPHGRID_BIN ?? resolve(fileURLToPath(new URL(".", import.meta.url)), "../../target/debug/alephgrid");
const PYTHON = process.env.ALEPHGRID_TEST_PYTHON
  ?? ["/opt/homebrew/bin/python3.12", "/opt/homebrew/bin/python3", "/usr/local/bin/python3", "/usr/bin/python3"].find(existsSync)
  ?? "python3";
const runtime = join(mkdtempSync(join(tmpdir(), "alephgrid-e2e-py-")), "runtime");
const made = spawnSync(BIN, ["python-runtime", "create", "--python", PYTHON, runtime], { encoding: "utf8" });
if (made.status !== 0) throw new Error(`could not make a Python runtime: ${made.stderr}`);

const server = new Server(["--python-runtime", runtime]);
test.beforeAll(async () => server.start());
test.afterAll(async () => server.dispose());

async function select(page: Page, row: number, column: number): Promise<void> {
  // By address, through Go to: arrow keys right after focusing can land a cell off in Firefox.
  const address = `${String.fromCharCode(65 + column)}${row + 1}`;
  await page.getByRole("button", { name: "Go to", exact: true }).click();
  const goTo = page.getByRole("dialog", { name: "Go to" });
  await goTo.getByLabel("Cell, range or name").fill(address);
  await goTo.getByLabel("Cell, range or name").press("Enter");
  await expect(page.locator(".cell-ref")).toHaveText(address);
}

async function enter(page: Page, row: number, column: number, text: string): Promise<void> {
  await select(page, row, column);
  const editor = page.getByRole("textbox", { name: "Cell value or formula", exact: true });
  await editor.fill(text);
  await editor.press("Enter");
}

/** What the page shows in a cell. */
async function shown(page: Page, row: number, column: number): Promise<string> {
  await select(page, row, column);
  return (await page.locator('[role="gridcell"][aria-selected="true"]').first().textContent()) ?? "";
}

test("define, test, publish, call, recalculate and reopen a Python function", async ({ page }) => {
  const wb = await server.createWorkbook("Python journey");
  const ws = await server.connect(wb);
  await ws.call("sheet_create", { name: "Sheet1" });
  ws.close();
  await page.goto(`${server.url}/w/${wb}`);
  await expect(page.locator(".status-dot.status-open")).toBeVisible();
  await page.getByRole("button", { name: "Functions", exact: true }).click();
  const dialog = page.getByRole("complementary", { name: "Functions" });
  await dialog.getByRole("combobox", { name: "Function engine" }).selectOption("python");
  await dialog.getByRole("textbox", { name: "Function name" }).fill("NORMALIZE_SKU");
  await dialog.getByRole("textbox", { name: "Arguments" }).fill("sku");
  await dialog.getByRole("combobox", { name: "Python result type" }).selectOption("text");
  await dialog.getByRole("textbox", { name: "Function source" }).fill("def normalize_sku(sku):\n    return sku.strip().upper().replace('-', '')\n");
  await dialog.getByRole("tab", { name: "Tests" }).click();
  await dialog.getByRole("button", { name: "Add test" }).click();
  await dialog.getByRole("textbox", { name: "Test 1 sku" }).fill(" ab-12 ");
  await dialog.getByRole("textbox", { name: "Test 1 expected" }).fill("AB12");
  await dialog.getByRole("button", { name: "Run tests", exact: true }).click();
  await expect(dialog.getByRole("region", { name: "Test results" })).toContainText("Passed · test 1 · actual AB12");
  await dialog.getByRole("button", { name: "Publish version", exact: true }).click();
  await expect(dialog.getByRole("combobox", { name: "Function", exact: true })).toHaveValue("NORMALIZE_SKU");
  await dialog.getByRole("tab", { name: "Versions" }).click();
  await expect(dialog.getByRole("list", { name: "Published versions" })).toContainText("Python");
  await dialog.getByRole("button", { name: "Close functions panel" }).click();

  await page.getByRole("tab", { name: /Sheet1/ }).click();
  await enter(page, 0, 0, " ab-12 ");
  await enter(page, 0, 1, "=NORMALIZE_SKU(A1)");
  await expect.poll(() => shown(page, 0, 1), { timeout: 30_000 }).toBe("AB12");
  // Its input changes: it is computed again (read on the server; the page shows it below).
  await enter(page, 0, 0, "cd-34");
  const check = await server.connect(wb);
  await expect.poll(async () => (await check.call("sheet_fetch", { sheet: 0, row_start: 0, row_count: 1, column_count: 2 })).rows[0][1][0], { timeout: 30_000 }).toBe("CD34");
  check.close();
  // Reopened: the result shows at once, from the workbook's cache.
  await page.reload();
  await expect(page.locator(".status-dot.status-open")).toBeVisible();
  await page.getByRole("tab", { name: /Sheet1/ }).click();
  await expect.poll(() => shown(page, 0, 1)).toBe("CD34");
});
