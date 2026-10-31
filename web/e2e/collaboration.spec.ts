// Phase 7 (7.08–7.09; 7.T02) in the browser: two people edit one workbook. A stale edit of
// the same cell is refused with the choice to overwrite; other cells merge; events that are
// dropped, repeated or late still leave each page showing the workbook as it is; table undo
// takes back your change, and the change history refuses an undo whose cells changed since.

import { expect, test, type Page, type WebSocketRoute } from "@playwright/test";
import { Server } from "./server";

const server = new Server();
test.beforeAll(async () => server.start());
test.afterAll(async () => server.dispose());

async function activeCell(page: Page): Promise<{ ref: string; value: string }> {
  return {
    ref: (await page.locator(".cell-ref").textContent()) ?? "",
    value: await page.locator(".formula-input").inputValue(),
  };
}

async function goTo(page: Page, row: number, col: number): Promise<void> {
  await page.locator(".grid-host").focus();
  await page.keyboard.press("ControlOrMeta+Home");
  for (let i = 0; i < row; i++) await page.keyboard.press("ArrowDown");
  for (let i = 0; i < col; i++) await page.keyboard.press("ArrowRight");
}

/** The value at a cell (data row `row`, column `col`), as the page shows it. */
async function valueAt(page: Page, row: number, col: number): Promise<string> {
  await goTo(page, row, col);
  return (await activeCell(page)).value;
}

async function typeInto(page: Page, row: number, col: number, text: string): Promise<void> {
  await goTo(page, row, col);
  await page.keyboard.type(text);
  await page.keyboard.press("Enter");
}

async function workbookWithTable(name: string): Promise<string> {
  const wb = await server.createWorkbook(name);
  await server.api(`/api/workbooks/${wb}/import?filename=t.csv`, { method: "POST", body: "id,name\n1,a\n2,b\n3,c\n" });
  return wb;
}

async function open(page: Page, wb: string): Promise<void> {
  await page.goto(`${server.url}/w/${wb}`);
  await expect(page.getByRole("grid", { name: "Table t" })).toBeVisible();
  await expect.poll(() => valueAt(page, 0, 1)).toBe("a");
}

/** `name` of row `id`, read on the server. */
async function stored(wb: string, id: number): Promise<string> {
  const ws = await server.connect(wb);
  try {
    const r = await ws.call("query", { sql: `SELECT name FROM t WHERE id = ${id}` });
    return String(r.rows[0][0]);
  } finally {
    ws.close();
  }
}

test("a stale edit of the same cell is refused and can be overwritten; other cells merge", async ({ browser }) => {
  const wb = await workbookWithTable("Together");
  const a = await (await browser.newContext()).newPage();
  const b = await (await browser.newContext()).newPage();
  await open(a, wb);
  await open(b, wb);

  // B starts editing "a"; A saves another value there first.
  await goTo(b, 0, 1);
  await b.keyboard.press("Enter");
  await expect(b.getByRole("textbox", { name: "Edit cell" })).toHaveValue("a");
  await typeInto(a, 0, 1, "from A");
  await expect.poll(() => stored(wb, 1)).toBe("from A");
  // B finishes (the editor's text is selected, so typing replaces it): refused, and B now
  // sees A's value.
  await b.keyboard.type("from B");
  await b.keyboard.press("Enter");
  const toast = b.locator(".toast-error").filter({ hasText: "someone else changed" });
  await expect(toast).toBeVisible();
  expect(await stored(wb, 1)).toBe("from A");
  await expect.poll(() => valueAt(b, 0, 1)).toBe("from A");
  // B chooses to overwrite.
  await toast.getByRole("button", { name: "Overwrite" }).click();
  await expect.poll(() => stored(wb, 1)).toBe("from B");
  await expect.poll(() => valueAt(a, 0, 1)).toBe("from B");

  // Different cells merge.
  await typeInto(a, 1, 1, "A two");
  await typeInto(b, 2, 1, "B three");
  for (const page of [a, b]) {
    await expect.poll(() => valueAt(page, 1, 1)).toBe("A two");
    await expect.poll(() => valueAt(page, 2, 1)).toBe("B three");
  }
});

test("dropped, repeated and late events still leave the page showing the workbook as it is", async ({ page }) => {
  const wb = await workbookWithTable("Flaky");
  type Mode = { drop?: string; hold?: string; duplicate?: boolean; cut?: boolean };
  const mode: Mode = {};
  let held: (() => void) | null = null;
  const sockets: WebSocketRoute[] = [];
  await page.routeWebSocket(/\/ws\//, (ws) => {
    sockets.push(ws);
    const srv = ws.connectToServer();
    ws.onMessage((m) => srv.send(m));
    srv.onMessage((m) => {
      const msg = JSON.parse(String(m));
      if (msg.event && msg.event === mode.drop) { mode.drop = undefined; return; }
      if (msg.event && msg.event === mode.hold) { mode.hold = undefined; held = () => ws.send(m); return; }
      ws.send(m);
      if (mode.duplicate) ws.send(m);
    });
  });
  await open(page, wb);
  const other = await server.connect(wb);

  // Each missed event is followed by an unrelated one (a saved query, which refreshes nothing
  // shown): only the skipped revision tells the page it is behind.
  let q = 0;
  const unrelated = () => other.call("save_query", { name: `q${++q}`, sql: "SELECT 1" });

  // A lost "tables_changed": the new table shows anyway.
  mode.drop = "tables_changed";
  await other.call("create_table_from_query", { name: "extra", sql: "SELECT 1 AS x" });
  await unrelated();
  await expect(page.getByRole("tab", { name: /extra/ })).toBeVisible();

  // A lost "table_changed": the edited value shows anyway.
  mode.drop = "table_changed";
  await other.call("sql_change", { sql: "UPDATE t SET name = 'after drop' WHERE id = 1" });
  await unrelated();
  await expect.poll(() => valueAt(page, 0, 1)).toBe("after drop");

  // Every event twice: applied as usual.
  mode.duplicate = true;
  await other.call("sql_change", { sql: "UPDATE t SET name = 'twice' WHERE id = 2" });
  await expect.poll(() => valueAt(page, 1, 1)).toBe("twice");
  mode.duplicate = false;

  // An event delivered after the next one: the page catches up before it arrives, and the
  // late copy changes nothing.
  mode.hold = "table_changed";
  await other.call("sql_change", { sql: "UPDATE t SET name = 'after late' WHERE id = 3" });
  await unrelated();
  await expect.poll(() => valueAt(page, 2, 1)).toBe("after late");
  await expect.poll(() => !!held).toBe(true);
  held!();
  await expect.poll(() => valueAt(page, 2, 1)).toBe("after late");
  await expect.poll(() => valueAt(page, 0, 1)).toBe("after drop");

  // The connection drops; changes made meanwhile show once it is back.
  await sockets.at(-1)!.close();
  await other.call("sql_change", { sql: "UPDATE t SET name = 'while away' WHERE id = 1" });
  await expect.poll(() => sockets.length).toBeGreaterThan(1);
  await expect(page.locator(".status-dot.status-open")).toBeVisible({ timeout: 30_000 });
  await expect.poll(() => valueAt(page, 0, 1)).toBe("while away");
  other.close();
});

test("table undo takes back your change; history refuses an undo whose cells changed since", async ({ page }) => {
  const wb = await workbookWithTable("Undo");
  await open(page, wb);
  await typeInto(page, 0, 1, "mine");
  await expect.poll(() => stored(wb, 1)).toBe("mine");
  await page.locator(".grid-host").focus();
  await page.keyboard.press("ControlOrMeta+z");
  await expect(page.locator(".toast-success").filter({ hasText: "Undid your change to t" })).toBeVisible();
  await expect.poll(() => stored(wb, 1)).toBe("a");
  await expect.poll(() => valueAt(page, 0, 1)).toBe("a");
  await page.keyboard.press("ControlOrMeta+Shift+z");
  await expect.poll(() => stored(wb, 1)).toBe("mine");
  await expect.poll(() => valueAt(page, 0, 1)).toBe("mine");

  // Someone else edits the same cell after this page's change.
  await typeInto(page, 1, 1, "page edit");
  await expect.poll(() => stored(wb, 2)).toBe("page edit");
  const other = await server.connect(wb);
  await other.call("sql_change", { sql: "UPDATE t SET name = 'theirs' WHERE id = 2" });
  other.close();
  await expect.poll(() => valueAt(page, 1, 1)).toBe("theirs");

  await page.getByRole("button", { name: "History" }).click();
  const dialog = page.getByRole("dialog", { name: "Change history" });
  const rows = dialog.getByRole("row");
  // Newest first: the other session's change, then this page's edit of the same cell.
  await expect(rows.first()).toContainText("sql change");
  await expect(rows.nth(1)).toContainText("edit");
  await rows.nth(1).getByRole("button", { name: /Undo revision/ }).click();
  await expect(page.locator(".toast-error").filter({ hasText: "cannot be undone" })).toBeVisible();
  expect(await stored(wb, 2)).toBe("theirs");
  // Their change itself can be taken back; it then says so.
  await rows.first().getByRole("button", { name: /Undo revision/ }).click();
  await expect.poll(() => stored(wb, 2)).toBe("page edit");
  await expect(rows.filter({ hasText: "sql change" })).toContainText(/Undone in revision \d+/);
  // The newest entry is that undo, by this page.
  await expect(rows.first()).toContainText("Undo of revision");
  await dialog.getByRole("button", { name: "Close" }).click();
  await expect.poll(() => valueAt(page, 1, 1)).toBe("page edit");
});

test("an edit of a cell this page just saved is not taken for someone else's change", async ({ page }) => {
  // A block fetched before this page's edit committed, delivered just before its reply,
  // still shows the old value; the next edit of that cell must be based on the page's own
  // saved value, not refused as a conflict.
  const wb = await server.createWorkbook("Own edits");
  const other = await server.connect(wb);
  await other.call("sheet_create", { name: "Plan" });
  const a1 = async () => {
    const block = await other.call("sheet_fetch", { sheet: 0, row_start: 0, row_count: 1, column_count: 1 });
    return block.rows[0]?.[0]?.[0] ?? "";
  };
  const ops = new Map<number, string>();
  const hold = new Set<string>();
  const held: { op: string; send: () => void }[] = [];
  await page.routeWebSocket(/\/ws\//, (ws) => {
    const srv = ws.connectToServer();
    ws.onMessage((m) => {
      const msg = JSON.parse(String(m));
      if (msg.op) ops.set(msg.id, msg.op);
      srv.send(m);
    });
    srv.onMessage((m) => {
      const op = ops.get(JSON.parse(String(m)).id);
      if (op && hold.has(op)) held.push({ op, send: () => ws.send(m) });
      else ws.send(m);
    });
  });
  const release = (op?: string) => {
    for (const h of held.filter((x) => !op || x.op === op)) {
      held.splice(held.indexOf(h), 1);
      h.send();
    }
  };
  await page.goto(`${server.url}/w/${wb}`);
  await expect(page.getByRole("grid", { name: /^Sheet / })).toHaveAttribute("aria-colcount", /\d+/);

  hold.add("sheet_fetch");
  hold.add("sheet_set");
  // Someone else's change elsewhere on the sheet: the page refetches its block (held).
  await other.call("sheet_set", { sheet: 0, inputs: [{ row: 9, column: 3, value: "x" }] });
  await expect.poll(() => held.filter((h) => h.op === "sheet_fetch").length).toBeGreaterThan(0);
  // The page saves A1; its reply is held.
  await typeInto(page, 0, 0, "2");
  await expect.poll(() => held.some((h) => h.op === "sheet_set")).toBe(true);
  await expect.poll(a1).toBe("2");
  // The old block (A1 empty) arrives first, then the reply; later fetches stay held.
  release("sheet_fetch");
  release("sheet_set");
  await typeInto(page, 0, 0, "5");
  await expect.poll(() => held.some((h) => h.op === "sheet_set")).toBe(true);
  hold.clear();
  release();
  await expect.poll(a1).toBe("5");
  await expect(page.locator(".toast-error")).toHaveCount(0);
  other.close();
});
