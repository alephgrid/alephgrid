// U.06a–d / U.T06: the functions side panel keeps the grid visible, resizes by keyboard and
// remembers its width, keeps its draft while closed, shares the right side with Analyze,
// runs tests only when asked (Mod+Enter in the panel) and marks results stale after edits.

import AxeBuilder from "@axe-core/playwright";
import { expect, test } from "@playwright/test";
import { Server } from "./server";

const server = new Server();
test.beforeAll(async () => server.start());
test.afterAll(async () => server.dispose());

test("the functions panel docks beside the grid, resizes, keeps its draft and runs only on request", async ({ page }) => {
  const wb = await server.createWorkbook("Function panel");
  const ops: string[] = [];
  await page.routeWebSocket(/\/ws\//, (ws) => {
    const srv = ws.connectToServer();
    ws.onMessage((m) => { try { ops.push(JSON.parse(String(m)).op); } catch { /* */ } srv.send(m); });
    srv.onMessage((m) => ws.send(m));
  });
  await page.addInitScript(() => { try { localStorage.setItem("alephgrid-welcome-done", "1"); } catch { /* */ } });
  await page.goto(`${server.url}/w/${wb}`);
  await expect(page.locator(".status-dot.status-open")).toBeVisible();
  await page.getByRole("button", { name: "Functions", exact: true }).click();
  const panel = page.getByRole("complementary", { name: "Functions" });
  await expect(panel).toBeVisible();
  await expect(page.locator(".grid-host")).toBeVisible();
  expect((await new AxeBuilder({ page }).include(".fn-panel").analyze()).violations.filter((v) => v.impact === "serious" || v.impact === "critical")).toEqual([]);

  // Opening the panel, or an example, never runs code.
  await panel.getByRole("button", { name: "Start from an example" }).click();
  expect(ops.filter((o) => o === "test_function")).toEqual([]);

  // Resize with the keyboard; the width is remembered after a reload.
  const width = async () => (await panel.boundingBox())!.width;
  const before = await width();
  const divider = panel.getByRole("separator", { name: "Resize functions panel" });
  await divider.focus();
  await page.keyboard.press("ArrowLeft"); await page.keyboard.press("ArrowLeft");
  await expect.poll(width).toBeGreaterThan(before + 40);
  const wider = await width();

  // Mod+Enter in the panel runs the tests.
  await panel.getByRole("textbox", { name: "Function source" }).focus();
  await page.keyboard.press("ControlOrMeta+Enter");
  await expect(panel.getByRole("region", { name: "Test results" })).toContainText("Passed · test 1 · actual 0.4");
  await expect(panel.locator(".fn-state")).toHaveText("Tested · ready to publish");
  // An edit makes those results stale.
  await panel.getByRole("textbox", { name: "Function source" }).fill("revenue-cost");
  await expect(panel.getByRole("region", { name: "Test results" })).toContainText("Edited since this run");
  await expect(panel.locator(".fn-state")).toHaveText("Draft");

  // Opening Analyze closes the panel; reopening it brings the draft back.
  await page.getByRole("button", { name: "Analyze", exact: true }).click();
  await expect(page.getByRole("complementary", { name: "Analyze" })).toBeVisible();
  await expect(panel).toHaveCount(0);
  await page.getByRole("button", { name: "Functions", exact: true }).click();
  await expect(page.getByRole("complementary", { name: "Analyze" })).toHaveCount(0);
  await expect(panel.getByRole("textbox", { name: "Function source" })).toHaveValue("revenue-cost");
  expect(ops.filter((o) => o === "test_function").length).toBe(1);

  // Escape closes the panel (outside the code editor) and nothing was published.
  await panel.getByRole("tab", { name: "Docs" }).focus();
  await page.keyboard.press("Escape");
  await expect(panel).toHaveCount(0);
  expect(ops).not.toContain("publish_function");

  await page.reload();
  await page.getByRole("button", { name: "Functions", exact: true }).click();
  await expect.poll(width).toBeGreaterThan(wider - 2);
});

async function openPanel(page: import("@playwright/test").Page, wb: string) {
  await page.addInitScript(() => { try { localStorage.setItem("alephgrid-welcome-done", "1"); } catch { /* */ } });
  await page.goto(`${server.url}/w/${wb}`);
  await expect(page.locator(".status-dot.status-open")).toBeVisible();
  await page.getByRole("button", { name: "Functions", exact: true }).click();
  return page.getByRole("complementary", { name: "Functions" });
}

/** Ops the page sends, with their frames. */
async function recordOps(page: import("@playwright/test").Page) {
  const frames: Record<string, any>[] = [];
  await page.routeWebSocket(/\/ws\//, (ws) => {
    const srv = ws.connectToServer();
    ws.onMessage((m) => { try { frames.push(JSON.parse(String(m))); } catch { /* */ } srv.send(m); });
    srv.onMessage((m) => ws.send(m));
  });
  return frames;
}

// U.06b: the code editor highlights from text nodes, shows problems by line, indents and has
// its own undo that never reaches the grid.
test("the code editor highlights, finds problems by line, indents and undoes locally", async ({ page }) => {
  const wb = await server.createWorkbook("Code editor");
  const frames = await recordOps(page);
  const panel = await openPanel(page, wb);
  await panel.getByRole("button", { name: "New", exact: true }).click();
  await panel.getByRole("combobox", { name: "Function engine" }).selectOption("python");
  await panel.getByRole("textbox", { name: "Function name" }).fill("TAG");
  await panel.getByRole("textbox", { name: "Arguments" }).fill("x");
  const source = panel.getByRole("textbox", { name: "Function source" });
  await source.fill('def tag(x):\n    s = "<img src=x onerror=alert(1)>"  # note\n    return (x\n');
  const highlight = panel.locator(".fn-highlight");
  await expect(highlight.locator(".tok-keyword").first()).toHaveText("def");
  await expect(highlight.locator(".tok-function").first()).toHaveText("tag");
  await expect(highlight.locator(".tok-string")).toHaveText('"<img src=x onerror=alert(1)>"');
  await expect(highlight.locator(".tok-comment")).toHaveText("# note");
  await expect(highlight.locator(".tok-name").first()).toHaveText("x");
  // Source is text, never markup.
  await expect(highlight.locator("img")).toHaveCount(0);
  // The unclosed bracket is a problem on line 3, in the list and the gutter.
  const problems = panel.getByRole("list", { name: "Problems" });
  await expect(problems).toContainText("Line 3: ( is not closed");
  await expect(panel.locator('.fn-gutter [data-line="3"]')).toHaveClass(/has-problem/);
  await expect(source).toHaveAttribute("aria-invalid", "true");
  // Choosing the problem selects its line.
  await problems.getByRole("button", { name: "Line 3: ( is not closed" }).click();
  await expect(source).toBeFocused();
  expect(await source.evaluate((t: HTMLTextAreaElement) => t.value.slice(t.selectionStart, t.selectionEnd))).toBe("    return (x");
  await source.fill("def tag(x):\n    return x\n");
  await expect(problems).toBeHidden();
  await expect(source).toHaveAttribute("aria-invalid", "false");

  // Enter keeps indentation, one level deeper after a colon; Shift+Tab outdents.
  await source.fill("");
  await source.pressSequentially("def tag(x):");
  await source.press("Enter");
  await expect(source).toHaveValue("def tag(x):\n    ");
  await source.pressSequentially("return x");
  await source.press("Enter");
  await expect(source).toHaveValue("def tag(x):\n    return x\n    ");
  await source.press("Shift+Tab");
  await expect(source).toHaveValue("def tag(x):\n    return x\n");

  // Undo and redo are the editor's own: Tab is a step, typing a run is a step.
  await source.fill("");
  await source.pressSequentially("abc");
  await source.press("Tab");
  await expect(source).toHaveValue("abc    ");
  await source.press("ControlOrMeta+z");
  await expect(source).toHaveValue("abc");
  await source.press("ControlOrMeta+z");
  await expect(source).toHaveValue("");
  await source.press("ControlOrMeta+Shift+z");
  await expect(source).toHaveValue("abc");
  expect(frames.map((f) => f.op)).not.toContain("sheet_undo");
  expect(frames.map((f) => f.op)).not.toContain("undo");

  // A formula's unclosed text is found too.
  await panel.getByRole("combobox", { name: "Function engine" }).selectOption("formula");
  await source.fill('IF(x>0,"pos,"neg")');
  await expect(problems).toContainText("Line 1: text is not closed");
  await expect(highlight.locator(".tok-function").first()).toHaveText("IF");
});

// U.06c: inputs and expected values can be a blank cell, exact text or an error value, and
// survive the JSON view and publication.
test("typed test values: blank, text and errors, through JSON and publication", async ({ page }) => {
  const wb = await server.createWorkbook("Typed tests");
  const frames = await recordOps(page);
  const panel = await openPanel(page, wb);
  await panel.getByRole("button", { name: "Start from an example" }).click();
  await panel.getByRole("tab", { name: "Tests" }).click();
  // Test 2: a blank revenue is 0 to the formula, so MARGIN is 0.
  await panel.getByRole("button", { name: "Add test" }).click();
  await panel.getByRole("combobox", { name: "Test 2 revenue type" }).selectOption("blank");
  await expect(panel.getByRole("textbox", { name: "Test 2 revenue" })).toBeDisabled();
  // Typing key by key keeps focus in the cell while the rows update.
  await panel.getByRole("textbox", { name: "Test 2 cost" }).pressSequentially("15");
  await expect(panel.getByRole("textbox", { name: "Test 2 cost" })).toBeFocused();
  await panel.getByRole("textbox", { name: "Test 2 expected" }).fill("0");
  // Test 3: an error input reaches the formula; IF passes #N/A on.
  await panel.getByRole("button", { name: "Add test" }).click();
  await panel.getByRole("combobox", { name: "Test 3 revenue type" }).selectOption("error");
  await panel.getByRole("combobox", { name: "Test 3 revenue", exact: true }).selectOption("#N/A");
  await panel.getByRole("textbox", { name: "Test 3 cost" }).fill("1");
  await panel.getByRole("combobox", { name: "Test 3 expected type" }).selectOption("error");
  await panel.getByRole("combobox", { name: "Test 3 expected", exact: true }).selectOption("#N/A");
  await panel.getByRole("button", { name: "Run tests", exact: true }).click();
  await expect(panel.getByRole("region", { name: "Test results" })).toContainText("Passed · test 3 · actual #N/A (error)");
  await expect(panel.locator(".fn-state")).toHaveText("Tested · ready to publish");
  const sent = frames.filter((f) => f.op === "test_function").pop()!;
  expect(sent.definition.tests[1].arguments[0]).toEqual({ type: "blank" });
  expect(sent.definition.tests[2]).toEqual({ arguments: [{ type: "error", value: "#N/A" }, "1"], expected: { type: "error", value: "#N/A" } });

  // The JSON view shows the same values; editing it changes the rows.
  await panel.getByText("Advanced: tests as JSON").click();
  const json = panel.getByRole("textbox", { name: "Function tests (JSON)" });
  expect(JSON.parse(await json.inputValue())[2].expected).toEqual({ type: "error", value: "#N/A" });
  const edited = JSON.parse(await json.inputValue());
  edited[2].expected = { type: "text", value: "#N/A" };
  await json.fill(JSON.stringify(edited));
  await panel.getByRole("button", { name: "Use this JSON" }).click();
  await expect(panel.getByRole("combobox", { name: "Test 3 expected type" })).toHaveValue("text");
  await expect(panel.getByRole("textbox", { name: "Test 3 expected" })).toHaveValue("#N/A");
  // Text "#N/A" is not the error #N/A.
  await panel.getByRole("button", { name: "Run tests", exact: true }).click();
  await expect(panel.getByRole("region", { name: "Test results" })).toContainText("Failed · test 3 · actual #N/A (error) · expected #N/A (text)");
  await panel.getByRole("combobox", { name: "Test 3 expected type" }).selectOption("error");
  await panel.getByRole("combobox", { name: "Test 3 expected", exact: true }).selectOption("#N/A");
  await panel.getByRole("button", { name: "Publish version", exact: true }).click();
  await expect(panel.locator(".fn-state")).toHaveText("Published · v1 · active");
  // Stored on the server exactly so.
  const ws = await server.connect(wb);
  const stored = (await ws.call("functions", {}))[0].versions[0].definition.tests;
  ws.close();
  expect(stored[1].arguments).toEqual([{ type: "blank" }, "15"]);
  expect(stored[2].expected).toEqual({ type: "error", value: "#N/A" });
});

// U.06d: the results strip resizes by keyboard and keeps its height.
test("the results strip resizes and keeps its height", async ({ page }) => {
  const wb = await server.createWorkbook("Results strip");
  const panel = await openPanel(page, wb);
  const strip = panel.getByRole("region", { name: "Test results" });
  const height = async () => (await strip.boundingBox())!.height;
  const before = await height();
  const handle = panel.getByRole("separator", { name: "Resize results" });
  await handle.focus();
  await page.keyboard.press("ArrowUp"); await page.keyboard.press("ArrowUp");
  await expect.poll(height).toBeGreaterThan(before + 30);
  const taller = await height();
  await page.reload();
  await page.getByRole("button", { name: "Functions", exact: true }).click();
  await expect.poll(height).toBeGreaterThan(taller - 2);
});

// U.06e: Activate shows what the version change affects, and changes nothing until confirmed.
test("activating a version shows its impact first", async ({ page }) => {
  const wb = await server.createWorkbook("Impact");
  const ws = await server.connect(wb);
  const def = (source: string, expected: string) => ({ name: "MARGIN", kind: "formula", arguments: ["revenue", "cost"], source, documentation: "Profit as a fraction of revenue.", tests: [{ arguments: ["100", "60"], expected }] });
  await ws.call("publish_function", { definition: def("IF(revenue=0,0,(revenue-cost)/revenue)", "0.4") });
  await ws.call("publish_function", { definition: def("revenue-cost", "40") });
  await ws.call("publish_function", { definition: { name: "DOUBLED", kind: "formula", arguments: ["a"], source: "MARGIN(a,0)*2", documentation: "", tests: [] } });
  await ws.call("sheet_set", { sheet: 0, inputs: [{ row: 0, column: 1, value: "=MARGIN(100,60)" }, { row: 3, column: 2, value: "=MARGIN(10,5)" }] });
  const panel = await openPanel(page, wb);
  await panel.getByRole("combobox", { name: "Function", exact: true }).selectOption("MARGIN");
  await panel.getByRole("tab", { name: "Versions" }).click();
  await panel.getByRole("button", { name: "Activate v2 in this workbook" }).click();
  const impact = panel.getByRole("group", { name: "Activate MARGIN v2" });
  await expect(impact).toContainText("Replaces v1");
  await expect(impact).toContainText("2 cells");
  await expect(impact).toContainText("Sheet1!B1");
  await expect(impact).toContainText("Sheet1!C4");
  await expect(impact).toContainText("DOUBLED v1 calls MARGIN v1");
  await expect(impact).toContainText("Other workbooks keep their own versions");
  // Nothing changed yet; Cancel leaves it so.
  expect((await ws.call("functions", {})).find((f: any) => f.name === "MARGIN").pinned).toBe(1);
  await impact.getByRole("button", { name: "Cancel" }).click();
  await expect(impact).toHaveCount(0);
  await panel.getByRole("button", { name: "Activate v2 in this workbook" }).click();
  await panel.getByRole("group", { name: "Activate MARGIN v2" }).getByRole("button", { name: "Activate v2", exact: true }).click();
  // DOUBLED still calls v1, so the server refuses: the panel says so and nothing changes.
  await expect(panel.getByRole("region", { name: "Test results" })).toContainText("dependen");
  expect((await ws.call("functions", {})).find((f: any) => f.name === "MARGIN").pinned).toBe(1);
  ws.close();
});

// U.06e: published functions in cell autocomplete with signature, description and an example.
test("cell autocomplete shows a published function's signature, help and example", async ({ page }) => {
  const wb = await server.createWorkbook("Autocomplete");
  const ws = await server.connect(wb);
  await ws.call("publish_function", { definition: { name: "MARGIN", kind: "formula", arguments: ["revenue", "cost"], source: "IF(revenue=0,0,(revenue-cost)/revenue)", documentation: "Profit as a fraction of revenue.", tests: [{ arguments: ["100", "60"], expected: "0.4" }] } });
  ws.close();
  await page.goto(`${server.url}/w/${wb}`);
  await expect(page.locator(".status-dot.status-open")).toBeVisible();
  await page.getByRole("tab", { name: /Sheet1/ }).click();
  const editor = page.getByRole("textbox", { name: "Cell value or formula", exact: true });
  await editor.fill("=MAR");
  const help = page.getByRole("region", { name: "Function suggestions" });
  await expect(help).toContainText("MARGIN(revenue, cost)");
  await editor.press("Tab");
  await expect(help).toContainText("Profit as a fraction of revenue.");
  await expect(help).toContainText("Example: =MARGIN(100, 60) → 0.4");
  // Calling a function never opens the panel.
  await expect(page.getByRole("complementary", { name: "Functions" })).toHaveCount(0);
});

// U.T06: both themes, three widths and 200% zoom: the panel fits, nothing scrolls sideways,
// and axe finds nothing serious.
for (const theme of ["light", "dark"]) {
  test(`the panel fits and passes axe at 1280/1440/1920 and 200% zoom (${theme})`, async ({ page }) => {
    const wb = await server.createWorkbook(`Fit ${theme}`);
    await page.addInitScript((t) => { try { localStorage.setItem("alephgrid-theme", t); } catch { /* */ } }, theme);
    for (const [w, h] of [[1280, 800], [1440, 900], [1920, 1080], [640, 400]]) {
      await page.setViewportSize({ width: w, height: h });
      const panel = await openPanel(page, wb);
      await expect(page.locator("html")).toHaveAttribute("data-theme", theme);
      if (!(await panel.getByRole("textbox", { name: "Function source" }).inputValue())) await panel.getByRole("button", { name: "Start from an example" }).click();
      const box = (await panel.boundingBox())!;
      expect(box.x + box.width).toBeLessThanOrEqual(w + 1);
      expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
      for (const tab of ["Code", "Tests", "Docs", "Versions"]) {
        await panel.getByRole("tab", { name: tab }).click();
        const body = panel.locator(`#fn-panel-${tab}`);
        expect(await body.evaluate((b) => b.scrollWidth <= b.clientWidth + 1), `${tab} at ${w}`).toBe(true);
      }
      await panel.getByRole("tab", { name: "Code" }).click();
      const serious = (await new AxeBuilder({ page }).include(".fn-panel").analyze()).violations.filter((v) => v.impact === "serious" || v.impact === "critical");
      expect(serious, `${theme} ${w}px`).toEqual([]);
      if (w > 760) await expect(page.locator(".grid-host")).toBeVisible();
    }
  });
}

// U.T06: the connection drops while a draft is open. The panel says so, keeps the draft
// (still editable), and Run tests / Publish wait for the connection with the reason; after
// the reconnect the same draft runs and publishes.
test("reconnect: the panel keeps its draft, says why it cannot run or publish, and recovers", async ({ page }) => {
  const wb = await server.createWorkbook("Function reconnect");
  // A switch for the network: while off, the socket is closed and new ones are refused.
  let offline = false;
  const live: import("@playwright/test").WebSocketRoute[] = [];
  const ops: string[] = [];
  await page.routeWebSocket(/\/ws\//, (ws) => {
    if (offline) { void ws.close(); return; }
    live.push(ws);
    const srv = ws.connectToServer();
    ws.onMessage((m) => { try { ops.push(JSON.parse(String(m)).op); } catch { /* */ } srv.send(m); });
    srv.onMessage((m) => ws.send(m));
  });
  const panel = await openPanel(page, wb);
  await panel.getByRole("button", { name: "Start from an example" }).click();
  const source = panel.getByRole("textbox", { name: "Function source" });
  await source.fill("IF(revenue=0,0,(revenue-cost)/revenue)*1");
  const run = panel.getByRole("button", { name: "Run tests", exact: true });
  const publish = panel.getByRole("button", { name: "Publish version", exact: true });
  await expect(run).toBeEnabled();

  offline = true;
  for (const ws of live.splice(0)) await ws.close();
  await expect(page.locator(".status-dot.status-closed")).toBeVisible();
  // The panel says what happened and what is kept; the actions say why they wait.
  await expect(panel.getByRole("status").filter({ hasText: "Connection lost" })).toContainText("draft is kept");
  for (const b of [run, publish]) {
    await expect(b).toBeDisabled();
    await expect(b).toHaveAccessibleDescription("Not connected: waiting for the server to reconnect");
  }
  // Mod+Enter does not run either, and the draft can still be edited.
  await source.focus();
  await page.keyboard.press("ControlOrMeta+Enter");
  await source.fill("IF(revenue=0,0,(revenue-cost)/revenue)");
  await expect(panel.locator(".fn-state")).toHaveText("Draft");
  expect(ops.filter((o) => o === "test_function" || o === "publish_function")).toEqual([]);

  offline = false;
  await expect(page.locator(".status-dot.status-open")).toBeVisible({ timeout: 30_000 });
  await expect(panel.getByRole("status").filter({ hasText: "Connection lost" })).toHaveCount(0);
  await expect(run).toBeEnabled();
  await expect(run).toHaveAccessibleDescription("Run tests (⌘↵ / Ctrl+Enter)");
  await expect(source).toHaveValue("IF(revenue=0,0,(revenue-cost)/revenue)");
  await run.click();
  await expect(panel.getByRole("region", { name: "Test results" })).toContainText("Passed · test 1 · actual 0.4");
  await publish.click();
  await expect(panel.locator(".fn-state")).toHaveText("Published · v1 · active");
  // The server has exactly the draft that was kept across the reconnect.
  const ws = await server.connect(wb);
  const fns = await ws.call("functions", {});
  ws.close();
  expect(fns.map((f: { name: string }) => f.name)).toEqual(["MARGIN"]);
  expect(fns[0].versions.map((v: { definition: { source: string } }) => v.definition.source)).toEqual(["IF(revenue=0,0,(revenue-cost)/revenue)"]);
});

// U.T06: a viewer reads the functions panel but cannot change, run or publish anything, and
// is told why. The server refuses a publish from them anyway.
const ADMIN = { email: "admin@example.com", password: "correct horse battery" };
const accounts = new Server(["--auth", "accounts"], { ALEPHGRID_BOOTSTRAP_EMAIL: ADMIN.email, ALEPHGRID_BOOTSTRAP_PASSWORD: ADMIN.password });
test.afterAll(async () => accounts.dispose());

test("denied: a viewer sees the code, tests and docs read-only, with the reason", async ({ page, browser }) => {
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
  const workbook = (await json("/api/workbooks", "POST", { name: "Shared functions", space: general })).id;
  /** One WebSocket operation as whoever `p` is signed in as. */
  const op = (p: import("@playwright/test").Page, name: string, params: Record<string, unknown>) => p.evaluate(async ({ workbook, name, params }) => new Promise<{ ok: boolean; result?: any; error?: string }>((ok) => {
    const ws = new WebSocket(`ws://${location.host}/ws/${workbook}`);
    ws.onmessage = (m) => {
      const msg = JSON.parse(String(m.data));
      if (msg.event === "hello") ws.send(JSON.stringify({ id: 1, op: name, ...params }));
      else if (msg.id === 1) { ws.close(); ok(msg); }
    };
  }), { workbook, name, params });
  const definition = { name: "MARGIN", kind: "formula", arguments: ["revenue", "cost"], source: "IF(revenue=0,0,(revenue-cost)/revenue)",
    documentation: "Profit as a fraction of revenue.", tests: [{ arguments: ["100", "60"], expected: "0.4" }], dependencies: {} };
  expect((await op(page, "publish_function", { definition })).ok).toBe(true);
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
  await expect(vic.locator(".status-dot.status-open")).toBeVisible();
  await vic.getByRole("button", { name: "Functions", exact: true }).click();
  const panel = vic.getByRole("complementary", { name: "Functions" });
  await expect(panel.getByRole("note").filter({ hasText: "View only" })).toBeVisible();
  // The published function is there to read.
  await panel.getByRole("combobox", { name: "Function", exact: true }).selectOption("MARGIN");
  const source = panel.getByRole("textbox", { name: "Function source" });
  await expect(source).toHaveValue(definition.source);
  for (const name of ["Function name", "Arguments", "Function source"]) {
    await expect(panel.getByRole("textbox", { name, exact: true })).not.toBeEditable();
  }
  await expect(panel.getByRole("combobox", { name: "Function engine" })).toBeDisabled();
  // Typing changes nothing.
  await source.focus();
  await vic.keyboard.type("+1");
  await vic.keyboard.press("Enter");
  await expect(source).toHaveValue(definition.source);
  for (const name of ["Run tests", "Publish version", "New"]) {
    const b = panel.getByRole("button", { name, exact: true });
    await expect(b).toBeDisabled();
    await expect(b).toHaveAccessibleDescription("View only");
  }
  await vic.keyboard.press("ControlOrMeta+Enter");
  await panel.getByRole("tab", { name: "Tests" }).click();
  await expect(panel.getByRole("textbox", { name: "Test 1 revenue" })).toHaveValue("100");
  await expect(panel.getByRole("textbox", { name: "Test 1 revenue" })).not.toBeEditable();
  await expect(panel.getByRole("button", { name: "Add test" })).toBeDisabled();
  await expect(panel.getByRole("button", { name: "Remove test 1" })).toBeDisabled();
  await panel.getByRole("tab", { name: "Docs" }).click();
  await expect(panel.getByRole("textbox", { name: "Function documentation" })).toHaveValue(definition.documentation);
  await expect(panel.getByRole("textbox", { name: "Function documentation" })).not.toBeEditable();
  // Nothing ran on opening or on Mod+Enter, and the server still has one version.
  await expect(panel.getByRole("region", { name: "Test results" })).not.toContainText("Passed");
  const fns = (await op(page, "functions", {})).result;
  expect(fns.map((f: { name: string; versions: unknown[] }) => [f.name, f.versions.length])).toEqual([["MARGIN", 1]]);
  // The server refuses the viewer's publish too.
  const refused = await op(vic, "publish_function", { definition: { ...definition, source: "1" } });
  expect(refused.ok).toBe(false);
  await vic.context().close();
});
