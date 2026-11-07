// U.11a: the shared component contract: disabled controls say why (menu items stay reachable
// by keyboard), refused form values are linked error text, and buttons are busy while waiting.

import { expect, test } from "@playwright/test";
import { Server, menuCommand } from "./server";

const server = new Server();
test.beforeAll(async () => server.start());
test.afterAll(async () => server.dispose());

test.beforeEach(async ({ page }) => {
  await page.addInitScript(() => { try { localStorage.setItem("alephgrid-welcome-done", "1"); } catch { /* */ } });
});

test("U.11a: disabled menu items are reachable by keyboard and say why; activating one does nothing", async ({ page }) => {
  const wb = await server.createWorkbook("Menus"), ws = await server.connect(wb);
  await ws.call("create_table_from_query", { name: "t", sql: "SELECT 1 AS x" });
  await page.goto(`${server.url}/w/${wb}`);
  await expect(page.getByRole("grid", { name: "Table t" })).toBeVisible();
  const edit = page.getByRole("navigation", { name: "Workbook menus" }).getByRole("button", { name: "Edit", exact: true });
  await edit.focus();
  await page.keyboard.press("ArrowDown");
  const menu = page.getByRole("menu", { name: "Edit" });
  await expect(menu).toBeVisible();
  // Home goes to the first item even though it does not apply to tables.
  await page.keyboard.press("Home");
  const undo = menu.getByRole("menuitem", { name: /^Undo/ });
  await expect(undo).toBeFocused();
  await expect(undo).toBeDisabled();
  await expect(undo).toHaveAccessibleDescription("Sheets only");
  // Enter on it runs nothing: the menu stays open and no toast appears.
  await page.keyboard.press("Enter");
  await expect(menu).toBeVisible();
  await expect(page.locator(".toast")).toHaveCount(0);
  await page.keyboard.press("Escape");
  await expect(edit).toBeFocused();
  ws.close();
});

test("U.11a: a form shows a refused value as linked error text and is busy while it waits", async ({ page }) => {
  await page.goto(server.url);
  let release!: () => void;
  const held = new Promise<void>((r) => (release = r));
  let refuse = true;
  await page.route("**/api/spaces", async (route) => {
    if (route.request().method() !== "POST" || !refuse) return route.continue();
    refuse = false;
    await held;
    await route.fulfill({ status: 400, contentType: "application/json", body: JSON.stringify({ error: "A space needs a name" }) });
  });
  await page.getByRole("button", { name: "New space" }).click();
  const dialog = page.getByRole("dialog", { name: "New space" });
  const create = dialog.getByRole("button", { name: "Create" });
  await create.click();
  // While the server decides, Create is busy and cannot be pressed twice.
  await expect(create).toHaveAttribute("aria-busy", "true");
  await expect(create).toBeDisabled();
  release();
  const name = dialog.getByRole("textbox", { name: "Name" });
  await expect(dialog.getByRole("alert")).toHaveText("A space needs a name");
  await expect(name).toHaveAttribute("aria-invalid", "true");
  await expect(name).toHaveAccessibleDescription(/A space needs a name/);
  await expect(name).toBeFocused();
  await expect(create).toBeEnabled();
  await expect(create).not.toHaveAttribute("aria-busy", "true");
  // Fixing it clears the error state and the form goes through.
  await name.fill("Finance");
  await expect(name).not.toHaveAttribute("aria-invalid", "true");
  await create.click();
  await expect(dialog).toHaveCount(0);
  await expect(page.getByRole("heading", { name: "Finance" })).toBeVisible();
});

test("U.11a / U.04a: toolbar controls that cannot run here say why", async ({ page }) => {
  const wb = await server.createWorkbook("Empty toolbar");
  await page.goto(`${server.url}/w/${wb}`);
  await expect(page.locator(".status-dot.status-open")).toBeVisible({ timeout: 60_000 });
  const tools = page.getByRole("toolbar", { name: "Workbook tools" });
  const exportBtn = tools.getByRole("button", { name: "Export" });
  await expect(exportBtn).toBeDisabled();
  await expect(exportBtn).toHaveAccessibleDescription("Open a table or sheet to export it");
});

test("U.11a: a cancel stays pending until the server acknowledges it", async ({ page }) => {
  const wb = await server.createWorkbook("Cancelling");
  // Hold the server's answer to the long query, so the moment between asking to cancel and
  // the server confirming it can be seen.
  let queryId = -1;
  let release: (() => void) | undefined;
  await page.routeWebSocket(/\/ws\//, (ws) => {
    const srv = ws.connectToServer();
    ws.onMessage((m) => {
      const msg = JSON.parse(String(m));
      if (msg.op === "query" && String(msg.sql).includes("range(100000000000)")) queryId = msg.id;
      srv.send(m);
    });
    srv.onMessage((m) => {
      const msg = JSON.parse(String(m));
      if (queryId !== -1 && msg.id === queryId) release = () => ws.send(m);
      else ws.send(m);
    });
  });
  await page.goto(`${server.url}/w/${wb}`);
  await expect(page.locator(".status-dot.status-open")).toBeVisible({ timeout: 60_000 });
  await menuCommand(page, "Tools", "SQL workspace");
  const sql = page.getByRole("region", { name: "SQL" });
  await sql.getByRole("combobox", { name: "SQL query" }).fill("SELECT sum(i) FROM range(100000000000) r(i)");
  await sql.getByRole("button", { name: "Run", exact: true }).click();
  await expect(sql.locator(".sql-status")).toContainText("Running");
  const cancel = sql.getByRole("button", { name: "Cancel", exact: true });
  await cancel.click();
  await expect.poll(() => !!release, { timeout: 30_000 }).toBe(true);
  // The server stopped it; until its answer arrives the cancel is pending, not done.
  await expect(sql.locator(".sql-status")).toHaveText("Cancelling…");
  await expect(cancel).toBeDisabled();
  await expect(cancel).toHaveAttribute("aria-busy", "true");
  release!();
  await expect(sql.locator(".sql-status")).toHaveText("Cancelled.");
  await expect(cancel).toBeHidden();
});

test("U.11a: dialogs and panels return focus to what opened them", async ({ page }) => {
  const wb = await server.createWorkbook("Focus"), ws = await server.connect(wb);
  await ws.call("sheet_create", { name: "S" });
  ws.close();
  await page.goto(`${server.url}/w/${wb}`);
  await page.getByRole("tab", { name: /^S/ }).click();
  const tools = page.getByRole("toolbar", { name: "Workbook tools" });
  // A dialog from the toolbar: Escape closes it and focus is back on its button.
  for (const name of ["Go to", "Names"]) {
    const button = tools.getByRole("button", { name, exact: true });
    await button.click();
    await expect(page.getByRole("dialog")).toBeVisible();
    await page.keyboard.press("Escape");
    await expect(page.getByRole("dialog")).toHaveCount(0);
    await expect(button).toBeFocused();
  }
  // A dialog from a menu returns focus to the menu bar button that led there.
  const help = page.getByRole("navigation", { name: "Workbook menus" }).getByRole("button", { name: "Help", exact: true });
  await help.click();
  await page.getByRole("menuitem", { name: /^Keyboard shortcuts/ }).click();
  await expect(page.getByRole("dialog")).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await expect(help).toBeFocused();
});

// ---- U.11a: the custom forms follow the form-dialog contract -----------------------------

test("U.11a: the import pane links a refused column type to its error and is busy while importing", async ({ page }) => {
  const wb = await server.createWorkbook("Import refusal");
  await page.goto(`${server.url}/w/${wb}`);
  await expect(page.locator(".status-dot.status-open")).toBeVisible();
  let release!: () => void;
  const held = new Promise<void>((r) => (release = r));
  await page.route("**/uploads/*/import", async (route) => { await held; await route.continue(); });
  await page.locator('.workbook input[type="file"]').setInputFiles({ name: "codes.csv", mimeType: "text/csv", buffer: Buffer.from("code,label\n1,apple\n2,pear\n") });
  const dialog = page.getByRole("dialog", { name: "Import codes.csv" });
  const label = dialog.getByLabel("Type for column label");
  await label.selectOption("BIGINT");
  const go = dialog.getByRole("button", { name: "Import table" });
  await go.click();
  // While the server imports, Import is busy and cannot be pressed twice.
  await expect(go).toHaveAttribute("aria-busy", "true");
  await expect(go).toBeDisabled();
  release();
  // "apple" is not a whole number: the type of that column is the refused field.
  await expect(dialog.getByRole("alert").first()).toContainText('"apple" in column "label"');
  await expect(label).toHaveAttribute("aria-invalid", "true");
  await expect(label).toHaveAccessibleDescription(/"apple" in column "label"/);
  await expect(label).toBeFocused();
  await expect(go).not.toHaveAttribute("aria-busy", "true");
  // Choosing another type clears the invalid state; the import goes through.
  await label.selectOption("VARCHAR");
  await expect(label).not.toHaveAttribute("aria-invalid", "true");
  await go.click();
  await expect(dialog).toHaveCount(0);
  expect(await server.exportCsv(wb, { table: "codes" })).toBe("code,label\n1,apple\n2,pear\n");
});

test("U.11a: Analyze links a refused calculated value to its field and is busy while running", async ({ page }) => {
  const wb = await server.createWorkbook("Analyze refusal"), ws = await server.connect(wb);
  await ws.call("create_table_from_query", { name: "sales", sql: "SELECT * FROM (VALUES ('north', 10), ('south', 30)) v(region, amount)" });
  ws.close();
  let hold = false;
  const held: (() => void)[] = [];
  await page.routeWebSocket(/\/ws\//, (sock) => {
    const srv = sock.connectToServer();
    const analyses = new Set<number>();
    sock.onMessage((m) => { const msg = JSON.parse(String(m)); if (msg.op === "analyze") analyses.add(msg.id); srv.send(m); });
    srv.onMessage((m) => {
      const msg = JSON.parse(String(m));
      if (hold && analyses.has(msg.id)) held.push(() => sock.send(m)); else sock.send(m);
    });
  });
  await page.goto(`${server.url}/w/${wb}`);
  await expect(page.getByRole("grid", { name: "Table sales" })).toBeVisible();
  await page.getByRole("button", { name: "Analyze", exact: true }).click();
  const pane = page.getByRole("complementary", { name: "Analyze" });
  await pane.getByRole("button", { name: "Add calculated value" }).click();
  const formula = pane.getByRole("textbox", { name: "Formula of calculated value 1" });
  await formula.fill("=[@nothing_here]*2");
  hold = true;
  const run = pane.getByRole("button", { name: "Run", exact: true });
  await run.click();
  await expect(run).toHaveAttribute("aria-busy", "true");
  await expect(run).toBeDisabled();
  await expect.poll(() => held.length).toBe(1);
  hold = false;
  held.shift()!();
  const alert = pane.getByRole("alert");
  await expect(alert).toContainText("calculated");
  await expect(formula).toHaveAttribute("aria-invalid", "true");
  await expect(formula).toHaveAccessibleDescription((await alert.textContent())!);
  await expect(formula).toBeFocused();
  await expect(run).not.toHaveAttribute("aria-busy", "true");
  // Fixing the formula clears the error state and the analysis runs.
  await formula.fill("=[@sum_amount]*2");
  await expect(formula).not.toHaveAttribute("aria-invalid", "true");
  await run.click();
  await expect(page.getByRole("region", { name: "Analysis result" })).toContainText("80");
  await expect(pane.locator("#analyze-error")).toBeEmpty();
});

test("U.11a: the function panel links a refused definition to its field and is busy while publishing", async ({ page }) => {
  const wb = await server.createWorkbook("Function refusal");
  let hold = false;
  const held: (() => void)[] = [];
  await page.routeWebSocket(/\/ws\//, (sock) => {
    const srv = sock.connectToServer();
    const publishes = new Set<number>();
    sock.onMessage((m) => { const msg = JSON.parse(String(m)); if (msg.op === "publish_function") publishes.add(msg.id); srv.send(m); });
    srv.onMessage((m) => {
      const msg = JSON.parse(String(m));
      if (hold && publishes.has(msg.id)) held.push(() => sock.send(m)); else sock.send(m);
    });
  });
  await page.goto(`${server.url}/w/${wb}`);
  await expect(page.locator(".status-dot.status-open")).toBeVisible();
  await page.getByRole("button", { name: "Functions", exact: true }).click();
  const panel = page.getByRole("complementary", { name: "Functions" });
  await panel.getByRole("button", { name: "Start from an example" }).click();

  // A source the server cannot parse: the code is the refused field.
  const source = panel.getByRole("textbox", { name: "Function source" });
  await source.fill("IF(revenue=0,0,");
  await panel.getByRole("button", { name: "Run tests" }).click();
  const error = panel.locator("#fn-error");
  await expect(error).toHaveAttribute("role", "alert");
  await expect(error).toContainText("invalid formula function signature");
  await expect(source).toHaveAttribute("aria-invalid", "true");
  await expect(source).toHaveAccessibleDescription(/invalid formula function signature/);
  await expect(source).toBeFocused();
  await source.fill("IF(revenue=0,0,(revenue-cost)/revenue)");
  await expect(source).not.toHaveAttribute("aria-invalid", "true");

  // A name taken by a built-in function: the name is the refused field, and Publish is busy
  // while the server decides.
  const name = panel.getByRole("textbox", { name: "Function name" });
  await name.fill("SUM");
  hold = true;
  const publish = panel.getByRole("button", { name: "Publish version" });
  await publish.click();
  await expect(publish).toHaveAttribute("aria-busy", "true");
  await expect(publish).toBeDisabled();
  await expect.poll(() => held.length).toBe(1);
  hold = false;
  held.shift()!();
  await expect(error).toContainText("built-in");
  await expect(name).toHaveAttribute("aria-invalid", "true");
  await expect(name).toHaveAccessibleDescription(/built-in/);
  await expect(name).toBeFocused();
  await expect(publish).not.toHaveAttribute("aria-busy", "true");
  // Nothing was published.
  const check = await server.connect(wb);
  expect(await check.call("functions")).toEqual([]);
  check.close();
});

// ---- U.11a: an error is never shown only in a toast --------------------------------------

test("U.11a: a rename the server refuses stays in its dialog with the field marked", async ({ page }) => {
  const wb = await server.createWorkbook("Rename refusal"), ws = await server.connect(wb);
  await ws.call("sheet_create", { name: "First" });
  await ws.call("sheet_create", { name: "Second" });
  await page.goto(`${server.url}/w/${wb}`);
  await page.getByRole("tab", { name: /^Second/ }).click({ button: "right" });
  await page.getByRole("menuitem", { name: "Rename…" }).click();
  const dialog = page.getByRole("dialog", { name: "Rename sheet" });
  const name = dialog.getByRole("textbox", { name: "Name" });
  await name.fill("First");
  await dialog.getByRole("button", { name: "Rename" }).click();
  // Refused (the name is taken): the dialog stays with the reason next to the field.
  await expect(dialog.getByRole("alert")).not.toHaveText("");
  await expect(name).toHaveAttribute("aria-invalid", "true");
  await expect(name).toBeFocused();
  expect((await ws.call<{ name: string }[]>("sheets")).map((s) => s.name)).toEqual(["First", "Second"]);
  await name.fill("Third");
  await dialog.getByRole("button", { name: "Rename" }).click();
  await expect(dialog).toHaveCount(0);
  await expect.poll(async () => (await ws.call<{ name: string }[]>("sheets")).map((s) => s.name)).toEqual(["First", "Third"]);
  ws.close();
});

test("U.11a: a refused confirmation stays open and says why; a refused pane action shows in the pane", async ({ page }) => {
  const wb = await server.createWorkbook("Refused actions"), ws = await server.connect(wb);
  await ws.call("create_table_from_query", { name: "keep", sql: "SELECT 1 AS x" });
  await ws.call("sheet_create", { name: "Notes" });
  await ws.call("add_comment", { sheet: 0, row: 0, column: 0, text: "Check this" });
  // The server refuses these two operations for this page (as it would for a lost race).
  let held: (() => void) | null = null;
  await page.routeWebSocket(/\/ws\//, (sock) => {
    const srv = sock.connectToServer();
    sock.onMessage((m) => {
      const msg = JSON.parse(String(m));
      const refuse = (error: string) => sock.send(JSON.stringify({ id: msg.id, ok: false, error }));
      if (msg.op === "drop_table") held = () => refuse("the table is in use by a running import");
      else if (msg.op === "resolve_comment") refuse("the comment was deleted by someone else");
      else srv.send(m);
    });
    srv.onMessage((m) => sock.send(m));
  });
  await page.goto(`${server.url}/w/${wb}`);
  await expect(page.getByRole("grid", { name: "Table keep" })).toBeVisible();

  await page.getByRole("tab", { name: /^keep/ }).click({ button: "right" });
  await page.getByRole("menuitem", { name: "Delete table" }).click();
  const confirm = page.getByRole("alertdialog", { name: "Delete “keep”?" });
  const del = confirm.getByRole("button", { name: "Delete" });
  await del.click();
  // Busy until the server answers; refused, the dialog stays and says why.
  await expect(del).toHaveAttribute("aria-busy", "true");
  await expect.poll(() => !!held).toBe(true);
  held!();
  await expect(confirm.getByRole("alert")).toHaveText("the table is in use by a running import");
  await expect(del).toBeEnabled();
  await expect(del).toBeFocused();
  await confirm.getByRole("button", { name: "Cancel" }).click();
  await expect(confirm).toHaveCount(0);
  expect((await ws.call<{ name: string }[]>("tables")).map((t) => t.name)).toEqual(["keep"]);

  await page.getByRole("tab", { name: /^Notes/ }).click();
  await expect(page.getByRole("grid", { name: /^Sheet Notes/ })).toBeVisible();
  await page.locator(".grid-host").click({ button: "right", position: { x: 120, y: 60 } });
  await page.getByRole("menuitem", { name: "Show comments" }).click();
  const pane = page.getByRole("complementary", { name: "Comments" });
  await pane.getByRole("button", { name: "Resolve" }).click();
  await expect(pane.getByRole("alert")).toHaveText("the comment was deleted by someone else");
  ws.close();
});
