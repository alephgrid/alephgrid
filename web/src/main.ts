import "./styles.css";
import "./theme";
import { accountButton, invitePage, isOrgAdmin, isUnauthenticated, libraryDialog, loginPage, loginUrl, membersDialog, setupPage, tokensDialog, usageDialog } from "./account";
import { api, isRuntimeFailure, seg, type Me, type SpaceListing } from "./api";
import { importFile } from "./importer";
import { bytes, formatCompact, formatCount } from "./format";
import type { WorkbookListing } from "./types";
import { confirmDialog, el, formDialog, refusal, showMenu, toast, toastError, type MenuItem } from "./ui";
import { WorkbookScreen } from "./workbook";
import { isNotFound, showNotFound, waitForCompute } from "./compute";
import { brandMark, icon, type IconName } from "./icons";
import { themeButton } from "./theme";
import { agentsDialog } from "./agents";
import { adminDialog } from "./admin";
import { spaceSettingsDialog } from "./share";

const app = document.getElementById("app")!;
let current: WorkbookScreen | null = null;

/** U.02: leaving a workbook with changes the server has not acknowledged asks first; they
 *  are kept and resent while the workbook stays open, and would be lost by leaving. */
async function mayLeave(): Promise<boolean> {
  if (!current?.hasUnsaved()) return true;
  return confirmDialog("Leave with unsaved changes?",
    "Some changes have not reached the server yet. Stay and they are sent when the connection is back; leave and they are lost.", "Leave anyway");
}

function navigate(path: string): void {
  void mayLeave().then((ok) => {
    if (!ok) return;
    history.pushState(null, "", path);
    route();
  });
}

let shownPath = location.pathname + location.search;
window.addEventListener("popstate", () => {
  if (current?.hasUnsaved()) {
    // Stay on the workbook (put its address back), then ask.
    const target = location.pathname + location.search;
    history.pushState(null, "", shownPath);
    void mayLeave().then((ok) => { if (ok) { history.pushState(null, "", target); route(); } });
    return;
  }
  route();
});
window.addEventListener("beforeunload", (e) => {
  if (current?.hasUnsaved()) { e.preventDefault(); e.returnValue = ""; }
});

let config: Awaited<ReturnType<typeof api.config>> | null = null;

async function route(): Promise<void> {
  shownPath = location.pathname + location.search;
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
    if (m && isNotFound(e)) {
      showNotFound(app, () => navigate(homeUrl()));
      return;
    }
    // U.03b: a failed load can be retried in place; filters and search are kept (module state).
    app.replaceChildren(el("div", { class: "fatal", role: "alert" },
      el("h2", {}, m ? "This workbook couldn't be loaded" : "Workbooks couldn't be loaded"), el("p", {}, (e as Error).message),
      el("div", { class: "empty-actions" },
        el("button", { class: "btn btn-primary", onclick: () => route() }, "Try again"),
        m ? el("button", { class: "btn", onclick: () => navigate("/") }, "Back to workbooks") : null)));
  }
}

async function showWorkbook(id: string): Promise<void> {
  // Opening starts its compute; while that takes a moment the page says what it waits for.
  const here = location.pathname;
  const ready = await waitForCompute(id, app, () => navigate(homeUrl()), () => location.pathname === here);
  if (!ready || location.pathname !== here) return;
  const { workbook, role } = await api.get(id);
  document.title = `${workbook.name} · Alephgrid`;
  const screen = new WorkbookScreen(workbook, () => navigate(homeUrl()), role);
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

const COMPUTE_LABEL: Record<string, string> = {
  sleeping: "Sleeping", queued: "Waiting for capacity", starting: "Starting", ready: "Ready", stopping: "Stopping",
};
const COMPUTE_TITLE: Record<string, string> = {
  sleeping: "Not running. Opening the workbook starts it; its data is kept on disk.",
  queued: "Waiting in line for server capacity; it opens by itself when there is room.",
  starting: "Opening its compute.",
  ready: "Running: open for editing.",
  stopping: "Finishing its work, then sleeping.",
};

/** Compute state of a workbook in the list (U.03): sleeping is neutral, not an error. */
function computeCell(w: WorkbookListing): HTMLElement {
  const c = w.compute ?? "sleeping";
  return el("span", { role: "cell", class: "muted hide-sm wb-compute", "data-compute": c, "data-id": w.id, title: COMPUTE_TITLE[c] },
    el("span", { class: "compute-dot", "aria-hidden": "true" }), COMPUTE_LABEL[c] ?? c);
}

let computeTimer = 0;
/** While the hub is shown, keep compute states current without re-rendering the page. */
function watchCompute(): void {
  clearInterval(computeTimer);
  computeTimer = window.setInterval(async () => {
    if (!document.querySelector(".home-layout")) { clearInterval(computeTimer); return; }
    if (document.hidden) return;
    try {
      const list = await api.list();
      for (const w of list) {
        const cell = document.querySelector<HTMLElement>(`.wb-compute[data-id="${w.id}"]`);
        if (cell && cell.dataset.compute !== (w.compute ?? "sleeping")) cell.replaceWith(computeCell(w));
      }
    } catch { /* the next tick tries again */ }
  }, 20_000);
}

function describeContents(w: WorkbookListing): string {
  const s = w.summary;
  if (!s || (!s.tables && !s.sheets)) return "Empty";
  const parts: string[] = [];
  if (s.tables) parts.push(`${s.tables} table${s.tables === 1 ? "" : "s"}`);
  if (s.sheets) parts.push(`${s.sheets} sheet${s.sheets === 1 ? "" : "s"}`);
  if (s.rows) parts.push(`${formatCompact(s.rows)} rows`);
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

/** U.03b: the hub's shape while its lists load: the sidebar and placeholder rows. */
function homeSkeleton(): HTMLElement {
  const bars = (n: number, cls: string) => Array.from({ length: n }, () => el("span", { class: cls }));
  return el("div", { class: "home-layout home-loading" },
    el("aside", { class: "sidebar", "aria-hidden": "true" },
      el("div", { class: "brand big" }, brandMark(26), el("span", { class: "wordmark" }, "alephgrid")),
      el("div", { class: "nav" }, ...bars(3, "skeleton-bar skeleton-nav"))),
    el("main", { class: "home-main" },
      el("div", { class: "home-top" }, el("span", { class: "skeleton-bar skeleton-search" })),
      el("div", { class: "home-skeleton", role: "status", "aria-label": "Loading workbooks", "aria-busy": "true" },
        el("span", { class: "skeleton-bar skeleton-title" }),
        ...Array.from({ length: 6 }, () => el("div", { class: "skeleton-row" },
          el("span", { class: "skeleton-bar skeleton-icon" }), el("span", { class: "skeleton-bar skeleton-name" }),
          el("span", { class: "skeleton-bar skeleton-meta hide-sm" }), el("span", { class: "skeleton-bar skeleton-meta hide-xs" }))))));
}

async function showHome(): Promise<void> {
  document.title = "Alephgrid";
  home.scope = new URLSearchParams(location.search).get("in") ?? "all";
  // A quick load shows the list at once; a slow one shows the placeholder (no flash).
  let loaded = false;
  const here = location.href;
  const placeholder = setTimeout(() => { if (!loaded && location.href === here) app.replaceChildren(homeSkeleton()); }, 120);
  let lists;
  try {
    lists = await Promise.all([api.list(), api.spaces(), api.config(), api.trash(), api.me()]);
  } finally {
    loaded = true;
    clearTimeout(placeholder);
  }
  const [workbooks, spaces, config, trash, me] = lists;
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
  const navItem = (scope: Scope, label: string, glyph: IconName) =>
    el("button", {
      class: `nav-item${home.scope === scope ? " active" : ""}`,
      "aria-current": home.scope === scope ? "page" : false,
      onclick: () => setScope(scope),
    }, el("span", { class: "nav-icon" }, icon(glyph)), el("span", { class: "nav-label" }, label),
    el("span", { class: "nav-count" }, String(count(scope))));

  const spaceItems = home.spaces.map((sp) => {
    const item = navItem(sp.id, sp.name, "folder");
    if (sp.role === "viewer") item.querySelector(".nav-label")?.append(el("span", { class: "role-tag" }, "view only"));
    item.addEventListener("contextmenu", (e) => { e.preventDefault(); spaceMenu(sp, e.clientX, e.clientY); });
    item.append(el("span", { class: "nav-more", role: "button", tabindex: "0", "aria-label": `${sp.name} options`, title: "Options",
      onclick: (e: MouseEvent) => { e.stopPropagation(); spaceMenu(sp, e.clientX, e.clientY); } }, "⋯"));
    return item;
  });

  const sidebar = el("aside", { class: "sidebar" },
    el("div", { class: "brand big" }, brandMark(26), el("span", { class: "wordmark" }, "alephgrid")),
    el("nav", { class: "nav", "aria-label": "Library" },
      navItem("all", "All workbooks", "home"),
      navItem("recent", "Recent", "clock"),
      navItem("starred", "Starred", "star"),
    ),
    el("div", { class: "nav-heading" },
      el("span", {}, "Spaces"),
      home.me?.kind === "token" ? null
        : el("button", { class: "icon-btn small", title: "New space", "aria-label": "New space", onclick: newSpace }, icon("plus", 14))),
    el("nav", { class: "nav", "aria-label": "Spaces" },
      ...spaceItems,
      home.spaces.length ? null : el("p", { class: "nav-empty" }, "You have no spaces yet. Ask an admin to add you to one."),
    ),
    el("nav", { class: "nav nav-bottom", "aria-label": "Trash" }, navItem("trash", "Trash", "trash")),
    el("div", { class: "sidebar-foot" }, themeButton(),
      home.me && (home.me.kind === "local" || home.me.instance_admin)
        ? el("button", { class: "btn btn-ghost btn-small", type: "button", onclick: () => adminDialog() }, "Administration") : null,
      el("span", { class: "muted sidebar-version" }, `v${home.version} · data stays on this server`)),
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
        el("span", { role: "cell", class: "muted hide-sm num" }, bytes(w.size_bytes)),
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
        home.query ? `No workbooks match “${home.query}”. ` : home.scope === "starred" ? "Star a workbook to pin it here."
          : "No workbooks here yet. Drop a file on this page to start.",
        home.query ? el("button", { class: "btn btn-small", type: "button", onclick: () => { home.query = ""; renderHome(); } }, "Clear search") : null));
      return;
    }
    const showSpace = ["all", "recent", "starred"].includes(home.scope) && home.spaces.length > 1;
    list.replaceChildren(
      el("div", { class: "wb-row wb-head", role: "row" },
        el("span", { role: "columnheader" }), el("span", { role: "columnheader" }, "Name"),
        el("span", { role: "columnheader", class: "hide-sm" }, "Contents"), el("span", { role: "columnheader", class: "hide-sm num" }, "Size"),
        el("span", { role: "columnheader", class: "hide-xs" }, "Opened"), el("span", { role: "columnheader", class: "hide-sm" }, "Compute"),
        el("span", { role: "columnheader" })),
      ...items.map((w) => {
        const space = showSpace ? spaceName(w.space) : undefined;
        return el("div", { class: "wb-row", role: "row" },
          el("span", { role: "cell", class: "cell-center" },
            el("button", { class: `star${w.starred ? " on" : ""}`, title: w.role === "viewer" ? "View only" : w.starred ? "Unstar" : "Star", disabled: w.role === "viewer",
              "aria-label": w.starred ? `Unstar ${w.name}` : `Star ${w.name}`, "aria-pressed": w.starred ? "true" : "false",
              onclick: () => toggleStar(w) }, w.starred ? "★" : "☆")),
          el("a", { role: "cell", class: "wb-name", href: `/w/${seg(w.id)}`, onclick: (e: MouseEvent) => { e.preventDefault(); navigate(`/w/${seg(w.id)}`); } },
            el("span", { class: "wb-icon" }, icon(w.summary?.tables ? "table" : "sheet")),
            el("strong", {}, w.name),
            space ? el("span", { class: "badge" }, space) : null,
            w.role === "viewer" ? el("span", { class: "badge" }, "view only") : null),
          el("span", { role: "cell", class: "muted hide-sm" }, describeContents(w)),
          el("span", { role: "cell", class: "muted hide-sm num" }, bytes(w.size_bytes)),
          el("span", { role: "cell", class: "muted hide-xs", title: w.opened_at ? new Date(w.opened_at * 1000).toLocaleString() : "" }, relativeTime(w.opened_at)),
          computeCell(w),
          el("span", { role: "cell", class: "cell-center" },
            el("button", { class: "icon-btn", title: "More", "aria-label": `${w.name} options`,
              onclick: (e: MouseEvent) => workbookMenu(w, e.clientX, e.clientY) }, icon("more"))),
        );
      }),
    );
  }
  renderList();

  const search = el("input", { type: "search", class: "search", placeholder: "Search workbooks", "aria-label": "Search workbooks",
    value: home.query, oninput: (e: Event) => { home.query = (e.target as HTMLInputElement).value; renderCards(); renderList(); } });
  const sort = el("select", { class: "sort", "aria-label": "Sort by",
    onchange: (e: Event) => { home.sort = (e.target as HTMLSelectElement).value as SortKey; renderList(); } },
    el("option", { value: "recent" }, "Last opened"),
    el("option", { value: "name" }, "Name"),
    el("option", { value: "created" }, "Date created"),
    el("option", { value: "size" }, "Size"));
  sort.value = home.sort;

  // The few most recently opened workbooks, as cards above the full list.
  const cards = el("section", { class: "wb-cards", "aria-label": "Recently opened" });
  function renderCards(): void {
    const recent = home.scope === "trash" || home.query ? []
      : home.workbooks.filter((w) => inScope(w, home.scope) && w.opened_at)
        .sort((a, b) => (b.opened_at ?? 0) - (a.opened_at ?? 0)).slice(0, 3);
    cards.hidden = recent.length < 2;
    cards.replaceChildren(...recent.map((w) => el("a", { class: "wb-card", href: `/w/${seg(w.id)}`, title: `Open ${w.name}`,
      onclick: (e: MouseEvent) => { e.preventDefault(); navigate(`/w/${seg(w.id)}`); } },
      el("span", { class: "wb-card-preview", "aria-hidden": "true" }, icon(w.summary?.tables ? "table" : "sheet", 20)),
      el("span", { class: "wb-card-name" }, w.name, w.starred ? el("span", { class: "wb-card-star", "aria-label": "starred" }, icon("star", 13)) : null),
      el("span", { class: "wb-card-meta" }, spaceName(w.space) ?? describeContents(w)),
      el("span", { class: "wb-card-meta" }, icon("clock", 12), relativeTime(w.opened_at)))));
  }
  renderCards();

  const subtitle: Record<string, string> = {
    all: "Every workbook you can open. Drop a CSV, Parquet, JSON or XLSX file anywhere on this page to import it.",
    recent: "Opened in the last two weeks.",
    starred: "Workbooks you pinned with a star.",
    trash: "Deleted workbooks stay here until you delete them forever.",
  };
  const actions = home.scope === "trash"
    ? [el("button", { class: "btn btn-danger", disabled: !home.trash.length, title: home.trash.length ? false : "The trash is empty", onclick: emptyTrash }, "Empty trash")]
    : writableSpaces().length
      ? [el("button", { class: "btn", onclick: () => fileInput.click() }, icon("upload"), "Open file"),
        el("button", { class: "btn btn-primary", onclick: newWorkbook }, icon("plus"), "New workbook")]
      : [];

  const main = el("main", { class: "home-main" },
    el("div", { class: "home-top" },
      el("label", { class: "search-box" }, icon("search", 15), search),
      el("div", { class: "spacer" }),
      home.me ? accountButton(home.me, navigate) : null,
    ),
    // U.03a: an empty account gets one short line and a sample, not a marketing banner.
    home.workbooks.length === 0 && home.scope !== "trash"
      ? el("section", { class: "home-welcome", "aria-label": "Get started" },
        el("p", {}, "Open a CSV, Parquet, JSON or Excel file of any size, or start a blank workbook. Files stay on this server."),
        el("button", { class: "btn", onclick: () => createDemo(10_000_000) }, "Try sample data (10M rows)"))
      : null,
    el("div", { class: "home-head" },
      el("div", {},
        el("h2", {}, scopeTitle(home.scope)),
        el("p", { class: "muted" }, subtitle[home.scope] ?? "Workbooks in this space.")),
      el("div", { class: "home-actions" }, ...actions),
    ),
    cards,
    el("div", { class: "home-bar" },
      el("h3", {}, home.scope === "trash" ? "Deleted" : "Workbooks"),
      el("div", { class: "spacer" }),
      sort,
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
  watchCompute();
}

function setScope(scope: Scope): void {
  home.scope = scope;
  history.replaceState(null, "", homeUrl());
  renderHome();
}

async function newWorkbook(): Promise<void> {
  const spaces = writableSpaces();
  let created = "";
  const v = await formDialog("New workbook", [
    { name: "name", label: "Name", value: "Untitled workbook" },
    ...(spaces.length > 1 ? [{ name: "space", label: "Space", type: "select" as const, value: targetSpace() ?? spaces[0].id,
      options: spaces.map((s) => ({ value: s.id, label: s.name })) }] : []),
  ], "Create",
  // Refused (a quota, a role): the dialog says why.
  (x) => refusal(async () => { created = (await api.create(x.name, x.space ?? targetSpace())).id; }));
  if (v && created) navigate(`/w/${seg(created)}?start=sheet`);
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
    { label: "Rename…", disabled: !owner && "Owners only", action: async () => {
      const v = await formDialog("Rename space", [{ name: "name", label: "Name", value: sp.name }], "Rename",
        (x) => refusal(() => api.renameSpace(sp.id, x.name)));
      if (v) await reloadHome();
    } },
    { label: "New workbook here", disabled: sp.role === "viewer" && "Viewers cannot add workbooks", action: async () => navigate(`/w/${seg((await api.create("Untitled workbook", sp.id)).id)}?start=sheet`) },
    { label: "Library…", action: () => libraryDialog(sp, home.workbooks.filter((w) => w.space === sp.id).map((w) => ({ id: w.id, name: w.name, role: w.role ?? undefined })),
      home.me ?? { kind: "local", instance_admin: true, orgs: [] }) },
  ];
  if (!local) {
    items.push("sep",
      { label: owner ? "Members…" : "Who has access…", action: () => membersDialog(sp, () => {}) },
      ...(owner ? [{ label: "Service tokens…", action: () => tokensDialog(sp) }] : []));
  }
  if (owner) items.push("sep", { label: "Agent connections…", hint: "MCP, read-only", action: () => agentsDialog(sp, home.workbooks.filter((w) => w.space === sp.id).map((w) => ({ id: w.id, name: w.name }))) });
  {
    const admin = !!home.me && home.me.kind !== "local" && isOrgAdmin(home.me, sp.org_id);
    items.push({ label: "Settings…", action: () => spaceSettingsDialog(sp, home.me ?? null,
      home.workbooks.filter((w) => w.space === sp.id).map((w) => ({ id: w.id, name: w.name, size_bytes: w.size_bytes })),
      { members: () => membersDialog(sp, () => {}), usage: admin ? () => usageDialog(sp.org_id) : null }) });
  }
  // A space with workbooks cannot be deleted: the item says so instead of a toast afterwards.
  const holding = home.workbooks.filter((w) => w.space === sp.id).length;
  items.push("sep", { label: "Delete space", danger: true,
    disabled: !owner ? "Owners only" : holding > 0 && `Move or delete its ${holding} workbook${holding === 1 ? "" : "s"} first`, action: async () => {
    if (!(await confirmDialog(`Delete space “${sp.name}”?`, "Its members and service tokens are removed.", "Delete", () => api.removeSpace(sp.id)))) return;
    if (home.scope === sp.id) home.scope = "all";
    history.replaceState(null, "", homeUrl());
    await reloadHome();
  } });
  showMenu(items, x, y);
}

async function toggleStar(w: WorkbookListing): Promise<void> {
  try {
    await api.update(w.id, { starred: !w.starred });
    await reloadHome();
  } catch (e) {
    toastError(e);
  }
}

function workbookMenu(w: WorkbookListing, x: number, y: number): void {
  const viewer = w.role === "viewer";
  const items: MenuItem[] = [
    { label: "Open", action: () => navigate(`/w/${seg(w.id)}`) },
    { label: "Open in new tab", action: () => window.open(`/w/${seg(w.id)}`, "_blank") },
    "sep",
    { label: "Rename…", disabled: viewer && "View only", action: async () => {
      const v = await formDialog("Rename workbook", [{ name: "name", label: "Name", value: w.name }], "Rename",
        (x) => refusal(() => api.update(w.id, { name: x.name })));
      if (v) await reloadHome();
    } },
    { label: "Duplicate", disabled: viewer && "View only", action: async () => {
      try {
        const copy = await api.duplicate(w.id);
        toast(`Created “${copy.name}”`, "success");
        await reloadHome();
      } catch (e) { toastError(e); }
    } },
    { label: "Move to space…", disabled: viewer ? "View only" : writableSpaces().filter((s) => s.id !== w.space).length === 0 && "No other space you can edit", action: async () => {
      const v = await formDialog(`Move “${w.name}”`, [{
        name: "space", label: "Space", type: "select", value: w.space,
        options: writableSpaces().map((s) => ({ value: s.id, label: s.name })),
      }], "Move", (x) => x.space === w.space ? null : refusal(() => api.update(w.id, { space: x.space })));
      if (v && v.space !== w.space) await reloadHome();
    } },
    { label: w.starred ? "Unstar" : "Star", disabled: viewer && "View only", action: () => toggleStar(w) },
    "sep",
    { label: "Delete", danger: true, disabled: viewer && "View only", action: async () => {
      if (!(await confirmDialog(`Move “${w.name}” to the trash?`, "Anyone who has it open is disconnected. You can restore it from Trash.", "Delete", () => api.remove(w.id)))) return;
      await reloadHome();
    } },
  ];
  showMenu(items, x, y);
}

async function restoreWorkbook(w: WorkbookListing): Promise<void> {
  try {
    await api.restore(w.id);
    toast(`Restored “${w.name}”`, "success");
  } catch (e) { toastError(e); }
  await reloadHome();
}

async function purgeWorkbook(w: WorkbookListing): Promise<void> {
  if (!(await confirmDialog(`Delete “${w.name}” forever?`, `Its tables and sheets (${bytes(w.size_bytes)}) are permanently removed from disk. This cannot be undone.`, "Delete", () => api.purge(w.id)))) return;
  await reloadHome();
}

async function emptyTrash(): Promise<void> {
  const n = home.trash.length;
  if (!(await confirmDialog("Empty the trash?", `${n} workbook${n === 1 ? " is" : "s are"} permanently removed from disk. This cannot be undone.`, "Delete", () => api.emptyTrash()))) return;
  await reloadHome();
}

async function openFile(file: File): Promise<void> {
  const wb = await api.create(file.name.replace(/\.[^.]+$/, ""), targetSpace());
  let result = null;
  try {
    result = await importFile(wb.id, file);
  } catch (e) {
    toastError(e);
  }
  if (!result) {
    // The workbook was created only for this file: remove it without a trash entry.
    await api.remove(wb.id).then(() => api.purge(wb.id)).catch(() => {});
    return;
  }
  toast(result.kind === "table"
    ? `${formatCount(result.table.row_count)} rows imported`
    : `${result.sheets.length} sheet(s) imported`, "success");
  navigate(`/w/${seg(wb.id)}`);
}

async function createDemo(rows: number): Promise<void> {
  const wb = await api.create("Demo: US flights", targetSpace());
  navigate(`/w/${seg(wb.id)}?demo=${rows}`);
}

route();
