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
// A short limit, to see a test run out of time (U.T06).
const quick = new Server(["--python-runtime", runtime, "--python-timeout", "2"]);
test.beforeAll(async () => { await server.start(); await quick.start(); });
test.afterAll(async () => { await server.dispose(); await quick.dispose(); });

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

async function spinning(page: Page, url: string, wb: string) {
  await page.goto(`${url}/w/${wb}`);
  await expect(page.locator(".status-dot.status-open")).toBeVisible();
  await page.getByRole("button", { name: "Functions", exact: true }).click();
  const panel = page.getByRole("complementary", { name: "Functions" });
  await panel.getByRole("button", { name: "New", exact: true }).click();
  await panel.getByRole("combobox", { name: "Function engine" }).selectOption("python");
  await panel.getByRole("textbox", { name: "Function name" }).fill("SPIN");
  await panel.getByRole("textbox", { name: "Arguments" }).fill("x");
  await panel.getByRole("textbox", { name: "Function source" }).fill("def spin(x):\n    while True:\n        pass\n");
  await panel.getByRole("tab", { name: "Tests" }).click();
  await panel.getByRole("button", { name: "Add test" }).click();
  await panel.getByRole("textbox", { name: "Test 1 x" }).fill("1");
  await panel.getByRole("textbox", { name: "Test 1 expected" }).fill("1");
  return panel;
}

// U.06d / U.T06: Cancel stops a running test on the server (its worker is killed; the reply
// arrives only once the run has ended, long before the 30 s limit), and a test that runs out
// of time says so.
test("a running Python test can be cancelled, and one that runs too long says so", async ({ page }) => {
  const wb = await server.createWorkbook("Cancel a test");
  const panel = await spinning(page, server.url, wb);
  await panel.getByRole("button", { name: "Run tests", exact: true }).click();
  const results = panel.getByRole("region", { name: "Test results" });
  await expect(results.getByRole("status")).toHaveText("Running…");
  await expect(panel.getByRole("button", { name: "Run tests", exact: true })).toBeHidden();
  // Let the worker start, then cancel it.
  await page.waitForTimeout(500);
  const t = Date.now();
  await panel.getByRole("button", { name: "Cancel", exact: true }).click();
  await expect(results.getByRole("status")).toHaveText("Cancelled", { timeout: 8_000 });
  expect(Date.now() - t).toBeLessThan(8_000);
  await expect(results).toContainText("stopped on the server");
  // A cancelled run proves nothing: still a draft.
  await expect(panel.locator(".fn-state")).toHaveText("Draft");
  await expect(panel.getByRole("button", { name: "Run tests", exact: true })).toBeEnabled();
  await expect(panel.getByRole("button", { name: "Publish version", exact: true })).toBeEnabled();

  const slow = await quick.createWorkbook("Too long");
  const again = await spinning(page, quick.url, slow);
  await again.getByRole("button", { name: "Run tests", exact: true }).click();
  await expect(again.getByRole("region", { name: "Test results" })).toContainText("ran for more than 2 s", { timeout: 20_000 });
  // Nothing was published.
  const ws = await quick.connect(slow);
  expect(await ws.call("functions", {})).toEqual([]);
  ws.close();
});

// U.06e: activating a Python version says whether it can compute on this server.
test("activating a Python version shows its runtime compatibility", async ({ page }) => {
  const wb = await server.createWorkbook("Python versions");
  const ws = await server.connect(wb);
  const def = (body: string) => ({ name: "KIND", kind: "python", arguments: ["x"], returns: "text", source: `def kind(x):\n    return ${body}\n`, documentation: "", tests: [] });
  await ws.call("publish_function", { definition: def("type(x).__name__") });
  await ws.call("publish_function", { definition: def("type(x).__name__.upper()") });
  ws.close();
  await page.goto(`${server.url}/w/${wb}`);
  await expect(page.locator(".status-dot.status-open")).toBeVisible();
  await page.getByRole("button", { name: "Functions", exact: true }).click();
  const panel = page.getByRole("complementary", { name: "Functions" });
  await panel.getByRole("combobox", { name: "Function", exact: true }).selectOption("KIND");
  await panel.getByRole("tab", { name: "Versions" }).click();
  await panel.getByRole("button", { name: "Activate v2 in this workbook" }).click();
  const impact = panel.getByRole("group", { name: "Activate KIND v2" });
  await expect(impact).toContainText("Runs in this server's Python");
  await expect(impact).toContainText("No cell or table column calls it yet.");
});
