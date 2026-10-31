// Phase 7 in the browser (7.01–7.05, 7.T01): set up the first admin with the console's setup
// code, sign in, make a space, invite someone into it as a viewer, see that the server keeps
// them to viewing, take their access away while their workbook is open, and sign out.
// Server-side results are checked through the API as the people themselves.

import { expect, test, type Browser, type Page } from "@playwright/test";
import { Server } from "./server";

const server = new Server(["--auth", "accounts"]);
test.beforeAll(async () => server.start());
test.afterAll(async () => server.dispose());

const ADMIN = { email: "admin@example.com", password: "correct horse battery" };
const VIEWER = { email: "vera@example.com", password: "a long enough password" };

async function signIn(page: Page, who: { email: string; password: string }): Promise<void> {
  await page.getByLabel("Email").fill(who.email);
  await page.getByLabel("Password").fill(who.password);
  await page.getByRole("button", { name: "Sign in", exact: true }).click();
}

async function fresh(browser: Browser): Promise<Page> {
  const context = await browser.newContext();
  return context.newPage();
}

/** The workbooks a page's own session can list (as the server decides). */
async function listed(page: Page): Promise<string[]> {
  return page.evaluate(async () => {
    const r = await fetch("/api/workbooks");
    return r.ok ? (await r.json()).map((w: { name: string }) => w.name) : [`HTTP ${r.status}`];
  });
}

test("setup, sign-in, spaces, invitation, viewer limits, revocation and sign-out", async ({ page, browser }) => {
  // Anything but setup sends a new server to it.
  await page.goto(server.url);
  await expect(page).toHaveURL(/\/setup$/);
  const code = /setup code (\S+)/.exec(server.output)?.[1];
  expect(code, server.output).toBeTruthy();
  await page.getByLabel("Setup code").fill("wrong");
  await page.getByLabel("Your name").fill("Ada Admin");
  await page.getByLabel("Email").fill(ADMIN.email);
  await page.getByLabel("Password").fill(ADMIN.password);
  await page.getByRole("button", { name: "Create administrator" }).click();
  await expect(page.getByRole("alert")).toContainText("setup code is wrong");
  await page.getByLabel("Setup code").fill(code!);
  await page.getByRole("button", { name: "Create administrator" }).click();
  await expect(page.getByRole("navigation", { name: "Spaces" })).toContainText("General");

  // A new space and a workbook in it.
  await page.getByRole("button", { name: "New space" }).click();
  await page.getByLabel("Name").fill("Finance");
  await page.getByRole("button", { name: "Create" }).click();
  await expect(page.getByRole("heading", { name: "Finance" })).toBeVisible();
  await page.getByRole("button", { name: "New workbook" }).click();
  await page.getByLabel("Name").fill("Budget");
  await page.getByRole("button", { name: "Create" }).click();
  await expect(page).toHaveURL(/\/w\//);
  const workbook = new URL(page.url()).pathname.split("/")[2];
  await page.goBack();
  await expect(page.locator("a.wb-name", { hasText: "Budget" })).toBeVisible();

  // Invite Vera into Finance as a viewer.
  await page.getByRole("button", { name: "Ada Admin" }).click();
  await page.getByRole("menuitem", { name: "People…" }).click();
  await page.getByLabel("Email to invite").fill(VIEWER.email);
  await page.getByLabel("Space", { exact: true }).selectOption({ label: "Finance" });
  await page.getByLabel("Space role").selectOption("viewer");
  await page.getByRole("button", { name: "Invite" }).click();
  const link = await page.getByLabel("Invitation link").inputValue();
  expect(link).toMatch(/\/invite#[0-9a-f]+\.[A-Za-z0-9_-]+$/);
  await page.getByRole("button", { name: "Close" }).click();

  // Vera accepts in her own browser.
  const vera = await fresh(browser);
  await vera.goto(link);
  await expect(vera.getByText(`${VIEWER.email} is invited`)).toContainText("Finance");
  await vera.getByLabel("Your name").fill("Vera");
  await vera.getByLabel("Choose a password").fill(VIEWER.password);
  await vera.getByRole("button", { name: "Create account and accept" }).click();
  await expect(vera.getByRole("navigation", { name: "Spaces" })).toContainText("Finance");
  await expect(vera.getByRole("navigation", { name: "Spaces" })).not.toContainText("General");
  expect(await listed(vera)).toEqual(["Budget"]);
  // She is offered no changes, and the server refuses them anyway.
  await expect(vera.getByRole("button", { name: "New workbook" })).toHaveCount(0);
  const refused = await vera.evaluate(async (id) => (await fetch(`/api/workbooks/${id}`, {
    method: "PATCH", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ name: "Mine now" }),
  })).status, workbook);
  expect(refused).toBe(403);
  await vera.locator("a.wb-name", { hasText: "Budget" }).click();
  await expect(vera).toHaveURL(new RegExp(`/w/${workbook}`));
  await expect(vera.locator(".status-dot.status-open")).toBeVisible();

  // The admin removes her from the space: her open workbook leaves, and the list empties.
  await page.getByRole("navigation", { name: "Spaces" }).getByRole("button", { name: /^Finance/ }).first().click({ button: "right" });
  await page.getByRole("menuitem", { name: "Members…" }).click();
  await page.getByRole("button", { name: "Remove Vera <vera@example.com>" }).click();
  await expect(page.getByRole("button", { name: "Remove Vera <vera@example.com>" })).toHaveCount(0);
  await expect(page.getByRole("table", { name: "Members" })).toContainText("Ada Admin <admin@example.com>");
  await page.getByRole("button", { name: "Close" }).click();
  await expect(vera).toHaveURL(new RegExp(`${server.url}/$`), { timeout: 15_000 });
  expect(await listed(vera)).toEqual([]);
  await expect(vera.getByRole("navigation", { name: "Spaces" })).not.toContainText("Finance");

  // Signing out ends the session for the server too.
  await page.getByRole("button", { name: "Ada Admin" }).click();
  await page.getByRole("menuitem", { name: "Sign out" }).click();
  await expect(page).toHaveURL(/\/login/);
  expect(await listed(page)).toEqual(["HTTP 401"]);
  await signIn(page, { email: ADMIN.email, password: "not the password" });
  await expect(page.getByRole("alert")).toContainText("email or password is wrong");
  await signIn(page, ADMIN);
  await expect(page.locator("a.wb-name", { hasText: "Budget" })).toBeVisible();
  await vera.context().close();
});
