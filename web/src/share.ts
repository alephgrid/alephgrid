// U.08: Share, on a workbook. Who can open it (the members of its space and their roles,
// enforced by the server), its link, and the role-aware next steps: owners manage members
// and agent connections; everyone can review the change history and take back their own
// changes there.

import { agentsDialog } from "./agents";
import { api, type Me, type SpaceListing } from "./api";
import { membersDialog } from "./account";
import { el, openModal, toast } from "./ui";

const ROLE: Record<string, string> = { viewer: "can view", editor: "can edit", owner: "owns the space" };

export async function shareDialog(workbook: { id: string; name: string; space: string }, history: () => void): Promise<void> {
  let me: Me | null = null, space: SpaceListing | undefined;
  try {
    [me, space] = await Promise.all([api.me(), api.spaces().then((s) => s.find((x) => x.id === workbook.space))]);
  } catch (e) { toast((e as Error).message, "error"); return; }
  const link = `${location.origin}/w/${encodeURIComponent(workbook.id)}`;
  const linkBox = el("input", { readonly: true, class: "mono", value: link, "aria-label": "Workbook link" }) as HTMLInputElement;
  const copy = el("button", { class: "btn", type: "button" }, "Copy link");
  copy.addEventListener("click", async () => {
    try { await navigator.clipboard.writeText(link); toast("Link copied"); } catch { linkBox.select(); }
  });
  const who = el("div", { class: "share-who" });
  const actions = el("div", { class: "dialog-actions share-actions" });
  const close = el("button", { class: "btn", type: "button" }, "Done");
  const dialog = el("div", { class: "dialog share-dialog" },
    el("h2", {}, `Share “${workbook.name}”`),
    el("p", { class: "dialog-lede" }, space ? `It belongs to the space “${space.name}”: everyone with a role there can open it, as that role allows. You ${ROLE[space.role] ?? space.role}.` : "It belongs to a space you can open."),
    el("div", { class: "share-link" }, linkBox, copy),
    who, actions, el("div", { class: "dialog-actions" }, close));
  const modal = openModal(dialog, () => modal.close());
  close.addEventListener("click", () => modal.close());
  linkBox.focus();

  if (me?.kind === "local") {
    who.append(el("p", { class: "notice", role: "note" },
      "This server runs without sign-in, so only this computer can open the link. To work with others, run it with --auth accounts and invite them; agents can connect read-only below."));
  } else if (space) {
    try {
      const members = await api.spaceMembers(space.id);
      who.append(el("h3", {}, "Who has access"),
        members.length
          ? el("ul", { class: "share-members", "aria-label": "Members" }, ...members.map((m) => el("li", {},
            el("span", {}, m.kind === "group" ? `Group ${m.principal}` : (m.user ? `${m.user.name || m.user.email}` : m.principal)),
            el("span", { class: "muted" }, ROLE[m.role] ?? m.role))))
          : el("p", { class: "muted" }, "Only organization admins (owners of every space)."));
    } catch (e) { who.append(el("p", { class: "dialog-error" }, (e as Error).message)); }
  }
  const owner = space?.role === "owner";
  if (space && owner && me?.kind !== "local") {
    actions.append(el("button", { class: "btn", type: "button", onclick: () => { modal.close(); membersDialog(space!, () => {}); } }, "Manage members…"));
  }
  if (space && owner) {
    actions.append(el("button", { class: "btn", type: "button", onclick: async () => {
      modal.close();
      const all = await api.list().catch(() => []);
      agentsDialog(space!, all.filter((w) => w.space === space!.id).map((w) => ({ id: w.id, name: w.name })));
    } }, "Agent connections…"));
  }
  actions.append(el("button", { class: "btn", type: "button", onclick: () => { modal.close(); history(); } }, "Change history…"));
}

/** U.09: a space's settings in one place: who has access, agents, usage and limits, compute. */
export function spaceSettingsDialog(space: SpaceListing, me: Me | null, workbooks: { id: string; name: string; size_bytes?: number }[], open: { members: () => void; usage: (() => void) | null }): void {
  const owner = space.role === "owner";
  const storage = workbooks.reduce((n, w) => n + (w.size_bytes ?? 0), 0);
  const fmt = (n: number) => (n >= 2 ** 30 ? `${(n / 2 ** 30).toFixed(1)} GB` : n >= 2 ** 20 ? `${(n / 2 ** 20).toFixed(1)} MB` : `${Math.max(1, Math.round(n / 1024))} KB`);
  const close = el("button", { class: "btn", type: "button" }, "Close");
  const section = (title: string, text: string, ...buttons: (HTMLElement | null)[]) =>
    el("section", { class: "settings-section", "aria-label": title }, el("h3", {}, title), el("p", { class: "dialog-lede" }, text),
      el("div", { class: "dialog-actions share-actions" }, ...buttons.filter((b): b is HTMLElement => !!b)));
  const local = me?.kind === "local";
  const dialog = el("div", { class: "dialog wide space-settings" },
    el("h2", {}, `Settings of “${space.name}”`),
    section("Members", local ? "This server runs without sign-in: only this computer opens its workbooks. Run with --auth accounts to add people." : `People and groups with a role here; you ${ROLE[space.role] ?? space.role}.`,
      local ? null : el("button", { class: "btn", type: "button", onclick: () => { modal.close(); open.members(); } }, owner ? "Manage members…" : "Who has access…")),
    section("Agents", "Read-only MCP connections for AI agents to the workbooks here.",
      owner ? el("button", { class: "btn", type: "button", onclick: () => { modal.close(); agentsDialog(space, workbooks); } }, "Agent connections…") : null),
    section("Storage and limits", `${workbooks.length} workbook${workbooks.length === 1 ? "" : "s"}, ${fmt(storage)} on disk. Organization admins set storage, workbook, job and compute limits.`,
      open.usage ? el("button", { class: "btn", type: "button", onclick: () => { modal.close(); open.usage!(); } }, "Usage and limits…") : null),
    section("Compute", "Each workbook runs only while someone has it open and sleeps after its idle time; opening it resumes it. Owners choose a workbook's size from its Compute panel; resizing finishes running work and keeps every saved change, and anything not yet acknowledged as saved is resent after the restart.", null),
    el("div", { class: "dialog-actions" }, close));
  const modal = openModal(dialog, () => modal.close());
  close.addEventListener("click", () => modal.close());
  close.focus();
}
