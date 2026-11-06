// F.04a–d, clipboard: copies of filtered rows hold only the rows shown, cells copied in other
// spreadsheets paste with their values and formats (HTML table clipboard data), and cells
// copy between sheets and from a table into a sheet. The HTML fixtures below are written by
// hand after the clipboard formats other spreadsheets put next to their tab-separated text:
// class rules with `mso-number-format` and `x:num` raw values, or inline styles with
// `data-sheets-value` / `data-sheets-numberformat` JSON attributes.

import { expect, test, type Page } from "@playwright/test";
import { Server, type WsClient } from "./server";

const server = new Server();
test.beforeAll(async () => server.start());
test.afterAll(async () => server.dispose());

/** Grant clipboard access (Chromium) and remember what the app writes there. */
async function recordClipboard(page: Page) {
  if (page.context().browser()?.browserType().name() === "chromium") {
    await page.context().grantPermissions(["clipboard-read", "clipboard-write"], { origin: server.url });
  }
  await page.addInitScript(() => {
    try { localStorage.setItem("alephgrid-welcome-done", "1"); } catch { /* */ }
    const write = navigator.clipboard.writeText.bind(navigator.clipboard);
    navigator.clipboard.writeText = async (text: string) => {
      (window as unknown as { __copied: string }).__copied = text;
      return write(text).catch(() => undefined);
    };
  });
}

const copied = (page: Page) => page.evaluate(() => (window as unknown as { __copied?: string }).__copied ?? "");

/** A paste event carrying `text` and, when given, `html` (what Mod+V delivers). */
async function paste(page: Page, text: string, html?: string) {
  await page.evaluate(([text, html]) => {
    const data = new DataTransfer();
    data.setData("text/plain", text);
    if (html) data.setData("text/html", html);
    document.querySelector(".grid-host")!.dispatchEvent(new ClipboardEvent("paste", { clipboardData: data, bubbles: true, cancelable: true }));
  }, [text, html ?? ""] as const);
}

async function cells(ws: WsClient, sheet: number, rows: number, columns: number) {
  const block = await ws.call("sheet_fetch", { sheet, row_start: 0, row_count: rows, column_count: columns });
  const content = (r: number, c: number) => { const x = block.rows[r]?.[c]; return x ? x[2] ?? x[0] : ""; };
  const display = (r: number, c: number) => block.rows[r]?.[c]?.[0] ?? "";
  const style = (r: number, c: number) => (block.styles ?? []).find((s: { row: number; column: number }) => s.row === r && s.column === c)?.style ?? {};
  return { content, display, style };
}

/** Go to `cell` with Mod+G, leaving the grid focused there. */
async function go(page: Page, cell: string) {
  await page.locator(".grid-host").focus();
  await page.keyboard.press("ControlOrMeta+g");
  const field = page.getByRole("dialog", { name: "Go to" }).getByLabel("Cell, range or name");
  await field.fill(cell);
  await field.press("Enter");
  await expect(page.locator(".cell-ref")).toHaveText(cell);
}

test("a filtered range copies and pastes only the rows it shows (Mod+A, Mod+C)", async ({ page, browserName }) => {
  test.skip(browserName === "firefox", "paste cannot be simulated in Playwright's Firefox");
  await recordClipboard(page);
  const wb = await server.createWorkbook("Filtered copy"), ws = await server.connect(wb);
  await ws.call("sheet_create", { name: "Data" });
  await ws.call("sheet_set", { sheet: 0, inputs: [
    ["kind", "n"], ["x", "1"], ["y", "2"], ["x", "=B2+2"], ["y", "4"],
  ].flatMap((row, r) => row.map((value, c) => ({ row: r, column: c, value }))) });
  // Keep the "x" rows: rows 3 and 5 are hidden.
  expect(await ws.call("sheet_filter", { sheet: 0, area: { row: 0, column: 0, rows: 5, columns: 2 }, header: true, column: 0, keep: ["x"] })).toBe(2);
  await page.goto(`${server.url}/w/${wb}`);
  await expect(page.locator(".cell-ref")).toHaveText("A1");
  await page.locator(".grid-host").focus();
  // Mod+A takes the whole block, hidden rows inside it included; the copy leaves them out.
  await page.keyboard.press("ControlOrMeta+a");
  await expect(page.locator(".cell-ref")).toHaveText("A1:B5");
  await page.keyboard.press("ControlOrMeta+c");
  await expect(page.locator(".toast").last()).toContainText("Copied 6 cells");
  await expect(page.locator(".toast").last()).toContainText("hidden rows left out");
  expect(await copied(page)).toBe("kind\tn\nx\t1\nx\t3");
  // Pasted at D1 the shown rows land next to each other; the formula moves with its cell.
  await go(page, "D1");
  await paste(page, await copied(page));
  await expect(page.locator(".cell-ref")).toHaveText("D1:E3");
  const got = await cells(ws, 0, 6, 5);
  expect([0, 1, 2, 3].map((r) => got.content(r, 3))).toEqual(["kind", "x", "x", ""]);
  expect(got.content(2, 4)).toBe("=E1+2");
  // A cut over hidden rows is refused and moves nothing.
  await page.keyboard.press("ControlOrMeta+Home");
  await page.keyboard.press("ControlOrMeta+a");
  await expect(page.locator(".cell-ref")).toHaveText("A1:B5");
  await page.keyboard.press("ControlOrMeta+x");
  await go(page, "G1");
  await paste(page, await copied(page));
  await expect(page.locator(".toast").last()).toContainText("hidden rows or columns");
  expect((await cells(ws, 0, 1, 7)).content(0, 6)).toBe("");
  ws.close();
});

test("a filtered table copies only the rows it shows", async ({ page }) => {
  await recordClipboard(page);
  const wb = await server.createWorkbook("Filtered table");
  await server.api(`/api/workbooks/${wb}/import?filename=people.csv`, { method: "POST", body: "name,amount\nAda,10\nLinus,-3\nGrace,7\nKen,-1\n" });
  await page.goto(`${server.url}/w/${wb}`);
  await expect(page.getByRole("grid", { name: "Table people" })).toBeVisible();
  await page.getByRole("button", { name: "Filter", exact: true }).click();
  const f = page.getByRole("dialog", { name: "Filter rows" });
  await f.locator('select[name="column"]').selectOption("amount");
  await f.locator('select[name="op"]').selectOption("gte");
  await f.locator('input[name="value"]').fill("0");
  await f.getByRole("button", { name: "Apply filter" }).click();
  await expect(page.locator(".chips")).toContainText("amount");
  await page.locator(".grid-host").focus();
  await page.keyboard.press("ControlOrMeta+a");
  await page.keyboard.press("ControlOrMeta+c");
  await expect.poll(() => copied(page)).toBe("Ada\t10\nGrace\t7");
});

// Hand-written after the HTML a desktop spreadsheet puts on the clipboard: number formats
// and fonts in class rules (CSS-escaped), raw numbers in x:num, merged cells as colspan.
const CLASS_RULES_HTML = `<html xmlns:o="urn:schemas-microsoft-com:office:office" xmlns:x="urn:schemas-microsoft-com:office:excel" xmlns="http://www.w3.org/TR/REC-html40">
<head><meta http-equiv=Content-Type content="text/html; charset=utf-8"><meta name=ProgId content=Excel.Sheet>
<style>
<!--table
	{mso-displayed-decimal-separator:"\\.";
	mso-displayed-thousand-separator:"\\,";}
td
	{padding-top:1px;
	color:black;
	font-size:11.0pt;
	font-weight:400;
	mso-number-format:General;}
.xl65
	{mso-number-format:"\\0022$\\0022\\#\\,\\#\\#0\\.00";
	font-weight:700;}
.xl66
	{mso-number-format:"0\\.00%";}
.xl67
	{mso-number-format:"yyyy\\\\-mm\\\\-dd";
	font-style:italic;
	color:#C00000;}
.xl68
	{mso-number-format:"\\@";
	background:yellow;
	mso-pattern:black none;}
-->
</style></head>
<body link="#0563C1" vlink="#954F72">
<table border=0 cellpadding=0 cellspacing=0 width=256 style='border-collapse:collapse;width:192pt'>
<!--StartFragment-->
 <tr height=20 style='height:15.0pt'>
  <td height=20 class=xl65 align=right style='height:15.0pt' x:num="1234.5">$1,234.50</td>
  <td class=xl66 align=right x:num="0.125">12.50%</td>
  <td class=xl67 align=right x:num="45306">2024-01-15</td>
  <td class=xl68>007</td>
 </tr>
 <tr height=20 style='height:15.0pt'>
  <td height=20 style='height:15.0pt'>Two<br>lines</td>
  <td colspan=2 style='mso-ignore:colspan'>merged</td>
  <td style='text-decoration:underline'>u</td>
 </tr>
<!--EndFragment-->
</table>
</body>
</html>`;
const CLASS_RULES_TEXT = "$1,234.50\t12.50%\t2024-01-15\t007\n\"Two\nlines\"\tmerged\t\tu\n";

// Hand-written after the HTML a web spreadsheet puts on the clipboard: inline styles, typed
// values and number formats as JSON attributes, formulas in R1C1 (not pasted as formulas).
const q = (o: unknown) => JSON.stringify(o).replace(/"/g, "&quot;");
const INLINE_HTML = `<meta charset="utf-8"><google-sheets-html-origin><style type="text/css"><!--td {border: 1px solid #cccccc;}br {mso-data-placement:same-cell;}--></style>`
  + `<table xmlns="http://www.w3.org/1999/xhtml" cellspacing="0" cellpadding="0" dir="ltr" border="1" style="table-layout:fixed;font-size:10pt;font-family:Arial;width:0px;border-collapse:collapse;border:none" data-sheets-root="1">`
  + `<colgroup><col width="100"/><col width="100"/><col width="100"/></colgroup><tbody>`
  + `<tr style="height:21px;"><td style="overflow:hidden;padding:2px 3px 2px 3px;vertical-align:bottom;font-weight:bold;color:#0000ff;" data-sheets-value="${q({ 1: 2, 2: "007" })}">007</td>`
  + `<td style="overflow:hidden;padding:2px 3px 2px 3px;vertical-align:bottom;background-color:#ffff00;text-align:right;" data-sheets-value="${q({ 1: 3, 3: 0.25 })}" data-sheets-numberformat="${q({ 1: 3, 2: "0%", 3: 1 })}">25%</td>`
  + `<td style="overflow:hidden;padding:2px 3px 2px 3px;vertical-align:bottom;text-align:right;" data-sheets-value="${q({ 1: 3, 3: 42 })}" data-sheets-formula="=R[0]C[-1]*168">42</td></tr>`
  + `<tr style="height:21px;"><td style="overflow:hidden;padding:2px 3px 2px 3px;vertical-align:bottom;font-style:italic;" data-sheets-value="${q({ 1: 4, 4: 1 })}">TRUE</td>`
  + `<td style="overflow:hidden;padding:2px 3px 2px 3px;vertical-align:bottom;" data-sheets-value="${q({ 1: 2, 2: "=not a formula" })}">=not a formula</td><td></td></tr>`
  + `</tbody></table>`;
const INLINE_TEXT = "007\t25%\t42\nTRUE\t=not a formula\t";

test("cells copied in other spreadsheets paste with their values and formats", async ({ page, browserName }) => {
  test.skip(browserName === "firefox", "paste cannot be simulated in Playwright's Firefox");
  await recordClipboard(page);
  const wb = await server.createWorkbook("Formatted paste"), ws = await server.connect(wb);
  await ws.call("sheet_create", { name: "Data" });
  await ws.call("sheet_set", { sheet: 0, inputs: [{ row: 1, column: 2, value: "old" }] });
  await page.goto(`${server.url}/w/${wb}`);
  await expect(page.locator(".cell-ref")).toHaveText("A1");

  // Class rules and x:num.
  await go(page, "A1");
  await paste(page, CLASS_RULES_TEXT, CLASS_RULES_HTML);
  await expect(page.locator(".cell-ref")).toHaveText("A1:D2");
  let got = await cells(ws, 0, 2, 4);
  expect([got.display(0, 0), got.content(0, 0)]).toEqual(["$1,234.50", "1234.5"]);
  expect(got.style(0, 0)).toMatchObject({ b: true, num_fmt: "\"$\"#,##0.00" });
  expect([got.display(0, 1), got.content(0, 1)]).toEqual(["12.50%", "0.125"]);
  expect(got.display(0, 2)).toBe("2024-01-15");
  expect(got.style(0, 2)).toMatchObject({ i: true, color: "#C00000" });
  expect([got.display(0, 3), got.content(0, 3)]).toEqual(["007", "007"]);
  expect(got.style(0, 3)).toMatchObject({ fill: "#FFFF00" });
  expect(got.display(1, 0)).toBe("Two\nlines");
  expect(got.style(1, 0)).toEqual({});
  expect([got.display(1, 1), got.display(1, 2)]).toEqual(["merged", ""]);
  expect(got.style(1, 3)).toMatchObject({ u: true });
  // One undo step takes it all back, the old value included.
  await page.keyboard.press("ControlOrMeta+z");
  await expect.poll(async () => (await cells(ws, 0, 2, 4)).display(1, 2)).toBe("old");
  expect((await cells(ws, 0, 1, 1)).content(0, 0)).toBe("");

  // Inline styles and typed JSON values, at B5.
  await go(page, "B5");
  await paste(page, INLINE_TEXT, INLINE_HTML);
  await expect(page.locator(".cell-ref")).toHaveText("B5:D6");
  got = await cells(ws, 0, 6, 4);
  expect([got.display(4, 1), got.content(4, 1)]).toEqual(["007", "007"]);
  expect(got.style(4, 1)).toMatchObject({ b: true, color: "#0000FF" });
  expect([got.display(4, 2), got.content(4, 2)]).toEqual(["25%", "0.25"]);
  expect(got.style(4, 2)).toMatchObject({ fill: "#FFFF00", num_fmt: "0%" });
  expect(got.content(4, 3)).toBe("42");
  expect(got.display(5, 1)).toBe("TRUE");
  expect(got.style(5, 1)).toMatchObject({ i: true });
  expect(got.display(5, 2)).toBe("=not a formula");

  // Pasted HTML never runs and never reaches the page; only the cell text is kept.
  await go(page, "A8");
  await paste(page, "a\tb", `<table><tr><td><img src="x" onerror="window.__ran=1">a</td><td><script>window.__ran=2</script>b<style>td{}</style></td></tr></table>`);
  await expect.poll(async () => (await cells(ws, 0, 8, 2)).display(7, 1)).toBe("b");
  expect((await cells(ws, 0, 8, 2)).display(7, 0)).toBe("a");
  expect(await page.evaluate(() => (window as unknown as { __ran?: number }).__ran)).toBeUndefined();
  expect(await page.locator('img[src="x"]').count()).toBe(0);

  // HTML without a table: the tab-separated text is pasted.
  await go(page, "A10");
  await paste(page, "7\t8", "<p>7 8</p>");
  await expect.poll(async () => (await cells(ws, 0, 10, 2)).content(9, 1)).toBe("8");
  ws.close();
});

test("HTML pasted into a table keeps its raw values", async ({ page, browserName }) => {
  test.skip(browserName === "firefox", "paste cannot be simulated in Playwright's Firefox");
  await recordClipboard(page);
  const wb = await server.createWorkbook("Table paste"), ws = await server.connect(wb);
  await ws.call("create_table_from_query", { name: "t", sql: "SELECT 'a' AS label, 1.0::DOUBLE AS amount" });
  await page.goto(`${server.url}/w/${wb}`);
  await expect(page.getByRole("grid", { name: "Table t" })).toBeVisible();
  await page.locator(".grid-host").focus();
  // The displayed "$1,234.50" would not fit a number column; its raw 1234.5 does.
  await paste(page, "b\t$1,234.50", `<table><tr><td>b</td><td class=xl65 x:num="1234.5" style="font-weight:700">$1,234.50</td></tr></table>`);
  await expect.poll(async () => (await ws.call("query", { sql: "SELECT label, amount FROM t" })).rows).toEqual([["b", 1234.5]]);
  ws.close();
});

test("cells copy between sheets, and from a table into a sheet with their types", async ({ page, browserName }) => {
  test.skip(browserName === "firefox", "paste cannot be simulated in Playwright's Firefox");
  await recordClipboard(page);
  const wb = await server.createWorkbook("Between sheets"), ws = await server.connect(wb);
  await ws.call("sheet_create", { name: "Data" });
  await ws.call("sheet_create", { name: "Other" });
  // B1 mixes relative, absolute, mixed and qualified references.
  await ws.call("sheet_set", { sheet: 0, inputs: [
    { row: 0, column: 0, value: "2" }, { row: 0, column: 1, value: "=A1*10+$A$1+A$1+$A1+Data!A1+Other!A1" },
  ] });
  await ws.call("sheet_set", { sheet: 1, inputs: [{ row: 0, column: 0, value: "5" }, { row: 1, column: 0, value: "7" }] });
  await page.goto(`${server.url}/w/${wb}`);
  await page.getByRole("tab", { name: /^Data/ }).click();
  await go(page, "B1");
  await page.keyboard.press("ControlOrMeta+c");
  await expect(page.locator(".toast").last()).toContainText("Copied 1 cells");
  // Pasted on Other at B2: unqualified references are relative to Other, `$` parts stay,
  // references naming a sheet keep naming it, as in other spreadsheets.
  await page.getByRole("tab", { name: /^Other/ }).click();
  await go(page, "B2");
  await paste(page, await copied(page));
  await expect.poll(async () => (await cells(ws, 1, 2, 2)).content(1, 1)).toBe("=A2*10+$A$1+A$1+$A2+Data!A2+Other!A2");
  // =7*10+5+5+7+0+7
  expect((await cells(ws, 1, 2, 2)).display(1, 1)).toBe("94");
  // A cut cannot move cells to another sheet yet: it says so and changes nothing.
  await page.getByRole("tab", { name: /^Data/ }).click();
  await go(page, "A1");
  await page.keyboard.press("ControlOrMeta+x");
  await page.getByRole("tab", { name: /^Other/ }).click();
  await go(page, "D1");
  await paste(page, await copied(page));
  await expect(page.locator(".toast").last()).toContainText("another sheet");
  expect((await cells(ws, 0, 1, 1)).content(0, 0)).toBe("2");

  // From a table: text stays text (leading zeros), numbers stay numbers.
  await ws.call("create_table_from_query", { name: "codes", sql: "SELECT '007' AS code, 12 AS n UNION ALL SELECT '010', 3" });
  await page.reload();
  await page.getByRole("tab", { name: /codes$/ }).click();
  await expect(page.getByRole("grid", { name: "Table codes" })).toBeVisible();
  // (Until its rows arrive the table has nothing to copy.)
  await expect(async () => {
    await page.locator(".grid-host").focus();
    await page.keyboard.press("ControlOrMeta+a");
    await page.keyboard.press("ControlOrMeta+c");
    expect(await copied(page)).toBe("007\t12\n010\t3");
  }).toPass();
  await page.getByRole("tab", { name: /^Other/ }).click();
  await go(page, "D4");
  await paste(page, await copied(page));
  await expect.poll(async () => (await cells(ws, 1, 5, 5)).display(3, 3)).toBe("007");
  const got = await cells(ws, 1, 5, 5);
  expect([got.content(3, 3), got.content(4, 3), got.content(3, 4), got.content(4, 4)]).toEqual(["007", "010", "12", "3"]);
  ws.close();
});
