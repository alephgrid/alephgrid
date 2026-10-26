import type { Imported, OpName, Params, Results, ServerEvent, SheetInfo, TableInfo, WorkbookListing, WorkbookMeta, WorkbookPatch, Workspace } from "./types";

const TOKEN_KEY = "rowfinity:token";

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
  constructor(public status: number, message: string) {
    super(message);
  }
}

async function http<T>(method: string, path: string, body?: unknown): Promise<T> {
  const res = await fetch(path, {
    method,
    headers: { ...authHeaders(), ...(body ? { "Content-Type": "application/json" } : {}) },
    body: body ? JSON.stringify(body) : undefined,
  });
  if (!res.ok) {
    let msg = res.statusText;
    try { msg = (await res.json()).error ?? msg; } catch { /* not json */ }
    throw new HttpError(res.status, msg);
  }
  if (res.status === 204) return undefined as T;
  return res.json();
}

export const api = {
  config: () => http<{ version: string; auth_required: boolean; allow_sql: boolean }>("GET", "/api/config"),
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
  xlsxUrl(id: string): string {
    const t = token();
    return `/api/workbooks/${id}/export.xlsx${t ? `?token=${encodeURIComponent(t)}` : ""}`;
  },
  exportUrl(id: string, spec: unknown, format: string): string {
    const q = new URLSearchParams({ spec: JSON.stringify(spec), format });
    const t = token();
    if (t) q.set("token", t);
    return `/api/workbooks/${id}/export?${q}`;
  },
};

/** Upload a file with progress; the server streams it to disk and imports it with DuckDB. */
export function uploadFile(
  workbookId: string,
  file: File,
  onProgress: (fraction: number) => void,
): Promise<Imported> {
  return new Promise((resolve, reject) => {
    const xhr = new XMLHttpRequest();
    xhr.open("POST", `/api/workbooks/${workbookId}/import?filename=${encodeURIComponent(file.name)}`);
    const t = token();
    if (t) xhr.setRequestHeader("Authorization", `Bearer ${t}`);
    xhr.upload.onprogress = (e) => e.lengthComputable && onProgress(e.loaded / e.total);
    xhr.onload = () => {
      let body: any = null;
      try { body = JSON.parse(xhr.responseText); } catch { /* ignore */ }
      if (xhr.status >= 200 && xhr.status < 300) resolve(body);
      else reject(new Error(body?.error ?? `upload failed (${xhr.status})`));
    };
    xhr.onerror = () => reject(new Error("upload failed: network error"));
    xhr.send(file);
  });
}

type Pending = { resolve: (v: any) => void; reject: (e: Error) => void };

/** JSON-RPC-ish client over one WebSocket per workbook, with automatic reconnect. */
export class Rpc {
  private ws: WebSocket | null = null;
  private pending = new Map<number, Pending>();
  private nextId = 1;
  private closed = false;
  private retry = 0;
  session = 0;
  onEvent: (e: ServerEvent) => void = () => {};
  onStatus: (s: "connecting" | "open" | "closed") => void = () => {};
  /** Called after a reconnect so views can be reopened. */
  onReconnect: () => void = () => {};

  constructor(private workbookId: string) {}

  connect(): Promise<void> {
    return new Promise((resolve, reject) => {
      const proto = location.protocol === "https:" ? "wss" : "ws";
      const t = token();
      const url = `${proto}://${location.host}/ws/${this.workbookId}${t ? `?token=${encodeURIComponent(t)}` : ""}`;
      this.onStatus("connecting");
      const ws = new WebSocket(url);
      let opened = false;
      ws.onopen = () => {
        opened = true;
        const reconnecting = this.retry > 0;
        this.retry = 0;
        this.onStatus("open");
        resolve();
        if (reconnecting) this.onReconnect();
      };
      ws.onmessage = (m) => this.receive(m.data);
      ws.onclose = () => {
        this.ws = null;
        for (const p of this.pending.values()) p.reject(new Error("connection lost"));
        this.pending.clear();
        this.onStatus("closed");
        if (!opened && this.retry === 0) reject(new Error("could not connect to workbook"));
        if (!this.closed) {
          const delay = Math.min(10_000, 500 * 2 ** this.retry++);
          setTimeout(() => !this.closed && this.connect().catch(() => {}), delay);
        }
      };
      this.ws = ws;
    });
  }

  close(): void {
    this.closed = true;
    this.ws?.close();
  }

  /** Send one operation; request and result types come from the generated protocol. */
  call<O extends OpName>(op: O, ...params: {} extends Params<O> ? [Params<O>?] : [Params<O>]): Promise<Results[O]> {
    const ws = this.ws;
    if (!ws || ws.readyState !== WebSocket.OPEN) return Promise.reject(new Error("not connected"));
    const id = this.nextId++;
    ws.send(JSON.stringify({ id, op, ...(params[0] ?? {}) }));
    return new Promise<Results[O]>((resolve, reject) => this.pending.set(id, { resolve, reject }));
  }

  private receive(data: string): void {
    const msg = JSON.parse(data);
    if (msg.event) {
      if (msg.event === "hello") this.session = msg.session;
      this.onEvent(msg as ServerEvent);
      return;
    }
    const p = this.pending.get(msg.id);
    if (!p) return;
    this.pending.delete(msg.id);
    if (msg.ok) p.resolve(msg.result);
    else p.reject(new Error(msg.error));
  }
}
