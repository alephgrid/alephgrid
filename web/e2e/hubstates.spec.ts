// U.03b: the hub's loading placeholder, failure with Try again, the not-found screen for a
// workbook that is gone or out of reach, and the list state (search, sort, scope) kept.

import { expect, test, type Page } from "@playwright/test";
import { Server } from "./server";

const server = new Server();
test.beforeAll(async () => server.start());
test.afterAll(async () => server.dispose());

test.beforeEach(async ({ page }) => {
  await page.addInitScript(() => { try { localStorage.setItem("alephgrid-welcome-done", "1"); } catch { /* */ } });
});

const names = (page: Page) => page.locator(".wb-row .wb-name strong");

test("U.03b: the hub shows a placeholder while loading, a failure with Try again, and keeps search, sort and scope", async ({ page }) => {
  for (const n of ["Banana", "Apricot", "Apple"]) await server.createWorkbook(n);

  // Hold the first list: a placeholder says the workbooks are loading.
  let release!: () => void;
  const held = new Promise<void>((r) => (release = r));
  let first = true;
  await page.route("**/api/workbooks", async (route) => {
    if (route.request().method() === "GET" && first) { first = false; await held; }
    await route.continue();
  });
  await page.goto(server.url);
  const loading = page.getByRole("status", { name: "Loading workbooks" });
  await expect(loading).toBeVisible();
  await expect(loading.locator(".skeleton-row").first()).toBeVisible();
  release();
  await expect(loading).toHaveCount(0);
  await expect(names(page)).toHaveCount(3);
  await page.unroute("**/api/workbooks");

  // Search, sort, then open a workbook.
  await page.getByRole("combobox", { name: "Sort by" }).selectOption("name");
  await page.getByRole("searchbox", { name: "Search workbooks" }).fill("ap");
  await expect(names(page)).toHaveText(["Apple", "Apricot"]);
  await page.locator("a.wb-name", { hasText: "Apricot" }).click();
  await expect(page.locator(".status-dot.status-open")).toBeVisible({ timeout: 60_000 });

  // Coming back, the list fails to load: the reason and Try again, nothing else lost.
  await page.route("**/api/workbooks", (route) => route.request().method() === "GET"
    ? route.fulfill({ status: 500, contentType: "application/json", body: JSON.stringify({ error: "The server's disk is busy" }) })
    : route.continue());
  await page.getByRole("button", { name: "Alephgrid — all workbooks" }).click();
  const alert = page.getByRole("alert");
  await expect(alert.getByRole("heading", { name: "Workbooks couldn't be loaded" })).toBeVisible();
  await expect(alert).toContainText("The server's disk is busy");
  await page.unroute("**/api/workbooks");
  await alert.getByRole("button", { name: "Try again" }).click();
  await expect(page.getByRole("searchbox", { name: "Search workbooks" })).toHaveValue("ap");
  await expect(page.getByRole("combobox", { name: "Sort by" })).toHaveValue("name");
  await expect(names(page)).toHaveText(["Apple", "Apricot"]);
});

test("U.03b: a workbook that is gone opens a not-found screen with the way back, not a retry", async ({ page }) => {
  const gone = await server.createWorkbook("Soon gone");
  await page.goto(server.url);
  await page.getByRole("searchbox", { name: "Search workbooks" }).fill("soon");
  await expect(names(page)).toHaveText(["Soon gone"]);
  // Deleted elsewhere while the list is on screen.
  await server.api(`/api/workbooks/${gone}`, { method: "DELETE" });
  await page.locator("a.wb-name", { hasText: "Soon gone" }).click();
  const screen = page.getByRole("alert");
  await expect(screen.getByRole("heading", { name: "Workbook not found" })).toBeVisible();
  await expect(screen).toContainText("deleted");
  await expect(screen.getByRole("button", { name: "Try again" })).toHaveCount(0);
  await screen.getByRole("button", { name: "Back to workbooks" }).click();
  await expect(page.getByRole("searchbox", { name: "Search workbooks" })).toHaveValue("soon");
  await expect(page.locator(".wb-empty")).toContainText("No workbooks match “soon”.");

  // A link to a workbook that never existed says the same.
  await page.goto(`${server.url}/w/nosuchworkbook`);
  await expect(page.getByRole("alert").getByRole("heading", { name: "Workbook not found" })).toBeVisible();
});

// Its own server with sign-in, the admin created from the environment.
const ADMIN = { email: "admin@example.com", password: "correct horse battery" };
const accounts = new Server(["--auth", "accounts"], { ALEPHGRID_BOOTSTRAP_EMAIL: ADMIN.email, ALEPHGRID_BOOTSTRAP_PASSWORD: ADMIN.password });
test.afterAll(async () => accounts.dispose());

test("U.03b: a workbook in a space someone is not in opens the not-found screen, revealing nothing", async ({ page, browser }) => {
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
  const secret = await json("/api/spaces", "POST", { name: "Private", org: null });
  const workbook = (await json("/api/workbooks", "POST", { name: "Salaries", space: secret.id })).id;
  const { code } = await json(`/api/orgs/${me.orgs[0].org.id}/invites`, "POST",
    { email: "eve@example.com", org_role: "member", space: general, space_role: "editor", days: 7 });

  // Eve joins General only, then follows a link to the workbook in Private.
  const eve = await (await browser.newContext()).newPage();
  await eve.goto(`${accounts.url}/login`);
  await eve.evaluate(async (code) => {
    const r = await fetch("/api/invites/accept", { method: "POST", headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ code, name: "Eve", password: "a long enough password" }) });
    if (!r.ok) throw new Error(await r.text());
  }, code);
  await eve.goto(`${accounts.url}/w/${workbook}`);
  const screen = eve.getByRole("alert");
  await expect(screen.getByRole("heading", { name: "Workbook not found" })).toBeVisible();
  await expect(screen).not.toContainText("Salaries");
  await expect(screen.getByRole("button", { name: "Try again" })).toHaveCount(0);
  await screen.getByRole("button", { name: "Back to workbooks" }).click();
  await expect(eve.getByRole("navigation", { name: "Spaces" })).toContainText("General");
  await expect(eve.getByRole("navigation", { name: "Spaces" })).not.toContainText("Private");
  await eve.context().close();
});
