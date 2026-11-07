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

/** Grant clipboard access (Chromium) and stand in for the system clipboard: what the app
 *  writes (text, and HTML through `navigator.clipboard.write`) is recorded instead of
 *  reaching the machine's clipboard (which other programs share), and a read from a menu
 *  (paste special) returns what a test put there with `setClipboard`, or is refused like a
 *  browser that does not let pages read the clipboard (`denyRead`). */
async function recordClipboard(page: Page) {
  if (page.context().browser()?.browserType().name() === "chromium") {
    await page.context().grantPermissions(["clipboard-read", "clipboard-write"], { origin: server.url });
  }
  await page.addInitScript(() => {
    try { localStorage.setItem("alephgrid-welcome-done", "1"); } catch { /* */ }
    type Board = { __copied?: string; __copiedHtml?: string; __noRead?: boolean };
    const w = window as unknown as Board;
    navigator.clipboard.writeText = async (text: string) => { w.__copied = text; w.__copiedHtml = undefined; };
    navigator.clipboard.write = async (items: ClipboardItem[]) => {
      for (const item of items) {
        w.__copied = item.types.includes("text/plain") ? await (await item.getType("text/plain")).text() : "";
        w.__copiedHtml = item.types.includes("text/html") ? await (await item.getType("text/html")).text() : undefined;
      }
    };
    navigator.clipboard.read = async () => {
      if (w.__noRead) throw new DOMException("Read permission denied.", "NotAllowedError");
      const parts: Record<string, Blob> = { "text/plain": new Blob([w.__copied ?? ""], { type: "text/plain" }) };
      if (w.__copiedHtml) parts["text/html"] = new Blob([w.__copiedHtml], { type: "text/html" });
      return [new ClipboardItem(parts)];
    };
  });
}

const copied = (page: Page) => page.evaluate(() => (window as unknown as { __copied?: string }).__copied ?? "");
const copiedHtml = (page: Page) => page.evaluate(() => (window as unknown as { __copiedHtml?: string }).__copiedHtml ?? "");

/** What another app put on the clipboard, for paste special to read. */
async function setClipboard(page: Page, text: string, html?: string) {
  await page.evaluate(([text, html]) => {
    const w = window as unknown as { __copied?: string; __copiedHtml?: string };
    w.__copied = text; w.__copiedHtml = html || undefined;
  }, [text, html ?? ""] as const);
}

const denyRead = (page: Page) => page.evaluate(() => { (window as unknown as { __noRead: boolean }).__noRead = true; });

/** A paste event carrying `text` and, when given, `html` (what Mod+V delivers). The data is
 *  attached as the event's `clipboardData` property: Playwright's Firefox drops the data of
 *  script-made ClipboardEvents, and this way every browser gets the same event. */
async function paste(page: Page, text: string, html?: string) {
  await page.evaluate(([text, html]) => {
    const data = new DataTransfer();
    data.setData("text/plain", text);
    if (html) data.setData("text/html", html);
    const event = new Event("paste", { bubbles: true, cancelable: true });
    Object.defineProperty(event, "clipboardData", { value: data });
    document.querySelector(".grid-host")!.dispatchEvent(event);
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

test("a filtered range copies and pastes only the rows it shows (Mod+A, Mod+C)", async ({ page }) => {
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
  await expect(page.locator(".grid-note")).toContainText("Nothing was moved: the cut cells include hidden rows or columns");
  expect((await cells(ws, 0, 1, 7)).content(0, 6)).toBe("");
  expect((await cells(ws, 0, 5, 2)).content(2, 0), "the hidden row stays where it was").toBe("y");
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
const CLASS_RULES_HTML = `<html xmlns="http://www.w3.org/TR/REC-html40">
<head><meta http-equiv=Content-Type content="text/html; charset=utf-8">
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
const INLINE_HTML = `<meta charset="utf-8"><style type="text/css"><!--td {border: 1px solid #cccccc;}br {mso-data-placement:same-cell;}--></style>`
  + `<table xmlns="http://www.w3.org/1999/xhtml" cellspacing="0" cellpadding="0" dir="ltr" border="1" style="table-layout:fixed;font-size:10pt;font-family:Arial;width:0px;border-collapse:collapse;border:none" data-sheets-root="1">`
  + `<colgroup><col width="100"/><col width="100"/><col width="100"/></colgroup><tbody>`
  + `<tr style="height:21px;"><td style="overflow:hidden;padding:2px 3px 2px 3px;vertical-align:bottom;font-weight:bold;color:#0000ff;" data-sheets-value="${q({ 1: 2, 2: "007" })}">007</td>`
  + `<td style="overflow:hidden;padding:2px 3px 2px 3px;vertical-align:bottom;background-color:#ffff00;text-align:right;" data-sheets-value="${q({ 1: 3, 3: 0.25 })}" data-sheets-numberformat="${q({ 1: 3, 2: "0%", 3: 1 })}">25%</td>`
  + `<td style="overflow:hidden;padding:2px 3px 2px 3px;vertical-align:bottom;text-align:right;" data-sheets-value="${q({ 1: 3, 3: 42 })}" data-sheets-formula="=R[0]C[-1]*168">42</td></tr>`
  + `<tr style="height:21px;"><td style="overflow:hidden;padding:2px 3px 2px 3px;vertical-align:bottom;font-style:italic;" data-sheets-value="${q({ 1: 4, 4: 1 })}">TRUE</td>`
  + `<td style="overflow:hidden;padding:2px 3px 2px 3px;vertical-align:bottom;" data-sheets-value="${q({ 1: 2, 2: "=not a formula" })}">=not a formula</td><td></td></tr>`
  + `</tbody></table>`;
const INLINE_TEXT = "007\t25%\t42\nTRUE\t=not a formula\t";

test("cells copied in other spreadsheets paste with their values and formats", async ({ page }) => {
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
  // Text that would read as a number keeps its quote prefix in its input, so editing it
  // again keeps it text.
  expect([got.display(0, 3), got.content(0, 3)]).toEqual(["007", "'007"]);
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
  expect([got.display(4, 1), got.content(4, 1)]).toEqual(["007", "'007"]);
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

  // Larger than the edit limit (1,000,000 cells here): refused before anything is sent.
  await go(page, "A12");
  await paste(page, "big", `<table><tr><td colspan=1000 rowspan=1000>big</td><td>one more</td></tr></table>`);
  await expect(page.locator(".grid-note")).toContainText("Nothing was pasted: that is more than 1,000,000 cells");
  expect((await cells(ws, 0, 12, 1)).content(11, 0)).toBe("");

  // HTML without a table: the tab-separated text is pasted.
  await go(page, "A10");
  await paste(page, "7\t8", "<p>7 8</p>");
  await expect.poll(async () => (await cells(ws, 0, 10, 2)).content(9, 1)).toBe("8");
  ws.close();
});

test("HTML pasted into a table keeps its raw values", async ({ page }) => {
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

// A slow server (CI) can answer a table's open_view after the grid is already shown and
// focused. A paste made then waits for the table's columns instead of pasting nothing, and a
// paste that reaches no editable cell says so instead of reporting "Pasted 0 cells".
test("a paste while the table is still opening waits for it; a paste with nowhere to go says so", async ({ page }) => {
  await recordClipboard(page);
  const wb = await server.createWorkbook("Opening paste"), ws = await server.connect(wb);
  await ws.call("create_table_from_query", { name: "t", sql: "SELECT 'a' AS label, 1.0::DOUBLE AS amount" });
  await ws.call("add_formula_column", { table: "t", name: "twice", formula: "amount * 2" });
  // Hold the server's answer to open_view until the paste has been made.
  let release!: () => void;
  const released = new Promise<void>((r) => (release = r));
  const openIds = new Set<number>();
  await page.routeWebSocket(/\/ws\//, (route) => {
    const srv = route.connectToServer();
    route.onMessage((m) => {
      try { const f = JSON.parse(String(m)); if (f.op === "open_view") openIds.add(f.id); } catch { /* */ }
      srv.send(m);
    });
    srv.onMessage(async (m) => {
      try { const f = JSON.parse(String(m)); if (openIds.has(f.id)) await released; } catch { /* */ }
      route.send(m);
    });
  });
  await page.goto(`${server.url}/w/${wb}`);
  await expect(page.getByRole("grid", { name: "Table t" })).toBeVisible();
  await expect.poll(() => openIds.size).toBeGreaterThan(0);
  await page.locator(".grid-host").focus();
  await paste(page, "b\t2");
  release();
  await expect.poll(async () => (await ws.call("query", { sql: "SELECT label, amount FROM t" })).rows).toEqual([["b", 2]]);
  await expect(page.locator(".toast").last()).toContainText("Pasted 2 cells");

  // Only the formula column (C) and past the end: nothing is sent, and the note says why.
  await page.locator(".grid-host").focus();
  await page.keyboard.press("ControlOrMeta+Home");
  await page.keyboard.press("ArrowRight");
  await page.keyboard.press("ArrowRight");
  await expect(page.locator(".cell-ref")).toContainText("twice");
  await paste(page, "9\t9");
  await expect(page.locator(".grid-note")).toContainText("Nothing was pasted: the cells it would fill are formula columns or past the table's last column.");
  expect((await ws.call("query", { sql: "SELECT label, amount FROM t" })).rows).toEqual([["b", 2]]);
  ws.close();
});

test("cells copy between sheets, and from a table into a sheet with their types", async ({ page }) => {
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
  // One undo step, redo, and it is still there after reopening.
  await page.keyboard.press("ControlOrMeta+z");
  await expect.poll(async () => (await cells(ws, 1, 2, 2)).content(1, 1)).toBe("");
  await page.keyboard.press("ControlOrMeta+Shift+z");
  await expect.poll(async () => (await cells(ws, 1, 2, 2)).display(1, 1)).toBe("94");
  await page.reload();
  await page.getByRole("tab", { name: /^Other/ }).click();
  await expect.poll(async () => (await cells(ws, 1, 2, 2)).content(1, 1)).toBe("=A2*10+$A$1+A$1+$A2+Data!A2+Other!A2");

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
  // The text cells' inputs keep their quote prefix, so editing them again keeps them text.
  expect([got.content(3, 3), got.content(4, 3), got.content(3, 4), got.content(4, 4)]).toEqual(["'007", "'010", "12", "3"]);
  ws.close();
});

test("copying sheet cells writes an HTML table with raw values and formats; cell text is escaped", async ({ page }) => {
  await recordClipboard(page);
  const wb = await server.createWorkbook("Copy out"), ws = await server.connect(wb);
  await ws.call("sheet_create", { name: "Data" });
  await ws.call("sheet_set", { sheet: 0, inputs: [
    { row: 0, column: 0, value: "1234.5" }, { row: 0, column: 1, value: "<script>window.__ran=1</script>" },
    { row: 1, column: 0, value: `say "hi" & bye` }, { row: 1, column: 1, value: "'007" },
    { row: 2, column: 0, value: "=A1*2" }, { row: 2, column: 1, value: "TRUE" },
  ] });
  await ws.call("sheet_style", { sheet: 0, area: { row: 0, column: 0, rows: 1, columns: 1 }, style: { b: true, num_fmt: "#,##0.00" } });
  await ws.call("sheet_style", { sheet: 0, area: { row: 0, column: 1, rows: 1, columns: 1 }, style: { i: true, u: true, s: true, color: "#FF0000" } });
  await ws.call("sheet_style", { sheet: 0, area: { row: 1, column: 0, rows: 1, columns: 1 }, style: { fill: "#FFFF00" } });
  await page.goto(`${server.url}/w/${wb}`);
  await expect(page.locator(".cell-ref")).toHaveText("A1");
  await page.locator(".grid-host").focus();
  await page.keyboard.press("Shift+ArrowRight"); await page.keyboard.press("Shift+ArrowDown"); await page.keyboard.press("Shift+ArrowDown");
  await page.keyboard.press("ControlOrMeta+c");
  await expect(page.locator(".toast").last()).toContainText("Copied 6 cells");
  // The text is the displayed values, as before.
  expect(await copied(page)).toBe(`1,234.50\t<script>window.__ran=1</script>\n"say ""hi"" & bye"\t007\n2469\tTRUE`);
  const html = await copiedHtml(page);
  // Every value is escaped: no markup from a cell, no attribute broken out of.
  expect(html).not.toContain("<script");
  expect(html).toContain("&lt;script&gt;window.__ran=1&lt;/script&gt;");
  expect(html).toContain("say \"hi\" &amp; bye");
  const parsed = await page.evaluate((html) => {
    const doc = new DOMParser().parseFromString(html, "text/html");
    return {
      scripts: doc.querySelectorAll("script").length,
      cells: [...doc.querySelectorAll("tr")].map((tr) => [...tr.querySelectorAll("td")].map((td) => ({
        text: td.textContent, value: td.getAttribute("data-sheets-value"), num: td.getAttribute("x:num"),
        bool: td.getAttribute("x:bool"), style: td.getAttribute("style") ?? "",
      }))),
    };
  }, html);
  expect(parsed.scripts).toBe(0);
  const [[a1, b1], [a2, b2], [a3, b3]] = parsed.cells;
  expect([a1.text, a1.value, a1.num]).toEqual(["1,234.50", '{"1":3,"3":1234.5}', "1234.5"]);
  expect(a1.style).toContain("font-weight:bold");
  expect(a1.style).toContain('mso-number-format:"#,##0.00"');
  expect(b1.text).toBe("<script>window.__ran=1</script>");
  expect(b1.style).toContain("font-style:italic");
  expect(b1.style).toContain("text-decoration:underline line-through");
  expect(b1.style).toContain("color:#FF0000");
  expect([a2.text, a2.value]).toEqual([`say "hi" & bye`, JSON.stringify({ 1: 2, 2: `say "hi" & bye` })]);
  expect(a2.style).toContain("background-color:#FFFF00");
  expect([b2.text, b2.value]).toEqual(["007", '{"1":2,"2":"007"}']);
  expect(b2.style).toContain('mso-number-format:"@"');
  expect([a3.text, a3.value, a3.num]).toEqual(["2469", '{"1":3,"3":2469}', "2469"]);
  expect([b3.text, b3.bool]).toEqual(["TRUE", "TRUE"]);
  expect(await page.evaluate(() => (window as unknown as { __ran?: number }).__ran)).toBeUndefined();

  // Pasted into another workbook (as another app would get it), values and formats survive.
  const wb2 = await server.createWorkbook("Copy in"), ws2 = await server.connect(wb2);
  await ws2.call("sheet_create", { name: "In" });
  const text = await copied(page);
  await page.goto(`${server.url}/w/${wb2}`);
  await expect(page.locator(".cell-ref")).toHaveText("A1");
  await page.locator(".grid-host").focus();
  await paste(page, text, html);
  await expect.poll(async () => (await cells(ws2, 0, 3, 2)).content(2, 1)).toBe("TRUE");
  const got = await cells(ws2, 0, 3, 2);
  expect([got.display(0, 0), got.content(0, 0)]).toEqual(["1,234.50", "1234.5"]);
  expect(got.style(0, 0)).toMatchObject({ b: true, num_fmt: "#,##0.00" });
  expect(got.content(0, 1)).toBe("<script>window.__ran=1</script>");
  expect(got.style(0, 1)).toMatchObject({ i: true, u: true, s: true, color: "#FF0000" });
  expect(got.content(1, 0)).toBe(`say "hi" & bye`);
  expect(got.style(1, 0)).toMatchObject({ fill: "#FFFF00" });
  expect([got.display(1, 1), got.content(1, 1)]).toEqual(["007", "'007"]);
  expect(got.content(2, 0)).toBe("2469");
  ws.close(); ws2.close();
});

// Cells as a spreadsheet that writes formulas into its clipboard HTML puts them there: typed
// values, and each formula in R1C1 notation relative to its own cell (written by hand).
const FORMULA_TEXT = "6\n99";
const FORMULA_HTML = `<meta charset="utf-8"><table data-sheets-root="1"><tbody>`
  + `<tr><td data-sheets-value='{"1":3,"3":6}' data-sheets-formula="=R[0]C[-1]*2">6</td></tr>`
  + `<tr><td data-sheets-value='{"1":3,"3":99}' data-sheets-formula="=STOCKQUOTE(&quot;X&quot;)" style="font-weight:bold">99</td></tr>`
  + `</tbody></table>`;

test("formulas from other spreadsheets: Mod+V pastes their values, paste special takes the formulas", async ({ page }) => {
  await recordClipboard(page);
  const wb = await server.createWorkbook("Formulas in"), ws = await server.connect(wb);
  await ws.call("sheet_create", { name: "Data" });
  await ws.call("sheet_set", { sheet: 0, inputs: [{ row: 0, column: 0, value: "3" }, { row: 1, column: 0, value: "4" }] });
  await page.goto(`${server.url}/w/${wb}`);
  await expect(page.locator(".cell-ref")).toHaveText("A1");
  // Mod+V at B1: values (and formats); the toast says where the formulas are.
  await go(page, "B1");
  await paste(page, FORMULA_TEXT, FORMULA_HTML);
  await expect.poll(async () => (await cells(ws, 0, 2, 2)).content(1, 1)).toBe("99");
  expect((await cells(ws, 0, 2, 2)).content(0, 1)).toBe("6");
  await expect(page.locator(".toast").last()).toContainText("Paste special → Formulas");
  // Paste special → Formulas at C1: R[0]C[-1] is the cell to the left of C1. The formula
  // calling a function Alephgrid lacks pastes as its value.
  await setClipboard(page, FORMULA_TEXT, FORMULA_HTML);
  await go(page, "C1");
  await page.keyboard.press("ControlOrMeta+Shift+v");
  await page.getByRole("menu", { name: "Paste special" }).getByRole("menuitem", { name: "Formulas (no formats)" }).click();
  await expect.poll(async () => (await cells(ws, 0, 2, 3)).content(0, 2)).toBe("=B1*2");
  let got = await cells(ws, 0, 2, 3);
  expect(got.display(0, 2)).toBe("12");
  expect(got.content(1, 2)).toBe("99");
  expect(got.style(1, 2)).toEqual({});
  // One undo step.
  await page.keyboard.press("ControlOrMeta+z");
  await expect.poll(async () => (await cells(ws, 0, 2, 3)).content(0, 2)).toBe("");
  // Plain text starting with = stays text.
  await go(page, "D1");
  await paste(page, "=A1*100");
  await expect.poll(async () => (await cells(ws, 0, 1, 4)).display(0, 3)).toBe("=A1*100");
  got = await cells(ws, 0, 1, 4);
  expect(got.content(0, 3), "its input is marked as text").toBe("'=A1*100");
  ws.close();
});

test("paste special works on text and HTML from other apps", async ({ page }) => {
  await recordClipboard(page);
  const wb = await server.createWorkbook("Special from apps"), ws = await server.connect(wb);
  await ws.call("sheet_create", { name: "Data" });
  await ws.call("sheet_set", { sheet: 0, inputs: [{ row: 0, column: 0, value: "3" }, { row: 2, column: 0, value: "keep" }] });
  await ws.call("sheet_style", { sheet: 0, area: { row: 2, column: 0, rows: 1, columns: 1 }, style: { i: true } });
  await page.goto(`${server.url}/w/${wb}`);
  await expect(page.locator(".cell-ref")).toHaveText("A1");
  const special = async (cell: string, item: string) => {
    await go(page, cell);
    await page.keyboard.press("ControlOrMeta+Shift+v");
    await page.getByRole("menu", { name: "Paste special" }).getByRole("menuitem", { name: item, exact: true }).click();
  };
  // Tab-separated text, values transposed at E1.
  await setClipboard(page, "1\t2");
  await special("E1", "Values only, transposed");
  await expect.poll(async () => (await cells(ws, 0, 2, 5)).content(1, 4)).toBe("2");
  expect((await cells(ws, 0, 2, 5)).content(0, 4)).toBe("1");
  // HTML, formats only, on A1: the value stays, the bold arrives.
  await setClipboard(page, "x", `<table><tr><td style="font-weight:bold">x</td></tr></table>`);
  await special("A1", "Formats only");
  await expect.poll(async () => (await cells(ws, 0, 1, 1)).style(0, 0)).toMatchObject({ b: true });
  expect((await cells(ws, 0, 1, 1)).content(0, 0)).toBe("3");
  // HTML, values only, on A3: the value arrives, the italic stays, no bold.
  await special("A3", "Values only");
  await expect.poll(async () => (await cells(ws, 0, 3, 1)).content(2, 0)).toBe("x");
  expect((await cells(ws, 0, 3, 1)).style(2, 0)).toEqual({ i: true });
  // Text has no formats: refused at the cell, nothing changed.
  await setClipboard(page, "y");
  await special("A3", "Formats only");
  await expect(page.locator(".grid-note")).toContainText("plain text, which has no formats");
  expect((await cells(ws, 0, 3, 1)).content(2, 0)).toBe("x");
  // A browser that will not let the page read the clipboard: the next Mod+V pastes as chosen.
  await denyRead(page);
  await special("B1", "Values only");
  await expect(page.locator(".grid-note")).toContainText(/Press (⌘V|Ctrl\+V) to paste values only/);
  await paste(page, "z", `<table><tr><td style="font-weight:bold">z</td></tr></table>`);
  await expect.poll(async () => (await cells(ws, 0, 1, 2)).content(0, 1)).toBe("z");
  expect((await cells(ws, 0, 1, 2)).style(0, 1)).toEqual({});
  await expect(page.locator(".grid-note")).toHaveCount(0);
  ws.close();
});

test("a cut pasted on another sheet moves the cells and the formulas that read them, as one undo step", async ({ page }) => {
  await recordClipboard(page);
  const wb = await server.createWorkbook("Cross-sheet move"), ws = await server.connect(wb);
  await ws.call("sheet_create", { name: "Data" });
  await ws.call("sheet_create", { name: "Other" });
  await ws.call("sheet_set", { sheet: 0, inputs: [
    { row: 0, column: 0, value: "5" }, { row: 1, column: 0, value: "=A1*2" }, { row: 0, column: 1, value: "=A1+A2" },
  ] });
  await page.goto(`${server.url}/w/${wb}`);
  await page.getByRole("tab", { name: /^Data/ }).click();
  await go(page, "A1");
  await page.keyboard.press("Shift+ArrowDown");
  await page.keyboard.press("ControlOrMeta+x");
  await expect(page.locator(".toast").last()).toContainText("Cut.");
  await page.getByRole("tab", { name: /^Other/ }).click();
  await go(page, "D6");
  await paste(page, await copied(page));
  await expect.poll(async () => (await cells(ws, 1, 7, 4)).content(6, 3)).toBe("=D6*2");
  expect((await cells(ws, 1, 7, 4)).content(5, 3)).toBe("5");
  let data = await cells(ws, 0, 2, 2);
  expect([data.content(0, 0), data.content(1, 0)]).toEqual(["", ""]);
  expect([data.content(0, 1), data.display(0, 1)]).toEqual(["=Other!D6+Other!D7", "15"]);
  // One undo step puts everything back on Data.
  await page.keyboard.press("ControlOrMeta+z");
  await expect.poll(async () => (await cells(ws, 0, 2, 2)).content(0, 0)).toBe("5");
  data = await cells(ws, 0, 2, 2);
  expect([data.content(1, 0), data.content(0, 1)]).toEqual(["=A1*2", "=A1+A2"]);
  expect((await cells(ws, 1, 7, 4)).content(5, 3)).toBe("");
  ws.close();
});

test("refused pastes and cuts change nothing and say why at the cell", async ({ page }) => {
  await recordClipboard(page);
  const wb = await server.createWorkbook("Refusals"), ws = await server.connect(wb);
  await ws.call("sheet_create", { name: "Data" });
  await ws.call("sheet_set", { sheet: 0, inputs: [
    { row: 0, column: 0, value: "keep" }, { row: 0, column: 1, value: "yes" }, { row: 1, column: 0, value: "a2" }, { row: 2, column: 0, value: "a3" },
  ] });
  await ws.call("set_sheet_rules", { sheet: 0, rules: { colors: [], validations: [{ area: { row: 0, column: 1, rows: 5, columns: 1 }, list: ["yes", "no"] }] } });
  await page.goto(`${server.url}/w/${wb}`);
  await expect(page.locator(".cell-ref")).toHaveText("A1");
  const unchanged = async () => {
    const got = await cells(ws, 0, 3, 3);
    expect([got.content(0, 0), got.content(0, 1), got.content(1, 0), got.content(2, 0), got.content(0, 2)]).toEqual(["keep", "yes", "a2", "a3", ""]);
  };
  // Invalid: B2 does not allow "maybe"; the valid cells are not pasted either.
  await go(page, "A1");
  await paste(page, "new\tno\nnew\tmaybe");
  await expect(page.locator(".grid-note")).toContainText("Nothing was pasted: B2");
  await unchanged();
  // Oversized: more cells than one edit takes, refused before anything is sent.
  await page.keyboard.press("Escape");
  await expect(page.locator(".grid-note")).toHaveCount(0);
  await paste(page, Array.from({ length: 1001 }, () => "1\t".repeat(999) + "1").join("\n"));
  await expect(page.locator(".grid-note")).toContainText("more than 1,000,000 cells");
  await unchanged();
  // A refused cut: row 2, hidden inside the cut range, cannot move unseen; the source stays.
  await ws.call("sheet_set_hidden", { sheet: 0, axis: "rows", start: 1, count: 1, hidden: true });
  await page.reload();
  await expect(page.locator(".cell-ref")).toHaveText("A1");
  await go(page, "A1:A3");
  await page.keyboard.press("ControlOrMeta+x");
  await expect(page.locator(".toast").last()).toContainText("Cut.");
  await go(page, "C1");
  await paste(page, await copied(page));
  await expect(page.locator(".grid-note")).toContainText("hidden rows or columns");
  await unchanged();
  ws.close();
});

test("a viewer's paste and cut change nothing and say why at the cell", async ({ browser }) => {
  // A server with accounts: an administrator, and a viewer of the workbook's space.
  const accounts = new Server(["--auth", "accounts"]);
  await accounts.start();
  try {
    const adminContext = await browser.newContext(), viewerContext = await browser.newContext();
    const url = accounts.url;
    // Requests made with a session cookie must say they come from the server's own pages.
    const headers = { Origin: url };
    const post = async (ctx: typeof adminContext, path: string, data: unknown) => {
      const r = await ctx.request.post(`${url}${path}`, { data, headers });
      expect(r.ok(), `${path}: ${r.status()} ${await r.text()}`).toBe(true);
      return r.json();
    };
    const code = /setup code (\S+)/.exec(accounts.output)?.[1];
    expect(code, accounts.output).toBeTruthy();
    await post(adminContext, "/api/auth/setup", { code, email: "ada@example.com", name: "Ada", password: "correct horse battery" });
    const [space] = await (await adminContext.request.get(`${url}/api/spaces`)).json();
    const wb = (await post(adminContext, "/api/workbooks", { name: "Viewer book", space: space.id })).id;
    const invite = await post(adminContext, `/api/orgs/${space.org_id}/invites`, { email: "vera@example.com", space: space.id, space_role: "viewer" });
    await post(viewerContext, "/api/invites/accept", { code: invite.code, name: "Vera", password: "a long enough password" });
    // The administrator's session makes the sheet and reads it back.
    const adminPage = await adminContext.newPage();
    await adminPage.goto(url);
    const op = (name: string, params: Record<string, unknown> = {}) => adminPage.evaluate(async ({ wb, name, params }) => new Promise<any>((ok, fail) => {
      const ws = new WebSocket(`ws://${location.host}/ws/${wb}`);
      ws.onmessage = (m) => {
        const msg = JSON.parse(String(m.data));
        if (msg.event === "hello") ws.send(JSON.stringify({ id: 1, op: name, ...params }));
        else if (msg.id === 1) { ws.close(); msg.ok ? ok(msg.result) : fail(new Error(msg.error)); }
      };
      ws.onerror = () => fail(new Error("websocket failed"));
    }), { wb, name, params });
    await op("sheet_create", { name: "Data" });
    await op("sheet_set", { sheet: 0, inputs: [{ row: 0, column: 0, value: "keep" }, { row: 1, column: 0, value: "a2" }] });
    const shown = async () => {
      const block = await op("sheet_fetch", { sheet: 0, row_start: 0, row_count: 3, column_count: 3 });
      return [0, 1, 2].map((r) => [0, 1, 2].map((c) => block.rows[r]?.[c]?.[0] ?? ""));
    };
    const before = [["keep", "", ""], ["a2", "", ""], ["", "", ""]];
    expect(await shown()).toEqual(before);

    const page = await viewerContext.newPage();
    await recordClipboard(page);
    await page.goto(`${url}/w/${wb}`);
    await expect(page.locator(".cell-ref")).toHaveText("A1");
    await page.locator(".grid-host").focus();
    await paste(page, "new\tcells\nmore\tcells", `<table><tr><td style="font-weight:bold">new</td></tr></table>`);
    await expect(page.locator(".grid-note")).toContainText("Nothing was pasted: you can only view this workbook");
    expect(await shown()).toEqual(before);
    await page.keyboard.press("Escape");
    await page.keyboard.press("Shift+ArrowDown");
    await page.keyboard.press("ControlOrMeta+x");
    await expect(page.locator(".grid-note")).toContainText("Nothing was cut: you can only view this workbook");
    expect(await shown()).toEqual(before);
    await adminContext.close();
    await viewerContext.close();
  } finally {
    await accounts.dispose();
  }
});
