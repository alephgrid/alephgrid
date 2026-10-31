import "./styles.css";
import { accountButton, invitePage, isUnauthenticated, libraryDialog, loginPage, loginUrl, membersDialog, setupPage, tokensDialog } from "./account";
import { api, isRuntimeFailure, type Me, type SpaceListing } from "./api";
import { importFile } from "./importer";
import { formatCount } from "./format";
import type { WorkbookListing } from "./types";
import { confirmDialog, el, formDialog, showMenu, toast, type MenuItem } from "./ui";
import { WorkbookScreen } from "./workbook";
import { waitForCompute } from "./compute";

const app = document.getElementById("app")!;
let current: WorkbookScreen | null = null;

function navigate(path: string): void {
  history.pushState(null, "", path);
  route();
}

window.addEventListener("popstate", () => route());

let config: Awaited<ReturnType<typeof api.config>> | null = null;

async function route(): Promise<void> {
  current?.dispose();
  current = null;
  const m = location.pathname.match(/^\/w\/([A-Za-z0-9_-]+)/);
  try {
    config ??= await api.config();
    if (config.auth === "accounts") {
      // Pages that work without signing in.
      if (location.pathname === "/setup") return setupPage(app, (p) => { config = null; navigate(p); });
      if (config.setup_required) return navigate("/setup");
      if (location.pathname === "/login") return loginPage(app, config, navigate);
      if (location.pathname === "/invite") return await invitePage(app, config, navigate);
    }
    if (m) await showWorkbook(m[1]);
    else await showHome();
  } catch (e) {
    if (isUnauthenticated(e)) {
      history.replaceState(null, "", loginUrl());
      return route();
    }
    if (isRuntimeFailure(e)) {
      // The server could not open the workbook (incompatible, held elsewhere, out of
      // capacity): say so, with the server's reason, and let the user retry.
      app.replaceChildren(el("div", { class: "fatal workbook-failure", role: "alert" },
        el("h2", {}, "This workbook can't be opened right now"), el("p", {}, e.message),
        el("div", { class: "empty-actions" },
          el("button", { class: "btn btn-primary", onclick: () => route() }, "Try again"),
          el("button", { class: "btn", onclick: () => navigate("/") }, "Back to workbooks"))));
      return;
    }
    app.replaceChildren(el("div", { class: "fatal" }, el("h2", {}, "Something went wrong"), el("p", {}, (e as Error).message),
      el("button", { class: "btn", onclick: () => navigate("/") }, "Back to workbooks")));
  }
}

async function showWorkbook(id: string): Promise<void> {
  // Opening starts its compute; while that takes a moment the page says what it waits for.
  const here = location.pathname;
  const ready = await waitForCompute(id, app, () => navigate("/"), () => location.pathname === here);
  if (!ready || location.pathname !== here) return;
  const { workbook } = await api.get(id);
  document.title = `${workbook.name} · Alephgrid`;
  const screen = new WorkbookScreen(workbook, () => navigate(homeUrl()));
  current = screen;
  app.replaceChildren(screen.root);
  await screen.start();
}

// ------------------------------------------------------------------ home

/** "all" | "starred" | "recent" | "trash" | a space id */
type Scope = string;
type SortKey = "recent" | "name" | "created" | "size";

const home = {
  scope: "all" as Scope,
  query: "",
  sort: "recent" as SortKey,
  version: "",
  workbooks: [] as WorkbookListing[],
  spaces: [] as SpaceListing[],
  me: null as Me | null,
  trash: [] as WorkbookListing[],
};

function homeUrl(): string {
  return home.scope === "all" ? "/" : `/?in=${encodeURIComponent(home.scope)}`;
}

function relativeTime(secs?: number): string {
  if (!secs) return "–";
  const diff = Date.now() / 1000 - secs;
  if (diff < 60) return "just now";
  if (diff < 3600) return `${Math.floor(diff / 60)} min ago`;
  if (diff < 86400) return `${Math.floor(diff / 3600)} h ago`;
  if (diff < 86400 * 7) return `${Math.floor(diff / 86400)} d ago`;
  return new Date(secs * 1000).toLocaleDateString();
}

function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let v = n / 1024;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) { v /= 1024; i++; }
  return `${v < 10 ? v.toFixed(1) : Math.round(v)} ${units[i]}`;
}

function compactCount(n: number): string {
  return new Intl.NumberFormat(undefined, { notation: "compact", maximumFractionDigits: 1 }).format(n);
}

function describeContents(w: WorkbookListing): string {
  const s = w.summary;
  if (!s || (!s.tables && !s.sheets)) return "Empty";
  const parts: string[] = [];
  if (s.tables) parts.push(`${s.tables} table${s.tables === 1 ? "" : "s"}`);
  if (s.sheets) parts.push(`${s.sheets} sheet${s.sheets === 1 ? "" : "s"}`);
  if (s.rows) parts.push(`${compactCount(s.rows)} rows`);
  return parts.join(" · ");
}

function inScope(w: WorkbookListing, scope: Scope): boolean {
  switch (scope) {
    case "all": return true;
    case "starred": return !!w.starred;
    case "recent": return Date.now() / 1000 - (w.opened_at ?? 0) < 86400 * 14;
    default: return w.space === scope;
  }
}

function scopeTitle(scope: Scope): string {
  switch (scope) {
    case "all": return "All workbooks";
    case "starred": return "Starred";
    case "recent": return "Recent";
    case "trash": return "Trash";
    default: return home.spaces.find((s) => s.id === scope)?.name ?? "Space";
  }
}

/** Spaces where the principal may create workbooks. */
function writableSpaces(): SpaceListing[] {
  return home.spaces.filter((s) => s.role !== "viewer");
}

/** Space that new workbooks land in, given the sidebar selection (the server picks otherwise). */
function targetSpace(): string | null {
  return writableSpaces().some((s) => s.id === home.scope) ? home.scope : null;
}

function visibleWorkbooks(): WorkbookListing[] {
  const q = home.query.trim().toLowerCase();
  const recency = (w: WorkbookListing) => Math.max(w.opened_at ?? 0, w.updated_at ?? 0, w.created_at);
  return home.workbooks
    .filter((w) => inScope(w, home.scope) && (!q || w.name.toLowerCase().includes(q)))
    .sort((a, b) => {
      switch (home.sort) {
        case "name": return a.name.localeCompare(b.name);
        case "created": return b.created_at - a.created_at;
        case "size": return b.size_bytes - a.size_bytes;
        default: return recency(b) - recency(a);
      }
    });
}

async function showHome(): Promise<void> {
  document.title = "Alephgrid";
  home.scope = new URLSearchParams(location.search).get("in") ?? "all";
  const [workbooks, spaces, config, trash, me] = await Promise.all([api.list(), api.spaces(), api.config(), api.trash(), api.me()]);
  home.workbooks = workbooks;
  home.spaces = spaces;
  home.trash = trash;
  home.me = me;
  home.version = config.version;
  if (!["all", "starred", "recent", "trash"].includes(home.scope) && !spaces.some((s) => s.id === home.scope)) {
    home.scope = "all";
  }
  renderHome();
}

async function reloadHome(): Promise<void> {
  [home.workbooks, home.spaces, home.trash] = await Promise.all([api.list(), api.spaces(), api.trash()]);
  renderHome();
}

function renderHome(): void {
  const fileInput = el("input", { type: "file", hidden: true, accept: ".csv,.tsv,.txt,.parquet,.json,.jsonl,.ndjson,.gz,.xlsx",
    onchange: (e: Event) => {
      const f = (e.target as HTMLInputElement).files?.[0];
      if (f) openFile(f);
    } });

  const count = (scope: Scope) => scope === "trash" ? home.trash.length : home.workbooks.filter((w) => inScope(w, scope)).length;
  const navItem = (scope: Scope, label: string, icon: string) =>
    el("button", {
      class: `nav-item${home.scope === scope ? " active" : ""}`,
      "aria-current": home.scope === scope ? "page" : false,
      onclick: () => setScope(scope),
    }, el("span", { class: "nav-icon", "aria-hidden": "true" }, icon), el("span", { class: "nav-label" }, label),
    el("span", { class: "nav-count" }, String(count(scope))));

  const spaceItems = home.spaces.map((sp) => {
    const item = navItem(sp.id, sp.name, "▤");
    if (sp.role === "viewer") item.querySelector(".nav-label")?.append(el("span", { class: "role-tag" }, "view only"));
    item.addEventListener("contextmenu", (e) => { e.preventDefault(); spaceMenu(sp, e.clientX, e.clientY); });
    item.append(el("span", { class: "nav-more", role: "button", tabindex: "0", "aria-label": `${sp.name} options`, title: "Options",
      onclick: (e: MouseEvent) => { e.stopPropagation(); spaceMenu(sp, e.clientX, e.clientY); } }, "⋯"));
    return item;
  });

  const sidebar = el("aside", { class: "sidebar" },
    el("div", { class: "brand big" }, el("span", { class: "logo" }), "Alephgrid"),
    el("nav", { class: "nav", "aria-label": "Library" },
      navItem("all", "All workbooks", "◫"),
      navItem("recent", "Recent", "◷"),
      navItem("starred", "Starred", "★"),
    ),
    el("div", { class: "nav-heading" },
      el("span", {}, "Spaces"),
      home.me?.kind === "token" ? null
        : el("button", { class: "icon-btn small", title: "New space", "aria-label": "New space", onclick: newSpace }, "+")),
    el("nav", { class: "nav", "aria-label": "Spaces" },
      ...spaceItems,
      home.spaces.length ? null : el("p", { class: "nav-empty" }, "You have no spaces yet. Ask an admin to add you to one."),
    ),
    el("nav", { class: "nav nav-bottom", "aria-label": "Trash" }, navItem("trash", "Trash", "🗑")),
    el("div", { class: "sidebar-foot muted" }, `v${home.version} · data stays on this server`),
  );

  const spaceName = (id?: string | null) => home.spaces.find((s) => s.id === id)?.name;
  const list = el("div", { class: "wb-table", role: "table", "aria-label": "Workbooks" });

  function renderTrash(): void {
    const q = home.query.trim().toLowerCase();
    const items = home.trash.filter((w) => !q || w.name.toLowerCase().includes(q));
    if (!items.length) {
      list.replaceChildren(el("div", { class: "wb-empty" }, q ? `No deleted workbooks match “${home.query}”.`
        : "The trash is empty. Deleted workbooks stay here until you delete them forever."));
      return;
    }
    list.replaceChildren(
      el("div", { class: "wb-row wb-head trash-row", role: "row" },
        el("span", { role: "columnheader" }), el("span", { role: "columnheader" }, "Name"),
        el("span", { role: "columnheader", class: "hide-sm" }, "Contents"), el("span", { role: "columnheader", class: "hide-sm num" }, "Size"),
        el("span", { role: "columnheader", class: "hide-xs" }, "Deleted"), el("span", { role: "columnheader" })),
      ...items.map((w) => el("div", { class: "wb-row trash-row", role: "row", "data-trash-id": w.id },
        el("span", { role: "cell" }),
        el("span", { role: "cell", class: "wb-name" }, el("strong", {}, w.name)),
        el("span", { role: "cell", class: "muted hide-sm" }, describeContents(w)),
        el("span", { role: "cell", class: "muted hide-sm num" }, formatBytes(w.size_bytes)),
        el("span", { role: "cell", class: "muted hide-xs" }, relativeTime(w.deleted_at)),
        el("span", { role: "cell", class: "row-actions" },
          el("button", { class: "btn small", onclick: () => restoreWorkbook(w) }, "Restore"),
          el("button", { class: "btn small btn-danger", onclick: () => purgeWorkbook(w) }, "Delete forever")),
      )),
    );
  }

  function renderList(): void {
    if (home.scope === "trash") return renderTrash();
    const items = visibleWorkbooks();
    if (!items.length) {
      list.replaceChildren(el("div", { class: "wb-empty" },
        home.query ? `No workbooks match “${home.query}”.`
          : home.scope === "starred" ? "Star a workbook to pin it here."
          : "No workbooks here yet. Drop a file on this page to start."));
      return;
    }
    const showSpace = ["all", "recent", "starred"].includes(home.scope) && home.spaces.length > 1;
    list.replaceChildren(
      el("div", { class: "wb-row wb-head", role: "row" },
        el("span", { role: "columnheader" }), el("span", { role: "columnheader" }, "Name"),
        el("span", { role: "columnheader", class: "hide-sm" }, "Contents"), el("span", { role: "columnheader", class: "hide-sm num" }, "Size"),
        el("span", { role: "columnheader", class: "hide-xs" }, "Opened"), el("span", { role: "columnheader" })),
      ...items.map((w) => {
        const space = showSpace ? spaceName(w.space) : undefined;
        return el("div", { class: "wb-row", role: "row" },
          el("span", { role: "cell", class: "cell-center" },
            el("button", { class: `star${w.starred ? " on" : ""}`, title: w.starred ? "Unstar" : "Star", disabled: w.role === "viewer",
              "aria-label": w.starred ? `Unstar ${w.name}` : `Star ${w.name}`, "aria-pressed": w.starred ? "true" : "false",
              onclick: () => toggleStar(w) }, w.starred ? "★" : "☆")),
          el("a", { role: "cell", class: "wb-name", href: `/w/${w.id}`, onclick: (e: MouseEvent) => { e.preventDefault(); navigate(`/w/${w.id}`); } },
            el("strong", {}, w.name),
            space ? el("span", { class: "badge" }, space) : null,
            w.role === "viewer" ? el("span", { class: "badge" }, "view only") : null),
          el("span", { role: "cell", class: "muted hide-sm" }, describeContents(w)),
          el("span", { role: "cell", class: "muted hide-sm num" }, formatBytes(w.size_bytes)),
          el("span", { role: "cell", class: "muted hide-xs", title: w.opened_at ? new Date(w.opened_at * 1000).toLocaleString() : "" }, relativeTime(w.opened_at)),
          el("span", { role: "cell", class: "cell-center" },
            el("button", { class: "icon-btn", title: "More", "aria-label": `${w.name} options`,
              onclick: (e: MouseEvent) => workbookMenu(w, e.clientX, e.clientY) }, "⋯")),
        );
      }),
    );
  }
  renderList();

  const search = el("input", { type: "search", class: "search", placeholder: "Search workbooks", "aria-label": "Search workbooks",
    value: home.query, oninput: (e: Event) => { home.query = (e.target as HTMLInputElement).value; renderList(); } });
  const sort = el("select", { class: "sort", "aria-label": "Sort by",
    onchange: (e: Event) => { home.sort = (e.target as HTMLSelectElement).value as SortKey; renderList(); } },
    el("option", { value: "recent" }, "Last opened"),
    el("option", { value: "name" }, "Name"),
    el("option", { value: "created" }, "Date created"),
    el("option", { value: "size" }, "Size"));
  sort.value = home.sort;

  const main = el("main", { class: "home-main" },
    home.workbooks.length === 0
      ? el("section", { class: "hero" },
        el("h1", {}, "A spreadsheet for data that doesn't fit in a spreadsheet."),
        el("p", {}, "Open billion-row CSV and Parquet files, sort, filter, add formulas, build Excel-style sheets and query with SQL. Runs on your own machine or server — your data never leaves it."),
        el("div", { class: "hero-actions" },
          el("button", { class: "btn btn-primary", onclick: () => fileInput.click() }, "Open a file"),
          el("button", { class: "btn", onclick: () => createDemo(10_000_000) }, "Try 10M demo rows"),
          el("button", { class: "btn", onclick: newWorkbook }, "New blank workbook")))
      : null,
    el("div", { class: "home-bar" },
      el("h2", {}, scopeTitle(home.scope)),
      el("div", { class: "spacer" }),
      search,
      sort,
      ...(home.scope === "trash"
        ? [el("button", { class: "btn btn-danger", disabled: !home.trash.length, onclick: emptyTrash }, "Empty trash")]
        : writableSpaces().length
          ? [el("button", { class: "btn", onclick: () => fileInput.click() }, "Open file"),
            el("button", { class: "btn btn-primary", onclick: newWorkbook }, "New workbook")]
          : []),
      home.me ? accountButton(home.me, navigate) : null,
      fileInput,
    ),
    list,
  );

  const page = el("div", { class: "home-layout" }, sidebar, main);
  page.addEventListener("dragover", (e) => { e.preventDefault(); page.classList.add("dropping"); });
  page.addEventListener("dragleave", (e) => { if (e.target === page) page.classList.remove("dropping"); });
  page.addEventListener("drop", (e) => {
    e.preventDefault();
    page.classList.remove("dropping");
    const f = e.dataTransfer?.files?.[0];
    if (f) openFile(f);
  });
  app.replaceChildren(page);
}

function setScope(scope: Scope): void {
  home.scope = scope;
  history.replaceState(null, "", homeUrl());
  renderHome();
}

async function newWorkbook(): Promise<void> {
  const spaces = writableSpaces();
  const v = await formDialog("New workbook", [
    { name: "name", label: "Name", value: "Untitled workbook" },
    ...(spaces.length > 1 ? [{ name: "space", label: "Space", type: "select" as const, value: targetSpace() ?? spaces[0].id,
      options: spaces.map((s) => ({ value: s.id, label: s.name })) }] : []),
  ], "Create");
  if (!v) return;
  try {
    navigate(`/w/${(await api.create(v.name, v.space ?? targetSpace())).id}`);
  } catch (e) {
    // Refused (a quota, a role): say why.
    toast((e as Error).message, "error");
  }
}

async function newSpace(): Promise<void> {
  const v = await formDialog("New space", [{ name: "name", label: "Name", placeholder: "e.g. Finance, Q4 planning",
    hint: "You become its owner and can add members." }], "Create",
    async (vals) => {
      try {
        home.scope = (await api.createSpace(vals.name)).id;
        return null;
      } catch (e) {
        return (e as Error).message;
      }
    });
  if (v) {
    history.replaceState(null, "", homeUrl());
    await reloadHome();
  }
}

function spaceMenu(sp: SpaceListing, x: number, y: number): void {
  const owner = sp.role === "owner";
  const local = home.me?.kind === "local";
  const items: MenuItem[] = [
    { label: "Rename…", disabled: !owner, action: async () => {
      const v = await formDialog("Rename space", [{ name: "name", label: "Name", value: sp.name }], "Rename");
      if (!v) return;
      try { await api.renameSpace(sp.id, v.name); await reloadHome(); } catch (e) { toast((e as Error).message, "error"); }
    } },
    { label: "New workbook here", disabled: sp.role === "viewer", action: async () => navigate(`/w/${(await api.create("Untitled workbook", sp.id)).id}`) },
    { label: "Library…", action: () => libraryDialog(sp, home.workbooks.filter((w) => w.space === sp.id).map((w) => ({ id: w.id, name: w.name, role: w.role ?? undefined })),
      home.me ?? { kind: "local", instance_admin: true, orgs: [] }) },
  ];
  if (!local) {
    items.push("sep",
      { label: owner ? "Members…" : "Who has access…", action: () => membersDialog(sp, () => {}) },
      ...(owner ? [{ label: "Service tokens…", action: () => tokensDialog(sp) }] : []));
  }
  items.push("sep", { label: "Delete space", danger: true, disabled: !owner, action: async () => {
    const n = home.workbooks.filter((w) => w.space === sp.id).length;
    if (n) {
      toast(`Move or delete its ${n} workbook${n === 1 ? "" : "s"} first.`, "error");
      return;
    }
    if (!(await confirmDialog(`Delete space “${sp.name}”?`, "Its members and service tokens are removed."))) return;
    try {
      await api.removeSpace(sp.id);
      if (home.scope === sp.id) home.scope = "all";
      history.replaceState(null, "", homeUrl());
    } catch (e) { toast((e as Error).message, "error"); }
    await reloadHome();
  } });
  showMenu(items, x, y);
}

async function toggleStar(w: WorkbookListing): Promise<void> {
  try {
    await api.update(w.id, { starred: !w.starred });
    await reloadHome();
  } catch (e) {
    toast((e as Error).message, "error");
  }
}

function workbookMenu(w: WorkbookListing, x: number, y: number): void {
  const viewer = w.role === "viewer";
  const items: MenuItem[] = [
    { label: "Open", action: () => navigate(`/w/${w.id}`) },
    { label: "Open in new tab", action: () => window.open(`/w/${w.id}`, "_blank") },
    "sep",
    { label: "Rename…", disabled: viewer, action: async () => {
      const v = await formDialog("Rename workbook", [{ name: "name", label: "Name", value: w.name }], "Rename");
      if (!v) return;
      try { await api.update(w.id, { name: v.name }); await reloadHome(); } catch (e) { toast((e as Error).message, "error"); }
    } },
    { label: "Duplicate", disabled: viewer, action: async () => {
      try {
        const copy = await api.duplicate(w.id);
        toast(`Created “${copy.name}”`, "success");
        await reloadHome();
      } catch (e) { toast((e as Error).message, "error"); }
    } },
    { label: "Move to space…", disabled: viewer || writableSpaces().filter((s) => s.id !== w.space).length === 0, action: async () => {
      const v = await formDialog(`Move “${w.name}”`, [{
        name: "space", label: "Space", type: "select", value: w.space,
        options: writableSpaces().map((s) => ({ value: s.id, label: s.name })),
      }], "Move");
      if (!v || v.space === w.space) return;
      try { await api.update(w.id, { space: v.space }); await reloadHome(); } catch (e) { toast((e as Error).message, "error"); }
    } },
    { label: w.starred ? "Unstar" : "Star", disabled: viewer, action: () => toggleStar(w) },
    "sep",
    { label: "Delete", danger: true, disabled: viewer, action: async () => {
      if (!(await confirmDialog(`Move “${w.name}” to the trash?`, "Anyone who has it open is disconnected. You can restore it from Trash."))) return;
      try { await api.remove(w.id); } catch (e) { toast((e as Error).message, "error"); }
      await reloadHome();
    } },
  ];
  showMenu(items, x, y);
}

async function restoreWorkbook(w: WorkbookListing): Promise<void> {
  try {
    await api.restore(w.id);
    toast(`Restored “${w.name}”`, "success");
  } catch (e) { toast((e as Error).message, "error"); }
  await reloadHome();
}

async function purgeWorkbook(w: WorkbookListing): Promise<void> {
  if (!(await confirmDialog(`Delete “${w.name}” forever?`, `Its tables and sheets (${formatBytes(w.size_bytes)}) are permanently removed from disk. This cannot be undone.`))) return;
  try { await api.purge(w.id); } catch (e) { toast((e as Error).message, "error"); }
  await reloadHome();
}

async function emptyTrash(): Promise<void> {
  const n = home.trash.length;
  if (!(await confirmDialog("Empty the trash?", `${n} workbook${n === 1 ? " is" : "s are"} permanently removed from disk. This cannot be undone.`))) return;
  try { await api.emptyTrash(); } catch (e) { toast((e as Error).message, "error"); }
  await reloadHome();
}

async function openFile(file: File): Promise<void> {
  const wb = await api.create(file.name.replace(/\.[^.]+$/, ""), targetSpace());
  let result = null;
  try {
    result = await importFile(wb.id, file);
  } catch (e) {
    toast((e as Error).message, "error");
  }
  if (!result) {
    // The workbook was created only for this file: remove it without a trash entry.
    await api.remove(wb.id).then(() => api.purge(wb.id)).catch(() => {});
    return;
  }
  toast(result.kind === "table"
    ? `${formatCount(result.table.row_count)} rows imported`
    : `${result.sheets.length} sheet(s) imported`, "success");
  navigate(`/w/${wb.id}`);
}

async function createDemo(rows: number): Promise<void> {
  const wb = await api.create("Demo: US flights", targetSpace());
  navigate(`/w/${wb.id}?demo=${rows}`);
}

route();
