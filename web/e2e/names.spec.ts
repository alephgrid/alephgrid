import AxeBuilder from "@axe-core/playwright";
import { expect, test, type Page } from "@playwright/test";
import { parseAddress, quoteSheet, resolveTarget, absoluteReference } from "../src/names";
import { Server } from "./server";

const server = new Server();
test.beforeAll(async () => server.start());
test.afterAll(async () => server.dispose());

const formulaBar = (page: Page) => page.getByRole("textbox", { name: "Cell value or formula", exact: true });

test("addresses, quoting and name resolution", () => {
  expect(parseAddress("b2")).toEqual({ sheet: null, r0: 1, c0: 1, r1: 1, c1: 1 });
  expect(parseAddress("$C$9:A1")).toEqual({ sheet: null, r0: 0, c0: 0, r1: 8, c1: 2 });
  expect(parseAddress("'Q1 ''24'!A1:B2")).toEqual({ sheet: "Q1 '24", r0: 0, c0: 0, r1: 1, c1: 1 });
  expect(parseAddress("Data!A1")?.sheet).toBe("Data");
  for (const bad of ["Sales", "A0", "XFE1", "A1048577", "A1:", "=A1"]) expect(parseAddress(bad)).toBeNull();
  expect(quoteSheet("Data")).toBe("Data");
  expect(quoteSheet("Q1 sales")).toBe("'Q1 sales'");
  expect(quoteSheet("AB12")).toBe("'AB12'");
  expect(quoteSheet("it's")).toBe("'it''s'");
  expect(absoluteReference("Q1 sales", { r0: 0, c0: 1, r1: 2, c1: 1 })).toBe("='Q1 sales'!$B$1:$B$3");
  const names = [{ name: "Pick", scope: null, refers_to: "=Data!$A$1" }, { name: "Pick", scope: 1, refers_to: "=Other!$B$2" }];
  expect(resolveTarget("pick", names, 0)).toEqual({ sheet: "Data", r0: 0, c0: 0, r1: 0, c1: 0 });
  expect(resolveTarget("PICK", names, 1)).toEqual({ sheet: "Other", r0: 1, c0: 1, r1: 1, c1: 1 });
  expect(resolveTarget("missing", names, 0)).toBeNull();
});

test("named ranges: define from the selection, use, rename, go to, delete and undo", async ({ page }) => {
  const wb = await server.createWorkbook("Named range journey"), ws = await server.connect(wb);
  await ws.call("sheet_create", { name: "Data" });
  await ws.call("sheet_set", { sheet: 0, inputs: [{ row: 0, column: 1, value: "10" }, { row: 1, column: 1, value: "20" }, { row: 2, column: 1, value: "30" }] });
  const cell = async (r: number, c: number, i = 0) => (await ws.call("sheet_fetch", { sheet: 0, row_start: r, row_count: 1, column_count: c + 1 })).rows[0]?.[c]?.[i];
  await page.goto(`${server.url}/w/${wb}`);
  await expect(page.locator(".cell-ref")).toHaveText("A1");
  await page.locator(".grid-host").focus();
  await page.keyboard.press("ArrowRight"); await page.keyboard.press("Shift+ArrowDown"); await page.keyboard.press("Shift+ArrowDown");
  await expect(page.locator(".cell-ref")).toHaveText("B1:B3");
  await page.getByRole("button", { name: "Names", exact: true }).click();
  const dialog = page.getByRole("dialog", { name: "Named ranges" });
  await expect(dialog.getByLabel("Refers to")).toHaveValue("=Data!$B$1:$B$3");
  await dialog.getByLabel("Name", { exact: true }).fill("Sales");
  await dialog.getByRole("button", { name: "Define name" }).click();
  await expect(dialog.getByRole("status")).toHaveText("Defined Sales");
  await expect(dialog.getByRole("table")).toContainText("Sales");
  // Collisions are explained and nothing is saved.
  await dialog.getByLabel("Name", { exact: true }).fill("SUM");
  await dialog.getByRole("button", { name: "Define name" }).click();
  await expect(dialog.getByRole("alert")).toContainText("built-in function");
  const axe = await new AxeBuilder({ page }).include(".names-dialog").analyze();
  expect(axe.violations.filter((v) => v.impact === "serious" || v.impact === "critical")).toEqual([]);
  await dialog.getByRole("button", { name: "Close" }).click();

  await page.locator(".grid-host").focus();
  await page.keyboard.press("ControlOrMeta+Home"); await page.keyboard.press("ArrowRight"); await page.keyboard.press("ArrowRight"); await page.keyboard.press("ArrowRight");
  await formulaBar(page).fill("=SUM(Sales)"); await formulaBar(page).press("Enter");
  await expect.poll(() => cell(0, 3)).toBe("60");

  await page.getByRole("button", { name: "Names", exact: true }).click();
  await dialog.getByRole("button", { name: "Edit Sales" }).click();
  await dialog.getByLabel("Name", { exact: true }).fill("Revenue");
  await dialog.getByLabel("Refers to").fill("=Data!$B$1:$B$2");
  await dialog.getByRole("button", { name: "Save changes" }).click();
  await expect(dialog.getByRole("status")).toHaveText("Saved Revenue");
  await expect.poll(() => cell(0, 3, 2)).toBe("=SUM(Revenue)");
  await expect.poll(() => cell(0, 3)).toBe("30");
  await dialog.getByRole("button", { name: "Go to Revenue" }).click();
  await expect(page.locator(".cell-ref")).toHaveText("B1:B2");

  await page.reload();
  await expect(page.locator(".cell-ref")).toHaveText("A1");
  expect(await ws.call("sheet_names")).toEqual([{ name: "Revenue", scope: null, refers_to: "=Data!$B$1:$B$2" }]);
  await page.getByRole("button", { name: "Go to", exact: true }).click();
  const goTo = page.getByRole("dialog", { name: "Go to" });
  await goTo.getByLabel("Cell, range or name").fill("nowhere");
  await goTo.getByLabel("Cell, range or name").press("Enter");
  await expect(goTo.getByRole("alert")).toContainText("not a cell, range or name");
  await goTo.getByLabel("Cell, range or name").fill("revenue");
  await goTo.getByLabel("Cell, range or name").press("Enter");
  await expect(page.locator(".cell-ref")).toHaveText("B1:B2");
  await page.getByRole("button", { name: "Go to", exact: true }).click();
  await goTo.getByLabel("Cell, range or name").fill("Data!F5000");
  await goTo.getByLabel("Cell, range or name").press("Enter");
  await expect(page.locator(".cell-ref")).toHaveText("F5000");

  await page.getByRole("button", { name: "Names", exact: true }).click();
  await dialog.getByRole("button", { name: "Delete Revenue" }).click();
  await page.getByRole("alertdialog").getByRole("button", { name: "Delete" }).click();
  await expect(dialog.getByRole("status")).toHaveText("Deleted Revenue");
  await expect.poll(() => cell(0, 3)).toBe("#NAME?");
  await dialog.getByRole("button", { name: "Close" }).click();
  await page.getByRole("button", { name: "Undo" }).click();
  await expect.poll(() => cell(0, 3)).toBe("30");
  ws.close();
});

test("a formula that does not parse is kept as a draft with its error location", async ({ page }) => {
  const wb = await server.createWorkbook("Formula problems"), ws = await server.connect(wb);
  await ws.call("sheet_create", { name: "Data" });
  const cell = async (r: number) => (await ws.call("sheet_fetch", { sheet: 0, row_start: r, row_count: 1, column_count: 1 })).rows[0]?.[0];
  await page.goto(`${server.url}/w/${wb}`);
  await expect(page.locator(".cell-ref")).toHaveText("A1");
  const input = formulaBar(page);
  await input.fill("=1+)"); await input.press("Enter");
  const problem = page.locator(".formula-problem");
  await expect(problem).toContainText("Formula not saved");
  await expect(problem).toContainText("at character 4");
  await expect(input).toBeFocused();
  await expect(input).toHaveValue("=1+)");
  expect(await input.evaluate((e: HTMLInputElement) => e.selectionStart)).toBe(3);
  expect(await cell(0)).toBeFalsy();
  await input.press("Escape");
  await expect(problem).toBeHidden();
  // A missing final parenthesis is still completed; text with an apostrophe is text.
  await input.fill("=SUM(1,2"); await input.press("Enter");
  await expect.poll(async () => (await cell(0))?.[0]).toBe("3");
  // The inline editor hands an unparsable formula to the formula bar instead of saving it.
  await page.locator(".grid-host").focus(); await page.keyboard.press("ArrowDown"); await page.keyboard.press("F2");
  const inline = page.getByRole("textbox", { name: "Edit cell", exact: true });
  await inline.fill("=1 2"); await inline.press("Enter");
  await expect(problem).toContainText("Unexpected input after the end of the formula");
  await expect(input).toHaveValue("=1 2");
  expect(await cell(1)).toBeFalsy();
  await input.fill("'=1 2"); await input.press("Enter");
  await expect.poll(async () => (await cell(1))?.[0]).toBe("=1 2");
  ws.close();
});

test("errors show where they come from, and references can be picked on another sheet", async ({ page }) => {
  const wb = await server.createWorkbook("Origins"), ws = await server.connect(wb);
  await ws.call("sheet_create", { name: "Calc" });
  await ws.call("sheet_create", { name: "Q1 data" });
  await ws.call("sheet_set", { sheet: 0, inputs: [{ row: 0, column: 0, value: "=1/0" }, { row: 0, column: 1, value: "=A1+1" }] });
  await ws.call("sheet_set", { sheet: 1, inputs: [{ row: 0, column: 1, value: "10" }, { row: 1, column: 1, value: "20" }, { row: 2, column: 1, value: "30" }] });
  await page.goto(`${server.url}/w/${wb}`);
  await expect(page.locator(".cell-ref")).toHaveText("A1");
  await page.locator(".grid-host").focus(); await page.keyboard.press("ArrowRight");
  const note = page.locator(".cell-note");
  await expect(note).toContainText("#DIV/0!");
  await expect(note).toContainText("comes from Calc!A1");
  await note.getByRole("button", { name: "Go to source" }).click();
  await expect(page.locator(".cell-ref")).toHaveText("A1");

  // Calc!C1: =SUM( then pick B1:B3 on the other sheet.
  await page.locator(".grid-host").focus(); await page.keyboard.press("ArrowRight"); await page.keyboard.press("ArrowRight");
  await expect(page.locator(".cell-ref")).toHaveText("C1");
  const input = formulaBar(page);
  await input.fill("=SUM(");
  await page.getByRole("tab", { name: /Q1 data/ }).click();
  await expect(page.getByRole("tab", { name: /Q1 data/ })).toHaveAttribute("aria-selected", "true");
  await expect(input).toBeFocused();
  await expect(input).toHaveValue("=SUM(");
  const box = (await page.locator(".grid-scroller").boundingBox())!;
  await page.mouse.move(box.x + 190, box.y + 44); await page.mouse.down();
  await page.mouse.move(box.x + 190, box.y + 92); await page.mouse.up();
  await expect(input).toHaveValue("=SUM('Q1 data'!B1:B3");
  await input.press("End"); await input.pressSequentially(")"); await input.press("Enter");
  await expect(page.getByRole("tab", { name: /Calc/ })).toHaveAttribute("aria-selected", "true");
  await expect(page.locator(".cell-ref")).toHaveText("C1");
  const c1 = async () => (await ws.call("sheet_fetch", { sheet: 0, row_start: 0, row_count: 1, column_count: 3 })).rows[0]?.[2];
  await expect.poll(async () => (await c1())?.[0]).toBe("60");
  expect((await c1())?.[2]).toBe("=SUM('Q1 data'!B1:B3)");
  // Escape while picking on another sheet discards the draft and returns to the edit's sheet.
  await input.fill("=");
  await page.getByRole("tab", { name: /Q1 data/ }).click();
  await input.press("Escape");
  await expect(page.getByRole("tab", { name: /Calc/ })).toHaveAttribute("aria-selected", "true");
  await expect.poll(async () => (await c1())?.[0]).toBe("60");
  ws.close();
});
