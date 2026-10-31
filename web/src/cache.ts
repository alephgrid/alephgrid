import type { Rpc } from "./api";
import type { CellValue, Params, RowBlock, SheetCell } from "./types";

/** Rows per block. */
export const ROW_BLOCK = 100;
/** Columns per block: only the visible column range is fetched. */
export const COL_BLOCK = 32;
const MAX_IN_FLIGHT = 4;
/** Default bound of the cache, fresh and stale blocks together. */
export const DEFAULT_CACHE_BYTES = 32 << 20;
/** Keeps (row block, column block) keys exact up to 2^20 column blocks. */
const KEY_SPAN = 1 << 20;

/** What the cache needs from the connection (an `Rpc`, or a fake in tests). */
export interface FetchRpc {
  call(op: "fetch", params: Params<"fetch">): Promise<RowBlock>;
}

/**
 * Values this client wrote, by cell, shown (and used as the base of the next edit) until a
 * fetch that started after the write committed replaces them. A fetch already in flight when
 * the write was sent may still carry the old value; without this the next edit of the same
 * cell would be checked against it and refused as if someone else had changed the cell.
 */
class OwnWrites<V> {
  private map = new Map<string, { value: V; until: number }>();
  write(key: string, value: V): void {
    this.map.set(key, { value, until: Infinity });
  }
  /** The writes committed: blocks whose load started at `generation` or later show them. */
  committed(keys: string[], generation: number): void {
    for (const k of keys) {
      const e = this.map.get(k);
      if (e) e.until = Math.min(e.until, generation);
    }
  }
  drop(keys: string[]): void {
    for (const k of keys) this.map.delete(k);
  }
  clear(): void {
    this.map.clear();
  }
  /** The value written, unless the block (loaded at `loadedAt`) already has it. */
  get(key: string, loadedAt: number | undefined): V | undefined {
    const e = this.map.get(key);
    if (!e) return undefined;
    if (loadedAt !== undefined && loadedAt >= e.until) {
      this.map.delete(key);
      return undefined;
    }
    return e.value;
  }
}

export const cellKey = (r: number, c: number) => `${r},${c}`;

type Block = {
  rows: CellValue[][];
  /** Stable record id of each row. */
  ids: number[];
  /** The table generation its positions were read at (NaN when its parts disagree). */
  gen: number;
  /** The cache generation its load started at. */
  loadedAt: number;
  bytes: number;
  /** When the grid last wanted it (for least-recently-used eviction). */
  used: number;
};

/** Rough memory cost of some rows: strings 2 bytes per char + 16, other values 16, 16 per row. */
export function estimateBytes(rows: CellValue[][]): number {
  let n = 0;
  for (const row of rows) {
    n += 16;
    for (const v of row) n += typeof v === "string" ? 2 * v.length + 16 : 16;
  }
  return n;
}

const key = (rb: number, cb: number) => rb * KEY_SPAN + cb;
const rowBlockOf = (k: number) => Math.floor(k / KEY_SPAN);
const colBlockOf = (k: number) => k % KEY_SPAN;

/**
 * Block cache for view rows, in blocks of ROW_BLOCK rows by COL_BLOCK columns. Fetches only
 * what the grid shows (plus one block of margin each way), skips blocks that scrolled out
 * of view before their turn, and keeps stale rows on screen while a refresh is in flight
 * so edits from collaborators don't flicker. Memory is bounded by an estimated byte size;
 * blocks on screen are never evicted.
 */
export class RowCache {
  private blocks = new Map<number, Block>();
  private stale = new Map<number, Block>();
  private queue: number[] = [];
  private inFlight = new Set<number>();
  private generation = 0;
  private tick = 0;
  private total = 0;
  /** Row and column blocks wanted (visible plus margin), and those actually visible. */
  private wanted = { r0: 0, r1: -1, c0: 0, c1: -1 };
  private visible = { r0: 0, r1: -1, c0: 0, c1: -1 };
  viewId = 0;
  /** Columns of the view (column blocks beyond it are never requested). */
  columns = 0;
  /** The view's current table generation (see `ViewInfo.generation`). */
  viewGeneration = 0;
  /** Record ids learned without a fetch (rows this client just appended), valid for one
   *  generation: positions name other rows once it changes. */
  private known = { generation: NaN, ids: new Map<number, number>() };
  private own = new OwnWrites<CellValue>();
  readonly maxBytes: number;

  constructor(
    private rpc: FetchRpc,
    private onLoaded: () => void,
    private onError: (e: Error) => void,
    options: { maxBytes?: number } = {},
  ) {
    this.maxBytes = options.maxBytes ?? DEFAULT_CACHE_BYTES;
  }

  /** Cached value, or undefined while loading. */
  value(r: number, c: number): CellValue | undefined {
    const k = key(Math.floor(r / ROW_BLOCK), Math.floor(c / COL_BLOCK));
    const block = this.blocks.get(k) ?? this.stale.get(k);
    const own = this.own.get(cellKey(r, c), block?.loadedAt);
    if (own !== undefined) return own;
    const row = block?.rows[r % ROW_BLOCK];
    return row ? row[c % COL_BLOCK] : undefined;
  }

  /** Edits of these cells were saved (refresh first) or refused (`ok` false). */
  settled(cells: string[], ok: boolean): void {
    if (ok) this.own.committed(cells, this.generation);
    else this.own.drop(cells);
  }

  /** Rows `start…` of generation `generation` have these record ids (consecutive from `first`). */
  learn(start: number, first: number, count: number, generation: number): void {
    if (this.known.generation !== generation) this.known = { generation, ids: new Map() };
    for (let k = 0; k < count && k < 10_000; k++) this.known.ids.set(start + k, first + k);
  }

  /** Record id of a row from any loaded block of it (for edits by record). */
  record(r: number): number | undefined {
    if (this.known.generation === this.viewGeneration) {
      const id = this.known.ids.get(r);
      if (id !== undefined) return id;
    }
    const rb = Math.floor(r / ROW_BLOCK);
    const colBlocks = Math.max(1, Math.ceil(this.columns / COL_BLOCK));
    for (const map of [this.blocks, this.stale]) {
      for (let cb = 0; cb < colBlocks; cb++) {
        const id = map.get(key(rb, cb))?.ids[r % ROW_BLOCK];
        if (id !== undefined) return id;
      }
    }
    return undefined;
  }

  /** Table generations of the rows [r0, r1] on screen (fresh or stale blocks). One value
   *  means the positions shown all belong to it; none, that nothing there is loaded. */
  generations(r0: number, r1: number): Set<number> {
    const out = new Set<number>();
    const colBlocks = Math.max(1, Math.ceil(this.columns / COL_BLOCK));
    for (let rb = Math.floor(r0 / ROW_BLOCK); rb <= Math.floor(r1 / ROW_BLOCK); rb++) {
      for (let cb = 0; cb < colBlocks; cb++) {
        const b = this.blocks.get(key(rb, cb)) ?? this.stale.get(key(rb, cb));
        if (b) out.add(b.gen);
      }
    }
    return out;
  }

  /** Estimated size of everything cached, fresh and stale. */
  bytes(): number {
    return this.total;
  }

  /** Number of cached blocks, fresh and stale (for tests). */
  blockCount(): number {
    return this.blocks.size + this.stale.size;
  }

  /** Make sure rows [first, last] of columns [c0, c1] are (being) loaded. */
  ensure(first: number, last: number, c0 = 0, c1 = COL_BLOCK - 1): void {
    const maxCb = this.columns > 0 ? Math.ceil(this.columns / COL_BLOCK) - 1 : Infinity;
    const vr0 = Math.floor(first / ROW_BLOCK), vr1 = Math.floor(last / ROW_BLOCK);
    const vc0 = Math.min(Math.floor(Math.max(0, c0) / COL_BLOCK), maxCb), vc1 = Math.min(Math.floor(Math.max(0, c1) / COL_BLOCK), maxCb);
    this.visible = { r0: vr0, r1: vr1, c0: vc0, c1: vc1 };
    this.wanted = { r0: Math.max(0, vr0 - 1), r1: vr1 + 1, c0: Math.max(0, vc0 - 1), c1: Math.min(vc1 + 1, maxCb) };
    const tick = ++this.tick;
    const visibleFirst: number[] = [], margin: number[] = [];
    const { r0, r1, c0: wc0, c1: wc1 } = this.wanted;
    for (let rb = r0; rb <= r1; rb++) {
      for (let cb = wc0; cb <= wc1; cb++) {
        const k = key(rb, cb);
        const fresh = this.blocks.get(k);
        if (fresh) { fresh.used = tick; continue; }
        const old = this.stale.get(k);
        if (old) old.used = tick;
        if (this.inFlight.has(k)) continue;
        (this.isVisible(k) ? visibleFirst : margin).push(k);
      }
    }
    // Visible blocks first; blocks that scrolled away before their turn are dropped.
    this.queue = [...visibleFirst, ...margin];
    this.pump();
  }

  /** Drop everything (view changed shape). */
  reset(viewId: number, columns = 0): void {
    this.viewId = viewId;
    this.columns = columns;
    this.known = { generation: NaN, ids: new Map() };
    this.own.clear();
    this.generation++;
    this.blocks.clear();
    this.stale.clear();
    this.total = 0;
    this.queue = [];
    this.inFlight.clear();
  }

  /** Refetch on next draw but keep showing current rows meanwhile. */
  refresh(): void {
    this.generation++;
    for (const [k, b] of this.blocks) {
      this.dropFresh(k);
      this.putStale(k, b);
    }
    this.queue = [];
    this.inFlight.clear();
  }

  /** Optimistically show an edit before the server confirms it. */
  patch(r: number, c: number, value: CellValue): void {
    this.own.write(cellKey(r, c), value);
    const k = key(Math.floor(r / ROW_BLOCK), Math.floor(c / COL_BLOCK));
    const block = this.blocks.get(k) ?? this.stale.get(k);
    const row = block?.rows[r % ROW_BLOCK];
    if (!block || !row) return;
    const cost = (v: CellValue) => (typeof v === "string" ? 2 * v.length + 16 : 16);
    const delta = cost(value) - cost(row[c % COL_BLOCK] ?? null);
    row[c % COL_BLOCK] = value;
    block.bytes += delta;
    this.total += delta;
  }

  private isVisible(k: number): boolean {
    const rb = rowBlockOf(k), cb = colBlockOf(k), v = this.visible;
    return rb >= v.r0 && rb <= v.r1 && cb >= v.c0 && cb <= v.c1;
  }

  private isWanted(k: number): boolean {
    const rb = rowBlockOf(k), cb = colBlockOf(k), w = this.wanted;
    return rb >= w.r0 && rb <= w.r1 && cb >= w.c0 && cb <= w.c1;
  }

  private putFresh(k: number, b: Block): void {
    this.dropFresh(k);
    this.dropStale(k);
    this.blocks.set(k, b);
    this.total += b.bytes;
  }

  private putStale(k: number, b: Block): void {
    this.dropStale(k);
    this.stale.set(k, b);
    this.total += b.bytes;
  }

  private dropFresh(k: number): void {
    const b = this.blocks.get(k);
    if (b) { this.total -= b.bytes; this.blocks.delete(k); }
  }

  private dropStale(k: number): void {
    const b = this.stale.get(k);
    if (b) { this.total -= b.bytes; this.stale.delete(k); }
  }

  /** Evict least recently wanted blocks, those outside the wanted region first, never visible ones. */
  private evict(): void {
    if (this.total <= this.maxBytes) return;
    const candidates: { k: number; fresh: boolean; far: boolean; used: number }[] = [];
    for (const [map, fresh] of [[this.stale, false], [this.blocks, true]] as const) {
      for (const [k, b] of map) {
        if (!this.isVisible(k)) candidates.push({ k, fresh, far: !this.isWanted(k), used: b.used });
      }
    }
    // Far before near, then stale before fresh, then oldest first.
    candidates.sort((a, b) => Number(b.far) - Number(a.far) || Number(a.fresh) - Number(b.fresh) || a.used - b.used);
    for (const c of candidates) {
      if (this.total <= this.maxBytes) break;
      if (c.fresh) this.dropFresh(c.k);
      else this.dropStale(c.k);
    }
  }

  private pump(): void {
    while (this.inFlight.size < MAX_IN_FLIGHT && this.queue.length) {
      const k = this.queue.shift()!;
      if (!this.isWanted(k) || this.blocks.has(k) || this.inFlight.has(k)) continue;
      this.load(k);
    }
  }

  private async load(k: number): Promise<void> {
    const gen = this.generation;
    const rb = rowBlockOf(k), cb = colBlockOf(k);
    this.inFlight.add(k);
    const block: Block = { rows: [], ids: [], gen: NaN, loadedAt: gen, bytes: 0, used: this.tick };
    try {
      // A reply may stop short of the rows asked for (`truncated`): keep what came and
      // ask for the rest.
      for (;;) {
        const start = rb * ROW_BLOCK + block.rows.length;
        const res = await this.rpc.call("fetch", {
          view_id: this.viewId, row_start: start, row_count: ROW_BLOCK - block.rows.length,
          column_start: cb * COL_BLOCK, column_count: COL_BLOCK,
        });
        if (gen !== this.generation) return;
        // Error values of spreadsheet-formula columns arrive beside the rows; show their codes.
        for (const e of res.errors ?? []) res.rows[e.row][e.column] = e.code;
        block.gen = block.rows.length === 0 || block.gen === res.generation ? res.generation : NaN;
        block.rows.push(...res.rows);
        block.ids.push(...res.ids);
        block.bytes = estimateBytes(block.rows);
        const more = res.truncated && res.rows.length > 0 && block.rows.length < ROW_BLOCK;
        if (more) {
          // Show the part that arrived; the stale copy (if any) fills the rest meanwhile.
          if (!this.stale.has(k)) { this.dropFresh(k); this.blocks.set(k, block); this.total += block.bytes; }
          this.onLoaded();
          if (!this.isWanted(k)) { this.dropFresh(k); return; }
          continue;
        }
        this.putFresh(k, block);
        break;
      }
      this.evict();
      this.onLoaded();
    } catch (e) {
      if (gen === this.generation) {
        this.dropFresh(k);
        this.onError(e as Error);
      }
    } finally {
      if (gen === this.generation) this.inFlight.delete(k);
      this.pump();
    }
  }
}

export const FETCH_LIMIT = 5000;

const SHEET_BLOCK = 100;

/** Block cache for IronCalc sheets (display values, alignment and formula text). */
export class SheetCache {
  private blocks = new Map<number, (SheetCell | null)[][]>();
  private stale = new Map<number, (SheetCell | null)[][]>();
  /** The sheet layout each block (fresh or stale) was read at. */
  private layouts = new Map<number, number>();
  /** The cache generation each block's load started at. */
  private loadedAt = new Map<number, number>();
  private own = new OwnWrites<string>();
  private inFlight = new Set<number>();
  private generation = 0;
  sheet = 0;
  columns = 26;

  constructor(private rpc: Rpc, private onLoaded: () => void, private onError: (e: Error) => void) {}

  cell(r: number, c: number): SheetCell | null | undefined {
    const b = Math.floor(r / SHEET_BLOCK);
    const block = this.blocks.get(b) ?? this.stale.get(b);
    const own = this.own.get(cellKey(r, c), block ? this.loadedAt.get(b) : undefined);
    if (own !== undefined) return own === "" ? null : [own, /^-?\d+(\.\d+)?$/.test(own) ? "right" : "left"];
    if (!block) return undefined;
    return block[r - b * SHEET_BLOCK]?.[c] ?? null;
  }

  /** Edits of these cells were saved (refresh first) or refused (`ok` false). */
  settled(cells: string[], ok: boolean): void {
    if (ok) this.own.committed(cells, this.generation);
    else this.own.drop(cells);
  }

  /** Layouts of the rows [r0, r1] on screen; see `RowCache.generations`. */
  layoutsIn(r0: number, r1: number): Set<number> {
    const out = new Set<number>();
    for (let b = Math.floor(r0 / SHEET_BLOCK); b <= Math.floor(r1 / SHEET_BLOCK); b++) {
      const l = this.layouts.get(b);
      if (l !== undefined) out.add(l);
    }
    return out;
  }

  ensure(first: number, last: number): void {
    for (let b = Math.floor(first / SHEET_BLOCK); b <= Math.floor(last / SHEET_BLOCK); b++) {
      if (!this.blocks.has(b) && !this.inFlight.has(b)) this.load(b);
    }
  }

  reset(sheet: number, columns: number): void {
    this.sheet = sheet;
    this.columns = columns;
    this.generation++;
    this.blocks.clear();
    this.stale.clear();
    this.layouts.clear();
    this.loadedAt.clear();
    this.own.clear();
    this.inFlight.clear();
  }

  refresh(): void {
    this.generation++;
    for (const [b, rows] of this.blocks) this.stale.set(b, rows);
    this.blocks.clear();
    this.inFlight.clear();
  }

  patch(r: number, c: number, value: string): void {
    this.own.write(cellKey(r, c), value);
    const b = Math.floor(r / SHEET_BLOCK);
    const block = this.blocks.get(b);
    if (!block) return;
    const row = (block[r - b * SHEET_BLOCK] ??= []);
    row[c] = value === "" ? null : [value, /^-?\d+(\.\d+)?$/.test(value) ? "right" : "left"];
  }

  private async load(b: number): Promise<void> {
    const gen = this.generation;
    this.inFlight.add(b);
    try {
      const res = await this.rpc.call("sheet_fetch", {
        sheet: this.sheet, row_start: b * SHEET_BLOCK, row_count: SHEET_BLOCK, column_count: this.columns,
      });
      if (gen !== this.generation) return;
      this.blocks.set(b, res.rows);
      this.layouts.set(b, res.layout);
      this.loadedAt.set(b, gen);
      this.stale.delete(b);
      this.onLoaded();
    } catch (e) {
      if (gen === this.generation) this.onError(e as Error);
    } finally {
      if (gen === this.generation) this.inFlight.delete(b);
    }
  }
}
