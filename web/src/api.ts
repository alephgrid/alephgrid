import type { ErrorCode } from "./protocol";
import type {
  ImportProblem, ImportRequest, Imported, JobStatus, OpName, Params, Results, ServerEvent, SheetInfo, TableInfo, UploadCreated,
  UploadPreview, WorkbookListing, WorkbookMeta, WorkbookPatch, Workspace,
} from "./types";

const TOKEN_KEY = "alephgrid:token";

/** Pick up `?token=` once, remember it, and remove it from the address bar. */
export function initToken(): void {
  const url = new URL(location.href);
  const t = url.searchParams.get("token");
  if (t) {
    try { localStorage.setItem(TOKEN_KEY, t); } catch { /* private mode */ }
    url.searchParams.delete("token");
    history.replaceState(null, "", url.pathname + url.search + url.hash);
  }
}

export function token(): string | null {
  try { return localStorage.getItem(TOKEN_KEY); } catch { return null; }
}

export function setToken(t: string): void {
  try { localStorage.setItem(TOKEN_KEY, t); } catch { /* ignore */ }
}

function authHeaders(): Record<string, string> {
  const t = token();
  return t ? { Authorization: `Bearer ${t}` } : {};
}

export class HttpError extends Error {
  constructor(public status: number, message: string, public problem?: ImportProblem) {
    super(message);
  }
}

/** Opening the workbook failed on the server: incompatible (409), held by another process (423), no capacity or startup failed (503). */
export function isRuntimeFailure(e: unknown): e is HttpError {
  return e instanceof HttpError && (e.status === 409 || e.status === 423 || e.status === 503);
}

async function http<T>(method: string, path: string, body?: unknown): Promise<T> {
  const res = await fetch(path, {
    method,
    headers: { ...authHeaders(), ...(body ? { "Content-Type": "application/json" } : {}) },
    body: body ? JSON.stringify(body) : undefined,
  });
  if (!res.ok) {
    let msg = res.statusText;
    let problem: ImportProblem | undefined;
    try {
      const body = await res.json();
      msg = body.error ?? msg;
      problem = body.problem;
    } catch { /* not json */ }
    throw new HttpError(res.status, msg, problem);
  }
  if (res.status === 204) return undefined as T;
  return res.json();
}

export const api = {
  config: () => http<{
    version: string; auth_required: boolean; allow_sql: boolean; sheet_copy_rows?: number;
    max_message_bytes?: number; max_upload_bytes?: number; max_edit_cells?: number;
  }>("GET", "/api/config"),
  list: () => http<WorkbookListing[]>("GET", "/api/workbooks"),
  create: (name: string, workspace?: string | null) => http<WorkbookMeta>("POST", "/api/workbooks", { name, workspace: workspace ?? null }),
  get: (id: string) => http<{ workbook: WorkbookMeta; tables: TableInfo[]; sheets: SheetInfo[] }>("GET", `/api/workbooks/${id}`),
  update: (id: string, patch: WorkbookPatch) =>
    http<WorkbookMeta>("PATCH", `/api/workbooks/${id}`, patch),
  rename: (id: string, name: string) => http<WorkbookMeta>("PATCH", `/api/workbooks/${id}`, { name }),
  duplicate: (id: string) => http<WorkbookMeta>("POST", `/api/workbooks/${id}/duplicate`),
  /** Moves the workbook to the trash. */
  remove: (id: string) => http<void>("DELETE", `/api/workbooks/${id}`),
  trash: () => http<WorkbookListing[]>("GET", "/api/trash"),
  restore: (id: string) => http<WorkbookMeta>("POST", `/api/trash/${id}/restore`),
  purge: (id: string) => http<void>("DELETE", `/api/trash/${id}`),
  emptyTrash: () => http<void>("DELETE", "/api/trash"),
  workspaces: () => http<Workspace[]>("GET", "/api/workspaces"),
  createWorkspace: (name: string) => http<Workspace>("POST", "/api/workspaces", { name }),
  renameWorkspace: (id: string, name: string) => http<Workspace>("PATCH", `/api/workspaces/${id}`, { name }),
  removeWorkspace: (id: string) => http<void>("DELETE", `/api/workspaces/${id}`),
  previewUpload: (id: string, upload: string, req: ImportRequest) =>
    http<UploadPreview>("POST", `/api/workbooks/${id}/uploads/${upload}/preview`, req),
  importUpload: (id: string, upload: string, req: ImportRequest) =>
    http<Imported>("POST", `/api/workbooks/${id}/uploads/${upload}/import`, req),
  cancelUpload: (id: string, upload: string) => http<void>("DELETE", `/api/workbooks/${id}/uploads/${upload}`),
  jobStatus: (id: string, job: string) => http<JobStatus>("GET", `/api/workbooks/${id}/jobs/${job}`),
  cancelJob: (id: string, job: string) => http<void>("DELETE", `/api/workbooks/${id}/jobs/${job}`),
  xlsxUrl(id: string): string {
    const t = token();
    return `/api/workbooks/${id}/export.xlsx${t ? `?token=${encodeURIComponent(t)}` : ""}`;
  },
  /** A finished `export_query` file; it can be downloaded once. */
  downloadUrl(id: string, download: string): string {
    const t = token();
    return `/api/workbooks/${id}/downloads/${encodeURIComponent(download)}${t ? `?token=${encodeURIComponent(t)}` : ""}`;
  },
  exportUrl(id: string, spec: unknown, format: string): string {
    const q = new URLSearchParams({ spec: JSON.stringify(spec), format });
    const t = token();
    if (t) q.set("token", t);
    return `/api/workbooks/${id}/export?${q}`;
  },
};

/** Stage a file for preview (nothing is imported yet). `abort()` stops the upload. */
export function stageUpload(
  workbookId: string,
  file: File,
  onProgress: (fraction: number) => void,
): { done: Promise<UploadCreated>; abort: () => void } {
  const xhr = new XMLHttpRequest();
  const done = new Promise<UploadCreated>((resolve, reject) => {
    xhr.open("POST", `/api/workbooks/${workbookId}/uploads?filename=${encodeURIComponent(file.name)}`);
    const t = token();
    if (t) xhr.setRequestHeader("Authorization", `Bearer ${t}`);
    xhr.upload.onprogress = (e) => e.lengthComputable && onProgress(e.loaded / e.total);
    xhr.onload = () => {
      let body: any = null;
      try { body = JSON.parse(xhr.responseText); } catch { /* ignore */ }
      if (xhr.status >= 200 && xhr.status < 300) resolve(body);
      else reject(new HttpError(xhr.status, body?.error ?? `upload failed (${xhr.status})`, body?.problem));
    };
    xhr.onerror = () => reject(new Error("upload failed: network error"));
    xhr.onabort = () => reject(new DOMException("upload cancelled", "AbortError"));
    xhr.send(file);
  });
  return { done, abort: () => xhr.abort() };
}

/** Operations that change nothing: never given a mutation id, retried when the server is busy. */
const READS = new Set<OpName>([
  "tables", "open_view", "update_view", "view_info", "close_view", "fetch", "stats", "query", "sheets", "sheet_fetch",
  "analyze", "analyses", "replace_preview", "presentation", "functions", "test_function", "saved_views", "history", "cancel", "schema", "saved_queries", "query_versions", "materialized",
  // Writes a file to download but changes no data: never kept and resent after a reconnect.
  "export_query",
]);
/** Backoff before retrying a read the server refused as "overloaded". */
const OVERLOAD_RETRIES = [200, 400, 800];
/** Changes kept across a dropped connection: how long, and how many. */
const MUTATION_KEEP_MS = 30_000;
const MUTATION_KEEP_MAX = 100;

/** A failed request; `code` says why when the server knows (see `ErrorCode`). */
export class RpcError extends Error {
  constructor(message: string, public code?: ErrorCode) {
    super(message);
  }
}

/** Random id that makes a retried change apply once. */
function mutationId(): string {
  if (typeof crypto.randomUUID === "function") return crypto.randomUUID();
  const b = crypto.getRandomValues(new Uint8Array(16));
  return [...b].map((x) => x.toString(16).padStart(2, "0")).join("");
}

export type CallOptions = {
  /** Called with the request id each time the request is sent (for `cancel`). */
  onSent?: (id: number) => void;
  /** Progress of a long request, 0–100. */
  onProgress?: (percentage: number) => void;
  /** How long the server should keep working on it. */
  timeoutMs?: number;
};

type Pending = {
  /** The request without its id: resent as is (same mutation id) after a reconnect. */
  frame: Record<string, unknown>;
  mutation: boolean;
  attempt: number;
  opts: CallOptions;
  resolve: (v: any) => void;
  reject: (e: Error) => void;
  /** When it was kept for resending (changes only). */
  keptAt?: number;
};

/** JSON-RPC-ish client over one WebSocket per workbook, with automatic reconnect. */
export class Rpc {
  private ws: WebSocket | null = null;
  private pending = new Map<number, Pending>();
  /** Changes waiting for the connection to come back. */
  private kept: Pending[] = [];
  /** Requests made while the socket was connecting, sent when it opens. */
  private connecting: Pending[] = [];
  /** View ids of the old connection, mapped to their reopened replacements. */
  private viewAlias = new Map<number, number>();
  private timers = new Set<number>();
  private nextId = 1;
  private closed = false;
  private opened = false;
  private retry = 0;
  private retryTimer = 0;
  session = 0;
  onEvent: (e: ServerEvent) => void = () => {};
  onStatus: (s: "connecting" | "open" | "closed") => void = () => {};
  /** Called after a reconnect so views can be reopened; kept changes are resent once it settles. */
  onReconnect: () => void | Promise<void> = () => {};
  /** Called when a (re)connect attempt fails before the socket opened. */
  onConnectFailed: () => void = () => {};

  constructor(private workbookId: string) {}

  connect(): Promise<void> {
    return new Promise((resolve, reject) => {
      const proto = location.protocol === "https:" ? "wss" : "ws";
      const t = token();
      const url = `${proto}://${location.host}/ws/${this.workbookId}${t ? `?token=${encodeURIComponent(t)}` : ""}`;
      this.onStatus("connecting");
      const ws = new WebSocket(url);
      let opened = false;
      ws.onopen = async () => {
        if (this.closed) return;
        opened = true;
        const reconnecting = this.opened;
        this.opened = true;
        this.retry = 0;
        this.onStatus("open");
        for (const p of this.connecting.splice(0)) this.send(p);
        resolve();
        if (!reconnecting) return;
        try { await this.onReconnect(); } catch { /* the screen reports its own errors */ }
        if (this.ws === ws) this.resendKept();
      };
      ws.onmessage = (m) => !this.closed && this.receive(m.data);
      ws.onclose = () => {
        if (this.closed || this.ws !== ws) return;
        this.ws = null;
        for (const p of this.connecting.splice(0)) p.reject(new RpcError("not connected", "unavailable"));
        this.dropped();
        this.onStatus("closed");
        if (!opened) {
          if (!this.opened && this.retry === 0) reject(new Error("could not connect to workbook"));
          this.onConnectFailed();
        }
        if (!this.closed) {
          const delay = Math.min(10_000, 500 * 2 ** this.retry++);
          this.retryTimer = window.setTimeout(() => !this.closed && this.connect().catch(() => {}), delay);
        }
      };
      this.ws = ws;
    });
  }

  /** Try to reconnect now instead of waiting for the next retry. */
  reconnectNow(): void {
    if (this.closed || this.ws) return;
    clearTimeout(this.retryTimer);
    this.connect().catch(() => {});
  }

  /**
   * Close for good: no reconnects, no more events, and replies still in flight are
   * dropped (their promises never settle, so no callback runs after the screen is gone).
   */
  close(): void {
    this.closed = true;
    clearTimeout(this.retryTimer);
    for (const t of this.timers) clearTimeout(t);
    this.timers.clear();
    this.pending.clear();
    this.kept = [];
    this.connecting = [];
    this.onEvent = () => {};
    this.onStatus = () => {};
    this.onReconnect = () => {};
    this.onConnectFailed = () => {};
    const ws = this.ws;
    this.ws = null;
    ws?.close();
  }

  get isClosed(): boolean {
    return this.closed;
  }

  /** Changes waiting to be resent after a reconnect (for tests and status). */
  get keptCount(): number {
    return this.kept.length;
  }

  /** After a reconnect: a change sent for view `from` goes to the reopened view `to`. */
  aliasView(from: number, to: number): void {
    if (from !== to) this.viewAlias.set(from, to);
  }

  /** Send one operation; request and result types come from the generated protocol. */
  call<O extends OpName>(op: O, ...params: {} extends Params<O> ? [Params<O>?] : [Params<O>]): Promise<Results[O]> {
    return this.request(op, (params[0] ?? {}) as Params<O>, {});
  }

  /** `call` with options: the request id (to cancel it), progress, a server-side timeout. */
  request<O extends OpName>(op: O, params: Params<O>, opts: CallOptions): Promise<Results[O]> {
    const mutation = !READS.has(op);
    const frame: Record<string, unknown> = { op, ...params };
    if (mutation) frame.mutation_id = mutationId();
    if (opts.timeoutMs) frame.timeout_ms = opts.timeoutMs;
    return new Promise<Results[O]>((resolve, reject) => {
      const p: Pending = { frame, mutation, attempt: 0, opts, resolve, reject };
      if (this.ws?.readyState === WebSocket.OPEN) this.send(p);
      else if (mutation && this.opened && !this.closed) this.keep(p);
      // Still connecting (a just-loaded page, or a reconnect in progress): send it once the
      // socket opens, or fail if this attempt fails.
      else if (this.ws?.readyState === WebSocket.CONNECTING && !this.closed) this.connecting.push(p);
      else reject(new RpcError("not connected", "unavailable"));
    });
  }

  /** Ask the server to stop request `id` (it then fails with code "cancelled"). */
  cancel(id: number): Promise<boolean> {
    return this.call("cancel", { request: id });
  }

  private send(p: Pending): void {
    const id = this.nextId++;
    const frame: Record<string, unknown> = { ...p.frame, id };
    if (p.mutation && typeof frame.view_id === "number") {
      // Follow reopen after reopen (bounded, in case of a cycle).
      for (let i = 0; i < 16 && this.viewAlias.has(frame.view_id as number); i++) frame.view_id = this.viewAlias.get(frame.view_id as number);
    }
    this.pending.set(id, p);
    this.ws!.send(JSON.stringify(frame));
    p.opts.onSent?.(id);
  }

  /** Keep a change for resending once the connection is back (bounded in time and count). */
  private keep(p: Pending): void {
    p.keptAt ??= Date.now();
    this.kept.push(p);
    while (this.kept.length > MUTATION_KEEP_MAX) {
      this.kept.shift()!.reject(new RpcError("connection lost: too many unsaved changes", "unavailable"));
    }
    const t = window.setTimeout(() => {
      this.timers.delete(t);
      const i = this.kept.indexOf(p);
      if (i >= 0) {
        this.kept.splice(i, 1);
        p.reject(new RpcError("connection lost: the change could not be saved", "unavailable"));
      }
    }, Math.max(0, p.keptAt + MUTATION_KEEP_MS - Date.now()));
    this.timers.add(t);
  }

  /** The socket closed: keep unacknowledged changes, fail reads. */
  private dropped(): void {
    const pending = [...this.pending.values()];
    this.pending.clear();
    for (const p of pending) {
      if (p.mutation) this.keep(p);
      else p.reject(new RpcError("connection lost", "unavailable"));
    }
  }

  private resendKept(): void {
    const kept = this.kept;
    this.kept = [];
    for (const p of kept) {
      if (this.ws?.readyState === WebSocket.OPEN) this.send(p);
      else this.keep(p);
    }
  }

  private receive(data: string): void {
    const msg = JSON.parse(data);
    if (msg.event) {
      if (msg.event === "hello") this.session = msg.session;
      if (msg.event === "progress") {
        for (const [id, p] of this.pending) if (id === msg.request) p.opts.onProgress?.(msg.percentage);
      }
      this.onEvent(msg as ServerEvent);
      return;
    }
    const p = this.pending.get(msg.id);
    if (!p) return;
    this.pending.delete(msg.id);
    if (msg.ok) {
      p.resolve(msg.result);
      return;
    }
    // A busy server: try a read again shortly.
    if (msg.code === "overloaded" && !p.mutation && p.attempt < OVERLOAD_RETRIES.length) {
      const t = window.setTimeout(() => {
        this.timers.delete(t);
        if (this.ws?.readyState === WebSocket.OPEN) this.send(p);
        else p.reject(new RpcError("connection lost", "unavailable"));
      }, OVERLOAD_RETRIES[p.attempt++]);
      this.timers.add(t);
      return;
    }
    // The workbook is closing or restarting: a change may not have run, so send it again
    // after the reconnect (its mutation id keeps it from applying twice).
    if (msg.code === "unavailable" && p.mutation && this.opened && !this.closed) {
      this.keep(p);
      return;
    }
    p.reject(new RpcError(msg.error, msg.code));
  }
}
