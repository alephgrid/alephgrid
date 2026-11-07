// U.T05: the command-by-command check against the inventory in DESIGN.md (workbook layout).
// Every visible menu and toolbar command either works (opens its surface, downloads, asks for
// a file) or is disabled with its reason; opening and closing the tools leaves the persisted
// workbook unchanged; formats, merges and freezes apply, undo and survive a reload; range
// statistics use the cells' values, not their display; locale and time zone do not change
// explicit formats.

import { expect, test, type Page } from "@playwright/test";
import { Server, menuCommand, type WsClient } from "./server";

const server = new Server();
test.beforeAll(async () => server.start());
test.afterAll(async () => server.dispose());

test.beforeEach(async ({ page }) => {
  await page.addInitScript(() => { try { localStorage.setItem("alephgrid-welcome-done", "1"); } catch { /* */ } });
});

/** Everything a person could have saved in the workbook, read through the server. */
async function persisted(ws: WsClient): Promise<unknown> {
  const sheets = await ws.call<{ index: number; name: string; rows: number; columns: number; frozen_columns?: number; frozen_rows?: number; color?: string }[]>("sheets");
  const perSheet = [];
  for (const s of sheets) {
    perSheet.push({
      sheet: [s.name, s.rows, s.columns, s.frozen_columns ?? 0, s.frozen_rows ?? 0, s.color ?? null],
      charts: await ws.call("sheet_charts", { sheet: s.index }),
      rules: await ws.call("sheet_rules", { sheet: s.index }),
      comments: await ws.call("comments", { sheet: s.index }),
      cells: await ws.call("sheet_fetch", { sheet: s.index, row_start: 0, row_count: 10, column_count: 5 }),
    });
  }
  const tables = await ws.call<{ name: string; row_count: number; column_count: number }[]>("tables");
  const perTable = [];
  for (const t of tables) {
    perTable.push({
      table: [t.name, t.row_count, t.column_count],
      presentation: await ws.call("presentation", { table: t.name }),
      validations: await ws.call("validations", { table: t.name }),
      views: await ws.call("saved_views", { table: t.name }),
    });
  }
  return {
    perSheet, perTable,
    names: await ws.call("sheet_names"),
    hidden: await ws.call("hidden_sheets"),
    queries: await ws.call("saved_queries"),
    analyses: await ws.call("analyses", {}),
    dashboards: await ws.call("dashboards", {}),
    functions: await ws.call("functions", {}),
  };
}

type Outcome = "dialog" | "menu" | "find" | "analyze" | "functions" | "sql" | "download" | "file" | "toast" | "explorer" | "theme";

/** Run `act` and report what it visibly did. */
async function outcome(page: Page, act: () => Promise<void>): Promise<Outcome | "nothing"> {
  const download = page.waitForEvent("download", { timeout: 4_000 }).then(() => "download" as const).catch(() => null);
  const chooser = page.waitForEvent("filechooser", { timeout: 4_000 }).then(() => "file" as const).catch(() => null);
  const explorerBefore = await page.getByRole("navigation", { name: "Workbook explorer" }).isVisible().catch(() => false);
  // The appearance button names the chosen theme (System may look the same as Light).
  const theme = () => page.getByRole("button", { name: /^Appearance:/ }).first().getAttribute("aria-label");
  const themeBefore = await theme();
  const toastsBefore = await page.locator(".toast").count();
  await act();
  const seen = async (): Promise<Outcome | null> => {
    if (await page.getByRole("dialog").or(page.getByRole("alertdialog")).first().isVisible().catch(() => false)) return "dialog";
    if (await page.getByRole("menu").first().isVisible().catch(() => false)) return "menu";
    if (await page.getByRole("search").isVisible().catch(() => false)) return "find";
    if (await page.getByRole("complementary", { name: "Analyze" }).isVisible().catch(() => false)) return "analyze";
    if (await page.getByRole("button", { name: "Close functions panel" }).isVisible().catch(() => false)) return "functions";
    if (await page.getByRole("combobox", { name: "SQL query" }).isVisible().catch(() => false)) return "sql";
    if (await page.getByRole("navigation", { name: "Workbook explorer" }).isVisible().catch(() => false) !== explorerBefore) return "explorer";
    if (await theme() !== themeBefore) return "theme";
    return null;
  };
  // A toast alone counts only when nothing else (a download, a surface) follows it.
  const started = Date.now();
  for (;;) {
    const other = await Promise.race([download, chooser, new Promise<null>((r) => setTimeout(() => r(null), 100))]);
    if (other) return other;
    const s = await seen();
    if (s) return s;
    if (Date.now() - started > 1_500 && await page.locator(".toast").count() > toastsBefore) return "toast";
    if (Date.now() - started > 4_000) return "nothing";
  }
}

/** Close whatever a command opened, the way a person would. */
async function closeAll(page: Page): Promise<void> {
  for (let i = 0; i < 4; i++) {
    const open = page.getByRole("dialog").or(page.getByRole("alertdialog")).or(page.getByRole("menu")).or(page.getByRole("search"));
    if (!(await open.first().isVisible().catch(() => false))) break;
    await page.keyboard.press("Escape");
    await page.waitForTimeout(50);
  }
  for (const name of ["Close Analyze", "Close functions panel", "Close SQL panel"]) {
    const b = page.getByRole("button", { name });
    if (await b.isVisible().catch(() => false)) await b.click();
  }
  await page.locator(".toast").evaluateAll((ts) => ts.forEach((t) => t.remove()));
}

/** The menu bar's items: label, disabled reason (or null). */
async function menuItems(page: Page, menu: string): Promise<{ label: string; reason: string | null }[]> {
  await page.getByRole("navigation", { name: "Workbook menus" }).getByRole("button", { name: menu, exact: true }).click();
  const items = page.getByRole("menu", { name: menu }).getByRole("menuitem");
  const out = [];
  for (const item of await items.all()) {
    const label = (await item.locator("span").first().textContent())!.trim();
    const disabled = (await item.getAttribute("aria-disabled")) === "true" || await item.isDisabled();
    out.push({ label, reason: disabled ? ((await item.locator(".menu-reason").textContent()) ?? "").trim() : null });
  }
  await page.keyboard.press("Escape");
  return out;
}

/** Run the item of `menu` whose label is exactly `label`. */
async function runItem(page: Page, menu: string, label: string): Promise<void> {
  await page.getByRole("navigation", { name: "Workbook menus" }).getByRole("button", { name: menu, exact: true }).click();
  await page.getByRole("menu", { name: menu }).getByRole("menuitem").filter({ has: page.getByText(label, { exact: true }) }).click();
}

const MENUS = ["File", "Edit", "View", "Insert", "Format", "Data", "Tools", "Help"];

// What each command that opens a tool does (by hand, from DESIGN.md). Commands that change
// the workbook on purpose are checked in the apply / undo / reload test below; "All workbooks"
// leaves the workbook and is covered by e2e/savestate.spec.ts.
const OPENS: Record<string, Outcome> = {
  "Import file…": "file", "Rename workbook…": "dialog",
  "Export CSV": "download", "Export CSV for Excel or Sheets": "download", "Export Parquet": "download",
  "Export JSON lines": "download",
  // A workbook with tables gets its compatibility notes first (tables are not sheets in .xlsx).
  "Export Sheets as .xlsx": "dialog", "Export Excel workbook (.xlsx), all sheets": "dialog",
  // Paste special opens its menu: it applies to cells copied here and to other apps' clipboard.
  "Paste special…": "menu", "Go to…": "dialog", "Find…": "find", "Find and replace…": "find",
  "Zoom in": "toast", "Zoom out": "toast", "Actual size (100%)": "toast", "Explorer": "explorer", "Saved views…": "menu",
  "Light theme": "theme", "Dark theme": "theme", "System theme": "theme",
  "Column…": "dialog", "Named range…": "dialog", "Table from SQL query…": "sql",
  "Format table…": "dialog", "Conditional colour…": "dialog", "Allowed values…": "dialog", "Rules on this sheet…": "dialog",
  "Filter…": "dialog", "Analyze…": "analyze", "Dashboards…": "dialog", "Data tools…": "dialog",
  "SQL workspace": "sql", "Functions…": "functions", "Change history…": "dialog", "Compute and size…": "dialog",
  "Command palette": "dialog", "Formula help": "dialog", "Keyboard shortcuts": "dialog",
};
const CHANGES = new Set(["New sheet", "Sheet", "Demo data: 1M rows", "Bold", "Italic", "Underline", "Number: 1,234.56", "Percent: 12.35%",
  "Currency: $1,234.56", "Date: 2026-11-01", "General number format", "Merge cells", "Merge and centre", "Merge each row", "Unmerge cells",
  "Clear formatting", "Unfreeze columns", "Unfreeze rows", "All workbooks"]);
const label = (l: string) => l.replace(/^✓ /, "").replace(/^Freeze (columns|rows) .*/, "Freeze $1");
CHANGES.add("Freeze columns").add("Freeze rows");

test("U.T05: every visible menu and toolbar command works or says why; tools leave the workbook unchanged", async ({ page }) => {
  test.setTimeout(240_000);
  const wb = await server.createWorkbook("Inventory"), ws = await server.connect(wb);
  await ws.call("create_table_from_query", { name: "t", sql: "SELECT * FROM (VALUES (1, 'a'), (2, 'b'), (3, 'c')) v(x, y)" });
  await ws.call("sheet_create", { name: "S" });
  await ws.call("sheet_set", { sheet: 0, inputs: [{ row: 0, column: 0, value: "10" }, { row: 1, column: 0, value: "20" }] });
  const before = await persisted(ws);
  await page.goto(`${server.url}/w/${wb}`);
  const tools = page.getByRole("toolbar", { name: "Workbook tools" });
  const problems: string[] = [];

  for (const tab of [/^t/, /^S/]) {
    await page.getByRole("tab", { name: tab }).click();
    await expect(page.getByRole("tab", { name: tab })).toHaveAttribute("aria-selected", "true");
    if (tab.source === "^t") await expect(tools.getByRole("button", { name: "Filter", exact: true })).toBeEnabled();
    for (const menu of MENUS) {
      for (const item of await menuItems(page, menu)) {
        const key = label(item.label);
        if (item.reason !== null) {
          if (!item.reason) problems.push(`${tab.source} ${menu} › ${item.label}: disabled without a reason`);
          continue;
        }
        if (CHANGES.has(key)) continue;
        // The theme already chosen is shown checked; choosing it again changes nothing.
        if (item.label.startsWith("✓ ") && key.endsWith(" theme")) continue;
        if (tab.source === "^S" && (key === "Undo" || key === "Redo")) {
          // In a sheet these take back (and redo) the last change: here, the two values set above.
          const cells = async () => JSON.stringify((await ws.call("sheet_fetch", { sheet: 0, row_start: 0, row_count: 2, column_count: 1 })).rows);
          const was = await cells();
          await runItem(page, menu, item.label);
          await expect.poll(cells).not.toBe(was);
          continue;
        }
        const want = OPENS[key];
        if (!want) { problems.push(`${tab.source} ${menu} › ${item.label}: not in the inventory`); continue; }
        const got = await outcome(page, () => runItem(page, menu, item.label));
        if (got !== want) problems.push(`${tab.source} ${menu} › ${item.label}: expected ${want}, got ${got}`);
        await closeAll(page);
      }
    }
    // The toolbar: every visible button opens its tool, or is disabled with a reason.
    const TOOLBAR: Record<string, Outcome> = {
      "Explorer": "explorer", "Views": "menu", "Filter": "dialog", "Add column": "dialog", "Format table": "dialog",
      "Find / replace": "find", "Names": "dialog", "Go to": "dialog", "Import": "file",
      "Analyze": "analyze", "SQL": "sql", "Functions": "functions", "Export": "menu",
    };
    for (const button of await tools.getByRole("button").all()) {
      if (!(await button.isVisible())) continue;
      const name = (await button.getAttribute("aria-label"))!;
      if (await button.isDisabled()) {
        if (!(await button.getAttribute("title"))) problems.push(`${tab.source} toolbar ${name}: disabled without a reason`);
        continue;
      }
      if (name === "Undo" || name === "Redo") {
        const cells = async () => JSON.stringify((await ws.call("sheet_fetch", { sheet: 0, row_start: 0, row_count: 2, column_count: 1 })).rows);
        const was = await cells();
        await button.click();
        await expect.poll(cells).not.toBe(was);
        continue;
      }
      const want = TOOLBAR[name];
      if (!want) { problems.push(`${tab.source} toolbar ${name}: not in the inventory`); continue; }
      const got = await outcome(page, () => button.click());
      if (got !== want) problems.push(`${tab.source} toolbar ${name}: expected ${want}, got ${got}`);
      await closeAll(page);
      // The explorer toggles: put it back.
      if (name === "Explorer") await button.click();
    }
  }
  expect(problems).toEqual([]);
  // Opening and closing every tool saved nothing.
  expect(await persisted(ws)).toEqual(before);
  ws.close();
});

/** The displayed value of a sheet cell, as exposed to assistive technology. */
async function shown(page: Page, r: number, c: number): Promise<string> {
  await page.locator(".grid-host").focus();
  await page.keyboard.press("ControlOrMeta+Home");
  for (let i = 0; i < r; i++) await page.keyboard.press("ArrowDown");
  for (let i = 0; i < c; i++) await page.keyboard.press("ArrowRight");
  await page.evaluate(() => new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r))));
  const id = await page.locator(".grid-host").getAttribute("aria-activedescendant");
  return (await page.locator(`#${id}`).textContent()) ?? "";
}

async function select(page: Page, r: number, c: number): Promise<void> {
  await page.locator(".grid-host").focus();
  await page.keyboard.press("ControlOrMeta+Home");
  for (let i = 0; i < r; i++) await page.keyboard.press("ArrowDown");
  for (let i = 0; i < c; i++) await page.keyboard.press("ArrowRight");
}

test.describe("in another locale and time zone", () => {
  // Far from UTC and with a comma decimal: explicit formats must not move.
  test.use({ locale: "de-DE", timezoneId: "Pacific/Kiritimati" });

  test("U.T05: format examples apply as shown, undo, survive a reload; statistics use values; freeze and merge persist", async ({ page }) => {
    test.setTimeout(180_000);
    const wb = await server.createWorkbook("Formats"), ws = await server.connect(wb);
    await ws.call("sheet_create", { name: "S" });
    // 46327 is 2026-11-01 as a spreadsheet date serial (2026-01-01 is 46023, plus 304 days).
    await ws.call("sheet_set", { sheet: 0, inputs: [
      { row: 0, column: 0, value: "1234.56" }, { row: 1, column: 0, value: "0.1235" },
      { row: 2, column: 0, value: "1234.56" }, { row: 3, column: 0, value: "46327" },
    ] });
    await page.goto(`${server.url}/w/${wb}`);
    await page.getByRole("tab", { name: /^S/ }).click();
    await expect.poll(() => shown(page, 0, 0)).toBe("1234.56");

    // Each Format menu example is exactly what the cell shows.
    const examples: [number, string, string][] = [
      [0, "Number: 1,234.56", "1,234.56"], [1, "Percent: 12.35%", "12.35%"],
      [2, "Currency: $1,234.56", "$1,234.56"], [3, "Date: 2026-11-01", "2026-11-01"],
    ];
    for (const [row, item, display] of examples) {
      await select(page, row, 0);
      await menuCommand(page, "Format", item);
      await expect.poll(() => shown(page, row, 0)).toBe(display);
    }

    // Range statistics come from the values: 1234.56 + 0.1235 + 1234.56 + 46327 = 48796.2435.
    await select(page, 0, 0);
    for (let i = 0; i < 3; i++) await page.keyboard.press("Shift+ArrowDown");
    await expect(page.locator(".stats")).toContainText("Count 4");
    // German grouping: 48.796,24 (two decimals shown).
    const sum = async () => Number(/Sum ([^ ]+)/.exec((await page.locator(".stats").textContent()) ?? "")?.[1].replace(/\./g, "").replace(",", "."));
    await expect.poll(sum).toBeCloseTo(48796.2435, 2);

    // Undo takes the date format back, redo restores it; one step each.
    await select(page, 3, 0);
    await menuCommand(page, "Edit", /^Undo/);
    await expect.poll(() => shown(page, 3, 0)).toBe("46327");
    await menuCommand(page, "Edit", /^Redo/);
    await expect.poll(() => shown(page, 3, 0)).toBe("2026-11-01");

    // Merge B1:C1 and freeze the first column, then reload: all of it is kept.
    await select(page, 0, 1);
    await page.keyboard.press("Shift+ArrowRight");
    await runItem(page, "Format", "Merge cells");
    await select(page, 0, 0);
    await menuCommand(page, "View", /^Freeze columns A–A/);
    await expect.poll(async () => (await ws.call<{ frozen_columns?: number }[]>("sheets"))[0].frozen_columns ?? 0).toBe(1);
    await page.reload();
    await page.getByRole("tab", { name: /^S/ }).click();
    for (const [row, , display] of examples) await expect.poll(() => shown(page, row, 0)).toBe(display);
    const sheets = await ws.call<{ name: string; frozen_columns?: number }[]>("sheets");
    expect(sheets.map((s) => [s.name, s.frozen_columns ?? 0])).toEqual([["S", 1]]);
    const layout = await ws.call<{ merged: unknown[] }>("sheet_layout", { sheet: 0 });
    expect(layout.merged).toHaveLength(1);
    expect(await ws.call("sheet_charts", { sheet: 0 })).toEqual([]);
    ws.close();
  });
});

test("U.T05: context menus give a reason for every disabled command; sheet constraints are kept", async ({ page }) => {
  const wb = await server.createWorkbook("Context menus"), ws = await server.connect(wb);
  await ws.call("create_table_from_query", { name: "one", sql: "SELECT 1 AS x" });
  await ws.call("sheet_create", { name: "Only" });
  await page.goto(`${server.url}/w/${wb}`);

  /** Every disabled item of the open menu, with its reason. */
  const disabled = async () => {
    const menu = page.getByRole("menu");
    await expect(menu).toBeVisible();
    const out: [string, string][] = [];
    for (const item of await menu.getByRole("menuitem").all()) {
      if ((await item.getAttribute("aria-disabled")) !== "true") continue;
      out.push([(await item.locator("span").first().textContent())!, (await item.getAttribute("title")) ?? ""]);
    }
    await page.keyboard.press("Escape");
    return out;
  };

  // The only sheet: it cannot move or be hidden, and says so.
  await page.getByRole("tab", { name: /^Only/ }).click({ button: "right" });
  expect(await disabled()).toEqual([["Move left", "Already first"], ["Move right", "Already last"], ["Hide sheet", "The only visible sheet stays visible"]]);

  // A one-column table without a sort: the header menu says why.
  await page.getByRole("tab", { name: /^one/ }).click();
  await expect(page.getByRole("grid", { name: "Table one" })).toBeVisible();
  await page.locator(".grid-scroller").click({ button: "right", position: { x: 70, y: 16 } });
  expect(await disabled()).toEqual([["Then sort by this", "Sort by another column first"], ["Hide column", "The last visible column stays"]]);

  // A sheet name already in use is refused with the reason; nothing changes.
  await ws.call("sheet_create", { name: "Second" });
  await page.reload();
  await page.getByRole("tab", { name: /^Second/ }).click({ button: "right" });
  await page.getByRole("menuitem", { name: "Rename…" }).click();
  const dialog = page.getByRole("dialog", { name: "Rename sheet" });
  await dialog.getByRole("textbox", { name: "Name" }).fill("Only");
  await dialog.getByRole("button", { name: "Rename" }).click();
  // U.11a: the reason shows in the dialog, on the field, not only in a toast.
  await expect(dialog.getByRole("alert")).not.toBeEmpty();
  await expect(dialog.getByRole("textbox", { name: "Name" })).toHaveAttribute("aria-invalid", "true");
  expect((await ws.call<{ name: string }[]>("sheets")).map((s) => s.name)).toEqual(["Only", "Second"]);
  ws.close();
});

// Its own server with sign-in, the admin created from the environment.
const ADMIN = { email: "admin@example.com", password: "correct horse battery" };
const accounts = new Server(["--auth", "accounts"], { ALEPHGRID_BOOTSTRAP_EMAIL: ADMIN.email, ALEPHGRID_BOOTSTRAP_PASSWORD: ADMIN.password });
test.afterAll(async () => accounts.dispose());

test("U.T05: a viewer sees which commands their role does not allow, and why", async ({ page, browser }) => {
  await accounts.start();
  await page.goto(`${accounts.url}/login`);
  const json = async (path: string, method = "GET", body?: unknown) => page.evaluate(async ({ path, method, body }) => {
    const r = await fetch(path, { method, headers: { "Content-Type": "application/json" }, body: body === undefined ? undefined : JSON.stringify(body) });
    if (!r.ok) throw new Error(`${path}: ${r.status} ${await r.text()}`);
    return r.status === 204 ? null : r.json();
  }, { path, method, body });
  await json("/api/auth/login", "POST", ADMIN);
  const me = await json("/api/me");
  const general = (await json("/api/spaces")).find((s: { name: string }) => s.name === "General").id;
  const workbook = (await json("/api/workbooks", "POST", { name: "Read me", space: general })).id;
  await page.evaluate(async (workbook) => new Promise((ok, fail) => {
    const ws = new WebSocket(`ws://${location.host}/ws/${workbook}`);
    ws.onmessage = (m) => {
      const msg = JSON.parse(String(m.data));
      if (msg.event === "hello") ws.send(JSON.stringify({ id: 1, op: "sheet_create", name: "S" }));
      else if (msg.id === 1) { ws.close(); msg.ok ? ok(null) : fail(new Error(msg.error)); }
    };
  }), workbook);
  const { code } = await json(`/api/orgs/${me.orgs[0].org.id}/invites`, "POST",
    { email: "vic@example.com", org_role: "member", space: general, space_role: "viewer", days: 7 });

  const vic = await (await browser.newContext()).newPage();
  await vic.addInitScript(() => { try { localStorage.setItem("alephgrid-welcome-done", "1"); } catch { /* */ } });
  await vic.goto(`${accounts.url}/login`);
  await vic.evaluate(async (code) => {
    const r = await fetch("/api/invites/accept", { method: "POST", headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ code, name: "Vic", password: "a long enough password" }) });
    if (!r.ok) throw new Error(await r.text());
  }, code);
  await vic.goto(`${accounts.url}/w/${workbook}`);
  await vic.getByRole("tab", { name: /^S/ }).click();
  await expect(vic.getByText("View only", { exact: true }).first()).toBeVisible();

  // Commands that would change the workbook are disabled with the reason; viewing ones work.
  await vic.getByRole("navigation", { name: "Workbook menus" }).getByRole("button", { name: "File", exact: true }).click();
  const file = vic.getByRole("menu", { name: "File" });
  for (const name of [/^New sheet/, /^Import file…/, /^Rename workbook…/]) {
    await expect(file.getByRole("menuitem", { name })).toHaveAttribute("aria-disabled", "true");
    await expect(file.getByRole("menuitem", { name })).toHaveAccessibleDescription("View only");
  }
  await expect(file.getByRole("menuitem", { name: /^Export Excel workbook/ })).not.toHaveAttribute("aria-disabled", "true");
  await vic.keyboard.press("Escape");
  await vic.getByRole("navigation", { name: "Workbook menus" }).getByRole("button", { name: "Format", exact: true }).click();
  await expect(vic.getByRole("menu", { name: "Format" }).getByRole("menuitem", { name: /^Bold/ })).toHaveAccessibleDescription("View only");
  await vic.keyboard.press("Escape");
  const tools = vic.getByRole("toolbar", { name: "Workbook tools" });
  await expect(tools.getByRole("button", { name: "Import" })).toBeDisabled();
  await expect(tools.getByRole("button", { name: "Import" })).toHaveAccessibleDescription("View only");
  await expect(tools.getByRole("button", { name: "Go to" })).toBeEnabled();
  await vic.context().close();
});

test("U.T05: Analyze in a workbook without tables says what it needs", async ({ page }) => {
  const wb = await server.createWorkbook("No tables"), ws = await server.connect(wb);
  await ws.call("sheet_create", { name: "S" });
  ws.close();
  await page.goto(`${server.url}/w/${wb}`);
  await page.getByRole("tab", { name: /^S/ }).click();
  await page.getByRole("toolbar", { name: "Workbook tools" }).getByRole("button", { name: "Analyze" }).click();
  const pane = page.getByRole("complementary", { name: "Analyze" });
  await expect(pane.getByRole("status")).toHaveText("Analyze summarizes tables, and this workbook has none yet. Import a file, or turn a sheet into a table from its tab menu (Convert to table).");
  // Nothing to run against: Run is not offered.
  await expect(pane.getByRole("button", { name: "Run", exact: true })).toHaveCount(0);
});
