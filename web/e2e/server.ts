import { spawn, type ChildProcess } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const BIN = process.env.ALEPHGRID_BIN ?? resolve(fileURLToPath(new URL(".", import.meta.url)), "../../target/debug/alephgrid");

async function freePort(): Promise<number> {
  return new Promise((ok) => {
    const s = createServer();
    s.listen(0, "127.0.0.1", () => {
      const port = (s.address() as { port: number }).port;
      s.close(() => ok(port));
    });
  });
}

/** One `alephgrid` process on a private data directory; can be killed and restarted. */
export class Server {
  dataDir = mkdtempSync(join(tmpdir(), "alephgrid-e2e-"));
  port = 0;
  private proc: ChildProcess | null = null;

  /** What the server printed on its console (the setup code, for instance). */
  output = "";

  /** `extraArgs` are added to the command line of every start; `env` to its environment. */
  constructor(private extraArgs: string[] = [], private env: Record<string, string> = {}) {}

  get url(): string {
    return `http://127.0.0.1:${this.port}`;
  }

  async start(): Promise<void> {
    if (!this.port) this.port = await freePort();
    this.proc = spawn(BIN, ["--data-dir", this.dataDir, "--port", String(this.port), "--threads", "2", "--sheet-copy-rows", "1000", ...this.extraArgs],
      { stdio: ["ignore", "pipe", "ignore"], env: { ...process.env, ...this.env } });
    this.proc.stdout?.on("data", (d: Buffer) => { this.output = (this.output + d.toString()).slice(-16_384); });
    const deadline = Date.now() + 30_000;
    for (;;) {
      try {
        const r = await fetch(`${this.url}/api/config`, { signal: AbortSignal.timeout(2_000) });
        if (r.ok) return;
      } catch { /* not up yet */ }
      if (Date.now() > deadline) throw new Error(`server did not start (${BIN})`);
      await new Promise((r) => setTimeout(r, 50));
    }
  }

  /** Stop the process: SIGTERM (graceful) or SIGKILL (crash). */
  async stop(signal: "SIGTERM" | "SIGKILL" = "SIGTERM"): Promise<void> {
    const p = this.proc;
    if (!p || p.exitCode !== null) return;
    const exited = new Promise((r) => p.once("exit", r));
    p.kill(signal);
    await exited;
    this.proc = null;
  }

  async dispose(): Promise<void> {
    await this.stop("SIGKILL");
    rmSync(this.dataDir, { recursive: true, force: true });
  }

  async api<T = any>(path: string, init?: RequestInit): Promise<T> {
    const r = await fetch(this.url + path, init);
    if (!r.ok) throw new Error(`${path}: ${r.status} ${await r.text()}`);
    return (r.status === 204 ? undefined : r.json()) as T;
  }

  /** Create a workbook through the API; returns its id. */
  async createWorkbook(name: string): Promise<string> {
    return (await this.api<{ id: string }>("/api/workbooks", {
      method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ name }),
    })).id;
  }

  /** A minimal WebSocket client for one workbook (Node's global WebSocket). */
  async connect(workbook: string): Promise<WsClient> {
    const ws = new WebSocket(`ws://127.0.0.1:${this.port}/ws/${workbook}`);
    const pending = new Map<number, { ok: (v: any) => void; fail: (e: Error) => void }>();
    let next = 1;
    ws.onmessage = (m) => {
      const msg = JSON.parse(String(m.data));
      if (msg.event) return;
      const p = pending.get(msg.id);
      pending.delete(msg.id);
      if (msg.ok) p?.ok(msg.result);
      else p?.fail(new Error(msg.error));
    };
    await new Promise<void>((ok, fail) => {
      ws.onopen = () => ok();
      ws.onerror = () => fail(new Error("websocket failed"));
    });
    return {
      call: (op, params = {}) => new Promise((ok, fail) => {
        const id = next++;
        pending.set(id, { ok, fail });
        ws.send(JSON.stringify({ id, op, ...params }));
      }),
      close: () => ws.close(),
    };
  }

  async exportCsv(workbook: string, spec: unknown): Promise<string> {
    const q = new URLSearchParams({ spec: JSON.stringify(spec), format: "csv" });
    const r = await fetch(`${this.url}/api/workbooks/${workbook}/export?${q}`);
    if (!r.ok) throw new Error(await r.text());
    return r.text();
  }
}

export type WsClient = {
  call<T = any>(op: string, params?: Record<string, unknown>): Promise<T>;
  close(): void;
};

/** Run a command from the workbook's menu bar (File, Edit, View, Insert, Format, Data, Tools, Help). */
export async function menuCommand(page: import("@playwright/test").Page, menu: string, item: string | RegExp): Promise<void> {
  await page.getByRole("navigation", { name: "Workbook menus" }).getByRole("button", { name: menu, exact: true }).click();
  await page.getByRole("menuitem", { name: item }).click();
}
