import type { Rpc } from "./api";
import type { HistoryEntry } from "./protocol";
import { el, openModal, toast } from "./ui";

/** How many recent changes the dialog lists. */
const SHOWN = 200;

function describe(h: HistoryEntry): string {
  if (h.op === "undo" || h.op === "redo") return `${h.op === "undo" ? "Undo" : "Redo"} of revision ${h.undo?.undoes ?? "?"}`;
  return `${h.op.replace(/_/g, " ")}: ${h.summary}`;
}

/**
 * The workbook's recent changes, newest first (7.09). Table changes that can still be taken
 * back have Undo: it is refused, with the reason, when someone changed what it would undo
 * since. Undone changes say which revision took them back.
 */
export function historyDialog(rpc: Rpc, onUndone: () => void): void {
  const body = el("div", { class: "panel-body" });
  const box = el("div", { class: "dialog wide" },
    el("h2", {}, "Change history"),
    el("p", { class: "muted" }, "Everyone's changes to this workbook. Undo takes a table change back as a new change, unless someone changed the same cells or rows since."),
    body,
    el("div", { class: "dialog-actions" }, el("button", { class: "btn", onclick: () => modal.close() }, "Close")));
  const modal = openModal(box, () => modal.close());
  const render = async (): Promise<void> => {
    const after = Math.max(0, rpc.revisions.last - SHOWN);
    const entries = (await rpc.call("history", { after, limit: SHOWN })).reverse();
    if (!entries.length) {
      body.replaceChildren(el("p", {}, "No changes yet."));
      return;
    }
    body.replaceChildren(el("div", { class: "member-table", role: "table", "aria-label": "Changes" }, ...entries.map((h) => {
      const u = h.undo;
      const action = !u ? el("span", { role: "cell", class: "muted" }, "")
        : u.undone_by ? el("span", { role: "cell", class: "muted" }, `Undone in revision ${u.undone_by}`)
        : el("span", { role: "cell" }, el("button", {
          class: "btn btn-small", "aria-label": `Undo revision ${h.revision}`,
          onclick: async () => {
            try {
              const out = await rpc.call("undo", { revision: h.revision });
              if (out) toast(`Took back revision ${out.undone} in ${out.table}`, "success");
              onUndone();
              await render();
            } catch (e) {
              toast((e as Error).message, "error");
            }
          },
        }, "Undo"));
      return el("div", { class: "member-row", role: "row" },
        el("span", { role: "cell", class: "muted" }, `#${h.revision}`),
        el("span", { role: "cell", class: "muted" }, new Date(h.at_ms).toLocaleString()),
        el("span", { role: "cell" }, h.actor ?? ""),
        el("span", { role: "cell" }, describe(h)),
        action);
    })));
  };
  render().catch((e) => body.replaceChildren(el("p", { role: "alert" }, (e as Error).message)));
}
