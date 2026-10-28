import AxeBuilder from "@axe-core/playwright";
import { expect, test } from "@playwright/test";
import { lockReferences, referenceTokens, referenceInsertion, referenceRanges } from "../src/references";
import { Server } from "./server";

const server = new Server();
test.beforeAll(async () => server.start());
test.afterAll(async () => server.dispose());

test("F4 changes only valid A1 reference tokens and cycles mixed anchors", () => {
  let text = '=SUM(A1:B3)';
  let start = 5, end = 10;
  for (const expected of ['=SUM($A$1:$B$3)', '=SUM(A$1:B$3)', '=SUM($A1:$B3)', '=SUM(A1:B3)']) {
    const changed = lockReferences(text, start, end)!;
    expect(changed.text).toBe(expected); ({ text, start, end } = changed);
  }
  expect(lockReferences('="A1"', 3)).toBeNull();
  expect(lockReferences('=LOG10(100)', 5)).toBeNull();
  expect(lockReferences('=LOG10 (100)', 5)).toBeNull();
  expect(lockReferences('=name_A1+1', 7)).toBeNull();
  expect(lockReferences('=Table[A1]', 8)).toBeNull();
  expect(lockReferences('=XFE1+A1048577', 4)).toBeNull();
  expect(lockReferences("='A1'' sheet'!B2", 15)?.text).toBe("='A1'' sheet'!$B$2");
  expect(referenceTokens('=SUM(A1,"A2""B3",C4)').map(t => t.row)).toEqual([0,3]);
  expect(referenceRanges("='Other'!A1:B4+C2")).toEqual([{ r0: 1, r1: 1, c0: 2, c1: 2 }]);
  expect(referenceInsertion('="hello', 7, 7)).toBeNull();
  expect(referenceInsertion('=SUM(', 5, 5)).toEqual({ start: 5, end: 5 });
  expect(referenceInsertion('=SUM(A1:B3)', 5, 10)).toEqual({ start: 5, end: 10 });
});

test("dragging a formula reference preserves its destination, F4 and Escape are reversible", async ({ page }) => {
  const wb = await server.createWorkbook("Reference editing"), ws = await server.connect(wb);
  await ws.call("sheet_create", { name: "Inputs" });
  await ws.call("sheet_set", { sheet: 0, inputs: [
    { row: 0, column: 0, value: "10" }, { row: 1, column: 0, value: "20" }, { row: 2, column: 0, value: "30" },
  ] });
  await page.goto(`${server.url}/w/${wb}`);
  await expect(page.locator('.cell-ref')).toHaveText('A1');
  const grid = page.locator('.grid-host');
  await grid.focus(); await page.keyboard.press('ArrowRight'); await page.keyboard.press('ArrowRight');
  await expect(page.locator('.cell-ref')).toHaveText('C1');
  const input = page.getByRole('textbox', { name: 'Cell value or formula', exact: true });
  await input.fill('=SUM(');
  const box = (await page.locator('.grid-scroller').boundingBox())!;
  // First column, rows 1–3. Drag must not select a different edit destination.
  await page.mouse.move(box.x + 80, box.y + 44); await page.mouse.down();
  await page.mouse.move(box.x + 80, box.y + 92); await page.mouse.up();
  await expect(input).toHaveValue('=SUM(A1:A3');
  await expect(input).toBeFocused();
  await expect(page.locator('.cell-ref')).toHaveText('C1');
  await expect(page.getByRole('region', { name: 'Formula references' })).toHaveText('A1:A3');
  await input.press('End'); await input.press('F4');
  await expect(input).toHaveValue('=SUM(A1:$A$3');
  await input.press('End'); await input.pressSequentially(')'); await input.press('Enter');
  const result = async () => (await ws.call('sheet_fetch', { sheet: 0, row_start: 0, row_count: 1, column_count: 3 })).rows[0]?.[2]?.[0];
  await expect.poll(result).toBe('60');
  await input.fill('=A1'); await input.press('F4'); await input.press('Escape');
  await expect.poll(result).toBe('60');
  await expect(page.getByRole('region', { name: 'Formula references' })).toBeHidden();
  await page.reload(); await expect.poll(result).toBe('60');
  ws.close();
});

test("reference hints are accessible in light and dark mode and F4 works inside cells", async ({ page }) => {
  const wb = await server.createWorkbook('Inline references'), ws = await server.connect(wb);
  await ws.call('sheet_create', { name: 'Sheet' });
  await page.goto(`${server.url}/w/${wb}`);
  await expect(page.locator('.cell-ref')).toHaveText('A1');
  const input = page.getByRole('textbox', { name: 'Cell value or formula', exact: true });
  for (const colorScheme of ['light','dark'] as const) {
    await page.emulateMedia({ colorScheme }); await input.fill('=B1+C2+D3+E4');
    const results = await new AxeBuilder({ page }).include('.reference-legend').analyze();
    expect(results.violations.filter(v => v.impact === 'serious' || v.impact === 'critical')).toEqual([]);
  }
  await input.press('Escape');
  await page.locator('.grid-host').focus(); await page.keyboard.press('F2');
  const inline = page.getByRole('textbox', { name: 'Edit cell', exact: true });
  await inline.fill('=B1'); await inline.press('End'); await inline.press('F4');
  await expect(inline).toHaveValue('=$B$1'); await inline.press('Escape');
  expect((await ws.call('sheet_fetch', { sheet: 0, row_start: 0, row_count: 1, column_count: 1 })).rows[0]?.[0]).toBeFalsy();
  ws.close();
});
