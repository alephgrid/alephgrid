#!/usr/bin/env node
// Reproducible benchmark (9.05): seeds narrow, wide and text-heavy tables into a NEW scratch
// data directory, starts the given alephgrid binary on it, runs a mixed workload with 1, 5
// and 20 concurrent clients over the public protocol (HTTP import, WebSocket ops), and writes
// raw latencies, RSS samples and a p50/p95/p99 summary as CSV and JSON.
//
//   node tools/bench/bench.mjs --bin target/release/alephgrid            # smoke size
//   node tools/bench/bench.mjs --bin target/release/alephgrid --scale full --seconds 60
//
// It never touches an existing data directory: --scratch must not exist or be empty, and the
// scratch directory is deleted at the end unless --keep. Results go to --out. Usage and the
// meaning of each number: docs/OPERATIONS.md#benchmarks. Node 22+, no dependencies.

import { spawn, execFile } from "node:child_process";
import fs from "node:fs";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import { Readable } from "node:stream";
import { once } from "node:events";

const SCALES = {
  // Smoke: well under 2 minutes and 1 GB of RAM on a laptop.
  smoke: { narrow: 50_000, wide: 5_000, text: 20_000, seconds: 5 },
  // Full: the sizes for published results; needs a dedicated machine (docs/OPERATIONS.md).
  full: { narrow: 10_000_000, wide: 1_000_000, text: 2_000_000, seconds: 60 },
};

const HELP = `usage: node tools/bench/bench.mjs --bin PATH [options]

  --bin PATH            alephgrid binary to start (required)
  --scale smoke|full    dataset sizes and level duration (default smoke)
  --rows-narrow N       rows of the narrow table (6 columns)
  --rows-wide N         rows of the wide table
  --wide-columns N      columns of the wide table (default 100)
  --rows-text N         rows of the text-heavy table (5 columns, ~0.5 KB per row)
  --datasets LIST       comma-separated subset of narrow,wide,text (default all)
  --clients LIST        concurrent clients per level (default 1,5,20)
  --seconds N           measured seconds per dataset and level
  --warmup N            unmeasured seconds before each level (default 1)
  --seed N              PRNG seed for data and workload (default 1)
  --memory-budget SIZE  passed to the server (default 512MiB)
  --threads N           passed to the server (default: the server's default)
  --scratch DIR         new or empty directory for the data directory and CSV files
                        (default: a new directory under the system temp directory)
  --out DIR             results directory (default bench-results/<UTC time>)
  --label TEXT          free text recorded with the results (machine, purpose)
  --keep                keep the scratch directory`;

// ------------------------------------------------------------------ arguments

function parseArgs(argv) {
  const a = { scale: "smoke", wideColumns: 100, datasets: "narrow,wide,text", clients: "1,5,20", warmup: 1, seed: 1, memoryBudget: "512MiB", keep: false, label: "" };
  const names = { "--bin": "bin", "--scale": "scale", "--rows-narrow": "narrow", "--rows-wide": "wide", "--wide-columns": "wideColumns", "--rows-text": "text",
    "--datasets": "datasets", "--clients": "clients", "--seconds": "seconds", "--warmup": "warmup", "--seed": "seed", "--memory-budget": "memoryBudget",
    "--threads": "threads", "--scratch": "scratch", "--out": "out", "--label": "label" };
  for (let i = 2; i < argv.length; i++) {
    const k = argv[i];
    if (k === "--help" || k === "-h") { console.log(HELP); process.exit(0); }
    if (k === "--keep") { a.keep = true; continue; }
    if (!(k in names) || i + 1 >= argv.length) { console.error(`unknown or incomplete option ${k}\n\n${HELP}`); process.exit(2); }
    a[names[k]] = argv[++i];
  }
  if (!a.bin) { console.error(HELP); process.exit(2); }
  const scale = SCALES[a.scale];
  if (!scale) { console.error(`--scale is smoke or full`); process.exit(2); }
  const int = (v, d) => (v === undefined ? d : Number.parseInt(v, 10));
  const out = {
    bin: path.resolve(a.bin), scale: a.scale, label: a.label, keep: a.keep, memoryBudget: a.memoryBudget, threads: a.threads,
    rows: { narrow: int(a.narrow, scale.narrow), wide: int(a.wide, scale.wide), text: int(a.text, scale.text) },
    wideColumns: int(a.wideColumns), datasets: a.datasets.split(",").map((s) => s.trim()).filter(Boolean),
    clients: a.clients.split(",").map((s) => int(s)), seconds: Number(a.seconds ?? scale.seconds), warmup: Number(a.warmup), seed: int(a.seed),
    scratch: a.scratch ? path.resolve(a.scratch) : null,
    out: path.resolve(a.out ?? path.join("bench-results", new Date().toISOString().replace(/[:.]/g, "-"))),
  };
  for (const d of out.datasets) if (!["narrow", "wide", "text"].includes(d)) { console.error(`unknown dataset ${d}`); process.exit(2); }
  if (out.clients.some((n) => !(n >= 1))) { console.error("--clients takes positive numbers"); process.exit(2); }
  return out;
}

// ------------------------------------------------------------------ deterministic data

function prng(seed) { // mulberry32
  let s = seed >>> 0;
  return () => {
    s = (s + 0x6d2b79f5) >>> 0;
    let t = s;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

const WORDS = "lorem ipsum dolor sit amet consectetur adipiscing elit sed do eiusmod tempor incididunt ut labore et dolore magna aliqua enim ad minim veniam quis nostrud exercitation ullamco laboris nisi aliquip ex ea commodo consequat duis aute irure in reprehenderit voluptate velit esse cillum fugiat nulla pariatur excepteur sint occaecat cupidatat non proident sunt culpa qui officia deserunt mollit anim id est laborum".split(" ");
const CITIES = ["Pune", "Oslo", "Lima", "Kyoto", "Accra", "Quito", "Perth", "Turin", "Hanoi", "Dakar"];

function words(rand, min, max) {
  const n = min + Math.floor(rand() * (max - min + 1));
  const out = [];
  for (let i = 0; i < n; i++) out.push(WORDS[Math.floor(rand() * WORDS.length)]);
  return out.join(" ");
}

const quote = (s) => `"${s.replaceAll('"', '""')}"`;

const DATASETS = {
  narrow: {
    table: "bench_narrow",
    header: () => ["id", "category", "amount", "qty", "day", "flag"],
    row: (rand, i) => {
      const day = new Date(Date.UTC(2020, 0, 1) + Math.floor(rand() * 1800) * 86_400_000).toISOString().slice(0, 10);
      return [i, `cat${String(Math.floor(rand() * 20)).padStart(2, "0")}`, (rand() * 1000).toFixed(2), Math.floor(rand() * 100), day, rand() < 0.5 ? "true" : "false"];
    },
    numeric: ["amount", "qty"],
    sortable: ["amount", "qty", "day", "category"],
    filter: (rand) => ({ column: "category", op: "eq", value: `cat${String(Math.floor(rand() * 20)).padStart(2, "0")}` }),
    edit: (rand) => ({ column: "amount", value: (rand() * 1000).toFixed(2) }),
    query: (t) => `SELECT category, count(*) AS n, avg(amount) AS mean FROM ${t} GROUP BY 1 ORDER BY 1`,
  },
  wide: {
    table: "bench_wide",
    header: (cfg) => ["id", ...Array.from({ length: cfg.wideColumns - 1 }, (_, c) => `c${String(c + 1).padStart(3, "0")}`)],
    row: (rand, i, cfg) => [i, ...Array.from({ length: cfg.wideColumns - 1 }, () => (rand() * 1000).toFixed(2))],
    numeric: null, // every c column
    sortable: null,
    filter: (rand) => ({ column: "c001", op: "gt", value: String(Math.floor(rand() * 1000)) }),
    edit: (rand, cfg) => ({ column: `c${String(1 + Math.floor(rand() * (cfg.wideColumns - 1))).padStart(3, "0")}`, value: (rand() * 1000).toFixed(2) }),
    query: (t) => `SELECT avg(c001) AS a, max(c002) AS b, min(c003) AS c, count(*) AS n FROM ${t}`,
  },
  text: {
    table: "bench_text",
    header: () => ["id", "title", "body", "tag", "city"],
    row: (rand, i) => [i, quote(words(rand, 3, 8)), quote(words(rand, 40, 100)), WORDS[Math.floor(rand() * 12)], CITIES[Math.floor(rand() * CITIES.length)]],
    numeric: ["id"],
    sortable: ["title", "tag", "city"],
    filter: (rand) => ({ column: "body", op: "contains", value: WORDS[Math.floor(rand() * WORDS.length)] }),
    edit: (rand) => ({ column: "title", value: words(rand, 3, 8) }),
    query: (t) => `SELECT tag, count(*) AS n FROM ${t} WHERE body LIKE '%labore%' GROUP BY 1 ORDER BY 2 DESC`,
  },
};

async function writeCsv(file, dataset, rows, cfg, seed) {
  const d = DATASETS[dataset];
  const rand = prng(seed);
  const out = fs.createWriteStream(file);
  const write = async (s) => { if (!out.write(s)) await once(out, "drain"); };
  await write(d.header(cfg).join(",") + "\n");
  let chunk = "";
  for (let i = 0; i < rows; i++) {
    chunk += d.row(rand, i, cfg).join(",") + "\n";
    if (chunk.length > 1 << 20) { await write(chunk); chunk = ""; }
  }
  await write(chunk);
  out.end();
  await once(out, "finish");
  return fs.statSync(file).size;
}

// ------------------------------------------------------------------ server

async function freePort() {
  const srv = net.createServer();
  srv.listen(0, "127.0.0.1");
  await once(srv, "listening");
  const port = srv.address().port;
  srv.close();
  await once(srv, "close");
  return port;
}

async function startServer(cfg, dataDir, logFile) {
  const port = await freePort();
  const args = ["--data-dir", dataDir, "--port", String(port), "--memory-budget", cfg.memoryBudget];
  if (cfg.threads) args.push("--threads", String(cfg.threads));
  const log = fs.openSync(logFile, "a");
  const child = spawn(cfg.bin, args, { stdio: ["ignore", log, log] });
  const url = `http://127.0.0.1:${port}`;
  for (let i = 0; i < 300; i++) {
    if (child.exitCode !== null) throw new Error(`the server exited (${child.exitCode}); see ${logFile}`);
    try {
      const r = await fetch(`${url}/api/config`);
      if (r.ok) return { child, url, port, config: await r.json() };
    } catch { /* not up yet */ }
    await new Promise((r) => setTimeout(r, 100));
  }
  child.kill("SIGTERM");
  throw new Error(`the server did not answer within 30 s; see ${logFile}`);
}

function rssKb(pid) {
  return new Promise((resolve) => execFile("ps", ["-o", "rss=", "-p", String(pid)], (err, out) => resolve(err ? null : Number.parseInt(out.trim(), 10) || null)));
}

async function http(url, init) {
  const r = await fetch(url, init);
  const text = await r.text();
  if (!r.ok) throw new Error(`${init?.method ?? "GET"} ${url}: ${r.status} ${text}`);
  return text ? JSON.parse(text) : null;
}

// ------------------------------------------------------------------ protocol client

class Client {
  static async connect(wsUrl) {
    const c = new Client();
    c.ws = new WebSocket(wsUrl);
    c.next = 0;
    c.pending = new Map();
    c.ws.onmessage = (m) => {
      const d = JSON.parse(m.data);
      if (d.event) return;
      const p = c.pending.get(d.id);
      if (!p) return;
      c.pending.delete(d.id);
      if (d.ok) p.resolve(d.result);
      else p.reject(Object.assign(new Error(d.error), { code: d.code ?? "error" }));
    };
    c.ws.onclose = () => { for (const p of c.pending.values()) p.reject(Object.assign(new Error("connection closed"), { code: "closed" })); c.pending.clear(); };
    await new Promise((resolve, reject) => { c.ws.onopen = resolve; c.ws.onerror = () => reject(new Error(`could not connect to ${wsUrl}`)); });
    return c;
  }

  // A request unanswered after 60 s counts as an error ("client_timeout"), so one stuck
  // request cannot stretch a level indefinitely; the server's own deadline is shorter.
  call(op, params = {}) {
    const id = ++this.next;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(Object.assign(new Error(`${op}: no reply in 60 s`), { code: "client_timeout" }));
      }, 60_000);
      const done = (f) => (v) => { clearTimeout(timer); f(v); };
      this.pending.set(id, { resolve: done(resolve), reject: done(reject) });
      this.ws.send(JSON.stringify({ id, op, ...params }));
    });
  }

  close() { this.ws.close(); }
}

// One client's mixed workload: mostly scrolling reads, with sorts, filters, column
// statistics, single-cell edits and aggregate queries mixed in.
const MIX = [["fetch", 50], ["sort", 10], ["filter", 10], ["stats", 10], ["edit", 10], ["query", 10]];

async function runClient(c, ctx, rand, until, measureFrom, record) {
  const d = DATASETS[ctx.dataset];
  const columns = ctx.columns;
  const numeric = d.numeric ?? columns.filter((n) => n !== "id");
  const sortable = d.sortable ?? numeric;
  const t0 = performance.now();
  const view = await c.call("open_view", { spec: { table: d.table } });
  record("open_view", t0, performance.now(), null, measureFrom);
  let rows = view.row_count;
  const total = MIX.reduce((s, [, w]) => s + w, 0);
  while (performance.now() < until) {
    let r = rand() * total;
    const op = MIX.find(([, w]) => (r -= w) < 0)[0];
    const start = performance.now();
    let error = null;
    try {
      const at = Math.floor(rand() * Math.max(1, rows - 100));
      if (op === "fetch") {
        const wide = columns.length > 30;
        const colStart = wide ? Math.floor(rand() * (columns.length - 30)) : 0;
        await c.call("fetch", { view_id: view.view_id, row_start: at, row_count: 100, ...(wide ? { column_start: colStart, column_count: 30 } : {}) });
      } else if (op === "sort") {
        const v = await c.call("update_view", { view_id: view.view_id, spec: { table: d.table, sort: [{ column: sortable[Math.floor(rand() * sortable.length)], desc: rand() < 0.5 }] } });
        rows = v.row_count;
      } else if (op === "filter") {
        // Alternate between a filter and back to all rows, so later reads cover both.
        const v = await c.call("update_view", { view_id: view.view_id, spec: rows === ctx.rows ? { table: d.table, filters: [d.filter(rand)] } : { table: d.table } });
        rows = v.row_count;
      } else if (op === "stats") {
        await c.call("stats", { view_id: view.view_id, column: numeric[Math.floor(rand() * numeric.length)], start: at, end: Math.min(rows, at + 10_000) });
      } else if (op === "edit") {
        const e = d.edit(rand, ctx.cfg);
        await c.call("edit", { view_id: view.view_id, edits: [{ row: Math.floor(rand() * Math.max(1, rows)), column: e.column, value: e.value }] });
      } else {
        await c.call("query", { sql: d.query(d.table), limit: 100 });
      }
    } catch (e) {
      error = e.code ?? "error";
    }
    record(op, start, performance.now(), error, measureFrom);
  }
  await c.call("close_view", { view_id: view.view_id }).catch(() => {});
}

// ------------------------------------------------------------------ statistics

function percentile(sorted, p) { // nearest rank
  if (sorted.length === 0) return null;
  return sorted[Math.min(sorted.length - 1, Math.max(0, Math.ceil((p / 100) * sorted.length) - 1))];
}

const round = (v, digits = 2) => (v === null || v === undefined ? null : Number(v.toFixed(digits)));

// ------------------------------------------------------------------ main

async function main() {
  const cfg = parseArgs(process.argv);
  if (!fs.existsSync(cfg.bin)) throw new Error(`no binary at ${cfg.bin}`);
  let scratch = cfg.scratch;
  if (scratch) {
    if (fs.existsSync(scratch) && fs.readdirSync(scratch).length > 0) throw new Error(`--scratch ${scratch} is not empty; the benchmark only writes into a new or empty directory`);
    fs.mkdirSync(scratch, { recursive: true });
  } else {
    scratch = fs.mkdtempSync(path.join(os.tmpdir(), "alephgrid-bench-"));
  }
  fs.mkdirSync(cfg.out, { recursive: true });
  const dataDir = path.join(scratch, "data");
  const csvDir = path.join(scratch, "csv");
  fs.mkdirSync(csvDir);
  console.log(`scratch ${scratch}\nresults ${cfg.out}`);

  const raw = fs.createWriteStream(path.join(cfg.out, "raw.csv"));
  raw.write("dataset,clients,client,op,start_ms,latency_ms,error\n");
  const rssOut = fs.createWriteStream(path.join(cfg.out, "rss.csv"));
  rssOut.write("phase,dataset,clients,t_ms,rss_kb\n");
  const started = performance.now();

  const server = await startServer(cfg, dataDir, path.join(scratch, "server.log"));
  const pid = server.child.pid;
  let phase = { name: "start", dataset: "", clients: 0, samples: [] };
  const sampler = setInterval(async () => {
    const kb = await rssKb(pid);
    if (kb === null) return;
    phase.samples.push(kb);
    rssOut.write(`${phase.name},${phase.dataset},${phase.clients},${Math.round(performance.now() - started)},${kb}\n`);
  }, 250);
  const phases = [];
  const setPhase = (name, dataset = "", clients = 0) => { phases.push(phase); phase = { name, dataset, clients, samples: [] }; };

  const results = { seed: {}, levels: [] };
  try {
    console.log(`server ${server.config.version} on ${server.url} (pid ${pid})`);
    const wb = await http(`${server.url}/api/workbooks`, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ name: "benchmark" }) });

    for (const [i, dataset] of cfg.datasets.entries()) {
      const d = DATASETS[dataset];
      const rows = cfg.rows[dataset];
      setPhase("seed", dataset);
      const file = path.join(csvDir, `${d.table}.csv`);
      let t = performance.now();
      const bytes = await writeCsv(file, dataset, rows, cfg, cfg.seed * 1000 + i);
      const generateMs = performance.now() - t;
      t = performance.now();
      await http(`${server.url}/api/workbooks/${wb.id}/import?filename=${d.table}.csv&table=${d.table}`, {
        method: "POST", headers: { "content-type": "text/csv" }, body: Readable.toWeb(fs.createReadStream(file)), duplex: "half",
      });
      const importMs = performance.now() - t;
      fs.rmSync(file);
      const columns = d.header(cfg);
      results.seed[dataset] = { table: d.table, rows, columns: columns.length, csv_bytes: bytes, generate_ms: round(generateMs, 0), import_ms: round(importMs, 0) };
      console.log(`seeded ${dataset}: ${rows} rows x ${columns.length} columns, ${(bytes / 1e6).toFixed(1)} MB CSV, import ${(importMs / 1000).toFixed(1)} s`);

      for (const n of cfg.clients) {
        setPhase("load", dataset, n);
        const latencies = new Map();
        const record = (op, start, end, error, measureFrom) => {
          if (start < measureFrom) return;
          const ms = end - start;
          raw.write(`${dataset},${n},${record.client},${op},${Math.round(start - started)},${ms.toFixed(3)},${error ?? ""}\n`);
          const l = latencies.get(op) ?? { ms: [], errors: {} };
          if (error) l.errors[error] = (l.errors[error] ?? 0) + 1;
          else l.ms.push(ms);
          latencies.set(op, l);
        };
        const clients = await Promise.all(Array.from({ length: n }, () => Client.connect(`${server.url.replace("http", "ws")}/ws/${wb.id}`)));
        const levelStart = performance.now();
        const measureFrom = levelStart + cfg.warmup * 1000;
        const until = measureFrom + cfg.seconds * 1000;
        await Promise.all(clients.map((c, k) => {
          const rec = (...a) => { record.client = k; record(...a); };
          return runClient(c, { dataset, columns, rows, cfg }, prng(cfg.seed * 7919 + i * 101 + n * 13 + k), until, measureFrom, rec);
        }));
        const measured = (performance.now() - measureFrom) / 1000;
        clients.forEach((c) => c.close());
        const ops = [];
        for (const [op, l] of [...latencies].sort()) {
          const s = l.ms.sort((a, b) => a - b);
          const errors = Object.values(l.errors).reduce((a, b) => a + b, 0);
          ops.push({ op, count: s.length, errors, error_codes: l.errors, ops_per_s: round((s.length + errors) / measured), p50_ms: round(percentile(s, 50)), p95_ms: round(percentile(s, 95)), p99_ms: round(percentile(s, 99)), max_ms: round(s.at(-1) ?? null) });
        }
        const samples = [...phase.samples].sort((a, b) => a - b);
        const level = { dataset, clients: n, seconds: round(measured), ops, rss_peak_mb: round((samples.at(-1) ?? 0) / 1024, 1), rss_median_mb: round((percentile(samples, 50) ?? 0) / 1024, 1) };
        results.levels.push(level);
        const all = ops.reduce((s, o) => s + o.count, 0);
        const errs = ops.reduce((s, o) => s + o.errors, 0);
        const fetchOp = ops.find((o) => o.op === "fetch");
        console.log(`  ${dataset} x${String(n).padEnd(2)} ${all} ops (${errs} errors) in ${level.seconds}s; fetch p50/p95/p99 ${fetchOp?.p50_ms}/${fetchOp?.p95_ms}/${fetchOp?.p99_ms} ms; RSS peak ${level.rss_peak_mb} MB`);
      }
    }
  } finally {
    clearInterval(sampler);
    setPhase("stop");
    server.child.kill("SIGTERM");
    await Promise.race([once(server.child, "exit"), new Promise((r) => setTimeout(r, 15_000))]);
    raw.end();
    rssOut.end();
    await Promise.all([once(raw, "finish"), once(rssOut, "finish")]);
  }

  const allSamples = phases.flatMap((p) => p.samples).sort((a, b) => a - b);
  let commit = null;
  try { commit = await new Promise((res) => execFile("git", ["rev-parse", "HEAD"], (e, o) => res(e ? null : o.trim()))); } catch { /* not a checkout */ }
  const summary = {
    note: "Raw benchmark output. Not a published capacity figure unless run at --scale full on a dedicated machine and recorded in docs/OPERATIONS.md.",
    label: cfg.label,
    when: new Date().toISOString(),
    scale: cfg.scale,
    settings: { rows: cfg.rows, wide_columns: cfg.wideColumns, datasets: cfg.datasets, clients: cfg.clients, seconds: cfg.seconds, warmup: cfg.warmup, seed: cfg.seed, memory_budget: cfg.memoryBudget, threads: cfg.threads ?? null, mix: Object.fromEntries(MIX) },
    environment: { server_version: server.config.version, commit, node: process.version, os: `${os.type()} ${os.release()} ${os.arch()}`, cpus: os.cpus().length, cpu_model: os.cpus()[0]?.model ?? null, memory_gb: round(os.totalmem() / 2 ** 30, 1), load_average_at_end: os.loadavg().map((v) => round(v)) },
    seed: results.seed,
    levels: results.levels,
    rss_peak_mb: round((allSamples.at(-1) ?? 0) / 1024, 1),
    total_seconds: round((performance.now() - started) / 1000, 1),
  };
  fs.writeFileSync(path.join(cfg.out, "summary.json"), JSON.stringify(summary, null, 2) + "\n");
  const lines = ["dataset,clients,op,count,errors,ops_per_s,p50_ms,p95_ms,p99_ms,max_ms,rss_peak_mb"];
  for (const l of results.levels) for (const o of l.ops) lines.push([l.dataset, l.clients, o.op, o.count, o.errors, o.ops_per_s, o.p50_ms, o.p95_ms, o.p99_ms, o.max_ms, l.rss_peak_mb].join(","));
  fs.writeFileSync(path.join(cfg.out, "summary.csv"), lines.join("\n") + "\n");
  if (!cfg.keep) fs.rmSync(scratch, { recursive: true, force: true });
  console.log(`done in ${summary.total_seconds}s; peak server RSS ${summary.rss_peak_mb} MB; results in ${cfg.out}${cfg.keep ? `; scratch kept at ${scratch}` : ""}`);
}

main().catch((e) => { console.error(`benchmark failed: ${e.message}`); process.exit(1); });
