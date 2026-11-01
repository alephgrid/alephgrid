// U.T07: the spreadsheet-first journey with SQL closed throughout. A new workbook opens into a
// sheet; two files are imported; lookups and conditional totals are typed into cells (no
// formula dialog) and filled with the fill handle and Mod+D; a range is copied and pasted by
// keyboard; undo and redo; a chart is previewed and created; everything survives a reload.
// Expected values are worked out by hand from the two files.

import { expect, test, type Page } from "@playwright/test";
import { Server } from "./server";

const server = new Server();
test.beforeAll(async () => server.start());
test.afterAll(async () => server.dispose());

const ORDERS = "id,customer,amount\n1,10,100\n2,11,50\n3,10,25\n4,12,10\n";
const CUSTOMERS = "id,name,region\n10,Ann,West\n11,Bo,East\n12,Cy,West\n";
const HEADER_H = 32, ROW_H = 24;

async function noSql(page: Page): Promise<void> {
  await expect(page.locator(".sql-workspace, .sql-editor")).toHaveCount(0);
}

async function cellText(page: Page, ref: string): Promise<string> {
  await page.getByRole("button", { name: "Go to", exact: true }).click();
  const d = page.getByRole("dialog", { name: "Go to" });
  await d.getByLabel("Cell, range or name").fill(ref);
  await d.getByLabel("Cell, range or name").press("Enter");
  await page.locator(".grid-host").focus();
  await expect(page.locator(".cell-ref")).toHaveText(ref);
  await page.evaluate(() => new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r))));
  const id = await page.locator(".grid-host").getAttribute("aria-activedescendant");
  return (await page.locator(`#${id}`).textContent()) ?? "";
}

async function typeAt(page: Page, ref: string, text: string): Promise<void> {
  await cellText(page, ref);
  await page.keyboard.type(text);
  await page.keyboard.press("Enter");
}

test("U.T07: a business user works in a sheet with SQL closed, without formula dialogs", async ({ page, browserName }) => {
  test.setTimeout(120_000);
  if (browserName === "chromium") await page.context().grantPermissions(["clipboard-read", "clipboard-write"], { origin: server.url });
  await page.addInitScript(() => {
    try { localStorage.setItem("alephgrid-welcome-done", "1"); } catch { /* */ }
    const write = navigator.clipboard.writeText.bind(navigator.clipboard);
    navigator.clipboard.writeText = async (text: string) => { (window as unknown as { __copied: string }).__copied = text; return write(text); };
  });

  // New workbook from the hub: straight into a blank sheet.
  await page.goto(server.url);
  await page.getByRole("button", { name: "New workbook" }).click();
  await page.getByRole("dialog", { name: "New workbook" }).getByRole("button", { name: "Create" }).click();
  await expect(page.getByRole("tab", { name: /Sheet 1/ })).toHaveAttribute("aria-selected", "true");
  await noSql(page);
  const wb = new URL(page.url()).pathname.split("/")[2];

  // Import the two files as tables.
  for (const [name, csv] of [["orders.csv", ORDERS], ["customers.csv", CUSTOMERS]] as const) {
    await page.locator('.workbook input[type="file"]').setInputFiles({ name, mimeType: "text/csv", buffer: Buffer.from(csv) });
    await page.getByRole("dialog", { name: `Import ${name}` }).getByRole("button", { name: "Import table", exact: true }).click();
    await expect(page.getByRole("tab", { name: new RegExp(name.replace(".csv", "")) })).toBeVisible();
  }
  await noSql(page);

  // Back to the sheet: customer ids typed in, lookup and total typed into cells.
  await page.getByRole("tab", { name: /Sheet 1/ }).click();
  await typeAt(page, "A1", "Customer");
  await typeAt(page, "B1", "Region");
  await typeAt(page, "C1", "Total");
  await typeAt(page, "A2", "10");
  await typeAt(page, "A3", "11");
  await typeAt(page, "A4", "12");
  await typeAt(page, "B2", '=XLOOKUP(A2,customers[id],customers[region],"none")');
  await typeAt(page, "C2", "=SUMIFS(orders[amount],orders[customer],A2)");
  // Formulas went straight into cells: no dialog is open.
  await expect(page.getByRole("dialog")).toHaveCount(0);
  expect(await cellText(page, "B2")).toContain("West");
  expect(await cellText(page, "C2")).toContain("125");

  // Fill B2 down to B4 with the fill handle.
  await cellText(page, "B2");
  const box = (await page.locator(".grid-scroller").boundingBox())!;
  const y = box.y + HEADER_H + ROW_H * 2;
  let hx = -1;
  for (let x = box.x + 40; x < box.x + 500; x += 5) {
    await page.mouse.move(x, y);
    if (await page.locator(".grid-scroller").evaluate((e) => (e as HTMLElement).style.cursor === "crosshair")) { hx = x; break; }
  }
  expect(hx).toBeGreaterThan(0);
  await page.mouse.down();
  await page.mouse.move(hx, y + ROW_H * 2, { steps: 5 });
  await page.mouse.up();
  await expect.poll(() => cellText(page, "B3")).toContain("East");
  expect(await cellText(page, "B4")).toContain("West");

  // Fill C2:C4 with Mod+D.
  await cellText(page, "C2");
  await page.keyboard.press("Shift+ArrowDown"); await page.keyboard.press("Shift+ArrowDown");
  await page.keyboard.press("ControlOrMeta+d");
  await expect.poll(() => cellText(page, "C3")).toContain("50");
  expect(await cellText(page, "C4")).toContain("10");

  // Undo the fill, then redo it.
  await page.keyboard.press("ControlOrMeta+z");
  await expect.poll(() => cellText(page, "C3")).not.toContain("50");
  await page.keyboard.press("ControlOrMeta+Shift+z");
  await expect.poll(() => cellText(page, "C3")).toContain("50");

  // Paste cannot be simulated in Playwright's Firefox (as in commands.spec): the copy-paste
  // step is skipped there; the rest of the journey runs in every browser.
  const canPaste = browserName !== "firefox";
  if (!canPaste) test.info().annotations.push({ type: "limit", description: "keyboard paste not simulated in Firefox" });
  const ws = await server.connect(wb);
  const sheets = await ws.call("sheets", {});
  const sheet = sheets.find((s: { name: string }) => s.name === "Sheet 1").index;
  if (canPaste) {
    // Copy A1:C4 and paste at E1 with the keyboard: formulas follow (=SUMIFS(…,E2)).
    await cellText(page, "A1");
    for (let i = 0; i < 2; i++) await page.keyboard.press("Shift+ArrowRight");
    for (let i = 0; i < 3; i++) await page.keyboard.press("Shift+ArrowDown");
    await page.keyboard.press("ControlOrMeta+c");
    await expect(page.locator(".toast").last()).toContainText("Copied 12 cells");
    await cellText(page, "E1");
    await page.evaluate(() => {
      const data = new DataTransfer();
      data.setData("text/plain", (window as unknown as { __copied?: string }).__copied ?? "");
      document.querySelector(".grid-host")!.dispatchEvent(new ClipboardEvent("paste", { clipboardData: data, bubbles: true, cancelable: true }));
    });
    await expect.poll(() => cellText(page, "G3")).toContain("50");
    const detail = await ws.call("sheet_cell_detail", { sheet, row: 2, column: 6 });
    expect(JSON.stringify(detail)).toContain("E3");
  }

  // A chart of A1:C4: previewed first, stored only on Create chart.
  await cellText(page, "A1");
  for (let i = 0; i < 2; i++) await page.keyboard.press("Shift+ArrowRight");
  for (let i = 0; i < 3; i++) await page.keyboard.press("Shift+ArrowDown");
  await page.mouse.click(box.x + 90, box.y + HEADER_H + ROW_H / 2, { button: "right" });
  await page.getByRole("menuitem", { name: "Insert chart" }).click();
  expect((await ws.call("sheet_charts", { sheet })).length).toBe(0);
  await page.getByRole("region", { name: "Chart preview" }).getByRole("button", { name: "Create chart" }).click();
  await expect.poll(async () => (await ws.call("sheet_charts", { sheet })).length).toBe(1);
  await noSql(page);

  // Everything is still there after a reload.
  await page.reload();
  await page.getByRole("tab", { name: /Sheet 1/ }).click();
  expect(await cellText(page, "C2")).toContain("125");
  expect(await cellText(page, "B3")).toContain("East");
  if (canPaste) expect(await cellText(page, "G4")).toContain("10");
  await noSql(page);
  ws.close();
});
