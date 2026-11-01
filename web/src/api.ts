import { RevisionTracker } from "./revisions";
import type { ComputeProfile, ErrorCode, ProfileChoice, RuntimeRequest, RuntimeStatus } from "./protocol";
import type {
  AuditEvent, AuthMode, ImportProblem, ImportRequest, Imported, Invite, JobStatus, MemberKind, Org, OrgMember, OrgRole, OpName, Params,
  PublicUser, Results, ServerEvent, SheetInfo, Space, SpaceMember, SpaceRole, TableInfo, Token, UploadCreated, UploadPreview,
  WorkbookListing, WorkbookMeta, WorkbookPatch, QuotaLimits, SpaceUsage, LibraryEntry, LibraryKind, SavedQuery, SavedView, WorkbookFunction,
} from "./types";

/** A space as listed for the signed-in principal. */
export type SpaceListing = { id: string; name: string; org_id: string; org_name: string | null; created_at: number; role: SpaceRole; publish_role: SpaceRole };

/** Who is signed in (`/api/me`). */
export type Me = {
  kind: "local" | "user" | "token";
  user?: PublicUser;
  instance_admin: boolean;
  orgs: { org: Org; role: OrgRole }[];
  expires_at?: number;
};

export type InviteInfo = {
  email: string; organization: string | null; space: string | null; org_role: OrgRole; space_role: SpaceRole | null;
  expires_at: number; existing_user: boolean; sso: boolean;
};

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
    headers: body ? { "Content-Type": "application/json" } : {},
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

/** Thrown for an id that is not a plain path segment (SEC.06). */
export class BadId extends Error {}

/**
 * An id from the server, as one URL path segment. Ids are `[A-Za-z0-9_-]{1,64}`; anything
 * else (`..`, a slash) is refused so it cannot retarget a request (SEC.06).
 */
const ID = /^[A-Za-z0-9_-]{1,64}$/;
export const isId = (id: string): boolean => ID.test(id);
export function seg(id: string | number): string {
  const s = String(id);
  if (!ID.test(s)) throw new BadId(`invalid id ${JSON.stringify(s).slice(0, 80)}`);
  return encodeURIComponent(s);
}

export const api = {
  config: () => http<{
    version: string; auth: AuthMode; setup_required: boolean; sso: { label: string } | null; allow_sql: boolean; sheet_copy_rows?: number;
    max_message_bytes?: number; max_upload_bytes?: number; max_edit_cells?: number; profiles?: ComputeProfile[];
  }>("GET", "/api/config"),
  me: () => http<Me>("GET", "/api/me"),
  login: (email: string, password: string) => http<Me>("POST", "/api/auth/login", { email, password }),
  logout: () => http<void>("POST", "/api/auth/logout"),
  setup: (code: string, email: string, name: string, password: string) => http<Me>("POST", "/api/auth/setup", { code, email, name, password }),
  changePassword: (current: string, next: string) => http<void>("POST", "/api/me/password", { current, new: next }),
  lookupInvite: (code: string) => http<InviteInfo>("POST", "/api/invites/lookup", { code }),
  acceptInvite: (code: string, name: string, password: string) => http<Me>("POST", "/api/invites/accept", { code, name, password }),
  // A listing entry whose id is not a plain segment is dropped, never linked (SEC.06).
  list: () => http<WorkbookListing[]>("GET", "/api/workbooks").then((ws) => ws.filter((w) => isId(w.id))),
  create: (name: string, space?: string | null) => http<WorkbookMeta>("POST", "/api/workbooks", { name, space: space ?? null }),
  get: (id: string) => http<{ workbook: WorkbookMeta; role: SpaceRole; tables: TableInfo[]; sheets: SheetInfo[] }>("GET", `/api/workbooks/${seg(id)}`),
  update: (id: string, patch: WorkbookPatch) =>
    http<WorkbookMeta>("PATCH", `/api/workbooks/${seg(id)}`, patch),
  rename: (id: string, name: string) => http<WorkbookMeta>("PATCH", `/api/workbooks/${seg(id)}`, { name }),
  duplicate: (id: string) => http<WorkbookMeta>("POST", `/api/workbooks/${seg(id)}/duplicate`),
  /** Moves the workbook to the trash. */
  remove: (id: string) => http<void>("DELETE", `/api/workbooks/${seg(id)}`),
  trash: () => http<WorkbookListing[]>("GET", "/api/trash").then((ws) => ws.filter((w) => isId(w.id))),
  restore: (id: string) => http<WorkbookMeta>("POST", `/api/trash/${seg(id)}/restore`),
  purge: (id: string) => http<void>("DELETE", `/api/trash/${seg(id)}`),
  emptyTrash: () => http<void>("DELETE", "/api/trash"),
  spaces: () => http<SpaceListing[]>("GET", "/api/spaces"),
  createSpace: (name: string, org?: string) => http<Space>("POST", "/api/spaces", { name, org: org ?? null }),
  renameSpace: (id: string, name: string) => http<Space>("PATCH", `/api/spaces/${seg(id)}`, { name }),
  removeSpace: (id: string) => http<void>("DELETE", `/api/spaces/${seg(id)}`),
  spaceMembers: (id: string) => http<SpaceMember[]>("GET", `/api/spaces/${seg(id)}/members`),
  setSpaceMember: (id: string, kind: MemberKind, principal: string, role: SpaceRole) =>
    http<void>("PUT", `/api/spaces/${seg(id)}/members`, { kind, principal, role }),
  removeSpaceMember: (id: string, kind: MemberKind, principal: string) =>
    http<void>("DELETE", `/api/spaces/${seg(id)}/members/${seg(kind)}/${encodeURIComponent(principal)}`),
  tokens: (space: string) => http<Token[]>("GET", `/api/spaces/${seg(space)}/tokens`),
  createToken: (space: string, name: string, role: SpaceRole, days: number) =>
    http<{ token: Token; secret: string }>("POST", `/api/spaces/${seg(space)}/tokens`, { name, role, days }),
  revokeToken: (space: string, token: string) => http<void>("DELETE", `/api/spaces/${seg(space)}/tokens/${seg(token)}`),
  orgMembers: (org: string) => http<OrgMember[]>("GET", `/api/orgs/${seg(org)}/members`),
  setOrgRole: (org: string, user: string, role: OrgRole) => http<void>("PATCH", `/api/orgs/${seg(org)}/members/${seg(user)}`, { role }),
  removeOrgMember: (org: string, user: string) => http<void>("DELETE", `/api/orgs/${seg(org)}/members/${seg(user)}`),
  invites: (org: string) => http<Invite[]>("GET", `/api/orgs/${seg(org)}/invites`),
  invite: (org: string, email: string, org_role: OrgRole, space: string | null, space_role: SpaceRole | null, days = 7) =>
    http<{ invite: Invite; code: string; path: string }>("POST", `/api/orgs/${seg(org)}/invites`, { email, org_role, space, space_role, days }),
  revokeInvite: (org: string, invite: string) => http<void>("DELETE", `/api/orgs/${seg(org)}/invites/${seg(invite)}`),
  audit: (org: string, before?: number) => http<AuditEvent[]>("GET", `/api/orgs/${seg(org)}/audit${before ? `?before=${before}` : ""}`),
  library: (space: string) => http<LibraryEntry[]>("GET", `/api/spaces/${seg(space)}/library`),
  publishToLibrary: (space: string, workbook: string, kind: LibraryKind, object: string) =>
    http<LibraryEntry>("POST", `/api/spaces/${seg(space)}/library`, { workbook, kind, object }),
  changeLibraryEntry: (space: string, entry: string, patch: { name?: string; retired?: boolean }) =>
    http<void>("PATCH", `/api/spaces/${seg(space)}/library/${seg(entry)}`, patch),
  useLibraryEntry: (workbook: string, entry: string) => http<{ result: unknown; revision: number }>("POST", `/api/workbooks/${seg(workbook)}/library`, { entry }),
  shareable: (workbook: string) => http<{ queries: SavedQuery[]; views: SavedView[]; functions: WorkbookFunction[] }>("GET", `/api/workbooks/${seg(workbook)}/shareable`),
  setPublishRole: (space: string, role: SpaceRole) => http<Space>("PATCH", `/api/spaces/${seg(space)}`, { publish_role: role }),
  usage: (org: string) => http<SpaceUsage[]>("GET", `/api/orgs/${seg(org)}/usage`),
  setQuota: (space: string, limits: QuotaLimits) => http<SpaceUsage | null>("PUT", `/api/spaces/${seg(space)}/quota`, limits),
  setSpaceProfiles: (space: string, profiles: string[]) => http<SpaceUsage | null>("PUT", `/api/spaces/${seg(space)}/profiles`, { profiles }),
  /** The workbook's compute; starts nothing. */
  runtime: (id: string) => http<RuntimeStatus>("GET", `/api/workbooks/${seg(id)}/runtime`),
  runtimeAction: (id: string, req: RuntimeRequest) => http<RuntimeStatus>("POST", `/api/workbooks/${seg(id)}/runtime`, req),
  profiles: (id: string) => http<ProfileChoice[]>("GET", `/api/workbooks/${seg(id)}/profiles`),
  setUserDisabled: (user: string, disabled: boolean) => http<PublicUser | null>("PATCH", `/api/users/${seg(user)}`, { disabled }),
  previewUpload: (id: string, upload: string, req: ImportRequest) =>
    http<UploadPreview>("POST", `/api/workbooks/${seg(id)}/uploads/${seg(upload)}/preview`, req),
  importUpload: (id: string, upload: string, req: ImportRequest) =>
    http<Imported>("POST", `/api/workbooks/${seg(id)}/uploads/${seg(upload)}/import`, req),
  cancelUpload: (id: string, upload: string) => http<void>("DELETE", `/api/workbooks/${seg(id)}/uploads/${seg(upload)}`),
  jobStatus: (id: string, job: string) => http<JobStatus>("GET", `/api/workbooks/${seg(id)}/jobs/${seg(job)}`),
  cancelJob: (id: string, job: string) => http<void>("DELETE", `/api/workbooks/${seg(id)}/jobs/${seg(job)}`),
  // Downloads are authorized by the session cookie; no secret goes into these URLs.
  xlsxUrl(id: string): string {
    return `/api/workbooks/${seg(id)}/export.xlsx`;
  },
  /** A finished `export_query` file; it can be downloaded once. */
  downloadUrl(id: string, download: string): string {
    return `/api/workbooks/${seg(id)}/downloads/${seg(download)}`;
  },
  exportUrl(id: string, spec: unknown, format: string): string {
    const q = new URLSearchParams({ spec: JSON.stringify(spec), format });
    return `/api/workbooks/${seg(id)}/export?${q}`;
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
    xhr.open("POST", `/api/workbooks/${seg(workbookId)}/uploads?filename=${encodeURIComponent(file.name)}`);
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
  /** For a `conflict`: what the change addressed moved or is gone (refresh; nothing to overwrite). */
  moved = false;
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

/**
 * Whether this page's changes are saved: "saved" when every change it sent was acknowledged
 * (the server acknowledges after commit, docs/PROTOCOL.md), "saving" while some are in flight,
 * "unsaved" while some wait for the connection, "failed" when one was dropped unsaved (until
 * the next change is acknowledged).
 */
export type SaveState = "saved" | "saving" | "unsaved" | "failed";

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
  /** Not reconnecting until `reconnectNow` (the workbook was paused while idle). */
  private paused = false;
  session = 0;
  /** Revisions announced so far; a skip makes the client refresh (see `RevisionTracker`). */
  readonly revisions = new RevisionTracker();
  private greeted = false;
  onEvent: (e: ServerEvent) => void = () => {};
  onStatus: (s: "connecting" | "open" | "closed") => void = () => {};
  /** Called when `saveState` changes. */
  onSaveState: (s: SaveState) => void = () => {};
  private lastSave: SaveState = "saved";
  private lostChange = false;

  /** See `SaveState`. */
  get saveState(): SaveState {
    const unacked = [...this.pending.values()].filter((p) => p.mutation).length
      + this.kept.length + this.connecting.filter((p) => p.mutation).length;
    if (unacked) return this.ws?.readyState === WebSocket.OPEN ? "saving" : "unsaved";
    return this.lostChange ? "failed" : "saved";
  }

  private notifySave(): void {
    if (this.closed) return;
    const s = this.saveState;
    if (s !== this.lastSave) { this.lastSave = s; this.onSaveState(s); }
  }

  /** A change was given up without being saved (the connection did not come back in time). */
  private lose(p: Pending, message: string): void {
    this.lostChange = true;
    p.reject(new RpcError(message, "unavailable"));
  }
  /** Called after a reconnect so views can be reopened; kept changes are resent once it settles. */
  onReconnect: () => void | Promise<void> = () => {};
  /** Called when a (re)connect attempt fails before the socket opened. */
  onConnectFailed: () => void = () => {};

  constructor(private workbookId: string) {}

  connect(): Promise<void> {
    return new Promise((resolve, reject) => {
      const proto = location.protocol === "https:" ? "wss" : "ws";
      const url = `${proto}://${location.host}/ws/${seg(this.workbookId)}`;
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
        this.notifySave();
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
        this.notifySave();
        if (!opened) {
          if (!this.opened && this.retry === 0) reject(new Error("could not connect to workbook"));
          this.onConnectFailed();
        }
        if (!this.closed && !this.paused) {
          const delay = Math.min(10_000, 500 * 2 ** this.retry++);
          this.retryTimer = window.setTimeout(() => !this.closed && this.connect().catch(() => {}), delay);
        }
      };
      this.ws = ws;
    });
  }

  /** Try to reconnect now instead of waiting for the next retry (ends a pause). */
  reconnectNow(): void {
    this.paused = false;
    if (this.closed || this.ws) return;
    clearTimeout(this.retryTimer);
    this.connect().catch(() => {});
  }

  /** Stop reconnecting by itself until `reconnectNow`: the server closed an idle workbook. */
  pause(): void {
    this.paused = true;
    clearTimeout(this.retryTimer);
  }

  get isPaused(): boolean {
    return this.paused;
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
    this.onSaveState = () => {};
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
      this.notifySave();
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
      this.lose(this.kept.shift()!, "connection lost: too many unsaved changes");
    }
    const t = window.setTimeout(() => {
      this.timers.delete(t);
      const i = this.kept.indexOf(p);
      if (i >= 0) {
        this.kept.splice(i, 1);
        this.lose(p, "connection lost: the change could not be saved");
        this.notifySave();
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
    this.notifySave();
  }

  private receive(data: string): void {
    const msg = JSON.parse(data);
    if (msg.event) {
      if (msg.event === "hello") {
        this.session = msg.session;
        // A reconnect refreshes everything anyway (`onReconnect`); the first hello starts the count.
        this.revisions.hello(msg.revision);
        this.greeted = true;
      }
      let gap = false;
      if (msg.event === "resync") this.revisions.caughtUp(msg.revision ?? this.revisions.last);
      else if (typeof msg.revision === "number" && msg.event !== "hello" && this.greeted) gap = this.revisions.see(msg.revision);
      // The server closes the connection next; reconnecting would be refused.
      if (msg.event === "access_revoked") {
        this.closed = true;
        clearTimeout(this.retryTimer);
      }
      if (msg.event === "progress") {
        for (const [id, p] of this.pending) if (id === msg.request) p.opts.onProgress?.(msg.percentage);
      }
      this.onEvent(msg as ServerEvent);
      // A revision was never announced: refresh what is shown, as after dropped events.
      if (gap) this.onEvent({ event: "resync", revision: msg.revision } as ServerEvent);
      return;
    }
    const p = this.pending.get(msg.id);
    if (!p) return;
    this.pending.delete(msg.id);
    if (msg.ok) {
      if (p.mutation) this.lostChange = false;
      p.resolve(msg.result);
      this.notifySave();
      return;
    }
    try { this.settleFailure(p, msg); } finally { this.notifySave(); }
  }

  /** A request failed: retry a busy read, keep a change the server could not run yet, or reject. */
  private settleFailure(p: Pending, msg: { error: string; code?: ErrorCode; moved?: boolean }): void {
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
    const err = new RpcError(msg.error, msg.code);
    err.moved = !!msg.moved;
    p.reject(err);
  }
}
