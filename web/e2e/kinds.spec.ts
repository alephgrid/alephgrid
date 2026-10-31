// 2.06: Tables and Sheets are labelled, and copying data into a sheet states its limits.

import { expect, test } from "@playwright/test";
import { Server } from "./server";

const server = new Server();
test.beforeAll(async () => server.start());
test.afterAll(async () => server.dispose());

test("tables and sheets are labelled and copies report what was left out", async ({ page }) => {
  const wb = (await server.api<{ id: string }>("/api/workbooks", {
    method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ name: "Kinds" }),
  })).id;
  let csv = "i,label\n";
  for (let i = 0; i < 250; i++) csv += `${i},r${i}\n`;
  await server.api(`/api/workbooks/${wb}/import?filename=rows.csv`, { method: "POST", body: csv });
  // The e2e server copies at most 1,000 rows into a sheet (--sheet-copy-rows 1000).
  await page.goto(`${server.url}/w/${wb}`);
  const badge = page.locator(".kind-badge");
  await expect(badge).toHaveText("Table");
  await expect(badge).toHaveAttribute("title", /any size/);

  // Table → sheet from the tab menu.
  await page.getByRole("tab", { name: /rows/ }).click({ button: "right" });
  await page.getByRole("menuitem", { name: "Copy to a sheet…" }).click();
  const dialog = page.getByRole("dialog", { name: "Copy rows to a sheet" });
  await expect(dialog).toContainText("Copies the first 250 of 250 rows");
  await dialog.getByRole("button", { name: "Copy to sheet" }).click();
  await expect(page.locator(".toast-success")).toContainText("Copied 250 rows of rows into sheet rows");
  await expect(badge).toHaveText("Sheet");
  await expect(badge).toHaveAttribute("title", /at most 1,000 rows/);

  // Query → sheet over the limit says so up front, and asks before leaving rows out.
  await page.getByRole("button", { name: "SQL" }).click();
  const sql = page.getByRole("region", { name: "SQL" });
  await sql.locator("textarea").fill("SELECT i FROM range(1500) r(i)");
  await sql.getByRole("button", { name: "Open in sheet" }).click();
  const name = page.getByRole("dialog", { name: "Open result in a new sheet" });
  await expect(name).toContainText("at most the first 1,000 rows");
  await name.getByRole("button", { name: "Create sheet" }).click();
  const confirm = page.getByRole("alertdialog", { name: "Copy only the first 1,000 rows?" });
  await confirm.getByRole("button", { name: "Copy first 1,000 rows" }).click();
  await expect(page.locator(".toast").last()).toContainText("Copied the first 1,000 rows of the query result", { timeout: 60_000 });
});
