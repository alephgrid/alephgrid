// A workbook's compute as people see it (7.13–7.16, docs/COMPUTE.md): opening it starts
// its compute without a separate step; while that happens the page says what it is waiting
// for, failures say what to do, a paused workbook says what is kept, and owners can choose a
// size.

import { api, HttpError } from "./api";
import type { ComputeProfile, ProfileChoice, RuntimeStatus } from "./protocol";
import { el, openModal, toast } from "./ui";

/** How often the opening screen asks for the status. */
const POLL_MS = 400;

export function bytes(n: number): string {
  const units = ["B", "KB", "MB", "GB", "TB"];
  let v = n, i = 0;
  while (v >= 1024 && i < units.length - 1) { v /= 1024; i++; }
  return `${v < 10 && i ? v.toFixed(1) : Math.round(v)} ${units[i]}`;
}

export function minutes(ms: number): string {
  const m = Math.round(ms / 60_000);
  return m >= 1 ? `${m} minute${m === 1 ? "" : "s"}` : `${Math.max(1, Math.round(ms / 1000))} seconds`;
}

export function profileLabel(id: string): string {
  return id.charAt(0).toUpperCase() + id.slice(1);
}

/** What a profile reserves, in words. */
export function describeProfile(p: ComputeProfile): string {
  if (p.memory_bytes === undefined) return "an equal share of the server";
  const threads = p.threads ?? 1;
  return `${bytes(p.memory_bytes)} and ${threads} thread${threads === 1 ? "" : "s"}${p.dedicated ? ", dedicated: never closed to make room for others" : ""}`;
}

const FAILURE_TITLE: Record<string, string> = {
  capacity: "The server is at capacity",
  quota: "This space reached its compute limit",
  incompatible: "This workbook needs a different server version",
  owned_elsewhere: "Another server has this workbook open",
  startup_failed: "This workbook can't be opened right now",
  lost: "The workbook's compute was lost",
  cancelled: "Opening was cancelled",
};

const FAILURE_HINT: Record<string, string> = {
  capacity: "Try again in a few minutes, or close workbooks you are not using.",
  quota: "Close a workbook of this space (they also close by themselves when idle), or ask an organization admin to raise the limit.",
  incompatible: "Open it with a server version that supports it, or ask your administrator.",
  owned_elsewhere: "Only one server process can open a workbook at a time. Close it there, or ask your administrator.",
  startup_failed: "Try again. If it keeps failing, ask your administrator; the reason is below.",
  lost: "Try again.",
  cancelled: "Open it again when you are ready.",
};

function failureKind(e: unknown): string {
  if (!(e instanceof HttpError)) return "startup_failed";
  return e.status === 429 ? "quota" : e.status === 423 ? "owned_elsewhere" : e.status === 409 ? "incompatible" : "startup_failed";
}

type Shown = { title: string; text: string; actions: HTMLElement[]; busy?: boolean };

function show(host: HTMLElement, s: Shown): void {
  host.replaceChildren(el("div", { class: "fatal workbook-failure compute-status", role: s.busy ? "status" : "alert", "aria-live": "polite" },
    el("h2", {}, s.busy ? el("span", { class: "spinner", "aria-hidden": "true" }) : null, s.title),
    el("p", {}, s.text),
    s.actions.length ? el("div", { class: "empty-actions" }, ...s.actions) : null));
}

/**
 * Start `workbook`'s compute if it is not running and wait until it is ready, showing in
 * `host` what it waits for (after a moment, so a quick start does not flash). Resolves
 * true when ready, false when the person gave up (`leave` was called).
 */
export function waitForCompute(workbook: string, host: HTMLElement, leave: () => void, alive: () => boolean = () => true): Promise<boolean> {
  return new Promise((resolve) => {
    let done = false;
    let shownAt = 0;
    const started = Date.now();
    const finish = (ready: boolean) => {
      if (done) return;
      done = true;
      if (!ready) leave();
      resolve(ready);
    };
    const back = () => el("button", { class: "btn", onclick: () => finish(false) }, "Back to workbooks");
    const fail = (kind: string, message: string) => {
      show(host, {
        title: FAILURE_TITLE[kind] ?? FAILURE_TITLE.startup_failed,
        text: `${FAILURE_HINT[kind] ?? ""} ${message}`.trim(),
        actions: [el("button", { class: "btn btn-primary", onclick: () => { attempts = 0; void step(true); } }, "Try again"), back()],
      });
    };
    let attempts = 0;
    const step = async (start: boolean): Promise<void> => {
      if (!done && !alive()) { done = true; resolve(false); }
      if (done) return;
      let s: RuntimeStatus;
      try {
        s = start
          ? await api.runtimeAction(workbook, { action: "start", wait: false })
          : await api.runtime(workbook);
      } catch (e) {
        fail(failureKind(e), (e as Error).message);
        return;
      }
      if (done) return;
      const a = s.allocation;
      const state = a?.state ?? "stopped";
      if (state === "ready") { finish(true); return; }
      if (state === "failed") { fail(a?.failure?.kind ?? "startup_failed", a?.failure?.message ?? ""); return; }
      if (state === "stopped") {
        // Stopped by someone (or between a close and the next start): start it again.
        if (++attempts > 3) { fail(a?.failure?.kind ?? "cancelled", a?.failure?.message ?? ""); return; }
        setTimeout(() => void step(true), POLL_MS);
        return;
      }
      if (shownAt || Date.now() - started > 300) {
        shownAt ||= Date.now();
        const cancel = el("button", { class: "btn", onclick: async () => {
          try { await api.runtimeAction(workbook, { action: "cancel" }); } catch { /* shown by the next status */ }
          finish(false);
        } }, "Cancel");
        if (state === "requested" && s.queue_position) {
          const ahead = s.queue_position - 1;
          show(host, {
            title: "Waiting for room on the server", busy: true,
            text: `${ahead ? `${ahead} workbook${ahead === 1 ? "" : "s"} ahead of this one. ` : "This workbook is next. "}It opens by itself as soon as the server has room.`,
            actions: [cancel],
          });
        } else if (state === "draining") {
          show(host, { title: "The workbook is closing", busy: true, text: "It is saving its work and opens again right after.", actions: [back()] });
        } else {
          show(host, { title: "Starting the workbook…", busy: true, text: s.profile !== "standard" ? `With the ${profileLabel(s.profile)} profile.` : "This takes a moment.", actions: [cancel] });
        }
      }
      setTimeout(() => void step(false), POLL_MS);
    };
    void step(true);
  });
}

/** What the paused screen says: what was released and what is kept. */
export function pausedText(s: RuntimeStatus | null): string {
  const idle = s ? minutes(s.policy.idle_timeout_ms) : "a while";
  const kept = s ? `${bytes(s.storage_bytes)} on disk, ` : "";
  return `Its compute was released after ${idle} without activity, so it reserves nothing on the server now. `
    + `Everything is saved and kept as it is (${kept}until it is deleted). Reopening takes a moment.`;
}

/**
 * The workbook's compute: its state, profile and what it reserves, how long it stays open
 * without activity, and (for those allowed) another profile, applied by a controlled
 * restart that collaborators are told about.
 */
export function computeDialog(workbook: string): void {
  const body = el("div", { class: "panel-body" });
  const box = el("div", { class: "dialog wide" },
    el("h2", {}, "Compute"),
    body,
    el("div", { class: "dialog-actions" }, el("button", { class: "btn", onclick: () => modal.close() }, "Close")));
  const modal = openModal(box, () => modal.close());
  const render = async (): Promise<void> => {
    const [s, choices] = await Promise.all([api.runtime(workbook), api.profiles(workbook)]);
    const a = s.allocation;
    const running = a && a.state === "ready";
    const current = choices.find((c) => c.profile.id === (a?.profile ?? s.profile));
    const facts = el("dl", { class: "facts" },
      el("dt", {}, "State"), el("dd", {}, running ? `Running (${s.sessions} connected)` : a ? a.state : "stopped"),
      el("dt", {}, "Profile"), el("dd", {}, `${profileLabel(a?.profile ?? s.profile)}: ${current ? describeProfile(current.profile) : ""}`),
      running ? el("dt", {}, "Reserved now") : null,
      running ? el("dd", {}, `${bytes(a.reservation.memory_bytes)}, ${a.reservation.threads} thread${a.reservation.threads === 1 ? "" : "s"}`) : null,
      el("dt", {}, "Closes after"), el("dd", {}, `${minutes(s.policy.idle_timeout_ms)} without activity; open tabs alone do not keep it running. Work in progress keeps it open.`),
      el("dt", {}, "Storage"), el("dd", {}, `${bytes(s.storage_bytes)}, kept while its compute is stopped`));
    const notes = s.note ? el("p", { class: "muted" }, s.note) : null;
    const options = choices.filter((c) => c.granted);
    const list = el("div", { class: "profile-list", role: "radiogroup", "aria-label": "Profile" },
      ...options.map((c: ProfileChoice) => el("label", { class: "profile-option" },
        el("input", { type: "radio", name: "profile", value: c.profile.id, checked: c.profile.id === s.profile, disabled: !c.allowed && c.profile.id !== s.profile }),
        el("span", {}, el("strong", {}, profileLabel(c.profile.id)), " — ", describeProfile(c.profile),
          c.reason && c.profile.id !== s.profile ? el("span", { class: "muted" }, ` (${c.reason})`) : null))));
    const apply = el("button", { class: "btn btn-primary", onclick: async () => {
      const chosen = list.querySelector<HTMLInputElement>("input:checked")?.value;
      if (!chosen) return;
      apply.disabled = true;
      apply.textContent = "Restarting…";
      try {
        const out = await api.runtimeAction(workbook, { action: "restart", profile: chosen });
        toast(`Running with the ${profileLabel(out.allocation?.profile ?? chosen)} profile.`, "success");
      } catch (e) {
        toast((e as Error).message, "error");
      }
      await render();
    } }, "Restart with this profile");
    const canChoose = options.some((c) => c.allowed && c.profile.id !== s.profile);
    body.replaceChildren(facts, notes ?? "",
      options.length > 1 ? el("h3", {}, "Size") : "",
      options.length > 1 ? list : "",
      options.length > 1 ? el("p", { class: "muted" }, "Changing the size restarts the workbook's compute: everyone connected sees it reconnect, and nothing is lost. A larger profile reserves its memory while the workbook is open.") : "",
      canChoose ? el("div", { class: "dialog-actions" }, apply) : "");
  };
  render().catch((e) => body.replaceChildren(el("p", { role: "alert" }, (e as Error).message)));
}
