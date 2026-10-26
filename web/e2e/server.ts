import { spawn, type ChildProcess } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const BIN = process.env.ROWFINITY_BIN ?? resolve(fileURLToPath(new URL(".", import.meta.url)), "../../target/debug/rowfinity");

async function freePort(): Promise<number> {
  return new Promise((ok) => {
    const s = createServer();
    s.listen(0, "127.0.0.1", () => {
      const port = (s.address() as { port: number }).port;
      s.close(() => ok(port));
    });
  });
}

/** One `rowfinity` process on a private data directory; can be killed and restarted. */
export class Server {
  dataDir = mkdtempSync(join(tmpdir(), "rowfinity-e2e-"));
  port = 0;
  private proc: ChildProcess | null = null;

  get url(): string {
    return `http://127.0.0.1:${this.port}`;
  }

  async start(): Promise<void> {
    if (!this.port) this.port = await freePort();
    this.proc = spawn(BIN, ["--data-dir", this.dataDir, "--port", String(this.port), "--threads", "2"], { stdio: "ignore" });
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

  async exportCsv(workbook: string, spec: unknown): Promise<string> {
    const q = new URLSearchParams({ spec: JSON.stringify(spec), format: "csv" });
    const r = await fetch(`${this.url}/api/workbooks/${workbook}/export?${q}`);
    if (!r.ok) throw new Error(await r.text());
    return r.text();
  }
}
