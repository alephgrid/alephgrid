// SEC.06 / SEC.03 / SEC.T01: ids from the server never retarget a request (a workbook whose
// id is `../trash` cannot turn Delete into "empty the trash"), and every page carries the
// browser-boundary headers.

import { expect, test } from "@playwright/test";
import { Server } from "./server";

const server = new Server();
test.beforeAll(async () => server.start());
test.afterAll(async () => server.dispose());

test("a hostile id in a listing is dropped, and no request reaches /api/trash", async ({ page }) => {
  const real = await server.createWorkbook("Real one");
  // A trashed workbook that must survive.
  const kept = await server.createWorkbook("Kept in trash");
  await server.api(`/api/workbooks/${kept}`, { method: "DELETE" });

  const requests: string[] = [];
  page.on("request", (r) => requests.push(`${r.method()} ${new URL(r.url()).pathname}`));
  // The listing as a crafted data directory or archive could produce it.
  await page.route("**/api/workbooks", async (route) => {
    if (route.request().method() !== "GET") return route.continue();
    const res = await route.fetch();
    const list = await res.json();
    const evil = { ...list[0], id: "../trash", name: "Evil" };
    const dots = { ...list[0], id: "%2e%2e", name: "Dots" };
    await route.fulfill({ response: res, json: [...list, evil, dots] });
  });
  await page.goto(server.url);
  await expect(page.locator(".wb-row", { hasText: "Real one" })).toBeVisible();
  await expect(page.locator(".wb-row", { hasText: "Evil" })).toHaveCount(0);
  await expect(page.locator(".wb-row", { hasText: "Dots" })).toHaveCount(0);

  // Deleting the real workbook sends exactly its own request.
  await page.locator(".wb-row", { hasText: "Real one" }).click({ button: "right" });
  await page.getByRole("menuitem", { name: "Delete" }).click();
  const confirm = page.getByRole("dialog");
  if (await confirm.isVisible().catch(() => false)) await confirm.getByRole("button", { name: /Delete|Move/ }).click();
  await expect.poll(() => requests.filter((r) => r.startsWith("DELETE"))).toEqual([`DELETE /api/workbooks/${real}`]);
  expect(requests.some((r) => r.includes("/api/trash") && r.startsWith("DELETE"))).toBe(false);
  const trash = await (await fetch(`${server.url}/api/trash`)).json();
  expect(trash.map((w: { id: string }) => w.id)).toContain(kept);

  // A dot-segment workbook URL opens nothing and sends no workbook request.
  requests.length = 0;
  await page.goto(`${server.url}/w/..%2Ftrash`);
  await page.waitForLoadState("networkidle");
  expect(requests.filter((r) => r.includes("/api/workbooks/") || r.includes("/api/trash/") || r.startsWith("DELETE"))).toEqual([]);
});

test("pages and API replies carry the browser-boundary headers", async ({ request }) => {
  for (const path of ["/", "/api/config", "/api/workbooks/nope"]) {
    const res = await request.get(`${server.url}${path}`);
    const h = res.headers();
    expect(h["content-security-policy"], path).toContain("frame-ancestors 'none'");
    expect(h["content-security-policy"], path).toContain("script-src 'self'");
    expect(h["referrer-policy"], path).toBe("no-referrer");
    expect(h["x-content-type-options"], path).toBe("nosniff");
  }
});

test("the app runs under its content security policy without violations", async ({ page }) => {
  const violations: string[] = [];
  page.on("console", (m) => { if (/Content Security Policy|Refused to/i.test(m.text())) violations.push(m.text()); });
  const wb = await server.createWorkbook("CSP");
  await server.api(`/api/workbooks/${wb}/import?filename=t.csv`, { method: "POST", body: "a,b\n1,2\n" });
  await page.goto(`${server.url}/w/${wb}`);
  await expect(page.locator(".status-dot.status-open")).toBeVisible();
  await expect(page.getByRole("tab", { name: /t/ }).first()).toBeVisible();
  expect(violations).toEqual([]);
});
