// S.11: the comments pane of a sheet: threads by cell with author and time, reply, resolve,
// reopen and delete. Changes by others arrive as sheet events and the pane reloads.

import type { Rpc } from "./api";
import { clearError, el, toastError } from "./ui";
import { icon } from "./icons";
import { columnLetter } from "./format";
import type { CellComment } from "./protocol";

export interface CommentsHost {
  sheet(): number | null;
  go(row: number, column: number): void;
  pane(panel: HTMLElement): { close: () => void };
  changed(list: CellComment[]): void;
}

const when = (ms: number) => new Date(ms).toLocaleString();

export class CommentsPane {
  private list = el("div", { class: "comments-list" });
  private panel: HTMLElement;
  private handle: { close: () => void };
  closed = false;

  constructor(private rpc: Rpc, private host: CommentsHost) {
    const close = el("button", { class: "icon-btn", type: "button", "aria-label": "Close comments" }, icon("x", 14));
    close.addEventListener("click", () => this.close());
    // Errors of the pane's actions show in it, not only in a toast (U.11a).
    this.panel = el("aside", { class: "comments-pane analyze-pane", "aria-label": "Comments", "data-errors": true },
      el("header", { class: "analyze-head" }, el("h2", {}, "Comments"), close), this.list);
    this.panel.addEventListener("keydown", (e) => { if (e.key === "Escape") { e.preventDefault(); this.close(); } });
    this.handle = host.pane(this.panel);
    void this.reload();
  }

  async reload(): Promise<void> {
    const sheet = this.host.sheet();
    if (sheet === null || this.closed) return;
    let all: CellComment[];
    try { all = await this.rpc.call("comments", { sheet }); } catch (e) { toastError(e, this.panel); return; }
    this.host.changed(all);
    const threads = all.filter((c) => !c.parent);
    if (!threads.length) { this.list.replaceChildren(el("p", { class: "analyze-hint" }, "No comments on this sheet. Right-click a cell and choose Add comment.")); return; }
    this.list.replaceChildren(...threads.map((t) => {
      const replies = all.filter((c) => c.parent === t.id);
      const item = (c: CellComment) => el("div", { class: "comment" },
        el("div", { class: "comment-meta" }, el("strong", {}, c.author), el("span", { class: "muted" }, ` · ${when(c.created_ms)}${c.edited_ms ? " · edited" : ""}`)),
        el("p", { class: "comment-text" }, c.text));
      const ref = `${columnLetter(t.column)}${t.row + 1}`;
      const reply = el("input", { type: "text", placeholder: "Reply", "aria-label": `Reply to the comment on ${ref}` }) as HTMLInputElement;
      reply.addEventListener("keydown", (e) => { if (e.key === "Enter" && reply.value.trim()) void this.act(() => this.rpc.call("add_comment", { sheet: this.host.sheet()!, row: t.row, column: t.column, text: reply.value, parent: t.id })); });
      const btn = (label: string, run: () => Promise<unknown>) => { const b = el("button", { class: "btn btn-ghost btn-small", type: "button" }, label); b.addEventListener("click", () => void this.act(run)); return b; };
      const go = el("button", { class: "comment-cell", type: "button", "aria-label": `Go to ${ref}` }, ref);
      go.addEventListener("click", () => this.host.go(t.row, t.column));
      return el("section", { class: `comment-thread${t.resolved ? " resolved" : ""}`, "aria-label": `Comment on ${ref}` },
        el("div", { class: "comment-head" }, go, t.resolved ? el("span", { class: "badge" }, "Resolved") : null),
        item(t), ...replies.map(item), reply,
        el("div", { class: "comment-actions" },
          btn(t.resolved ? "Reopen" : "Resolve", () => this.rpc.call("resolve_comment", { sheet: this.host.sheet()!, comment: t.id, resolved: !t.resolved })),
          btn("Delete", () => this.rpc.call("delete_comment", { sheet: this.host.sheet()!, comment: t.id }))));
    }));
  }

  private async act(run: () => Promise<unknown>): Promise<void> {
    clearError(this.panel);
    try { await run(); await this.reload(); } catch (e) { toastError(e, this.panel); }
  }

  close(): void {
    if (this.closed) return;
    this.closed = true;
    this.handle.close();
  }
}
