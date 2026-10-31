// Phase 7 compute (7.13–7.16; 7.T03) in the browser: opening a workbook while the server is
// full waits in line and opens by itself; a refused start says what to do; an idle workbook
// pauses, says what is kept, and resumes on a key press; a controlled resize tells the other
// collaborator and both keep working.

import { expect, test, type Page } from "@playwright/test";
import { Server, type WsClient } from "./server";

const json = (body: unknown) => ({ method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) });
const put = (body: unknown) => ({ method: "PUT", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) });

async function stopped(server: Server, name: string): Promise<string> {
  const id = await server.createWorkbook(name);
  await server.api(`/api/workbooks/${id}/runtime`, json({ action: "stop" }));
  return id;
}

async function spaceOf(server: Server, id: string): Promise<string> {
  const list = await server.api<{ id: string; space: string }[]>("/api/workbooks");
  return list.find((w) => w.id === id)!.space;
}

async function runtime(server: Server, id: string): Promise<any> {
  return server.api(`/api/workbooks/${id}/runtime`);
}

test.describe("a full server", () => {
  const server = new Server(["--memory-budget", "512MiB", "--min-workbook-memory", "256MiB", "--start-queue-timeout", "60"]);
  test.beforeAll(async () => server.start());
  test.afterAll(async () => server.dispose());

  test("opening a workbook waits in line and opens by itself when there is room", async ({ page }) => {
    const [a, b, c] = [await stopped(server, "A"), await stopped(server, "B"), await stopped(server, "C")];
    // Both places are taken by workbooks someone has open.
    const held: WsClient[] = [await server.connect(a), await server.connect(b)];
    await page.goto(`${server.url}/w/${c}`);
    const waiting = page.getByRole("status").filter({ hasText: "Waiting for room on the server" });
    await expect(waiting).toBeVisible();
    await expect(waiting).toContainText("This workbook is next");
    expect((await runtime(server, c)).queue_position).toBe(1);
    // Someone closes A: C takes its place without the page doing anything.
    held[0].close();
    await expect(page.locator(".status-dot.status-open")).toBeVisible({ timeout: 30_000 });
    await expect(waiting).toHaveCount(0);
    expect((await runtime(server, c)).allocation.state).toBe("ready");
    expect((await runtime(server, a)).allocation.state).toBe("stopped");
    held[1].close();
  });

  test("a start refused by the space's limit says what to do, and Try again works", async ({ page }) => {
    // Nothing else of the space open (the previous journey left C running).
    for (const w of await server.api<{ id: string }[]>("/api/workbooks")) await server.api(`/api/workbooks/${w.id}/runtime`, json({ action: "stop" }));
    const [a, b] = [await stopped(server, "Q1"), await stopped(server, "Q2")];
    const space = await spaceOf(server, a);
    await server.api(`/api/spaces/${space}/quota`, put({ open_workbooks: 1 }));
    try {
      const held = await server.connect(a);
      await page.goto(`${server.url}/w/${b}`);
      const failure = page.getByRole("alert").filter({ hasText: "This space reached its compute limit" });
      await expect(failure).toBeVisible();
      await expect(failure).toContainText("ask an organization admin");
      held.close();
      await expect.poll(async () => (await runtime(server, a)).sessions ?? 0).toBe(0);
      await server.api(`/api/workbooks/${a}/runtime`, json({ action: "stop" }));
      await failure.getByRole("button", { name: "Try again" }).click();
      await expect(page.locator(".status-dot.status-open")).toBeVisible();
    } finally {
      await server.api(`/api/spaces/${space}/quota`, put({}));
    }
  });
});

test.describe("idle shutdown", () => {
  const server = new Server(["--idle-timeout", "2"]);
  test.beforeAll(async () => server.start());
  test.afterAll(async () => server.dispose());

  test("an idle workbook pauses, says what is kept, and resumes when the person continues", async ({ page }) => {
    const wb = await server.createWorkbook("Idle");
    await server.api(`/api/workbooks/${wb}/import?filename=t.csv`, { method: "POST", body: "id,name\n1,kept\n" });
    await page.goto(`${server.url}/w/${wb}`);
    await expect(page.getByRole("grid", { name: "Table t" })).toBeVisible();
    const epoch = (await runtime(server, wb)).allocation.epoch;
    // The tab stays open and does nothing: its compute is released anyway.
    const paused = page.getByRole("alert").filter({ hasText: "This workbook is paused" });
    await expect(paused).toBeVisible({ timeout: 20_000 });
    await expect(paused).toContainText("reserves nothing on the server");
    await expect(paused).toContainText("Everything is saved");
    await expect(page.locator(".status-dot.status-paused")).toBeVisible();
    expect((await runtime(server, wb)).allocation.state).toBe("stopped");
    // It does not reconnect by itself.
    await page.waitForTimeout(1500);
    expect((await runtime(server, wb)).allocation.state).toBe("stopped");
    // A key press resumes it, with the data as it was.
    await page.keyboard.press("ArrowDown");
    await expect(page.locator(".status-dot.status-open")).toBeVisible();
    await expect(paused).toBeHidden();
    const s = await runtime(server, wb);
    expect(s.allocation.epoch).toBe(epoch + 1);
    await page.locator(".grid-host").focus();
    await page.keyboard.press("ControlOrMeta+Home");
    await page.keyboard.press("ArrowRight");
    await expect(page.locator(".formula-input")).toHaveValue("kept");
  });
});

test.describe("profiles", () => {
  const server = new Server(["--profile", "large=512MiB:1"]);
  test.beforeAll(async () => server.start());
  test.afterAll(async () => server.dispose());

  async function open(page: Page, wb: string): Promise<void> {
    await page.goto(`${server.url}/w/${wb}`);
    await expect(page.getByRole("grid", { name: "Table t" })).toBeVisible();
  }

  test("a controlled resize tells the other collaborator, and both keep working", async ({ browser }) => {
    const wb = await server.createWorkbook("Resize");
    await server.api(`/api/workbooks/${wb}/import?filename=t.csv`, { method: "POST", body: "id,name\n1,a\n2,b\n" });
    await server.api(`/api/spaces/${await spaceOf(server, wb)}/profiles`, put({ profiles: ["large"] }));
    const a = await (await browser.newContext()).newPage();
    const b = await (await browser.newContext()).newPage();
    await open(a, wb);
    await open(b, wb);
    const before = await runtime(server, wb);

    await a.getByRole("button", { name: "Compute" }).click();
    const dialog = a.getByRole("dialog", { name: "Compute" });
    await expect(dialog).toContainText("Running (2 connected)");
    await expect(dialog).toContainText("10 minutes without activity; open tabs alone do not keep it running");
    await dialog.getByRole("radio", { name: /Large/ }).check();
    await dialog.getByRole("button", { name: "Restart with this profile" }).click();
    await expect(a.locator(".toast-success").filter({ hasText: "Running with the Large profile" })).toBeVisible();
    await expect(dialog).toContainText("Large: 512 MB and 1 thread");
    await dialog.getByRole("button", { name: "Close" }).click();

    // The other collaborator is told, and is back by itself.
    await expect(b.locator(".toast").filter({ hasText: "restarting with a new size" })).toBeVisible();
    await expect(b.locator(".toast-success").filter({ hasText: "Back, running with the Large profile" })).toBeVisible();
    const after = await runtime(server, wb);
    expect(after.allocation.profile).toBe("large");
    expect(after.allocation.epoch).toBe(before.allocation.epoch + 1);
    expect(after.spec_id).toBe(before.spec_id);
    expect(b.url()).toBe(`${server.url}/w/${wb}`);

    // Both keep working on the same workbook.
    await a.locator(".grid-host").focus();
    await a.keyboard.press("ControlOrMeta+Home");
    await a.keyboard.press("ArrowRight");
    await a.keyboard.type("after resize");
    await a.keyboard.press("Enter");
    await b.locator(".grid-host").focus();
    await expect.poll(async () => {
      await b.keyboard.press("ControlOrMeta+Home");
      await b.keyboard.press("ArrowRight");
      return b.locator(".formula-input").inputValue();
    }).toBe("after resize");
  });
});
