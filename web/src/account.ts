// Sign-in, setup and invitation pages; the account menu; space members, service tokens and
// organization people dialogs. The server enforces every role (docs/TEAMS.md); these screens
// only offer what the signed-in principal may do.

import { api, HttpError, type InviteInfo, type Me, type SpaceListing } from "./api";
import type { AuditEvent, Invite, OrgMember, OrgRole, SpaceMember, SpaceRole, Token } from "./types";
import { confirmDialog, el, formDialog, openModal, showMenu, toast } from "./ui";

type Navigate = (path: string) => void;
type Config = Awaited<ReturnType<typeof api.config>>;

const ROLE_LABEL: Record<SpaceRole, string> = { viewer: "Viewer", editor: "Editor", owner: "Owner" };
const ROLE_HINT: Record<SpaceRole, string> = {
  viewer: "Open, read, query and download",
  editor: "Also change workbooks, add and delete them",
  owner: "Also manage members, tokens and the space",
};

/** Where to go after signing in: a path on this server only. */
export function nextPath(): string {
  const n = new URLSearchParams(location.search).get("next") ?? "/";
  return n.startsWith("/") && !n.startsWith("//") ? n : "/";
}

export function loginUrl(): string {
  const here = location.pathname + location.search + location.hash;
  return here === "/" ? "/login" : `/login?next=${encodeURIComponent(here)}`;
}

function card(title: string, ...body: (Node | null)[]): HTMLElement {
  return el("main", { class: "auth-page" },
    el("div", { class: "auth-card" },
      el("div", { class: "brand big" }, el("span", { class: "logo" }), "Alephgrid"),
      el("h1", {}, title),
      ...body));
}

function field(label: string, input: HTMLInputElement, hint?: string): HTMLElement {
  return el("label", { class: "field" }, el("span", {}, label), input, hint ? el("small", {}, hint) : null);
}

function ssoButton(config: Config, next: string): HTMLElement | null {
  if (!config.sso) return null;
  return el("a", { class: "btn sso-btn", href: `/api/auth/oidc/start?next=${encodeURIComponent(next)}` }, config.sso.label);
}

export function loginPage(app: HTMLElement, config: Config, navigate: Navigate): void {
  document.title = "Sign in · Alephgrid";
  const error = el("div", { class: "dialog-error", role: "alert" }, new URLSearchParams(location.search).get("error") ?? "");
  const email = el("input", { type: "email", name: "email", autocomplete: "username", required: true });
  const password = el("input", { type: "password", name: "password", autocomplete: "current-password", required: true });
  const submit = el("button", { type: "submit", class: "btn btn-primary" }, "Sign in");
  const form = el("form", { class: "auth-form", onsubmit: async (e: Event) => {
    e.preventDefault();
    submit.disabled = true;
    error.textContent = "";
    try {
      await api.login(email.value, password.value);
      navigate(nextPath());
    } catch (err) {
      error.textContent = (err as Error).message;
      password.select();
    } finally {
      submit.disabled = false;
    }
  } }, field("Email", email), field("Password", password), error, submit);
  const sso = ssoButton(config, nextPath());
  app.replaceChildren(card("Sign in", sso, sso ? el("p", { class: "auth-or muted" }, "or with your password") : null, form));
  email.focus();
}

export function setupPage(app: HTMLElement, navigate: Navigate): void {
  document.title = "Set up · Alephgrid";
  const error = el("div", { class: "dialog-error", role: "alert" });
  const code = el("input", { name: "code", autocomplete: "off", required: true, class: "mono" });
  const name = el("input", { name: "name", autocomplete: "name", value: "" });
  const email = el("input", { type: "email", name: "email", autocomplete: "username", required: true });
  const password = el("input", { type: "password", name: "password", autocomplete: "new-password", required: true, minlength: 10 });
  const form = el("form", { class: "auth-form", onsubmit: async (e: Event) => {
    e.preventDefault();
    error.textContent = "";
    try {
      await api.setup(code.value, email.value, name.value, password.value);
      navigate("/");
    } catch (err) {
      error.textContent = (err as Error).message;
    }
  } },
  el("p", { class: "muted" }, "Create the administrator of this server. The setup code is printed in the server's console when it starts."),
  field("Setup code", code), field("Your name", name), field("Email", email), field("Password", password, "At least 10 characters."),
  error, el("button", { type: "submit", class: "btn btn-primary" }, "Create administrator"));
  app.replaceChildren(card("Set up Alephgrid", form));
  code.focus();
}

function describeInvite(i: InviteInfo): string {
  const org = i.organization ?? "an organization";
  const space = i.space ? ` and to the space “${i.space}” as ${ROLE_LABEL[i.space_role ?? "viewer"].toLowerCase()}` : "";
  return `${i.email} is invited to ${org} as ${i.org_role === "admin" ? "an admin" : "a member"}${space}.`;
}

/** `/invite#<code>`: the code stays in the fragment, which the browser never sends. */
export async function invitePage(app: HTMLElement, config: Config, navigate: Navigate): Promise<void> {
  document.title = "Invitation · Alephgrid";
  const code = decodeURIComponent(location.hash.slice(1));
  let info: InviteInfo;
  try {
    info = await api.lookupInvite(code);
  } catch (e) {
    app.replaceChildren(card("Invitation", el("p", { role: "alert" }, (e as Error).message),
      el("button", { class: "btn", onclick: () => navigate("/") }, "Go to Alephgrid")));
    return;
  }
  const me = await api.me().catch(() => null);
  const error = el("div", { class: "dialog-error", role: "alert" });
  const accept = async (name: string, password: string) => {
    error.textContent = "";
    try {
      await api.acceptInvite(code, name, password);
      history.replaceState(null, "", "/");
      navigate("/");
    } catch (e) {
      error.textContent = (e as Error).message;
    }
  };
  const intro = el("p", {}, describeInvite(info));
  if (me?.kind === "user") {
    app.replaceChildren(card("Accept the invitation", intro,
      el("p", { class: "muted" }, `You are signed in as ${me.user!.email}.`), error,
      el("button", { class: "btn btn-primary", onclick: () => accept("", "") }, "Accept")));
    return;
  }
  if (info.existing_user) {
    const back = `/invite#${encodeURIComponent(code)}`;
    app.replaceChildren(card("Accept the invitation", intro,
      el("p", {}, `Sign in as ${info.email} first, then open this link again.`),
      el("a", { class: "btn btn-primary", href: `/login?next=${encodeURIComponent(back)}` }, "Sign in")));
    return;
  }
  const name = el("input", { name: "name", autocomplete: "name" });
  const password = el("input", { type: "password", name: "password", autocomplete: "new-password", minlength: 10, required: true });
  const form = el("form", { class: "auth-form", onsubmit: (e: Event) => { e.preventDefault(); accept(name.value, password.value); } },
    field("Your name", name), field("Choose a password", password, "At least 10 characters."), error,
    el("button", { type: "submit", class: "btn btn-primary" }, "Create account and accept"));
  const sso = info.sso ? ssoButton(config, "/") : null;
  app.replaceChildren(card("Accept the invitation", intro,
    sso, sso ? el("p", { class: "auth-or muted" }, `or create a password for ${info.email}`) : null, form));
  name.focus();
}

// ------------------------------------------------------------- account menu

export function isOrgAdmin(me: Me, org: string): boolean {
  return me.instance_admin || me.orgs.some((o) => o.org.id === org && o.role === "admin");
}

/** The account button of the home screen (nothing for `--auth local`). */
export function accountButton(me: Me, navigate: Navigate): HTMLElement | null {
  if (me.kind === "local") return null;
  const label = me.kind === "user" ? (me.user!.name || me.user!.email) : "Service token";
  const btn = el("button", { class: "btn account-btn", "aria-haspopup": "menu", title: me.user?.email ?? "" }, label);
  btn.addEventListener("click", () => {
    const r = btn.getBoundingClientRect();
    const admin = me.orgs.filter((o) => o.role === "admin");
    showMenu([
      ...admin.map((o) => ({ label: me.orgs.length > 1 ? `People of ${o.org.name}…` : "People…", action: () => peopleDialog(o.org.id, o.org.name, me) })),
      ...admin.map((o) => ({ label: me.orgs.length > 1 ? `Activity of ${o.org.name}…` : "Activity…", action: () => auditDialog(o.org.id) })),
      ...(me.user?.sign_in.includes("password") ? [{ label: "Change password…", action: changePassword }] : []),
      ...(admin.length || me.user?.sign_in.includes("password") ? ["sep" as const] : []),
      { label: "Sign out", action: async () => {
        await api.logout().catch(() => {});
        navigate("/login");
      } },
    ], r.left, r.bottom + 4);
  });
  return btn;
}

async function changePassword(): Promise<void> {
  await formDialog("Change password", [
    { name: "current", label: "Current password", type: "password" },
    { name: "next", label: "New password", type: "password", hint: "At least 10 characters. Your other sign-ins end." },
  ], "Change", async (v) => {
    try {
      await api.changePassword(v.current, v.next);
      toast("Password changed", "success");
      return null;
    } catch (e) {
      return (e as Error).message;
    }
  });
}

// --------------------------------------------------------------- dialogs

/** Replace an element's children, skipping absent ones. */
function fill(node: HTMLElement, ...children: (Node | string | null | undefined | false)[]): void {
  node.replaceChildren(...children.filter((c): c is Node | string => c !== null && c !== undefined && c !== false));
}

/** A dialog that re-renders its body; `close` ends it. */
function panel(title: string, render: (body: HTMLElement, close: () => void) => Promise<void>): void {
  const body = el("div", { class: "panel-body" });
  const box = el("div", { class: "dialog wide" }, el("h2", {}, title), body,
    el("div", { class: "dialog-actions" }, el("button", { class: "btn", onclick: () => modal.close() }, "Close")));
  const modal = openModal(box, () => modal.close());
  render(body, () => modal.close()).catch((e) => body.replaceChildren(el("p", { role: "alert" }, (e as Error).message)));
}

function roleSelect(value: string, roles: readonly string[], labels: Record<string, string>, onchange?: (v: string) => void, label = "Role"): HTMLSelectElement {
  const s = el("select", { "aria-label": label }, ...roles.map((r) => el("option", { value: r }, labels[r] ?? r)));
  s.value = value;
  if (onchange) s.addEventListener("change", () => onchange(s.value));
  return s;
}

const SPACE_ROLES = ["viewer", "editor", "owner"] as const;
const ORG_ROLES = ["member", "admin"] as const;

/** Who has which role in a space; owners can change them. */
export function membersDialog(space: SpaceListing, onChange: () => void): void {
  const manage = space.role === "owner";
  panel(`Members of “${space.name}”`, async function render(body) {
    const members: SpaceMember[] = await api.spaceMembers(space.id);
    const rows = members.map((m) => {
      const who = m.kind === "group" ? `Group: ${m.principal}` : (m.user ? `${m.user.name} <${m.user.email}>` : m.principal);
      const act = async (f: () => Promise<void>) => {
        try { await f(); onChange(); await render(body); } catch (e) { toast((e as Error).message, "error"); }
      };
      return el("div", { class: "member-row", role: "row" },
        el("span", { role: "cell" }, who),
        el("span", { role: "cell" }, manage
          ? roleSelect(m.role, SPACE_ROLES, ROLE_LABEL, (r) => act(() => api.setSpaceMember(space.id, m.kind, m.principal, r as SpaceRole)), `Role of ${who}`)
          : ROLE_LABEL[m.role]),
        el("span", { role: "cell" }, manage
          ? el("button", { class: "btn small", "aria-label": `Remove ${who}`, onclick: () => act(() => api.removeSpaceMember(space.id, m.kind, m.principal)) }, "Remove")
          : null));
    });
    const kind = roleSelect("user", ["user", "group"], { user: "Person (email)", group: "Identity-provider group" }, undefined, "Member kind");
    const who = el("input", { placeholder: "name@example.com or group name", "aria-label": "Email or group" });
    const role = roleSelect("viewer", SPACE_ROLES, ROLE_LABEL, undefined, "New member's role");
    const error = el("div", { class: "dialog-error", role: "alert" });
    const add = el("form", { class: "member-add", onsubmit: async (e: Event) => {
      e.preventDefault();
      error.textContent = "";
      try {
        await api.setSpaceMember(space.id, kind.value as "user" | "group", who.value.trim(), role.value as SpaceRole);
        onChange();
        await render(body);
      } catch (err) { error.textContent = (err as Error).message; }
    } }, kind, who, role, el("button", { type: "submit", class: "btn btn-primary" }, "Add"));
    fill(body,
      el("p", { class: "muted" }, "Organization admins are owners of every space. ",
        ...SPACE_ROLES.map((r) => el("span", { class: "role-hint" }, `${ROLE_LABEL[r]}: ${ROLE_HINT[r]}. `))),
      rows.length ? el("div", { class: "member-table", role: "table", "aria-label": "Members" }, ...rows) : el("p", {}, "No direct members yet."),
      manage ? add : null, manage ? error : null);
  });
}

/** Service tokens of a space: create (the secret is shown once) and revoke. */
export function tokensDialog(space: SpaceListing): void {
  panel(`Service tokens of “${space.name}”`, async function render(body) {
    const tokens: Token[] = await api.tokens(space.id);
    const fresh = el("div", { class: "token-secret" });
    const name = el("input", { placeholder: "e.g. nightly import", "aria-label": "Token name" });
    const role = roleSelect("viewer", ["viewer", "editor"], ROLE_LABEL, undefined, "Token role");
    const days = el("input", { type: "number", min: 1, max: 366, value: "90", "aria-label": "Days valid" });
    const error = el("div", { class: "dialog-error", role: "alert" });
    const form = el("form", { class: "member-add", onsubmit: async (e: Event) => {
      e.preventDefault();
      error.textContent = "";
      try {
        const t = await api.createToken(space.id, name.value, role.value as SpaceRole, Number(days.value));
        await render(body);
        const out = el("input", { readonly: true, class: "mono", value: t.secret, "aria-label": "New token" });
        body.prepend(el("div", { class: "token-secret", role: "status" },
          el("p", {}, "Copy this token now; it is not shown again. Send it as the header “Authorization: Bearer …”."), out));
        out.select();
      } catch (err) { error.textContent = (err as Error).message; }
    } }, name, role, days, el("span", { class: "muted" }, "days"), el("button", { type: "submit", class: "btn btn-primary" }, "Create token"));
    const rows = tokens.map((t) => el("div", { class: "member-row", role: "row" },
      el("span", { role: "cell" }, t.name),
      el("span", { role: "cell" }, ROLE_LABEL[t.role]),
      el("span", { role: "cell", class: "muted" }, t.revoked_at ? "revoked" : `expires ${new Date(t.expires_at).toLocaleDateString()}`),
      el("span", { role: "cell" }, t.revoked_at ? null : el("button", { class: "btn small", onclick: async () => {
        if (!(await confirmDialog(`Revoke “${t.name}”?`, "Anything using it stops working at once.", "Revoke"))) return;
        try { await api.revokeToken(space.id, t.id); await render(body); } catch (e) { toast((e as Error).message, "error"); }
      } }, "Revoke"))));
    body.replaceChildren(fresh,
      rows.length ? el("div", { class: "member-table", role: "table", "aria-label": "Tokens" }, ...rows) : el("p", {}, "No tokens."),
      form, error);
  });
}

/** Members of an organization and its invitations (admins). */
export function peopleDialog(org: string, orgName: string, me: Me): void {
  panel(`People of ${orgName}`, async function render(body) {
    const [members, invites, spaces] = await Promise.all([api.orgMembers(org), api.invites(org), api.spaces()]);
    const act = async (f: () => Promise<unknown>) => {
      try { await f(); await render(body); } catch (e) { toast((e as Error).message, "error"); }
    };
    const rows = members.map((m: OrgMember) => el("div", { class: "member-row", role: "row" },
      el("span", { role: "cell" }, `${m.user.name} <${m.user.email}>`, m.user.disabled ? el("span", { class: "badge" }, "disabled") : null),
      el("span", { role: "cell" }, roleSelect(m.role, ORG_ROLES, { member: "Member", admin: "Admin" },
        (r) => act(() => api.setOrgRole(org, m.user.id, r as OrgRole)), `Organization role of ${m.user.email}`)),
      el("span", { role: "cell" },
        me.instance_admin && m.user.id !== me.user?.id
          ? el("button", { class: "btn small", onclick: () => act(() => api.setUserDisabled(m.user.id, !m.user.disabled)) }, m.user.disabled ? "Enable" : "Disable")
          : null,
        m.user.id !== me.user?.id
          ? el("button", { class: "btn small", onclick: async () => {
            if (await confirmDialog(`Remove ${m.user.email}?`, "They lose access to every space of this organization at once.", "Remove")) await act(() => api.removeOrgMember(org, m.user.id));
          } }, "Remove")
          : null)));
    const email = el("input", { type: "email", placeholder: "name@example.com", "aria-label": "Email to invite", required: true });
    const orgRole = roleSelect("member", ORG_ROLES, { member: "Member", admin: "Admin" }, undefined, "Organization role");
    const orgSpaces = spaces.filter((s) => s.org_id === org);
    const space = el("select", { "aria-label": "Space" }, el("option", { value: "" }, "No space"), ...orgSpaces.map((s) => el("option", { value: s.id }, s.name)));
    const spaceRole = roleSelect("viewer", SPACE_ROLES, ROLE_LABEL, undefined, "Space role");
    const link = el("div", { class: "token-secret" });
    const error = el("div", { class: "dialog-error", role: "alert" });
    const form = el("form", { class: "member-add", onsubmit: async (e: Event) => {
      e.preventDefault();
      error.textContent = "";
      try {
        const r = await api.invite(org, email.value, orgRole.value as OrgRole, space.value || null, space.value ? spaceRole.value as SpaceRole : null);
        await render(body);
        const out = el("input", { readonly: true, class: "mono", value: location.origin + r.path, "aria-label": "Invitation link" });
        body.prepend(el("div", { class: "token-secret", role: "status" },
          el("p", {}, `Send this link to ${r.invite.email}. It works once, within 7 days, and is not shown again.`), out));
        out.select();
      } catch (err) { error.textContent = (err as Error).message; }
    } }, email, orgRole, space, spaceRole, el("button", { type: "submit", class: "btn btn-primary" }, "Invite"));
    const open = invites.filter((i: Invite) => !i.accepted_at && !i.revoked_at && i.expires_at > Date.now());
    fill(body, link,
      el("div", { class: "member-table", role: "table", "aria-label": "People" }, ...rows),
      el("h3", {}, "Invite someone"), form, error,
      open.length ? el("h3", {}, "Open invitations") : null,
      ...open.map((i) => el("div", { class: "member-row", role: "row" },
        el("span", { role: "cell" }, i.email), el("span", { role: "cell", class: "muted" }, `expires ${new Date(i.expires_at).toLocaleDateString()}`),
        el("span", { role: "cell" }, el("button", { class: "btn small", onclick: () => act(() => api.revokeInvite(org, i.id)) }, "Revoke")))));
  });
}

function describeEvent(e: AuditEvent): string {
  const d = e.detail as Record<string, unknown>;
  const extra = Object.entries(d).filter(([, v]) => v !== null && typeof v !== "object").map(([k, v]) => `${k}: ${v}`).join(", ");
  return `${e.action.replaceAll("_", " ")}${extra ? ` (${extra})` : ""}`;
}

/** Recent audit events of an organization (admins). */
export function auditDialog(org: string): void {
  panel("Activity", async (body) => {
    const events = await api.audit(org);
    body.replaceChildren(events.length
      ? el("div", { class: "member-table", role: "table", "aria-label": "Activity" }, ...events.map((e) => el("div", { class: "member-row", role: "row" },
        el("span", { role: "cell", class: "muted" }, new Date(e.at).toLocaleString()),
        el("span", { role: "cell" }, e.actor),
        el("span", { role: "cell" }, describeEvent(e), e.outcome === "denied" ? el("span", { class: "badge" }, "denied") : null))))
      : el("p", {}, "Nothing recorded yet."));
  });
}

export function isUnauthenticated(e: unknown): boolean {
  return e instanceof HttpError && e.status === 401;
}
