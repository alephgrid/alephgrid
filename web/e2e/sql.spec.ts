// Phase 4: the SQL workspace in the browser. Schema browser and completion (4.01), bound
// parameters (4.02), bounded previews, cancellation and complete exports (4.03), saved
// queries with versions (4.04), result to table / sheet (4.05), live queries and
// materialized snapshots (4.06), policy refusals and confirmed changes (4.07, 4.08).
// Expected values are computed by hand from the fixture below.

import AxeBuilder from "@axe-core/playwright";
import { expect, test, type Locator, type Page } from "@playwright/test";
import { readFileSync } from "node:fs";
import { Server } from "./server";

const server = new Server();
test.beforeAll(async () => server.start());
test.afterAll(async () => server.dispose());

/** The region value that tries to break out of a string literal. */
const HOSTILE = "x'; DROP TABLE t; --";

/** id, region, amount, "Prix €": north = 10 + 30, south = 20, hostile = 40; 100 in all. */
const SALES = `id,region,amount,Prix €\n1,north,10,1.5\n2,south,20,2.5\n3,north,30,3.5\n4,"${HOSTILE}",40,4.5\n`;

async function workbook(name: string): Promise<string> {
  const wb = await server.createWorkbook(name);
  await server.api(`/api/workbooks/${wb}/import?filename=sales.csv`, { method: "POST", body: SALES });
  return wb;
}

/** Open the workbook and its SQL workspace; returns the dialog. */
async function openSql(page: Page, wb: string): Promise<Locator> {
  await page.goto(`${server.url}/w/${wb}`);
  await expect(page.locator(".status-dot.status-open")).toBeVisible();
  await expect(page.getByRole("tab", { name: /sales/ })).toBeVisible();
  await page.getByRole("button", { name: "SQL", exact: true }).click();
  const dialog = page.getByRole("dialog", { name: "SQL" });
  await expect(dialog).toBeVisible();
  // The schema has loaded once the table is listed.
  await expect(dialog.getByRole("treeitem", { name: "sales 4 rows" })).toBeVisible();
  return dialog;
}

/** The preview table's body as text, row by row. */
async function resultRows(page: Page): Promise<string[][]> {
  return page.locator(".result-table tbody tr").evaluateAll((rows) =>
    rows.map((r) => [...r.querySelectorAll("td")].map((td) => td.textContent ?? "")));
}

async function resultHeader(page: Page): Promise<string[]> {
  return page.locator(".result-table thead th").allTextContents();
}

async function run(page: Page): Promise<void> {
  await page.getByRole("button", { name: "Run", exact: true }).click();
}

const csvOf = (wb: string, table: string) => server.exportCsv(wb, { table, sort: [], filters: [], hidden: [] });

test("browse the schema, insert a column by keyboard, bind a hostile parameter, cancel and rerun", async ({ page, browserName }) => {
  const wb = await workbook("Schema journey");
  const sent: { op: string; sql?: string; params?: { name: string; type: string; value?: string }[] }[] = [];
  // Playwright's Firefox reports text frames decoded as Latin-1; re-read them as UTF-8.
  const frame = (p: string | Buffer) => (typeof p === "string" && browserName === "firefox" ? Buffer.from(p, "latin1").toString("utf8") : String(p));
  page.on("websocket", (ws) => ws.on("framesent", (f) => sent.push(JSON.parse(frame(f.payload)))));
  const dialog = await openSql(page, wb);
  const editor = page.locator(".sql-editor");
  await editor.fill("SELECT ");

  // The tree: expand the table with the keyboard and insert a quoted column at the cursor.
  const sales = dialog.getByRole("treeitem", { name: "sales 4 rows" });
  await expect(sales).toHaveAttribute("aria-expanded", "false");
  await sales.focus();
  await page.keyboard.press("ArrowRight");
  await expect(sales).toHaveAttribute("aria-expanded", "true");
  await expect(sales).toBeFocused();
  for (const [name, type] of [["id", "BIGINT"], ["region", "VARCHAR"], ["amount", "BIGINT"], ["Prix €", "DOUBLE"]]) {
    await page.keyboard.press("ArrowDown");
    await expect(dialog.getByRole("treeitem", { name: `${name} ${type}` })).toBeFocused();
  }
  await page.keyboard.press("Enter");
  await expect(editor).toHaveValue('SELECT "Prix €"');
  await expect(editor).toBeFocused();
  // Left goes back to the table, and collapses it.
  await sales.focus();
  await page.keyboard.press("ArrowLeft");
  await expect(sales).toHaveAttribute("aria-expanded", "false");

  // Compose the rest; a $name becomes a parameter row.
  await editor.press("End");
  await page.keyboard.type(", region FROM sales WHERE region = $region ORDER BY id");
  const value = dialog.getByLabel("Value of $region");
  await expect(value).toBeVisible();
  await expect(dialog.getByLabel("Type of $region")).toHaveValue("text");
  await value.fill(HOSTILE);
  await editor.focus();
  await page.keyboard.press("ControlOrMeta+Enter");
  await expect(page.locator(".sql-status")).toHaveText(/^1 row · [\d,]+ ms · revision \d+$/);
  expect(await resultHeader(page)).toEqual(["Prix €", "region"]);
  expect(await resultRows(page)).toEqual([["4.5", HOSTILE]]);
  // The value travelled as a bound parameter, never as SQL text.
  const q = sent.filter((m) => m.op === "query").at(-1)!;
  expect(q.sql).toBe('SELECT "Prix €", region FROM sales WHERE region = $region ORDER BY id');
  expect(q.params).toEqual([{ name: "region", type: "text", value: HOSTILE }]);

  await value.fill("north");
  await run(page);
  await expect.poll(() => resultRows(page)).toEqual([["1.5", "north"], ["3.5", "north"]]);
  // NULL binds SQL NULL: `region = NULL` matches nothing.
  await dialog.getByLabel("NULL").check();
  await expect(value).toBeDisabled();
  await run(page);
  await expect(page.locator(".sql-status")).toHaveText(/^0 rows · /);
  expect(sent.filter((m) => m.op === "query").at(-1)!.params).toEqual([{ name: "region", type: "text" }]);
  // A value that is not of its type is reported next to the parameters.
  await dialog.getByLabel("NULL").uncheck();
  await dialog.getByLabel("Type of $region").selectOption("integer");
  await value.fill("north");
  await run(page);
  await expect(page.locator(".sql-param-error")).toContainText("$region");
  await expect(page.locator(".sql-param-error")).toContainText("north");

  // A long query is cancelled; the next run shows a fresh result.
  await editor.fill("SELECT sum(i) FROM range(100000000000) r(i)");
  await run(page);
  await expect(page.locator(".sql-status")).toContainText("Running");
  await dialog.getByRole("button", { name: "Cancel", exact: true }).click();
  await expect(page.locator(".sql-status")).toHaveText("Cancelled.");
  await editor.fill("SELECT 7 AS fresh");
  await run(page);
  await expect.poll(() => resultRows(page)).toEqual([["7"]]);
  expect(await resultHeader(page)).toEqual(["fresh"]);
  await expect(page.locator(".toast-error")).toHaveCount(0);

  // Closing the workspace cancels what it is running.
  await editor.fill("SELECT sum(i) FROM range(100000000000) r(i)");
  await run(page);
  await expect(page.locator(".sql-status")).toContainText("Running");
  const running = sent.filter((m) => m.op === "query").at(-1) as unknown as { id: number };
  await page.keyboard.press("Escape");
  await expect(dialog).toBeHidden();
  await expect.poll(() => sent.some((m) => m.op === "cancel" && (m as unknown as { request: number }).request === running.id)).toBe(true);
});

test("names inserted from the tree are quoted so they select exactly that column", async ({ page }) => {
  const wb = await workbook("Odd names");
  const client = await server.connect(wb);
  const info = await client.call<{ name: string }>("create_table_from_query", {
    name: "odd", sql: `SELECT 1 AS "a ""b""", 2 AS "select", 3 AS "Mixed", 4 AS plain, 5 AS "Prix €"`,
  });
  await client.call("add_formula_column", { table: info.name, name: "twice", formula: "[plain] * 2" });
  client.close();
  const dialog = await openSql(page, wb);
  const editor = page.locator(".sql-editor");
  const table = dialog.getByRole("treeitem", { name: `${info.name} 1 row` });
  await table.focus();
  await page.keyboard.press("ArrowRight");
  // The formula column is marked as such.
  await expect(dialog.getByRole("treeitem", { name: /^twice .*formula column$/ })).toBeVisible();
  await editor.fill("SELECT ");
  for (let i = 0; i < 6; i++) {
    if (i) await page.keyboard.type(", ");
    await table.focus();
    for (let j = 0; j <= i; j++) await page.keyboard.press("ArrowDown");
    await page.keyboard.press("Enter");
  }
  await page.keyboard.type(" FROM ");
  await table.focus();
  await page.keyboard.press("Enter");
  await expect(editor).toHaveValue(`SELECT "a ""b""", "select", "Mixed", plain, "Prix €", twice FROM ${info.name}`);
  await page.keyboard.press("ControlOrMeta+Enter");
  await expect.poll(() => resultRows(page)).toEqual([["1", "2", "3", "4", "5", "8"]]);
  expect(await resultHeader(page)).toEqual(['a "b"', "select", "Mixed", "plain", "Prix €", "twice"]);
});

test("completion offers tables after FROM and columns of tables in use, by keyboard", async ({ page }) => {
  const wb = await workbook("Completion");
  const dialog = await openSql(page, wb);
  const editor = page.locator(".sql-editor");
  const listbox = dialog.getByRole("listbox", { name: "Suggestions" });
  await editor.fill("");
  await editor.focus();
  await page.keyboard.type("SELECT * FROM sa");
  await expect(listbox).toBeVisible();
  await expect(editor).toHaveAttribute("aria-expanded", "true");
  await expect(listbox.getByRole("option")).toHaveText([/^sales/]);
  const active = await editor.getAttribute("aria-activedescendant");
  await expect(page.locator(`#${active}`)).toHaveAttribute("aria-selected", "true");
  await page.keyboard.press("Enter");
  await expect(editor).toHaveValue("SELECT * FROM sales");
  await expect(listbox).toBeHidden();
  await expect(editor).toHaveAttribute("aria-expanded", "false");

  // Columns of the table named in the statement; the quoted one is inserted quoted.
  await page.keyboard.type(" WHERE P");
  await expect(listbox.getByRole("option")).toHaveText([/^Prix €/]);
  await page.keyboard.press("Tab");
  await expect(editor).toHaveValue('SELECT * FROM sales WHERE "Prix €"');
  await page.keyboard.type(" > 2 AND a");
  await expect(listbox.getByRole("option").first()).toHaveText(/^amount/);
  // Escape closes the list, not the workspace.
  await page.keyboard.press("Escape");
  await expect(listbox).toBeHidden();
  await expect(dialog).toBeVisible();
  // ArrowDown/ArrowUp move through several suggestions.
  await page.keyboard.type("m");
  await expect(listbox).toBeVisible();
  await page.keyboard.press("ArrowDown");
  await page.keyboard.press("ArrowUp");
  await page.keyboard.press("Enter");
  await expect(editor).toHaveValue('SELECT * FROM sales WHERE "Prix €" > 2 AND amount');
  // Internal tables are never suggested.
  await page.keyboard.type(" > 0 AND _rf");
  await expect(listbox).toBeHidden();
  // Ctrl/⌘+Enter still runs.
  await editor.fill('SELECT * FROM sales WHERE "Prix €" > 2 AND amount > 20 ORDER BY id');
  await page.keyboard.press("ControlOrMeta+Enter");
  await expect.poll(() => resultRows(page)).toEqual([["3", "north", "30", "3.5"], ["4", HOSTILE, "40", "4.5"]]);
});

test("a superseded run's reply never replaces the newer result", async ({ page }) => {
  const wb = await workbook("Superseded");
  // Hold the first query's reply until the second one has been shown, and keep its
  // cancel from reaching the server, so the stale reply really arrives last.
  let firstId = -1;
  let held: string | null = null;
  let secondShown = false;
  let staleDelivered = false;
  let probeSeen = false;
  await page.routeWebSocket(/\/ws\//, (ws) => {
    const srv = ws.connectToServer();
    ws.onMessage((m) => {
      const msg = JSON.parse(String(m));
      if (msg.op === "query" && String(msg.sql).includes("'first'")) firstId = msg.id;
      if (msg.op === "cancel" && msg.request === firstId) return;
      if (staleDelivered && msg.op === "saved_queries") probeSeen = true;
      srv.send(m);
    });
    srv.onMessage((m) => {
      const msg = JSON.parse(String(m));
      if (firstId >= 0 && msg.id === firstId) {
        if (secondShown) deliverStale(String(m));
        else held = String(m);
        return;
      }
      ws.send(m);
      if (msg.ok && JSON.stringify(msg.result ?? null).includes("second")) {
        secondShown = true;
        if (held) deliverStale(held);
      }
    });
    // After the stale reply, an event the page answers with a request: once that request
    // arrives, the page has handled everything sent before it (one socket, in order).
    function deliverStale(m: string) {
      ws.send(m);
      staleDelivered = true;
      ws.send(JSON.stringify({ event: "queries_changed", origin: 0, revision: 0 }));
    }
  });
  const dialog = await openSql(page, wb);
  const editor = page.locator(".sql-editor");
  await editor.fill("SELECT 'first' AS v");
  await run(page);
  await editor.fill("SELECT 'second' AS v");
  await run(page);
  await expect.poll(() => probeSeen).toBe(true);
  await expect(dialog).toBeVisible();
  expect(await resultRows(page)).toEqual([["second"]]);
  await expect(page.locator(".sql-status")).toHaveText(/^1 row · /);
});

test("previews say when they are truncated, with timing and the revision read", async ({ page }) => {
  const wb = await workbook("Truncation");
  const client = await server.connect(wb);
  const revision = (await client.call<{ revision: number }>("query", { sql: "SELECT 1" })).revision;
  client.close();
  const dialog = await openSql(page, wb);
  const editor = page.locator(".sql-editor");
  const notice = page.locator(".sql-notice");

  await editor.fill("SELECT i FROM range(500) r(i)");
  await run(page);
  await expect(page.locator(".sql-status")).toHaveText(new RegExp(`^500 rows · [\\d,]+ ms · revision ${revision}$`));
  await expect(notice).toBeHidden();
  await expect(page.locator(".result-table tbody tr")).toHaveCount(500);

  await editor.fill("SELECT i FROM range(1200) r(i) ORDER BY i");
  await run(page);
  await expect(page.locator(".sql-status")).toHaveText(new RegExp(`^500\\+ rows · [\\d,]+ ms · revision ${revision}$`));
  await expect(notice).toBeVisible();
  await expect(notice).toHaveText("Showing the first 500 rows — the result has more. Export to get every row.");
  await expect(page.locator(".result-table tbody tr")).toHaveCount(500);
  expect((await resultRows(page)).at(-1)).toEqual(["499"]);
  await expect(dialog).toBeVisible();
});

test("saved queries survive a reload, can be renamed, and keep their versions", async ({ page }) => {
  const wb = await workbook("Saved");
  const dialog = await openSql(page, wb);
  const editor = page.locator(".sql-editor");
  const v1 = "SELECT id, amount FROM sales WHERE region = $region ORDER BY id";
  await editor.fill(v1);
  await dialog.getByLabel("Value of $region").fill("north");
  await dialog.getByRole("button", { name: "Save query" }).click();
  const save = page.getByRole("dialog", { name: "Save query" });
  await expect(save).toContainText("Saved in this workbook — visible to anyone who can open it");
  await save.getByLabel("Name").fill("North sales");
  await save.getByRole("button", { name: "Save" }).click();
  await expect(page.locator(".toast-success").last()).toContainText("Saved “North sales”");
  const item = dialog.locator(".sql-saved", { hasText: "North sales" });
  await expect(item).toContainText("Saved query");
  await expect(item).toContainText("v1");

  // After a reload the query is still listed; opening it restores text and parameters.
  await page.reload();
  await page.getByRole("button", { name: "SQL", exact: true }).click();
  // The unsaved draft (text and parameter values) is kept in this browser.
  await expect(editor).toHaveValue(v1);
  await expect(dialog.getByLabel("Value of $region")).toHaveValue("north");
  await editor.fill("SELECT 1");
  await expect(dialog.getByLabel("Value of $region")).toHaveCount(0);
  await dialog.getByRole("button", { name: /^North sales/ }).click();
  await expect(editor).toHaveValue(v1);
  await expect(dialog.getByLabel("Value of $region")).toHaveValue("north");
  await expect(page.locator(".sql-loaded")).toContainText("Saved query “North sales” · v1");
  // A plain saved query only loads its text.
  await expect(page.locator(".result-table")).toHaveCount(0);

  // Rename.
  await dialog.getByRole("button", { name: "More actions for North sales" }).click();
  await page.getByRole("menuitem", { name: "Rename…" }).click();
  const rename = page.getByRole("dialog", { name: "Rename saved query" });
  await rename.getByLabel("Name").fill("Northern sales");
  await rename.getByRole("button", { name: "Rename" }).click();
  await expect(dialog.locator(".sql-saved", { hasText: "Northern sales" })).toContainText("v1");

  // Saving unchanged text adds no version; a changed SQL does.
  await dialog.getByRole("button", { name: "Save query" }).click();
  await expect(page.locator(".toast-success").last()).toContainText("No changes since version 1");
  const v2 = "SELECT id, amount * 2 AS doubled FROM sales WHERE region = $region ORDER BY id";
  await editor.fill(v2);
  await dialog.getByRole("button", { name: "Save query" }).click();
  await expect(page.locator(".toast-success").last()).toContainText("Saved “Northern sales” as version 2");
  await expect(dialog.locator(".sql-saved", { hasText: "Northern sales" })).toContainText("v2");

  // Versions: both listed; open version 1 into the editor.
  await dialog.getByRole("button", { name: "More actions for Northern sales" }).click();
  await page.getByRole("menuitem", { name: "Versions…" }).click();
  const versions = page.getByRole("dialog", { name: "Versions of “Northern sales”" });
  await expect(versions.locator("tbody tr")).toHaveCount(2);
  await expect(versions.locator("tbody tr").first()).toContainText("v2");
  await versions.getByRole("button", { name: "Open version 1" }).click();
  await expect(editor).toHaveValue(v1);
  await expect(page.locator(".sql-loaded")).toContainText("v1 (latest is v2)");

  // Persisted on the server, with both versions.
  const client = await server.connect(wb);
  const saved = await client.call<{ id: string; name: string; version: number; kind: string }[]>("saved_queries");
  expect(saved.map((q) => [q.name, q.kind, q.version])).toEqual([["Northern sales", "query", 2]]);
  const all = await client.call<{ version: number; sql: string }[]>("query_versions", { saved: saved[0].id });
  expect(all.map((v) => [v.version, v.sql])).toEqual([[1, v1], [2, v2]]);
  client.close();
});

test("a live query runs on open and turns stale when a source changes", async ({ page }) => {
  const wb = await workbook("Live");
  const dialog = await openSql(page, wb);
  const editor = page.locator(".sql-editor");
  await editor.fill("SELECT sum(amount) AS total FROM sales");
  await dialog.getByRole("button", { name: "Save query" }).click();
  const save = page.getByRole("dialog", { name: "Save query" });
  await save.getByLabel("Name").fill("Total");
  await save.getByLabel("Kind").selectOption("live");
  await save.getByRole("button", { name: "Save" }).click();
  await expect(dialog.locator(".sql-saved", { hasText: "Total" })).toContainText("Live query");

  await editor.fill("SELECT 0");
  await dialog.getByRole("button", { name: /^Total/ }).click();
  await expect(editor).toHaveValue("SELECT sum(amount) AS total FROM sales");
  await expect.poll(() => resultRows(page)).toEqual([["100"]]);
  const live = page.locator(".sql-live");
  await expect(live).toHaveText(/^Live · read at revision \d+$/);

  // Another session changes a source table.
  const client = await server.connect(wb);
  await client.call("sql_change", { sql: "UPDATE sales SET amount = 50 WHERE id = 1" });
  client.close();
  await expect(live).toContainText("Stale — the source changed");
  expect(await resultRows(page)).toEqual([["100"]]);
  await live.getByRole("button", { name: "Refresh result" }).click();
  await expect.poll(() => resultRows(page)).toEqual([["140"]]);
  await expect(live).not.toContainText("Stale");
});

test("a materialized table keeps exact values, survives a restart, and refreshes when stale", async ({ page }) => {
  const wb = await workbook("Materialized");
  const dialog = await openSql(page, wb);
  await page.locator(".sql-editor").fill(
    "SELECT region, sum(amount) AS total, 9007199254740993::BIGINT AS big FROM sales GROUP BY region ORDER BY region");
  await dialog.getByRole("button", { name: "Save as table" }).click();
  const name = page.getByRole("dialog", { name: "Save result as a table" });
  await name.getByLabel("Table name").fill("region_totals");
  await name.getByRole("button", { name: "Create table" }).click();
  await expect(page.locator(".toast-success").last()).toContainText("Created region_totals (3 rows, a snapshot of the query)");
  await expect(page.getByRole("tab", { name: /region_totals/ })).toHaveAttribute("aria-selected", "true");
  const expected = (south: number) =>
    `region,total,big\nnorth,40,9007199254740993\nsouth,${south},9007199254740993\n${HOSTILE},40,9007199254740993\n`;
  expect(await csvOf(wb, "region_totals")).toBe(expected(20));
  // The grid shows the integer beyond 2^53 exactly.
  await page.locator(".grid-host").focus();
  await page.keyboard.press("ControlOrMeta+Home");
  await page.keyboard.press("ArrowRight");
  await page.keyboard.press("ArrowRight");
  await expect(page.locator(".formula-input")).toHaveValue("9007199254740993");
  const banner = page.locator(".snapshot-banner");
  await expect(banner).toContainText(/Snapshot of a query · computed at revision \d+/);
  await expect(banner).toContainText("Up to date with its sources");

  await server.stop("SIGTERM");
  await server.start();
  await page.reload();
  await page.getByRole("tab", { name: /region_totals/ }).click();
  await expect(banner).toContainText(/Snapshot of a query · computed at revision \d+/);
  expect(await csvOf(wb, "region_totals")).toBe(expected(20));

  const client = await server.connect(wb);
  await client.call("sql_change", { sql: "UPDATE sales SET amount = amount + 1 WHERE region = 'south'" });
  client.close();
  await expect(banner).toContainText("Stale: sales changed");
  expect(await seriousViolations(page)).toEqual([]);
  await banner.getByRole("button", { name: "Refresh snapshot" }).click();
  await expect(page.locator(".toast-success").last()).toContainText(/Refreshed region_totals: computed at revision \d+/);
  await expect(banner).toContainText("Up to date with its sources");
  expect(await csvOf(wb, "region_totals")).toBe(expected(21));
});

test("a result larger than a sheet asks first: declining copies nothing, accepting copies 1,000 rows", async ({ page }) => {
  const wb = await workbook("To sheet");
  const dialog = await openSql(page, wb);
  await page.locator(".sql-editor").fill("SELECT i FROM range(1500) r(i)");
  const ask = async () => {
    await dialog.getByRole("button", { name: "Open in sheet" }).click();
    const name = page.getByRole("dialog", { name: "Open result in a new sheet" });
    await expect(name).toContainText("Copies at most the first 1,000 rows");
    await name.getByRole("button", { name: "Create sheet" }).click();
    const confirm = page.getByRole("alertdialog", { name: "Copy only the first 1,000 rows?" });
    await expect(confirm).toContainText("save the result as a table instead");
    return confirm;
  };
  await (await ask()).getByRole("button", { name: "Cancel" }).click();
  await expect(page.locator(".sql-status")).toHaveText("Nothing was copied.");
  const client = await server.connect(wb);
  expect(await client.call("sheets")).toEqual([]);

  await (await ask()).getByRole("button", { name: "Copy first 1,000 rows" }).click();
  await expect(page.locator(".toast").last()).toContainText("Copied the first 1,000 rows of the query result");
  await expect(page.locator(".kind-badge").first()).toHaveText("Sheet");
  const sheets = await client.call<{ name: string; rows: number }[]>("sheets");
  expect(sheets.map((s) => s.name)).toEqual(["Query result"]);
  // A header row plus 1,000 values.
  expect(sheets[0].rows).toBe(1001);
  client.close();
});

test("export writes every row of the result, downloaded once", async ({ page }) => {
  const wb = await workbook("Export");
  const dialog = await openSql(page, wb);
  await page.locator(".sql-editor").fill("SELECT i, 'r' || i AS label FROM range(2500) r(i) ORDER BY i");
  await run(page);
  await expect(page.locator(".sql-notice")).toBeVisible();
  await dialog.getByRole("button", { name: "Export…" }).click();
  const exp = page.getByRole("dialog", { name: "Export the query result" });
  await expect(exp.getByLabel("Format")).toHaveValue("csv");
  const downloading = page.waitForEvent("download");
  await exp.getByRole("button", { name: "Export" }).click();
  const download = await downloading;
  const text = readFileSync((await download.path())!, "utf8");
  const lines = text.trimEnd().split("\n");
  expect(lines).toHaveLength(2501);
  expect(lines[0]).toBe("i,label");
  for (let i = 0; i < 2500; i++) if (lines[i + 1] !== `${i},r${i}`) throw new Error(`line ${i + 1}: ${lines[i + 1]}`);
  await expect(page.locator(".toast-success").last()).toContainText(/Exported 2,500 rows \(revision \d+\)/);
  // The file is served once.
  const again = await fetch(download.url());
  expect(again.status).toBe(404);
});

test("refused statements change nothing; a change is applied only after confirmation", async ({ page }) => {
  const wb = await workbook("Errors");
  const client = await server.connect(wb);
  const revision = async () => (await client.call<{ revision: number }>("query", { sql: "SELECT 1" })).revision;
  const before = await revision();
  const dialog = await openSql(page, wb);
  const editor = page.locator(".sql-editor");
  const error = page.locator(".sql-error");

  await editor.fill("SELECT * FROM read_csv('/etc/passwd')");
  await run(page);
  await expect(error).toBeVisible();
  await expect(error).toContainText("read_csv");
  expect(await revision()).toBe(before);

  await editor.fill("DROP TABLE sales");
  await run(page);
  await expect(error).toContainText("not allowed");
  expect(await revision()).toBe(before);
  expect((await client.call<{ name: string }[]>("tables")).map((t) => t.name)).toEqual(["sales"]);

  // Declined: nothing changes.
  await editor.fill("UPDATE sales SET amount = 99 WHERE id = 2");
  await run(page);
  let confirm = page.getByRole("alertdialog", { name: "Apply this change?" });
  await expect(confirm).toContainText("This statement changes table data (UPDATE)");
  await confirm.getByRole("button", { name: "Cancel" }).click();
  await expect(page.locator(".sql-status")).toHaveText("Not applied — nothing changed.");
  expect(await revision()).toBe(before);

  // Accepted: applied once, and the preview shows the new value.
  await run(page);
  confirm = page.getByRole("alertdialog", { name: "Apply this change?" });
  await confirm.getByRole("button", { name: "Apply change" }).click();
  await expect(page.locator(".sql-status")).toHaveText("Updated 1 row in sales");
  expect(await revision()).toBe(before + 1);
  await editor.fill("SELECT amount FROM sales WHERE id = 2");
  await run(page);
  await expect.poll(() => resultRows(page)).toEqual([["99"]]);
  // And so does the grid behind the workspace.
  await dialog.getByRole("button", { name: "Close" }).click();
  await expect(dialog).toBeHidden();
  await page.locator(".grid-host").focus();
  await page.keyboard.press("ControlOrMeta+Home");
  await page.keyboard.press("ArrowDown");
  await page.keyboard.press("ArrowRight");
  await page.keyboard.press("ArrowRight");
  await expect(page.locator(".formula-input")).toHaveValue("99");
  expect(await csvOf(wb, "sales")).toContain("\n2,south,99,2.5\n");
  client.close();
});

async function seriousViolations(page: Page): Promise<string[]> {
  const results = await new AxeBuilder({ page }).analyze();
  return results.violations
    .filter((v) => v.impact === "serious" || v.impact === "critical")
    .map((v) => `${v.id}: ${v.help} (${v.nodes.map((n) => n.target.join(" ") + " " + n.failureSummary).join(", ")})`);
}

test("the SQL workspace is accessible and works from the keyboard", async ({ page, browserName }) => {
  // Safari's Tab reaches only text fields unless "Press Tab to highlight each item" is on
  // (Option+Tab reaches every control), and in macOS Safari and Firefox End scrolls a text
  // area instead of moving the caret (⌘↓ moves it to the end).
  const tab = browserName === "webkit" ? "Alt+Tab" : "Tab";
  const end = process.platform === "darwin" && browserName !== "chromium" ? "Meta+ArrowDown" : "End";
  const wb = await workbook("Accessible SQL");
  const dialog = await openSql(page, wb);
  const editor = page.locator(".sql-editor");
  await editor.fill("SELECT region, amount FROM sales WHERE region <> $skip ORDER BY id");
  await dialog.getByLabel("Value of $skip").fill("south");

  // Run from the keyboard: Tab from the editor reaches the Run button.
  await editor.focus();
  const runButton = page.getByRole("button", { name: "Run", exact: true });
  for (let i = 0; i < 8 && !(await runButton.evaluate((b) => b === document.activeElement)); i++) await page.keyboard.press(tab);
  await expect(runButton).toBeFocused();
  await page.keyboard.press("Enter");
  await expect.poll(() => resultRows(page)).toEqual([["north", "10"], ["north", "30"], [HOSTILE, "40"]]);
  // The schema tree is reachable with Shift+Tab from the editor.
  await editor.focus();
  await page.keyboard.press("Shift+Tab");
  await expect(dialog.getByRole("treeitem", { name: "sales 4 rows" })).toBeFocused();
  await page.keyboard.press("ArrowRight");
  await page.keyboard.press("ArrowDown");
  await expect(dialog.getByRole("treeitem", { name: "id BIGINT" })).toBeFocused();
  // A visible focus ring on the tree item.
  const ring = await dialog.getByRole("treeitem", { name: "id BIGINT" }).evaluate((e) => getComputedStyle(e).outlineStyle);
  expect(ring).toBe("solid");

  // Results, parameter rows and the suggestion list open together.
  await editor.fill("SELECT region, amount FROM sales WHERE region <> $skip AND am");
  await editor.press(end);
  await page.keyboard.type("o");
  await expect(dialog.getByRole("listbox", { name: "Suggestions" })).toBeVisible();
  await expect(dialog.getByLabel("Value of $skip")).toBeVisible();
  await expect(page.locator(".result-table")).toBeVisible();
  expect(await seriousViolations(page)).toEqual([]);
});
