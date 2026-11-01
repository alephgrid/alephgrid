// M.07: Agent connections. An owner gives an MCP client read-only access to a space's
// workbooks (all of them, or some), for a number of days; the token is shown once with
// setup instructions; connections can be revoked, and each one's tool calls are listed.

import { api, type SpaceListing } from "./api";
import type { AgentActivity, AgentConnection } from "./types";
import { confirmDialog, el, openModal, toast } from "./ui";

function when(ms?: number | null): string {
  return ms ? new Date(ms).toLocaleString() : "never";
}

function setup(token: string): HTMLElement {
  const origin = location.origin;
  const http = `${origin}/mcp`;
  const claudeCode = `claude mcp add --transport http alephgrid ${http} --header "Authorization: Bearer ${token}"`;
  const desktop = JSON.stringify({ mcpServers: { alephgrid: { command: "alephgrid", args: ["mcp", "--url", origin], env: { ALEPHGRID_AGENT_TOKEN: token } } } }, null, 2);
  const copyable = (label: string, text: string, multiline = false) => {
    const box = multiline
      ? el("textarea", { readonly: true, class: "mono agent-setup-text", rows: 7, "aria-label": label }, text) as HTMLTextAreaElement
      : el("input", { readonly: true, class: "mono", value: text, "aria-label": label }) as HTMLInputElement;
    const copy = el("button", { class: "btn btn-small", type: "button", onclick: async () => {
      try { await navigator.clipboard.writeText(text); toast("Copied"); } catch { box.select(); }
    } }, "Copy");
    return el("div", { class: "agent-setup-item" }, el("span", { class: "agent-setup-label" }, label), el("div", { class: "agent-setup-row" }, box, copy));
  };
  return el("div", { class: "token-secret agent-setup", role: "status" },
    el("p", {}, el("strong", {}, "Copy the token now; it is not shown again. "), "Use it with any MCP client:"),
    copyable("Token", token),
    copyable("Streamable HTTP endpoint", http),
    copyable("Claude Code", claudeCode),
    copyable("Claude Desktop and other stdio clients (config)", desktop, true),
    el("p", { class: "dialog-lede" }, "The stdio form runs “alephgrid mcp”, which forwards to this server; the alephgrid program must be on the client's path."));
}

function activityDialog(c: AgentConnection): void {
  const body = el("div", { class: "agent-activity" }, el("p", { class: "muted" }, "Loading…"));
  const close = el("button", { class: "btn", type: "button" }, "Close");
  const dialog = el("div", { class: "dialog wide" }, el("h2", {}, `Activity of “${c.name}”`),
    el("p", { class: "dialog-lede" }, "Every tool call, newest first: what the agent asked for, the rows it got and the result handle, or the error."),
    body, el("div", { class: "dialog-actions" }, close));
  const modal = openModal(dialog, () => modal.close());
  close.addEventListener("click", () => modal.close());
  close.focus();
  api.agentActivity(c.id).then((list: AgentActivity[]) => {
    if (!list.length) { body.replaceChildren(el("p", { class: "muted" }, "No calls yet.")); return; }
    const rows = list.map((a) => {
      const d = a.detail as Record<string, unknown>;
      const what = (d.sql ?? d.table ?? d.sheet ?? d.query ?? d.analysis ?? d.handle ?? "") as string;
      const outcome = a.ok ? [d.rows_in_result !== undefined ? `${d.rows_in_result} rows` : "", d.truncated ? "truncated" : "", d.revision !== undefined ? `revision ${d.revision}` : ""].filter(Boolean).join(" · ") : String(d.error ?? "failed");
      return el("tr", {},
        el("td", {}, new Date(a.at_ms).toLocaleString()),
        el("td", {}, a.tool),
        el("td", { class: "mono agent-what" }, what),
        el("td", { class: a.ok ? "" : "agent-failed" }, outcome));
    });
    body.replaceChildren(el("table", { class: "result-table" },
      el("caption", { class: "sr-only" }, "Tool calls"),
      el("thead", {}, el("tr", {}, el("th", { scope: "col" }, "When"), el("th", { scope: "col" }, "Tool"), el("th", { scope: "col" }, "Asked for"), el("th", { scope: "col" }, "Outcome"))),
      el("tbody", {}, ...rows)));
  }).catch((e) => body.replaceChildren(el("p", { class: "dialog-error" }, (e as Error).message)));
}

/** The agent connections of one space, with a form for a new one. */
export function agentsDialog(space: SpaceListing, workbooks: { id: string; name: string }[]): void {
  const list = el("div", { class: "member-table", role: "table", "aria-label": "Agent connections" });
  const fresh = el("div", {});
  const name = el("input", { placeholder: "e.g. Claude on my laptop", "aria-label": "Connection name", required: true }) as HTMLInputElement;
  const days = el("input", { type: "number", min: 1, max: 366, value: "30", "aria-label": "Days valid" }) as HTMLInputElement;
  const scope = el("select", { "aria-label": "Workbooks it may read" },
    el("option", { value: "all" }, "Every workbook in this space"), el("option", { value: "some" }, "Only these workbooks…")) as HTMLSelectElement;
  const boxes = workbooks.map((w) => ({ w, box: el("input", { type: "checkbox", "aria-label": w.name }) as HTMLInputElement }));
  const picks = el("div", { class: "pick-list", hidden: true, role: "group", "aria-label": "Workbooks" },
    ...(boxes.length ? boxes.map(({ w, box }) => el("label", { class: "pick-item" }, box, w.name)) : [el("p", { class: "muted" }, "No workbooks in this space yet.")]));
  scope.addEventListener("change", () => { picks.hidden = scope.value !== "some"; });
  const error = el("div", { class: "dialog-error", role: "alert" });
  const create = el("button", { class: "btn btn-primary", type: "submit" }, "Create connection");
  const form = el("form", { class: "agent-form" },
    el("label", { class: "field" }, el("span", {}, "Name"), name),
    el("label", { class: "field" }, el("span", {}, "Can read"), scope), picks,
    el("label", { class: "field" }, el("span", {}, "Days valid"), days),
    el("div", { class: "dialog-actions" }, create)) as HTMLFormElement;

  const render = async () => {
    let all: AgentConnection[] = [];
    try { all = (await api.agents()).filter((c) => c.space_id === space.id); } catch (e) { error.textContent = (e as Error).message; }
    const label = (c: AgentConnection) => c.workbooks ? c.workbooks.map((id) => workbooks.find((w) => w.id === id)?.name ?? id).join(", ") : "every workbook";
    list.replaceChildren(...(all.length ? all.map((c) => el("div", { class: "member-row agent-row", role: "row" },
      el("span", { role: "cell" }, el("strong", {}, c.name), el("span", { class: "muted agent-scope" }, ` reads ${label(c)}`)),
      el("span", { role: "cell", class: "muted" }, c.revoked_at ? "revoked" : c.expires_at < Date.now() ? "expired" : `until ${new Date(c.expires_at).toLocaleDateString()}`),
      el("span", { role: "cell", class: "muted" }, `last used ${when(c.last_used)}`),
      el("span", { role: "cell", class: "agent-actions" },
        el("button", { class: "btn btn-small", type: "button", "aria-label": `Activity of ${c.name}`, onclick: () => activityDialog(c) }, "Activity"),
        c.revoked_at ? null : el("button", { class: "btn btn-small", type: "button", "aria-label": `Revoke ${c.name}`, onclick: async () => {
          if (!(await confirmDialog(`Revoke “${c.name}”?`, "The agent loses access at once, its results included.", "Revoke"))) return;
          try { await api.revokeAgent(c.id); await render(); } catch (e) { toast((e as Error).message, "error"); }
        } }, "Revoke"))))
      : [el("p", { class: "muted" }, "No agent connections yet.")]));
  };
  form.addEventListener("submit", async (e) => {
    e.preventDefault();
    error.textContent = "";
    const chosen = scope.value === "some" ? boxes.filter((b) => b.box.checked).map((b) => b.w.id) : null;
    if (chosen && !chosen.length) { error.textContent = "Choose at least one workbook."; return; }
    try {
      const out = await api.createAgent(name.value.trim(), space.id, chosen, Number(days.value));
      fresh.replaceChildren(setup(out.secret));
      name.value = "";
      await render();
    } catch (err) { error.textContent = (err as Error).message; }
  });
  const close = el("button", { class: "btn", type: "button" }, "Close");
  const dialog = el("div", { class: "dialog wide agents-dialog" },
    el("h2", {}, `Agent connections · ${space.name}`),
    el("p", { class: "dialog-lede" }, "Let an AI agent (an MCP client such as Claude) read workbooks here: discover tables, run read-only queries and analyses, with every result pinned to a receipt. Connections cannot change anything. What the agent reads goes to its model provider, under that provider's terms."),
    fresh, list, el("h3", {}, "New connection"), form, error,
    el("div", { class: "dialog-actions" }, close));
  const modal = openModal(dialog, () => modal.close());
  close.addEventListener("click", () => modal.close());
  void render();
  name.focus();
}
