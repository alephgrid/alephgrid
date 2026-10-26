// 3.05: the row cache fetches only the visible column range, stays under its byte bound
// (fresh and stale blocks together) without ever evicting what is on screen, and
// continues truncated replies. No browser page: RowCache runs in Node against a fake
// connection, and against the real server for the column-slice check.

import { expect, test } from "@playwright/test";
import { COL_BLOCK, ROW_BLOCK, RowCache, estimateBytes, type FetchRpc } from "../src/cache";
import type { CellValue, Params, RowBlock } from "../src/types";
import { Server } from "./server";

/** A view of `rows` × `columns` cells; cell (r, c) is "r<r>c<c>" (strings make sizes visible). */
class FakeRpc implements FetchRpc {
  calls: Params<"fetch">[] = [];
  inFlight = 0;
  maxInFlight = 0;
  constructor(private rows: number, private columns: number, private truncateAt = Infinity) {}

  async call(_op: "fetch", p: Params<"fetch">): Promise<RowBlock> {
    this.calls.push(p);
    this.inFlight++;
    this.maxInFlight = Math.max(this.maxInFlight, this.inFlight);
    await new Promise((r) => setTimeout(r, 1));
    this.inFlight--;
    const c0 = p.column_start ?? 0;
    const c1 = Math.min(this.columns, c0 + (p.column_count ?? this.columns));
    const end = Math.min(this.rows, p.row_start + p.row_count, p.row_start + this.truncateAt);
    const rows: CellValue[][] = [];
    const ids: number[] = [];
    for (let r = p.row_start; r < end; r++) {
      const row: CellValue[] = [];
      for (let c = c0; c < c1; c++) row.push(`r${r}c${c}`);
      rows.push(row);
      ids.push(1_000_000 + r);
    }
    return { row_start: p.row_start, rows, ids, column_start: c0, truncated: end < Math.min(this.rows, p.row_start + p.row_count) };
  }
}

/** Wait until the cache has nothing left to load. */
async function settle(rpc: FakeRpc): Promise<void> {
  let calls = -1;
  while (calls !== rpc.calls.length || rpc.inFlight > 0) {
    calls = rpc.calls.length;
    await new Promise((r) => setTimeout(r, 5));
  }
}

function newCache(rpc: FetchRpc, columns: number, maxBytes?: number): RowCache {
  const cache = new RowCache(rpc, () => {}, (e) => { throw e; }, { maxBytes });
  cache.reset(1, columns);
  return cache;
}

test("requests only the visible column blocks plus one block of margin", async () => {
  const rpc = new FakeRpc(10_000, 300);
  const cache = newCache(rpc, 300);
  // Rows 0..30, columns 140..150: column blocks 4 (visible) and 3, 5 (margin); row blocks 0, 1.
  cache.ensure(0, 30, 140, 150);
  await settle(rpc);
  expect(rpc.maxInFlight).toBeLessThanOrEqual(4);
  expect(rpc.calls.every((c) => c.column_count === COL_BLOCK)).toBe(true);
  expect(new Set(rpc.calls.map((c) => c.column_start))).toEqual(new Set([3 * COL_BLOCK, 4 * COL_BLOCK, 5 * COL_BLOCK]));
  expect(new Set(rpc.calls.map((c) => c.row_start))).toEqual(new Set([0, ROW_BLOCK]));
  expect(rpc.calls).toHaveLength(6);
  expect(cache.value(7, 145)).toBe("r7c145");
  expect(cache.value(7, 10)).toBeUndefined();
  expect(cache.record(7)).toBe(1_000_007);
  // The last column block is cut at the view's width; nothing past it is asked for.
  rpc.calls = [];
  cache.ensure(0, 10, 290, 299);
  await settle(rpc);
  expect(Math.max(...rpc.calls.map((c) => c.column_start ?? 0))).toBe(9 * COL_BLOCK);
  expect(cache.value(3, 299)).toBe("r3c299");
});

test("stays under its byte bound, counting stale blocks, and never evicts visible blocks", async () => {
  const rpc = new FakeRpc(1_000_000, 300);
  const limit = 2 << 20;
  const cache = newCache(rpc, 300, limit);
  for (let step = 0; step < 60; step++) {
    const r = (step * 7919) % 900_000;
    const c = (step * 37) % 260;
    cache.ensure(r, r + 30, c, c + 12);
    await settle(rpc);
    // Every visible cell is there.
    for (const [vr, vc] of [[r, c], [r + 30, c + 12], [r + 15, c + 6]]) expect(cache.value(vr, vc)).toBe(`r${vr}c${vc}`);
    expect(cache.bytes()).toBeLessThanOrEqual(limit);
    // Refresh now and then: the old blocks become stale and still count.
    if (step % 5 === 4) cache.refresh();
  }
  expect(cache.blockCount()).toBeGreaterThan(8);
  // One block costs: 100 rows × (16 + 32 strings of 2·len + 16).
  const one = estimateBytes(Array.from({ length: ROW_BLOCK }, (_, r) => Array.from({ length: COL_BLOCK }, (_, c) => `r${500_000 + r}c${100 + c}`)));
  expect(cache.blockCount() * one).toBeGreaterThan(limit * 0.5);

  // A bound smaller than the screen: the visible blocks stay, the margin goes.
  const tiny = newCache(rpc, 300, 1);
  tiny.ensure(250, 280, 40, 70);
  await settle(rpc);
  for (let r = 250; r <= 280; r += 5) for (let c = 40; c <= 70; c += 5) expect(tiny.value(r, c)).toBe(`r${r}c${c}`);
  // Row blocks 2 × column blocks 1..2 are visible; everything else was evicted.
  expect(tiny.blockCount()).toBe(2);
  expect(tiny.value(150, 50)).toBeUndefined();
  expect(tiny.bytes()).toBeGreaterThan(1);
});

test("a truncated reply is continued from where it stopped", async () => {
  const rpc = new FakeRpc(250, 50, 30);
  const cache = newCache(rpc, 50);
  cache.ensure(0, 10, 0, 5);
  await settle(rpc);
  const first = rpc.calls.filter((c) => c.column_start === 0 && c.row_start < ROW_BLOCK).map((c) => [c.row_start, c.row_count]);
  expect(first).toEqual([[0, 100], [30, 70], [60, 40], [90, 10]]);
  for (let r = 0; r < 200; r++) expect(cache.value(r, 3)).toBe(`r${r}c3`);
  expect(cache.record(99)).toBe(1_000_099);
  // The view ends at row 250: the last block is continued up to the end, and no further.
  rpc.calls = [];
  cache.ensure(240, 249, 0, 5);
  await settle(rpc);
  expect(cache.value(249, 0)).toBe("r249c0");
  expect(rpc.calls.filter((c) => c.column_start === 0 && c.row_start >= 200 && c.row_start < 300).map((c) => [c.row_start, c.row_count]))
    .toEqual([[200, 100], [230, 70]]);
});

test("column-range values equal the slice of a full-width fetch (real server)", async () => {
  const server = new Server();
  await server.start();
  try {
    const wb = await server.createWorkbook("Wide");
    const ws = await server.connect(wb);
    const cols = Array.from({ length: 80 }, (_, k) => k % 3 === 0 ? `concat('s', i * 100 + ${k}) AS c${k}` : `i * 100 + ${k} AS c${k}`);
    await ws.call("create_table_from_query", { name: "wide", sql: `SELECT ${cols.join(", ")} FROM range(250) t(i)` });
    const view = await ws.call("open_view", { spec: { table: "wide", sort: [{ column: "c1", desc: true }], filters: [], hidden: [] } });
    const full = await ws.call<RowBlock>("fetch", { view_id: view.view_id, row_start: 0, row_count: 250 });
    expect(full.rows[0]).toHaveLength(80);

    const rpc: FetchRpc & { calls: Params<"fetch">[] } = {
      calls: [],
      call(_op, p) { this.calls.push(p); return ws.call<RowBlock>("fetch", p); },
    };
    const cache = new RowCache(rpc, () => {}, (e) => { throw e; });
    cache.reset(view.view_id, 80);
    cache.ensure(110, 140, 45, 60);
    // Row blocks 0..2 × column blocks 0..2 (visible block 1,1 and its margin).
    await expect.poll(() => [0, 249].every((r) => [0, 79].every((c) => cache.value(r, c) !== undefined))).toBe(true);
    expect(rpc.calls.every((c) => c.column_count === COL_BLOCK)).toBe(true);
    expect(new Set(rpc.calls.map((c) => c.column_start))).toEqual(new Set([0, 32, 64]));
    for (let r = 0; r < 250; r++) {
      for (let c = 0; c < 80; c++) expect(cache.value(r, c)).toEqual(full.rows[r][c]);
      expect(cache.record(r)).toBe(full.ids[r]);
    }
    ws.close();
  } finally {
    await server.dispose();
  }
});
