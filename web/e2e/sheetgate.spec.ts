// S.12 gate: a spreadsheet user formats, finds, sorts, filters, freezes, charts and
// comments in a sheet with the UI, reloads and finds it all kept, downloads the XLSX (the
// compatibility list names what the file does not carry), in the light and the dark theme,
// with no serious accessibility violations on the way. Expected values are written out by
// hand from the cells the test types.

import AxeBuilder from "@axe-core/playwright";
import { expect, test, type Page } from "@playwright/test";
import { Server } from "./server";

const server = new Server();
test.beforeAll(async () => server.start());
test.afterAll(async () => server.dispose());

async function shown(page: Page, r: number, c: number): Promise<string> {
  await page.locator(".grid-host").focus();
  await page.keyboard.press("ControlOrMeta+Home");
  for (let i = 0; i < r; i++) await page.keyboard.press("ArrowDown");
  for (let i = 0; i < c; i++) await page.keyboard.press("ArrowRight");
  // The screen-reader cell follows the active cell on the next frame.
  await page.evaluate(() => new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r))));
  const id = await page.locator(".grid-host").getAttribute("aria-activedescendant");
  return (await page.locator(`#${id}`).textContent()) ?? "";
}

async function serious(page: Page): Promise<string[]> {
  const r = await new AxeBuilder({ page }).analyze();
  return r.violations.filter((v) => v.impact === "serious" || v.impact === "critical").map((v) => `${v.id}: ${v.nodes.map((n) => n.target.join(" ")).join(", ")}`);
}

for (const theme of ["light", "dark"] as const) {
  test(`S.12 gate in the ${theme} theme: format, find, sort, filter, freeze, chart, comment, reopen, XLSX`, async ({ page }) => {
    const wb = await server.createWorkbook(`Gate ${theme}`), ws = await server.connect(wb);
    await ws.call("sheet_create", { name: "Sales" });
    const data = [["region", "sales"], ["West", "125"], ["East", "50"], ["North", "80"], ["West", "40"], ["South", "95"]];
    await ws.call("sheet_set", { sheet: 0, inputs: data.flatMap((row, r) => row.map((value, c) => ({ row: r, column: c, value }))) });
    await page.addInitScript((t) => { try { localStorage.setItem("alephgrid-welcome-done", "1"); localStorage.setItem("alephgrid-theme", t); } catch { /* */ } }, theme);
    await page.goto(`${server.url}/w/${wb}`);
    await page.getByRole("tab", { name: /^Sales/ }).click();
    await expect.poll(() => shown(page, 0, 0)).toBe("region");
    const box = (await page.locator(".grid-scroller").boundingBox())!;
    const menus = page.getByRole("navigation", { name: "Workbook menus" });

    // Format: bold header row from the keyboard, a number format on the sales column.
    await page.keyboard.press("ControlOrMeta+Home");
    await page.keyboard.press("Shift+ArrowRight");
    await page.keyboard.press("ControlOrMeta+b");
    await expect.poll(async () => (await ws.call("sheet_fetch", { sheet: 0, row_start: 0, row_count: 1, column_count: 2 })).styles?.filter((s: { style: { b?: boolean } }) => s.style.b).length ?? 0).toBe(2);
    await page.keyboard.press("ControlOrMeta+Home");
    await page.keyboard.press("ArrowRight");
    await page.keyboard.press("ArrowDown");
    for (let i = 0; i < 4; i++) await page.keyboard.press("Shift+ArrowDown");
    await menus.getByRole("button", { name: "Format", exact: true }).click();
    await page.getByRole("menuitem", { name: "Currency: $1,234.56" }).click();
    await expect.poll(() => shown(page, 1, 1)).toBe("$125.00");

    // Find: two cells say West.
    await page.keyboard.press("ControlOrMeta+f");
    const bar = page.getByRole("search", { name: "Find in Sales" });
    await bar.getByRole("searchbox", { name: "Find" }).fill("west");
    await expect(bar.getByRole("status")).toHaveText("2 matches");
    await bar.getByRole("button", { name: "Close find" }).click();

    // Sort the data below the header by region, A → Z.
    await page.locator(".grid-host").focus();
    await page.keyboard.press("ControlOrMeta+Home");
    await page.keyboard.press("Shift+ArrowRight");
    for (let i = 0; i < 5; i++) await page.keyboard.press("Shift+ArrowDown");
    await page.mouse.click(box.x + 90, box.y + 44, { button: "right" });
    await page.getByRole("menuitem", { name: "Sort below the header row by A, A → Z" }).click();
    await expect.poll(() => shown(page, 1, 0)).toBe("East");
    await expect.poll(() => shown(page, 5, 0)).toBe("West");
    await expect.poll(() => shown(page, 1, 1)).toBe("$50.00");

    // Filter: keep only West (rows 5 and 6 after the sort); the others are hidden, not deleted.
    await page.keyboard.press("ControlOrMeta+Home");
    await page.keyboard.press("Shift+ArrowRight");
    for (let i = 0; i < 5; i++) await page.keyboard.press("Shift+ArrowDown");
    await page.mouse.click(box.x + 90, box.y + 44, { button: "right" });
    await page.getByRole("menuitem", { name: /^Filter by column A/ }).click();
    for (const v of ["East", "North", "South"]) await page.locator(".fp-item", { hasText: v }).locator("input").uncheck();
    await page.getByRole("button", { name: "Apply" }).click();
    await expect(page.getByText("3 rows hidden by the filter")).toBeVisible();
    await expect.poll(() => shown(page, 1, 0)).toBe("West");
    await page.mouse.click(box.x + 90, box.y + 44, { button: "right" });
    await page.getByRole("menuitem", { name: /^Filter by column A/ }).click();
    await page.getByRole("button", { name: "Clear", exact: true }).click();
    await expect.poll(() => shown(page, 1, 0)).toBe("East");

    // Freeze the header row.
    await page.keyboard.press("ControlOrMeta+Home");
    await menus.getByRole("button", { name: "View", exact: true }).click();
    await page.getByRole("menuitem", { name: /^Freeze rows 1–1/ }).click();
    await expect.poll(async () => (await ws.call("sheet_layout", { sheet: 0 })).frozen_rows).toBe(1);

    // Chart the data.
    await page.locator(".grid-host").focus();
    await page.keyboard.press("ControlOrMeta+Home");
    await page.keyboard.press("Shift+ArrowRight");
    for (let i = 0; i < 5; i++) await page.keyboard.press("Shift+ArrowDown");
    await page.mouse.click(box.x + 90, box.y + 44, { button: "right" });
    await page.getByRole("menuitem", { name: "Insert chart" }).click();
    const chart = page.getByRole("region", { name: "Chart of A1:B6" });
    await expect(chart.getByRole("table", { name: "Chart values" })).toContainText("East50");

    // Comment on a cell.
    await page.mouse.click(box.x + 90, box.y + 44, { button: "right" });
    await page.getByRole("menuitem", { name: /^Add comment/ }).click();
    const dialog = page.getByRole("dialog", { name: /^Comment on/ });
    await dialog.getByRole("textbox", { name: "Comment" }).fill("Check the East figure");
    await dialog.getByRole("button", { name: "Comment" }).click();
    await expect(page.getByRole("complementary", { name: "Comments" })).toContainText("Check the East figure");
    expect(await serious(page), "accessibility with the chart and comments open").toEqual([]);

    // Reopen: everything is kept.
    await page.reload();
    await page.getByRole("tab", { name: /^Sales/ }).click();
    await expect.poll(() => shown(page, 1, 1)).toBe("$50.00");
    await expect(page.getByRole("region", { name: "Chart of A1:B6" }).getByRole("table", { name: "Chart values" })).toContainText("East50");
    expect((await ws.call("sheet_layout", { sheet: 0 })).frozen_rows).toBe(1);
    expect((await ws.call("comments", { sheet: 0 })).map((c: { text: string }) => c.text)).toEqual(["Check the East figure"]);

    // XLSX: the file downloads, and the compatibility list names what it leaves out.
    const notes: string[] = await ws.call("xlsx_compatibility", {});
    expect(notes.some((n) => n.includes("sheet Sales: 1 chart(s) are not in the file"))).toBe(true);
    expect(notes.some((n) => n.includes("sheet Sales: 1 comment(s) are not in the file"))).toBe(true);
    const file = await page.request.get(`${server.url}/api/workbooks/${wb}/export.xlsx`);
    expect(file.status()).toBe(200);
    const bytes = await file.body();
    expect(bytes.subarray(0, 2).toString()).toBe("PK");
    ws.close();
  });
}
