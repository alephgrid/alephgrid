// M.07 in the browser: an owner creates a read-only agent connection for one workbook,
// copies the token with its setup instructions, an MCP client uses it, the activity is
// listed, and revoking ends access.

import AxeBuilder from "@axe-core/playwright";
import { expect, test } from "@playwright/test";
import { Server } from "./server";

const server = new Server();
test.beforeAll(async () => server.start());
test.afterAll(async () => server.dispose());

test("M.07: create an agent connection, see what the agent did, revoke it", async ({ page }) => {
  const wb = await server.createWorkbook("Agent book"), ws = await server.connect(wb);
  await ws.call("create_table_from_query", { name: "t", sql: "SELECT i AS n FROM range(3) r(i)" });
  await server.createWorkbook("Not shared");
  await page.addInitScript(() => { try { localStorage.setItem("alephgrid-welcome-done", "1"); } catch { /* */ } });
  await page.goto(server.url);
  const general = page.getByRole("navigation", { name: "Spaces" }).getByRole("button", { name: /^General/ }).first();
  await general.click({ button: "right" });
  await page.getByRole("menuitem", { name: /^Agent connections/ }).click();
  const dialog = page.getByRole("dialog", { name: "Agent connections · General" });
  await expect(dialog).toContainText("model provider");
  await expect(dialog).toContainText("No agent connections yet.");
  await dialog.getByRole("textbox", { name: "Connection name" }).fill("Laptop agent");
  await dialog.getByRole("combobox", { name: "Workbooks it may read" }).selectOption("some");
  await dialog.getByRole("checkbox", { name: "Agent book" }).check();
  await dialog.getByRole("button", { name: "Create connection" }).click();
  const token = await dialog.getByRole("textbox", { name: "Token", exact: true }).inputValue();
  expect(token).toMatch(/^agc_/);
  await expect(dialog.getByRole("textbox", { name: "Claude Code" })).toHaveValue(new RegExp(`claude mcp add --transport http alephgrid ${server.url}/mcp`));
  await expect(dialog.getByRole("textbox", { name: /stdio clients/ })).toHaveValue(/"ALEPHGRID_AGENT_TOKEN"/);
  await expect(dialog.getByRole("row")).toContainText("reads Agent book");
  const axe = await new AxeBuilder({ page }).include(".agents-dialog").analyze();
  expect(axe.violations.filter((v) => v.impact === "serious" || v.impact === "critical").map((v) => v.id)).toEqual([]);

  // An MCP client uses it: only the shared workbook is there.
  const call = async (name: string, args: unknown) => {
    const r = await fetch(`${server.url}/mcp`, { method: "POST", headers: { "Content-Type": "application/json", Authorization: `Bearer ${token}` },
      body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "tools/call", params: { name, arguments: args } }) });
    return { status: r.status, body: r.status === 200 ? await r.json() : null };
  };
  const list = await call("list_workbooks", {});
  expect(list.body.result.structuredContent.workbooks.map((w: { name: string }) => w.name)).toEqual(["Agent book"]);
  const q = await call("run_query", { workbook: wb, sql: "SELECT sum(n) AS s FROM t" });
  expect(q.body.result.structuredContent.rows).toEqual([[3]]);

  // The activity shows the query; revoking ends access.
  await dialog.getByRole("button", { name: "Activity of Laptop agent" }).click();
  const activity = page.getByRole("dialog", { name: "Activity of “Laptop agent”" });
  await expect(activity.getByRole("table", { name: "Tool calls" })).toContainText("SELECT sum(n) AS s FROM t");
  await expect(activity).toContainText("1 rows");
  await activity.getByRole("button", { name: "Close" }).click();
  await dialog.getByRole("button", { name: "Revoke Laptop agent" }).click();
  await page.getByRole("alertdialog").getByRole("button", { name: "Revoke" }).click();
  await expect(dialog.getByRole("row")).toContainText("revoked");
  expect((await call("list_workbooks", {})).status).toBe(401);
  ws.close();
});
