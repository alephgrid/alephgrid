// U.10: server administration for instance admins: what the server is (version, sign-in,
// SQL), its resources and the open workbooks, live sessions, backups, usage and limits per
// space, and the audit log. Standalone use stays simple: one dialog, opened from the hub.

import { bytes } from "./compute";
import type { AuditEvent, ResourceUsage, SpaceUsage } from "./protocol";
import { confirmDialog, el, openModal, toast } from "./ui";

type Backup = { name: string; created_at_ms: number; workbooks: number; bytes: number };
type Session = { session: number; workbook: string; outbox_bytes: number; peak_outbox_bytes: number };

async function get<T>(path: string): Promise<T> {
  const r = await fetch(path, { credentials: "same-origin" });
  if (!r.ok) throw new Error((await r.json().catch(() => ({ error: r.statusText }))).error ?? r.statusText);
  return r.json() as Promise<T>;
}

async function send(method: string, path: string): Promise<unknown> {
  const r = await fetch(path, { method, credentials: "same-origin", headers: { "Content-Type": "application/json" } });
  if (!r.ok) throw new Error((await r.json().catch(() => ({ error: r.statusText }))).error ?? r.statusText);
  return r.status === 204 ? null : r.json();
}

function table(caption: string, head: string[], rows: (string | Node)[][]): HTMLElement {
  if (!rows.length) return el("p", { class: "muted" }, "Nothing here yet.");
  return el("table", { class: "result-table admin-table" },
    el("caption", { class: "sr-only" }, caption),
    el("thead", {}, el("tr", {}, ...head.map((h) => el("th", { scope: "col" }, h)))),
    el("tbody", {}, ...rows.map((r) => el("tr", {}, ...r.map((c) => el("td", {}, c))))));
}

const SECTIONS = ["Overview", "Workbooks and memory", "Sessions", "Backups", "Usage and limits", "Audit log"] as const;
type Section = (typeof SECTIONS)[number];

async function render(section: Section, body: HTMLElement): Promise<void> {
  body.replaceChildren(el("p", { class: "muted" }, "Loading…"));
  switch (section) {
    case "Overview": {
      const c = await get<Record<string, unknown>>("/api/config");
      const r = await get<ResourceUsage>("/api/admin/resources");
      body.replaceChildren(el("dl", { class: "admin-facts" },
        el("dt", {}, "Version"), el("dd", {}, String(c.version)),
        el("dt", {}, "Sign-in"), el("dd", {}, c.auth === "local" ? "Local (no sign-in; this machine only)" : "Accounts (password or single sign-on)"),
        el("dt", {}, "SQL"), el("dd", {}, c.allow_sql ? "Allowed for people who may query" : "Disabled on this server"),
        el("dt", {}, "Memory budget"), el("dd", {}, bytes(r.budget.memory_bytes)),
        el("dt", {}, "Threads"), el("dd", {}, String(r.budget.threads)),
        el("dt", {}, "Temporary disk"), el("dd", {}, bytes(r.budget.temp_disk_bytes)),
        el("dt", {}, "Workbook size limit"), el("dd", {}, r.workbook_size_limit ? bytes(r.workbook_size_limit) : "none"),
        el("dt", {}, "Free disk"), el("dd", {}, r.disk_free_bytes == null ? "unknown" : bytes(r.disk_free_bytes)),
        el("dt", {}, "Disk reserve"), el("dd", { class: r.disk_reserve_bytes && !r.disk_reserve_held ? "admin-warning" : "" },
          !r.disk_reserve_bytes ? "off"
            : r.disk_reserve_held ? `${bytes(r.disk_reserve_bytes)} kept for a full disk`
            : `released: the disk filled up. Free space; the ${bytes(r.disk_reserve_bytes)} reserve comes back by itself`),
        el("dt", {}, "Open workbooks"), el("dd", {}, String(r.allocations.length))));
      break;
    }
    case "Workbooks and memory": {
      const r = await get<ResourceUsage>("/api/admin/resources");
      body.replaceChildren(
        el("p", { class: "dialog-lede" }, `Uploads in progress use ${bytes(r.upload_bytes)} of ${bytes(r.upload_budget)}. A workbook nobody has open sleeps and holds no memory; that is normal.`),
        table("Open workbooks", ["Workbook", "Memory", "Threads", "Jobs", "Database"], r.allocations.map((a) => [a.workbook, a.duckdb_memory_limit, a.duckdb_threads, String(a.jobs), bytes(a.database_bytes)])));
      break;
    }
    case "Sessions": {
      const s = await get<Session[]>("/api/admin/sessions");
      body.replaceChildren(table("Live sessions", ["Session", "Workbook", "Queued to send", "Peak"], s.map((x) => [String(x.session), x.workbook, bytes(x.outbox_bytes), bytes(x.peak_outbox_bytes)])));
      break;
    }
    case "Backups": {
      const list = await get<Backup[]>("/api/admin/backups");
      const make = el("button", { class: "btn btn-primary", type: "button" }, "Back up now");
      make.addEventListener("click", async () => {
        make.disabled = true;
        try { await send("POST", "/api/admin/backups"); toast("Backup made", "success"); await render(section, body); }
        catch (e) { toast((e as Error).message, "error"); make.disabled = false; }
      });
      const rows = list.map((b) => {
        const del = el("button", { class: "btn btn-small", type: "button", "aria-label": `Delete backup ${b.name}` }, "Delete");
        del.addEventListener("click", async () => {
          if (!(await confirmDialog(`Delete backup ${b.name}?`, "The copy is removed from the server's disk.", "Delete"))) return;
          try { await send("DELETE", `/api/admin/backups/${encodeURIComponent(b.name)}`); await render(section, body); } catch (e) { toast((e as Error).message, "error"); }
        });
        return [new Date(b.created_at_ms).toLocaleString(), String(b.workbooks), bytes(b.bytes), del];
      });
      body.replaceChildren(
        el("p", { class: "dialog-lede" }, "Backups are consistent copies of every workbook and the server's settings, kept on this server's disk under .backups (the newest few). Restore with alephgrid --restore-from while the server is stopped; copy them elsewhere for safety."),
        el("div", { class: "dialog-actions admin-actions" }, make),
        table("Backups", ["Made", "Workbooks", "Size", ""], rows));
      break;
    }
    case "Usage and limits": {
      const u = (await get<{ spaces: SpaceUsage[] }>("/api/admin/usage")).spaces;
      body.replaceChildren(table("Usage per space", ["Space", "Workbooks", "Open", "Storage", "Jobs", "Limits"], u.map((x) => {
        const l = x.limits;
        const parts = [
          l.storage_bytes !== undefined ? `storage ${bytes(l.storage_bytes)}` : "",
          l.workbooks !== undefined ? `${l.workbooks} workbooks` : "",
          l.open_workbooks !== undefined ? `${l.open_workbooks} open` : "",
          l.jobs !== undefined ? `${l.jobs} jobs` : "",
          l.compute_memory_bytes !== undefined ? `memory ${bytes(l.compute_memory_bytes)}` : "",
        ].filter(Boolean);
        return [x.space.name, String(x.workbooks), String(x.open_workbooks), bytes(x.storage_bytes), String(x.jobs), parts.join(", ") || "none"];
      })));
      break;
    }
    case "Audit log": {
      const events = await get<AuditEvent[]>("/api/admin/audit?limit=200");
      body.replaceChildren(table("Audit log", ["When", "Who", "What", "Where"], events.map((e) => [new Date(e.at).toLocaleString(), e.actor, e.action, e.workbook ?? e.space_id ?? ""])));
      break;
    }
  }
}

/** The administration dialog (instance admins; the local user is one). */
export function adminDialog(): void {
  const body = el("div", { class: "admin-body", role: "tabpanel", tabindex: "0" });
  const tabs = SECTIONS.map((s, i) => el("button", { class: "admin-tab", role: "tab", type: "button", id: `admin-tab-${i}`, "aria-selected": i === 0 ? "true" : "false", tabindex: i === 0 ? "0" : "-1" }, s));
  const list = el("div", { class: "admin-tabs", role: "tablist", "aria-label": "Administration sections" }, ...tabs);
  const pick = (i: number) => {
    tabs.forEach((t, j) => { t.setAttribute("aria-selected", j === i ? "true" : "false"); t.tabIndex = j === i ? 0 : -1; });
    body.setAttribute("aria-labelledby", tabs[i].id);
    render(SECTIONS[i], body).catch((e) => body.replaceChildren(el("p", { role: "alert", class: "dialog-error" }, (e as Error).message)));
  };
  tabs.forEach((t, i) => {
    t.addEventListener("click", () => pick(i));
    t.addEventListener("keydown", (e) => {
      const d = e.key === "ArrowRight" ? 1 : e.key === "ArrowLeft" ? -1 : 0;
      if (!d) return;
      e.preventDefault();
      const n = (i + d + tabs.length) % tabs.length;
      tabs[n].focus();
      pick(n);
    });
  });
  const close = el("button", { class: "btn", type: "button" }, "Close");
  const dialog = el("div", { class: "dialog wide admin-dialog" }, el("h2", {}, "Server administration"), list, body, el("div", { class: "dialog-actions" }, close));
  const modal = openModal(dialog, () => modal.close());
  close.addEventListener("click", () => modal.close());
  pick(0);
  tabs[0].focus();
}
