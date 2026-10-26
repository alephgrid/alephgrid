// 1.11 browser gate: a user imports, edits, filters, exports and reopens a table, and builds a
// sheet, through the UI alone. Results are checked on screen (formula bar, toasts, status
// bar) and in persisted state (export, after a server restart).

import { expect, test, type Page } from "@playwright/test";
import { Server } from "./server";

const server = new Server();
test.beforeAll(async () => server.start());
test.afterAll(async () => server.dispose());

const CSV = "id,name,amount\n1,Ada,10.5\n2,Linus,-3\n3,東京,0\n";

async function activeCell(page: Page): Promise<{ ref: string; value: string }> {
  return {
    ref: (await page.locator(".cell-ref").textContent()) ?? "",
    value: await page.locator(".formula-input").inputValue(),
  };
}

/** Move the active cell with the keyboard (the grid is a canvas). */
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

function workbookId(page: Page): string {
  return new URL(page.url()).pathname.split("/")[2];
}

test("table journey: import, edit, reject bad input, filter, export, restart, reopen", async ({ page }) => {
  await page.goto(server.url);
  await expect(page.getByRole("heading", { name: /doesn't fit in a spreadsheet/ })).toBeVisible();

  // Import through the file picker.
  await page.locator('input[type="file"]').setInputFiles({ name: "people.csv", mimeType: "text/csv", buffer: Buffer.from(CSV) });
  await page.getByRole("dialog", { name: "Import people.csv" }).getByRole("button", { name: "Import table" }).click();
  await expect(page.getByRole("tab", { name: /people/ })).toBeVisible();
  await expect(page.locator(".toast-success")).toContainText("3 rows imported");
  const wb = workbookId(page);
  await expect.poll(() => activeCell(page)).toEqual({ ref: "id · 1", value: "1" });

  // Edit a text cell and append a row by typing below the end.
  await typeInto(page, 1, 1, "Linus T.");
  await typeInto(page, 3, 0, "4");
  await goTo(page, 1, 1);
  await expect.poll(() => activeCell(page)).toEqual({ ref: "name · 2", value: "Linus T." });
  await expect.poll(() => server.exportCsv(wb, { table: "people" }))
    .toBe("id,name,amount\n1,Ada,10.5\n2,Linus T.,-3.0\n3,東京,0.0\n4,,\n");

  // Invalid input is refused with a visible error and changes nothing.
  await typeInto(page, 0, 2, "lots");
  await expect(page.locator(".toast-error")).toContainText("not a valid");
  await goTo(page, 0, 2);
  await expect.poll(() => activeCell(page)).toEqual({ ref: "amount · 1", value: "10.5" });
  expect(await server.exportCsv(wb, { table: "people" })).toContain("1,Ada,10.5\n");

  // Filter through the dialog.
  await page.getByRole("button", { name: "Filter", exact: true }).click();
  const dialog = page.locator("form.dialog");
  await dialog.locator('select[name="column"]').selectOption("amount");
  await dialog.locator('select[name="op"]').selectOption("gte");
  await dialog.locator('input[name="value"]').fill("0");
  await dialog.getByRole("button", { name: "Apply filter" }).click();
  await expect(page.locator(".chip-filter")).toContainText("amount");
  await goTo(page, 1, 1);
  await expect.poll(() => activeCell(page)).toEqual({ ref: "name · 2", value: "東京" });

  // Restart the server; the page reconnects by itself and shows the same data.
  await server.stop("SIGTERM");
  await expect(page.locator(".status-dot.status-closed, .status-dot.status-connecting")).toBeVisible();
  await server.start();
  await expect(page.locator(".status-dot.status-open")).toBeVisible({ timeout: 30_000 });
  await page.reload();
  await expect(page.getByRole("tab", { name: /people/ })).toBeVisible();
  await goTo(page, 1, 1);
  await expect.poll(() => activeCell(page)).toEqual({ ref: "name · 2", value: "Linus T." });
  expect(await server.exportCsv(wb, { table: "people" })).toBe("id,name,amount\n1,Ada,10.5\n2,Linus T.,-3.0\n3,東京,0.0\n4,,\n");
});

test("typing a value and pressing Enter moves down without reopening the editor", async ({ page }) => {
  const wb = await server.api<{ id: string }>("/api/workbooks", {
    method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ name: "Enter" }),
  });
  await page.goto(`${server.url}/w/${wb.id}`);
  await page.getByRole("button", { name: "New table or sheet" }).click();
  await page.getByRole("menuitem", { name: /New sheet/ }).click();
  await expect(page.getByRole("grid", { name: /Sheet/ })).toBeVisible();
  await page.locator(".grid-host").focus();
  // Type straight down a column, like a spreadsheet user does.
  for (const v of ["1", "2", "=A1+A2"]) {
    await page.keyboard.type(v);
    await page.keyboard.press("Enter");
  }
  await expect(page.locator(".grid-editor")).toBeHidden();
  await expect(page.locator(".grid-host")).toBeFocused();
  await expect(page.locator(".cell-ref")).toHaveText("A4");
  await page.keyboard.press("ArrowUp");
  await expect(page.locator(".formula-input")).toHaveValue("=A1+A2");
  await page.keyboard.press("Shift+ArrowUp");
  await page.keyboard.press("Shift+ArrowUp");
  await expect(page.locator(".stats")).toContainText("Sum 6");
});

test("sheet journey: formulas, undo, crash, reopen", async ({ page }) => {
  await page.goto(server.url);
  await page.getByRole("button", { name: "New workbook" }).click();
  await page.locator("form.dialog").getByRole("button", { name: "Create" }).click();
  await expect(page).toHaveURL(/\/w\//);
  const wb = workbookId(page);

  await page.getByRole("button", { name: "New table or sheet" }).click();
  await page.locator(".menu-item", { hasText: "New sheet" }).click();
  await expect(page.getByRole("tab", { name: /Sheet/ })).toBeVisible();

  await typeInto(page, 0, 0, "2");
  await typeInto(page, 1, 0, "=A1*21");
  await goTo(page, 1, 0);
  await expect.poll(() => activeCell(page)).toEqual({ ref: "A2", value: "=A1*21" });
  // The computed value shows in the status bar when both cells are selected.
  await goTo(page, 0, 0);
  await page.keyboard.press("Shift+ArrowDown");
  await expect(page.locator(".stats")).toContainText("Sum 44");

  // Change the input, then undo.
  await typeInto(page, 0, 0, "3");
  await goTo(page, 0, 0);
  await page.keyboard.press("Shift+ArrowDown");
  await expect(page.locator(".stats")).toContainText("Sum 66");
  await page.getByRole("button", { name: "Undo" }).click();
  await goTo(page, 0, 0);
  await expect.poll(() => activeCell(page)).toEqual({ ref: "A1", value: "2" });

  // Crash the server; acknowledged sheet edits survive.
  await server.stop("SIGKILL");
  await server.start();
  await page.goto(`${server.url}/w/${wb}`);
  await page.getByRole("tab", { name: /Sheet/ }).click();
  await goTo(page, 1, 0);
  await expect.poll(() => activeCell(page)).toEqual({ ref: "A2", value: "=A1*21" });
  await goTo(page, 0, 0);
  await page.keyboard.press("Shift+ArrowDown");
  await expect(page.locator(".stats")).toContainText("Sum 44");
});

test("trash: delete, see it in Trash, restore", async ({ page }) => {
  const wb = await server.api<{ id: string }>("/api/workbooks", {
    method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ name: "Disposable" }),
  });
  await page.goto(server.url);
  const row = page.locator(".wb-row", { hasText: "Disposable" });
  await row.getByRole("button", { name: "Disposable options" }).click();
  await page.getByText("Delete", { exact: true }).click();
  await page.locator(".dialog .btn-danger").click();
  await expect(page.locator(".wb-row", { hasText: "Disposable" })).toHaveCount(0);

  await page.getByRole("button", { name: /Trash/ }).click();
  const trashed = page.locator(`[data-trash-id="${wb.id}"]`);
  await expect(trashed).toBeVisible();
  await trashed.getByRole("button", { name: "Restore" }).click();
  await expect(page.locator(".toast-success")).toContainText("Restored");
  await page.getByRole("button", { name: /All workbooks/ }).click();
  await expect(page.locator(".wb-row", { hasText: "Disposable" })).toHaveCount(1);
});

test("a workbook deleted elsewhere closes for people who have it open", async ({ page }) => {
  const wb = await server.api<{ id: string }>("/api/workbooks", {
    method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ name: "Shared" }),
  });
  await page.goto(`${server.url}/w/${wb.id}`);
  await expect(page.locator(".status-dot.status-open")).toBeVisible();
  await server.api(`/api/workbooks/${wb.id}`, { method: "DELETE" });
  await expect(page.locator(".toast-error")).toContainText("moved to the trash");
  await expect(page).toHaveURL(server.url + "/");
});
