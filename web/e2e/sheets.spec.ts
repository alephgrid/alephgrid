// 2.01, 2.02, 2.04 and the 2.10 gate: sheets through the UI, live updates between two
// people, undo/redo with recalculation and reopen, and a new user's first session.

import { expect, test, type Page } from "@playwright/test";
import { Server } from "./server";

const server = new Server();
test.beforeAll(async () => server.start());
test.afterAll(async () => server.dispose());

/** Wait until a sheet is open and ready for keyboard input. */
async function sheetReady(page: Page): Promise<void> {
  await expect(page.getByRole("grid", { name: /^Sheet / })).toHaveAttribute("aria-colcount", /\d+/);
  await expect(page.locator(".cell-ref")).not.toHaveText("");
}

async function type(page: Page, cell: [number, number], text: string): Promise<void> {
  await page.locator(".grid-host").focus();
  await page.keyboard.press("ControlOrMeta+Home");
  for (let i = 0; i < cell[0]; i++) await page.keyboard.press("ArrowDown");
  for (let i = 0; i < cell[1]; i++) await page.keyboard.press("ArrowRight");
  await page.keyboard.type(text);
  await page.keyboard.press("Enter");
}

/** The displayed value of a cell, as exposed to assistive technology. */
async function shown(page: Page, cell: [number, number]): Promise<string> {
  await page.locator(".grid-host").focus();
  await page.keyboard.press("ControlOrMeta+Home");
  for (let i = 0; i < cell[0]; i++) await page.keyboard.press("ArrowDown");
  for (let i = 0; i < cell[1]; i++) await page.keyboard.press("ArrowRight");
  const id = await page.locator(".grid-host").getAttribute("aria-activedescendant");
  return (await page.locator(`#${id}`).getAttribute("aria-label")) ?? "";
}

test("blank sheet in a new workbook, edits, errors, and live updates for a second person", async ({ page, browser }) => {
  await page.goto(server.url);
  await page.getByRole("button", { name: /New (blank )?workbook/ }).first().click();
  await page.getByRole("dialog", { name: "New workbook" }).getByRole("button", { name: "Create" }).click();
  await expect(page.getByRole("heading", { name: "This workbook is empty" })).toBeVisible();
  await page.getByRole("button", { name: "New blank sheet" }).click();
  await expect(page.getByRole("tab", { name: /Sheet/ })).toHaveAttribute("aria-selected", "true");
  await expect(page.locator(".kind-badge")).toHaveText("Sheet");
  await sheetReady(page);

  await type(page, [0, 0], "10");
  await type(page, [1, 0], "0");
  await type(page, [2, 0], "=A1/A2");
  await type(page, [3, 0], "=A1*2");
  await expect.poll(() => shown(page, [2, 0])).toBe("A3: #DIV/0!");
  await expect.poll(() => shown(page, [3, 0])).toBe("A4: 20");

  // A second person sees changes without reloading.
  const other = await browser.newPage();
  await other.goto(page.url());
  await other.getByRole("tab", { name: /Sheet/ }).click();
  await expect.poll(() => shown(other, [3, 0])).toBe("A4: 20");
  await type(page, [0, 0], "21");
  await expect.poll(() => shown(other, [3, 0])).toBe("A4: 42");
  await expect.poll(() => shown(other, [2, 0])).toBe("A3: #DIV/0!");
  await type(other, [1, 0], "7");
  await expect.poll(() => shown(page, [2, 0])).toBe("A3: 3");
  await other.close();
});

test("undo and redo recalculate, and survive reopening", async ({ page }) => {
  const wb = (await server.api<{ id: string }>("/api/workbooks", {
    method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ name: "Recalculation" }),
  })).id;
  await page.goto(`${server.url}/w/${wb}`);
  await page.getByRole("button", { name: "New blank sheet" }).click();
  await sheetReady(page);
  await type(page, [0, 0], "2");
  await type(page, [0, 1], "=A1*A1");
  await type(page, [0, 0], "5");
  await expect.poll(() => shown(page, [0, 1])).toBe("B1: 25");
  await page.locator(".grid-host").focus();
  await page.keyboard.press("ControlOrMeta+z");
  await expect.poll(() => shown(page, [0, 1])).toBe("B1: 4");
  await page.keyboard.press("ControlOrMeta+Shift+z");
  await expect.poll(() => shown(page, [0, 1])).toBe("B1: 25");
  await page.getByRole("button", { name: "Undo", exact: true }).click();
  await expect.poll(() => shown(page, [0, 1])).toBe("B1: 4");
  await page.reload();
  await page.getByRole("tab", { name: /Sheet/ }).click();
  await expect.poll(() => shown(page, [0, 1])).toBe("B1: 4");
  await type(page, [0, 0], "-3");
  await expect.poll(() => shown(page, [0, 1])).toBe("B1: 9");
});

test("gate: a new user creates, imports, edits and reopens a table and a sheet with the UI alone", async ({ page }) => {
  await page.goto(server.url);
  await page.getByRole("button", { name: "New workbook" }).click();
  const create = page.getByRole("dialog", { name: "New workbook" });
  await create.getByLabel("Name").fill("Budget 2026");
  await create.getByRole("button", { name: "Create" }).click();
  await expect(page.getByRole("heading", { name: "This workbook is empty" })).toBeVisible();

  // Import a table.
  await page.locator('.workbook input[type="file"]').setInputFiles({ name: "costs.csv", mimeType: "text/csv", buffer: Buffer.from("item,cost\nrent,1200\nfood,350.5\n") });
  await page.getByRole("dialog", { name: "Import costs.csv" }).getByRole("button", { name: "Import table" }).click();
  await expect(page.getByRole("tab", { name: /costs/ })).toBeVisible();
  // Import publishes the tab before the table view has finished opening.
  await expect(page.locator(".row-count")).toHaveText("2 rows");
  await type(page, [2, 0], "travel");
  await type(page, [2, 1], "99.5");
  await expect(page.locator(".row-count")).toHaveText("3 rows");

  // A blank sheet next to it.
  await page.getByRole("button", { name: "New table or sheet" }).click();
  await page.getByRole("menuitem", { name: /New sheet/ }).click();
  await sheetReady(page);
  await type(page, [0, 0], "1650");
  await type(page, [1, 0], "=A1/12");
  await expect.poll(() => shown(page, [1, 0])).toBe("A2: 137.5");

  // Leave, come back from the home screen, and find both intact.
  await page.locator("button.brand").click();
  await page.locator(".wb-name", { hasText: "Budget 2026" }).click();
  await page.getByRole("tab", { name: /costs/ }).click();
  await expect(page.locator(".row-count")).toHaveText("3 rows");
  await expect.poll(() => shown(page, [2, 1])).toBe("cost, row 3: 99.5");
  await page.getByRole("tab", { name: /Sheet/ }).click();
  await expect.poll(() => shown(page, [1, 0])).toBe("A2: 137.5");
  // And after the server restarts.
  await server.stop("SIGTERM");
  await server.start();
  await page.reload();
  await page.getByRole("tab", { name: /Sheet/ }).click();
  await expect.poll(() => shown(page, [1, 0])).toBe("A2: 137.5");
});
