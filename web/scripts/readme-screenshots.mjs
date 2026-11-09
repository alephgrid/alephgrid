// Regenerates the README screenshots in assets/screenshots/ from the running product.
//
//   cargo build -p alephgrid-server && (cd web && npm run build)
//   cd web && node scripts/readme-screenshots.mjs
//
// It starts the debug binary on a throwaway data directory (deleted afterwards), seeds sample
// workbooks (a generated 10,000,000-row flights table, a 20,000-row orders table and a report
// sheet), and captures each screen at 1440×860, 2× scale, in the theme named below.
// Nothing here reads or writes your own data directory.
//
//   ALEPHGRID_BIN=path/to/alephgrid   a release binary instead, with the UI it embeds
//   SHOT_ROWS=200000                  a smaller flights table (default 10,000,000)

import { spawn } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { chromium } from "@playwright/test";

const here = fileURLToPath(new URL(".", import.meta.url));
const root = resolve(here, "../..");
const BIN = process.env.ALEPHGRID_BIN ?? join(root, "target/debug/alephgrid");
const OUT = join(root, "assets/screenshots");
const VIEW = { width: 1440, height: 860 };

const port = await new Promise((ok) => {
  const s = createServer();
  s.listen(0, "127.0.0.1", () => { const p = s.address().port; s.close(() => ok(p)); });
});
const url = `http://127.0.0.1:${port}`;
const dataDir = mkdtempSync(join(tmpdir(), "alephgrid-shots-"));
// A binary named explicitly serves the UI it embeds; the debug build serves web/dist.
const webArgs = process.env.ALEPHGRID_BIN ? [] : ["--web-dir", join(root, "web/dist")];
const ROWS = Number(process.env.SHOT_ROWS ?? 10_000_000);
const proc = spawn(BIN, ["--data-dir", dataDir, "--port", String(port), ...webArgs], { stdio: "ignore" });

async function api(path, body) {
  const r = await fetch(url + path, body ? { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) } : undefined);
  if (!r.ok) throw new Error(`${path}: ${r.status} ${await r.text()}`);
  return r.json();
}

async function connect(id) {
  const ws = new WebSocket(`ws://127.0.0.1:${port}/ws/${id}`);
  const pending = new Map();
  let next = 1;
  ws.onmessage = (m) => {
    const msg = JSON.parse(String(m.data));
    if (msg.event) return;
    const p = pending.get(msg.id);
    pending.delete(msg.id);
    msg.ok ? p?.ok(msg.result) : p?.fail(new Error(msg.error));
  };
  await new Promise((ok, fail) => { ws.onopen = ok; ws.onerror = () => fail(new Error("websocket failed")); });
  return {
    call: (op, params = {}) => new Promise((ok, fail) => { const id = next++; pending.set(id, { ok, fail }); ws.send(JSON.stringify({ id, op, ...params })); }),
    close: () => ws.close(),
  };
}

const area = (row, column, rows, columns) => ({ row, column, rows, columns });

try {
  for (const deadline = Date.now() + 30_000; ;) {
    try { if ((await fetch(`${url}/api/config`)).ok) break; } catch { /* starting */ }
    if (Date.now() > deadline) throw new Error(`server did not start (${BIN})`);
    await new Promise((r) => setTimeout(r, 100));
  }

  // Workbooks for the hub, oldest first so the report is the most recent.
  const retention = (await api("/api/workbooks", { name: "Customer retention" })).id;
  const flights = (await api("/api/workbooks", { name: "US flights" })).id;
  const report = (await api("/api/workbooks", { name: "Q3 sales report" })).id;
  { const ws = await connect(retention); await ws.call("sheet_create", { name: "Cohorts" }); ws.close(); }
  { const ws = await connect(flights); await ws.call("create_demo_table", { rows: ROWS }); ws.close(); }

  // The report: a 20,000-row orders table, and a sheet whose cells total it with SUMIFS.
  const ws = await connect(report);
  await ws.call("create_table_from_query", {
    name: "orders",
    sql: `SELECT i + 1 AS order_id,
                 ['North','South','East','West'][(i * 7) % 4 + 1] AS region,
                 ['Jul','Aug','Sep'][(i // 7) % 3 + 1] AS month,
                 ['Laptops','Monitors','Docks','Keyboards'][(i * 3) % 4 + 1] AS product,
                 round((40 + ((i * 37) % 960) * 1.25) * (0.6 + 0.2 * ((i * 7) % 4)) * (0.9 + 0.1 * ((i // 7) % 3)), 2) AS amount
          FROM range(20000) t(i)`,
  });
  await ws.call("sheet_create", { name: "Q3 report" });
  const sheet = (await ws.call("sheets", {})).find((s) => s.name === "Q3 report").index;
  const months = ["Jul", "Aug", "Sep"], regions = ["North", "South", "East", "West"];
  const inputs = [
    { row: 0, column: 0, value: "Region" },
    ...months.map((m, i) => ({ row: 0, column: i + 1, value: m })),
    { row: 0, column: 4, value: "Q3 total" },
    { row: 0, column: 5, value: "Share" },
  ];
  regions.forEach((r, i) => {
    const row = i + 1, n = row + 1;
    inputs.push({ row, column: 0, value: r });
    months.forEach((m, j) => inputs.push({ row, column: j + 1, value: `=SUMIFS(orders[amount],orders[region],$A${n},orders[month],$${String.fromCharCode(66 + j)}$1)` }));
    inputs.push({ row, column: 4, value: `=SUM(B${n}:D${n})` });
    inputs.push({ row, column: 5, value: `=E${n}/E$6` });
  });
  inputs.push({ row: 5, column: 0, value: "Total" });
  for (let c = 1; c <= 4; c++) { const L = String.fromCharCode(65 + c); inputs.push({ row: 5, column: c, value: `=SUM(${L}2:${L}5)` }); }
  inputs.push({ row: 5, column: 5, value: "=E6/E$6" });
  await ws.call("sheet_set", { sheet, inputs });
  await ws.call("sheet_style", { sheet, area: area(0, 0, 1, 6), style: { b: true, fill: "#E6F2EC", color: "#0B3D2A" } });
  await ws.call("sheet_style", { sheet, area: area(5, 0, 1, 6), style: { b: true } });
  // Charted cells use a plain number format: charts read shown text and skip "$" values.
  await ws.call("sheet_style", { sheet, area: area(1, 1, 4, 3), style: { num_fmt: "#,##0" } });
  await ws.call("sheet_style", { sheet, area: area(1, 4, 5, 1), style: { num_fmt: "$#,##0" } });
  await ws.call("sheet_style", { sheet, area: area(5, 1, 1, 3), style: { num_fmt: "$#,##0" } });
  await ws.call("sheet_style", { sheet, area: area(1, 5, 5, 1), style: { num_fmt: "0.0%" } });
  await ws.call("sheet_border", { sheet, area: area(5, 0, 1, 6), kind: "top", weight: "medium" });
  await ws.call("sheet_resize", { sheet, axis: "columns", start: 0, count: 1, size: 110 });
  await ws.call("sheet_resize", { sheet, axis: "columns", start: 1, count: 5, size: 100 });
  await ws.call("set_sheet_rules", { sheet, rules: { colors: [{ area: area(1, 5, 4, 1), condition: "greater", value: "0.25", color: "#C6EFCE" }], validations: [] } });
  await ws.call("set_sheet_charts", { sheet, charts: [{ chart: "q3", kind: "column", title: "Q3 revenue by region", data: area(0, 0, 5, 4), row: 7, column: 0, width: 620, height: 290 }] });
  await ws.call("add_comment", { sheet, row: 4, column: 4, text: "West is up again: best region this quarter." });
  ws.close();

  const browser = await chromium.launch();
  async function page(theme) {
    const context = await browser.newContext({ viewport: VIEW, deviceScaleFactor: 2, colorScheme: theme });
    await context.addInitScript((t) => {
      try { localStorage.setItem("alephgrid-welcome-done", "1"); localStorage.setItem("alephgrid-theme", t); } catch { /* */ }
    }, theme);
    return context.newPage();
  }
  async function openWorkbook(p, id, tab) {
    await p.goto(`${url}/w/${id}`);
    await p.locator(".status-dot.status-open").waitFor();
    if (tab) await p.getByRole("tab", { name: new RegExp(tab) }).click();
    await p.waitForTimeout(800);
  }
  const shot = async (p, name) => { await p.waitForTimeout(500); await p.screenshot({ path: join(OUT, name) }); console.log("wrote", name); };

  // 1. The report sheet: formatted cells totalling a table, a chart and a selection with its fill handle.
  {
    const p = await page("light");
    await openWorkbook(p, report, "Q3 report");
    await p.getByRole("button", { name: "Go to", exact: true }).click();
    const go = p.getByRole("dialog", { name: "Go to" }).getByLabel("Cell, range or name");
    await go.fill("B2:D5");
    await go.press("Enter");
    await p.locator(".grid-host").focus();
    await shot(p, "workbook-light.png");

    // 2. The same workbook with the functions side panel beside the grid.
    await p.getByRole("button", { name: "Functions", exact: true }).click();
    const panel = p.getByRole("complementary", { name: "Functions" });
    await panel.waitFor();
    await panel.getByRole("button", { name: "Start from an example" }).click();
    await panel.getByRole("button", { name: "Run tests", exact: true }).click();
    await p.waitForTimeout(1000);
    await shot(p, "functions-light.png");
    await p.context().close();
  }

  // 3. The 200,000-row table with SQL, opened on request and docked under the grid.
  {
    const p = await page("dark");
    await openWorkbook(p, flights);
    await p.getByRole("button", { name: "SQL", exact: true }).click();
    const sql = p.getByRole("region", { name: "SQL" });
    await sql.waitFor();
    await p.locator(".sql-editor").click();
    await p.keyboard.press("ControlOrMeta+a");
    await p.keyboard.type("SELECT carrier, count(*) AS flights, round(avg(dep_delay_min), 1) AS avg_delay\nFROM flights GROUP BY carrier ORDER BY flights DESC");
    await p.getByRole("button", { name: "Run", exact: true }).click();
    await p.locator(".result-table tbody tr").first().waitFor();
    // Room for the result: a taller panel and a shorter editor.
    const dockGrip = p.getByRole("separator", { name: "Resize SQL panel" });
    await dockGrip.focus();
    for (let i = 0; i < 24; i++) await p.keyboard.press("ArrowUp");
    const editorGrip = p.getByRole("separator", { name: "Resize SQL editor" });
    await editorGrip.focus();
    for (let i = 0; i < 6; i++) await p.keyboard.press("ArrowUp");
    await p.locator(".sql-editor").click();
    await shot(p, "sql-dark.png");
    await p.context().close();
  }

  // 4. The big table itself, sorted by flight number, half way down its rows.
  {
    const p = await page("light");
    await openWorkbook(p, flights);
    // The header row is drawn on a canvas: flight_number is the third column.
    await p.locator(".grid-scroller").click({ button: "right", position: { x: 440, y: 16 } });
    await p.getByRole("menuitem", { name: /Sort ascending/ }).click();
    await p.locator(".chips").getByText(/flight_number/).waitFor();
    await p.locator(".grid-scroller").evaluate((e) => { e.scrollTop = e.scrollHeight / 2; });
    await p.waitForTimeout(1500);
    // Selecting the part-hidden top row scrolls it fully into view: no row is cut in half.
    await p.locator(".grid-scroller").click({ position: { x: 560, y: 36 } });
    await p.waitForTimeout(500);
    await p.locator(".grid-scroller").click({ position: { x: 560, y: 270 } });
    await shot(p, "bigtable-light.png");
    await p.context().close();
  }

  // 5. Analyze, no SQL: revenue by region from the 20,000 orders, with its chart.
  {
    const p = await page("light");
    await openWorkbook(p, report, "orders");
    await p.getByRole("button", { name: "Analyze", exact: true }).click();
    const d = p.getByRole("complementary", { name: "Analyze" });
    await d.waitFor();
    await d.getByRole("combobox", { name: "Analysis kind", exact: true }).selectOption("pivot");
    const rows = d.getByRole("region", { name: "Rows" });
    while (await rows.getByRole("button", { name: /^Remove / }).count()) await rows.getByRole("button", { name: /^Remove / }).first().click();
    await d.getByRole("combobox", { name: "Add field to Rows" }).selectOption("region");
    await d.getByRole("combobox", { name: "Aggregate of value 1" }).selectOption("sum");
    await d.getByRole("combobox", { name: "Column of value 1" }).selectOption("amount");
    const totals = d.getByRole("checkbox", { name: "Totals and subtotals" });
    if (await totals.isChecked()) await totals.uncheck();
    await d.getByRole("textbox", { name: "Analysis name" }).fill("Revenue by region");
    await d.getByRole("button", { name: "Run", exact: true }).click();
    await p.getByRole("region", { name: "Analysis result" }).getByRole("figure", { name: "Analysis chart" }).waitFor();
    await d.evaluate((e) => { for (const x of [e, ...e.querySelectorAll("*")]) if (x.scrollTop) x.scrollTop = 0; });
    await shot(p, "analyze-light.png");
    await p.context().close();
  }

  // 6. Importing a file: the review step, where column types are checked before anything is written.
  {
    const p = await page("light");
    await openWorkbook(p, report, "orders");
    const csv = ["order_id,ordered_at,customer,country,plan,seats,amount_usd,renewed"];
    const plans = ["Starter", "Team", "Business"], countries = ["Germany", "India", "United States", "Brazil", "Japan"];
    for (let i = 1; i <= 5000; i++) {
      const d = new Date(Date.UTC(2026, 0, 1) + i * 5400_000).toISOString().slice(0, 19).replace("T", " ");
      csv.push(`${10_000 + i},${d},Customer ${i % 731},${countries[i % 5]},${plans[i % 3]},${1 + (i * 7) % 40},${((i * 37) % 900 + 19.5).toFixed(2)},${i % 4 ? "true" : "false"}`);
    }
    await p.locator('input[type="file"]').setInputFiles({ name: "subscriptions.csv", mimeType: "text/csv", buffer: Buffer.from(csv.join("\n") + "\n") });
    await p.getByRole("dialog", { name: "Import subscriptions.csv" }).getByRole("button", { name: "Import table", exact: true }).waitFor();
    await shot(p, "import-light.png");
    await p.context().close();
  }

  // 7. The hub.
  {
    const p = await page("light");
    for (const id of [retention, flights, report]) await openWorkbook(p, id);
    await p.goto(url);
    await p.getByRole("region", { name: "Recently opened" }).waitFor();
    await shot(p, "hub-light.png");
    await p.context().close();
  }
  await browser.close();
} finally {
  proc.kill("SIGKILL");
  rmSync(dataDir, { recursive: true, force: true });
}
