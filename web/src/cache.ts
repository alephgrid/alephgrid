import type { Rpc } from "./api";
import type { CellValue, SheetCell } from "./types";

const BLOCK = 100;
const MAX_BLOCKS = 400;
const MAX_IN_FLIGHT = 4;

/**
 * Block cache for view rows. Fetches only what the grid asks for, skips blocks that
 * scrolled out of view before their turn, and keeps stale rows on screen while a
 * refresh is in flight so edits from collaborators don't flicker.
 */
export class RowCache {
  private blocks = new Map<number, CellValue[][]>();
  private stale = new Map<number, CellValue[][]>();
  private queue: number[] = [];
  private inFlight = new Set<number>();
  private generation = 0;
  private wanted: [number, number] = [0, 0];
  viewId = 0;

  constructor(private rpc: Rpc, private onLoaded: () => void, private onError: (e: Error) => void) {}

  row(r: number): CellValue[] | undefined {
    const b = Math.floor(r / BLOCK);
    const block = this.blocks.get(b) ?? this.stale.get(b);
    return block?.[r - b * BLOCK];
  }

  ensure(first: number, last: number): void {
    const b0 = Math.max(0, Math.floor(first / BLOCK) - 1);
    const b1 = Math.floor(last / BLOCK) + 1;
    this.wanted = [b0, b1];
    for (let b = b0; b <= b1; b++) {
      if (!this.blocks.has(b) && !this.inFlight.has(b) && !this.queue.includes(b)) this.queue.push(b);
    }
    this.pump();
  }

  /** Drop everything (view changed shape). */
  reset(viewId: number): void {
    this.viewId = viewId;
    this.generation++;
    this.blocks.clear();
    this.stale.clear();
    this.queue = [];
    this.inFlight.clear();
  }

  /** Refetch on next draw but keep showing current rows meanwhile. */
  refresh(): void {
    this.generation++;
    for (const [b, rows] of this.blocks) this.stale.set(b, rows);
    this.blocks.clear();
    this.queue = [];
    this.inFlight.clear();
  }

  /** Optimistically show an edit before the server confirms it. */
  patch(r: number, c: number, value: CellValue): void {
    const row = this.row(r);
    if (row) row[c] = value;
  }

  private pump(): void {
    while (this.inFlight.size < MAX_IN_FLIGHT && this.queue.length) {
      const b = this.queue.shift()!;
      if (b < this.wanted[0] || b > this.wanted[1]) continue;
      this.load(b);
    }
  }

  private async load(b: number): Promise<void> {
    const gen = this.generation;
    this.inFlight.add(b);
    try {
      const res = await this.rpc.call("fetch", { view_id: this.viewId, row_start: b * BLOCK, row_count: BLOCK });
      if (gen !== this.generation) return;
      this.blocks.set(b, res.rows);
      this.stale.delete(b);
      if (this.blocks.size > MAX_BLOCKS) {
        const [lo, hi] = this.wanted;
        for (const key of this.blocks.keys()) {
          if (this.blocks.size <= MAX_BLOCKS * 0.75) break;
          if (key < lo - 20 || key > hi + 20) this.blocks.delete(key);
        }
      }
      this.onLoaded();
    } catch (e) {
      if (gen === this.generation) this.onError(e as Error);
    } finally {
      if (gen === this.generation) this.inFlight.delete(b);
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
  private inFlight = new Set<number>();
  private generation = 0;
  sheet = 0;
  columns = 26;

  constructor(private rpc: Rpc, private onLoaded: () => void, private onError: (e: Error) => void) {}

  cell(r: number, c: number): SheetCell | null | undefined {
    const b = Math.floor(r / SHEET_BLOCK);
    const block = this.blocks.get(b) ?? this.stale.get(b);
    if (!block) return undefined;
    return block[r - b * SHEET_BLOCK]?.[c] ?? null;
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
    this.inFlight.clear();
  }

  refresh(): void {
    this.generation++;
    for (const [b, rows] of this.blocks) this.stale.set(b, rows);
    this.blocks.clear();
    this.inFlight.clear();
  }

  patch(r: number, c: number, value: string): void {
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
      this.stale.delete(b);
      this.onLoaded();
    } catch (e) {
      if (gen === this.generation) this.onError(e as Error);
    } finally {
      if (gen === this.generation) this.inFlight.delete(b);
    }
  }
}
