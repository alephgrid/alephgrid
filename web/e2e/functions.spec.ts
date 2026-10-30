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
  const dialog = page.getByRole("dialog", { name: "Function library" });
  await dialog.getByRole("button", { name: "Run tests", exact: true }).click();
  await expect(dialog.getByRole("status")).toContainText("1: Passed · actual 0.4");
  await dialog.getByRole("button", { name: "Publish version", exact: true }).click();
  await expect(dialog.getByRole("button", { name: "MARGIN · v1", exact: true })).toBeVisible();
  await dialog.getByRole("button", { name: "Close", exact: true }).click();
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
  await dialog.getByRole("button", { name: "MARGIN · v1", exact: true }).click();
  await dialog.getByRole("textbox", { name: "Function source", exact: true }).fill("revenue-cost");
  await dialog.getByRole("textbox", { name: "Function tests", exact: true }).fill(JSON.stringify([{ arguments: ["100", "60"], expected: "40" }]));
  await dialog.getByRole("button", { name: "Publish version", exact: true }).click();
  await expect(dialog.getByRole("status")).toContainText("Published.");
  expect(await cell()).toBe("0.4");
  await dialog.getByRole("combobox", { name: "Function version", exact: true }).selectOption("2");
  await dialog.getByRole("button", { name: "Activate selected version", exact: true }).click();
  await expect.poll(cell).toBe("40");
  await expect.poll(() => !!releaseRefresh).toBe(true);
  await expect(dialog.getByRole("combobox", { name: "Function version", exact: true })).toBeDisabled();
  releaseRefresh!();
  await expect(dialog.getByRole("combobox", { name: "Function version", exact: true })).toBeEnabled();
  await dialog.getByRole("combobox", { name: "Function version", exact: true }).selectOption("1");
  await dialog.getByRole("button", { name: "Activate selected version", exact: true }).click();
  await expect.poll(cell).toBe("0.4");
  ws.close();
  await page.reload();
  await page.getByRole("button", { name: "Functions", exact: true }).click();
  await expect(dialog.getByRole("button", { name: "MARGIN · v1", exact: true })).toBeVisible();
});

test("SQL function editor validates errors and registers a query/column macro", async ({ page }) => {
  const wb = await server.createWorkbook("SQL functions");
  await page.goto(`${server.url}/w/${wb}`);
  await expect(page.locator(".status-dot.status-open")).toBeVisible();
  await page.getByRole("button", { name: "Functions", exact: true }).click();
  const dialog = page.getByRole("dialog", { name: "Function library" });
  await dialog.getByRole("combobox", { name: "Function engine", exact: true }).selectOption("sql");
  const source = dialog.getByRole("textbox", { name: "Function source", exact: true });
  await source.fill("current_setting('threads')");
  await dialog.getByRole("button", { name: "Run tests", exact: true }).click();
  await expect(dialog.getByRole("status")).toContainText("not available");
  await source.fill("(CAST(revenue AS DOUBLE)-CAST(cost AS DOUBLE))/CAST(revenue AS DOUBLE)");
  await dialog.getByRole("button", { name: "Publish version", exact: true }).click();
  await expect(dialog.getByRole("button", { name: "MARGIN · v1", exact: true })).toBeVisible();
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
