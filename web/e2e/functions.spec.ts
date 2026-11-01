import { expect, test } from "@playwright/test";
import { Server } from "./server";

const server = new Server();
test.beforeAll(async () => server.start());
test.afterAll(async () => server.dispose());

test("define, test, complete, call, upgrade, rollback and reopen a formula function", async ({ page }) => {
  const wb = await server.createWorkbook("Function journey");
  let pinSent = false;
  let refreshId = -1;
  let releaseRefresh: (() => void) | undefined;
  await page.routeWebSocket(/\/ws\//, ws => {
    const srv = ws.connectToServer();
    ws.onMessage(m => {
      const msg = JSON.parse(String(m));
      if (msg.op === "pin_functions") pinSent = true;
      if (pinSent && msg.op === "functions" && refreshId === -1) refreshId = msg.id;
      srv.send(m);
    });
    srv.onMessage(m => {
      const msg = JSON.parse(String(m));
      if (msg.id === refreshId && !releaseRefresh) releaseRefresh = () => ws.send(m);
      else ws.send(m);
    });
  });
  await page.goto(`${server.url}/w/${wb}`);
  await expect(page.locator(".status-dot.status-open")).toBeVisible();
  await page.getByRole("button", { name: "Functions", exact: true }).click();
  const dialog = page.getByRole("complementary", { name: "Functions" });
  // An empty library offers an example; nothing is saved until Publish.
  await dialog.getByRole("button", { name: "Start from an example" }).click();
  await dialog.getByRole("button", { name: "Run tests", exact: true }).click();
  await expect(dialog.getByRole("region", { name: "Test results" })).toContainText("Passed · test 1 · actual 0.4");
  await dialog.getByRole("button", { name: "Publish version", exact: true }).click();
  await expect(dialog.getByRole("combobox", { name: "Function", exact: true })).toHaveValue("MARGIN");
  await expect(dialog.locator(".fn-state")).toHaveText("Published · v1 · active");
  await dialog.getByRole("button", { name: "Close functions panel" }).click();
  await expect(dialog).toHaveCount(0);
  await expect(page.getByRole("tab", { name: /Sheet1/ })).toBeVisible();
  await page.getByRole("tab", { name: /Sheet1/ }).click();
  const editor = page.getByRole("textbox", { name: "Cell value or formula", exact: true });
  await editor.fill("=MAR");
  await expect(page.getByRole("region", { name: "Function suggestions" })).toContainText("MARGIN(revenue, cost)");
  await editor.press("Tab");
  await expect(editor).toHaveValue("=MARGIN(");
  await expect(page.getByRole("region", { name: "Function suggestions" })).toContainText("Profit as a fraction of revenue");
  await editor.fill("=MARGIN(100,60)");
  await editor.press("Enter");
  const ws = await server.connect(wb);
  const cell = async () => (await ws.call("sheet_fetch", { sheet: 0, row_start: 0, row_count: 1, column_count: 1 })).rows[0]?.[0]?.[0];
  await expect.poll(cell).toBe("0.4");
  await page.getByRole("button", { name: "Functions", exact: true }).click();
  // The panel kept its state while closed: MARGIN v1 is still open in it.
  await expect(dialog.getByRole("combobox", { name: "Function", exact: true })).toHaveValue("MARGIN");
  await dialog.getByRole("textbox", { name: "Function source", exact: true }).fill("revenue-cost");
  await dialog.getByRole("tab", { name: "Tests" }).click();
  await dialog.getByRole("textbox", { name: "Test 1 expected" }).fill("40");
  await dialog.getByRole("button", { name: "Publish version", exact: true }).click();
  // Published, but the workbook still uses v1 until v2 is activated.
  await expect(dialog.locator(".fn-state")).toHaveText("Published · v2");
  expect(await cell()).toBe("0.4");
  await dialog.getByRole("tab", { name: "Versions" }).click();
  await dialog.getByRole("button", { name: "Activate v2 in this workbook" }).click();
  await expect.poll(cell).toBe("40");
  await expect.poll(() => !!releaseRefresh).toBe(true);
  // While the activation's refresh is in flight, the version buttons wait for it.
  await expect(dialog.getByRole("button", { name: "Run tests", exact: true })).toBeDisabled();
  releaseRefresh!();
  await expect(dialog.getByRole("button", { name: "Run tests", exact: true })).toBeEnabled();
  await dialog.getByRole("button", { name: "Activate v1 in this workbook" }).click();
  await expect.poll(cell).toBe("0.4");
  ws.close();
  await page.reload();
  await page.getByRole("button", { name: "Functions", exact: true }).click();
  await expect(dialog.getByRole("combobox", { name: "Function", exact: true })).toContainText("MARGIN · v1");
});

test("SQL function editor validates errors and registers a query/column macro", async ({ page }) => {
  const wb = await server.createWorkbook("SQL functions");
  await page.goto(`${server.url}/w/${wb}`);
  await expect(page.locator(".status-dot.status-open")).toBeVisible();
  await page.getByRole("button", { name: "Functions", exact: true }).click();
  const dialog = page.getByRole("complementary", { name: "Functions" });
  await dialog.getByRole("button", { name: "Start from an example" }).click();
  await dialog.getByRole("combobox", { name: "Function engine", exact: true }).selectOption("sql");
  const source = dialog.getByRole("textbox", { name: "Function source", exact: true });
  await source.fill("current_setting('threads')");
  await dialog.getByRole("button", { name: "Run tests", exact: true }).click();
  await expect(dialog.getByRole("region", { name: "Test results" })).toContainText("not available");
  await source.fill("(CAST(revenue AS DOUBLE)-CAST(cost AS DOUBLE))/CAST(revenue AS DOUBLE)");
  await dialog.getByRole("button", { name: "Publish version", exact: true }).click();
  await expect(dialog.getByRole("combobox", { name: "Function", exact: true })).toHaveValue("MARGIN");
  const ws = await server.connect(wb);
  const result = await ws.call("query", { sql: "SELECT MARGIN(100,60)" });
  expect(result.rows).toEqual([[0.4]]);
  ws.close();
});

test("SQL-disabled server permits formula publication and pins but refuses SQL functions", async () => {
  const restricted = new Server(["--disable-sql"]);
  await restricted.start();
  try {
    const wb = await restricted.createWorkbook("Formula only");
    const ws = await restricted.connect(wb);
    const definition = { name: "ADDONE", kind: "formula", arguments: ["value"], source: "value+1", documentation: "", tests: [] };
    await ws.call("publish_function", { definition });
    await ws.call("pin_functions", { pins: { ADDONE: 1 } });
    await expect(ws.call("publish_function", { definition: { ...definition, name: "SQLFUNC", kind: "sql" } })).rejects.toThrow(/disabled/);
    ws.close();
  } finally { await restricted.dispose(); }
});

// U.06: the editor tells a draft, a tested draft and a published version apart.
test("the function editor shows Draft, Tested and Published states", async ({ page }) => {
  const wb = await server.createWorkbook("Function states");
  await page.goto(`${server.url}/w/${wb}`);
  await expect(page.locator(".status-dot.status-open")).toBeVisible();
  await page.getByRole("button", { name: "Functions", exact: true }).click();
  const dialog = page.getByRole("complementary", { name: "Functions" });
  const state = dialog.locator(".fn-state");
  await dialog.getByRole("textbox", { name: "Function name" }).fill("HALF");
  await dialog.getByRole("textbox", { name: "Arguments" }).fill("x");
  await dialog.getByRole("textbox", { name: "Function source" }).fill("x/2");
  await dialog.getByRole("tab", { name: "Tests" }).click();
  await dialog.getByRole("button", { name: "Add test" }).click();
  await dialog.getByRole("textbox", { name: "Test 1 x" }).fill("10");
  await dialog.getByRole("textbox", { name: "Test 1 expected" }).fill("6");
  await expect(dialog.locator(".fn-signature")).toHaveText("HALF(x)");
  await expect(state).toHaveText("Draft");

  // A failing test: still a draft, and it says so.
  await dialog.getByRole("button", { name: "Run tests", exact: true }).click();
  await expect(dialog.getByRole("region", { name: "Test results" })).toContainText("Failed · test 1 · actual 5 · expected 6");
  await expect(dialog.locator(".fn-tests tr.fail .fn-actual")).toHaveText("✗ 5");
  await expect(state).toHaveText("Draft · tests failing");

  // Fixed and passing: tested, for exactly this definition.
  await dialog.getByRole("textbox", { name: "Test 1 expected" }).fill("5");
  await expect(state).toHaveText("Draft");
  await expect(dialog.getByRole("region", { name: "Test results" })).toContainText("Edited since this run");
  await dialog.getByRole("button", { name: "Run tests", exact: true }).click();
  await expect(state).toHaveText("Tested · ready to publish");

  await dialog.getByRole("button", { name: "Publish version", exact: true }).click();
  await expect(state).toHaveText("Published · v1 · active");

  // Editing the published definition makes it a draft again.
  await dialog.getByRole("tab", { name: "Docs" }).click();
  await dialog.getByRole("textbox", { name: "Function documentation" }).fill("Half of x.");
  await expect(state).toHaveText("Draft");
});
