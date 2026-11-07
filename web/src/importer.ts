// Import with preview (2.05): upload with progress → preview with per-column types →
// import with progress and cancel → actionable errors that can be fixed and retried
// without uploading again.

import { api, HttpError, stageUpload } from "./api";
import { formatValue } from "./format";
import type { ColumnType, FilePreview, Imported, ImportProblem, Kind, UploadCreated } from "./types";
import { busy, el, markInvalid, openModal } from "./ui";

/** Friendly names for the type choices. */
const TYPE_LABELS: Record<string, string> = {
  VARCHAR: "Text",
  BIGINT: "Whole number",
  DOUBLE: "Number",
  "DECIMAL(18,2)": "Decimal, 2 places",
  "DECIMAL(38,10)": "Decimal, 10 places",
  BOOLEAN: "True/false",
  DATE: "Date",
  TIMESTAMP: "Date and time",
  TIMESTAMPTZ: "Date and time with zone",
  TIME: "Time",
};

function kindOf(sqlType: string): Kind {
  const t = sqlType.toUpperCase();
  if (["BIGINT", "INTEGER", "SMALLINT", "TINYINT", "HUGEINT", "UBIGINT"].includes(t)) return "int";
  if (t === "DOUBLE" || t === "FLOAT" || t.startsWith("DECIMAL")) return "number";
  if (t === "BOOLEAN") return "bool";
  if (t === "DATE") return "date";
  if (t.startsWith("TIMESTAMP")) return "datetime";
  if (t === "TIME") return "time";
  return "text";
}

function bytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 ** 2) return `${(n / 1024).toFixed(1)} KB`;
  if (n < 1024 ** 3) return `${(n / 1024 ** 2).toFixed(1)} MB`;
  return `${(n / 1024 ** 3).toFixed(2)} GB`;
}

/**
 * Run the whole import flow for one file. Resolves with what was imported, or null when
 * the user cancelled (nothing is left on the server in that case).
 */
export async function importFile(workbookId: string, file: File): Promise<Imported | null> {
  const staged = await upload(workbookId, file);
  if (!staged) return null;
  return review(workbookId, staged);
}

/** Upload with a progress dialog; null when cancelled. */
function upload(workbookId: string, file: File): Promise<UploadCreated | null> {
  return new Promise((resolve, reject) => {
    const bar = el("progress", { max: 1, value: 0, "aria-label": `Uploading ${file.name}` }) as HTMLProgressElement;
    const label = el("p", { class: "muted" }, `Uploading ${file.name} (${bytes(file.size)})…`);
    // A failed upload says why in this dialog (U.11a) and is reported once it is closed.
    let failure: unknown = null;
    const stop = () => { if (failure) { modal.close(); reject(failure); } else job.abort(); };
    const error = el("div", { class: "dialog-error", role: "alert" });
    const cancel = el("button", { class: "btn", type: "button", onclick: stop }, "Cancel");
    const modal = openModal(
      el("div", { class: "dialog", "data-testid": "upload-dialog" }, el("h2", {}, "Import a file"), label, bar, error,
        el("div", { class: "dialog-actions" }, cancel)),
      stop,
    );
    cancel.focus();
    const job = stageUpload(workbookId, file, (f) => {
      bar.value = f;
      label.textContent = f < 1 ? `Uploading ${file.name} · ${Math.round(f * 100)}%` : `Reading ${file.name}…`;
    });
    job.done.then(
      (staged) => { modal.close(); resolve(staged); },
      (e) => {
        if (e instanceof DOMException && e.name === "AbortError") { modal.close(); resolve(null); return; }
        failure = e;
        bar.hidden = true;
        label.textContent = `${file.name} could not be uploaded.`;
        error.textContent = (e as Error).message;
        cancel.textContent = "Close";
        cancel.focus();
      },
    );
  });
}

/** The preview dialog. Resolves after a successful import or a cancel. */
function review(workbookId: string, staged: UploadCreated): Promise<Imported | null> {
  return new Promise((resolve) => {
    const preview = staged.preview;
    const error = el("div", { class: "dialog-error", role: "alert" });
    const fixes = el("div", { class: "import-fixes" });
    const progress = el("div", { class: "import-progress", hidden: true },
      el("progress", { max: 100, "aria-label": "Import progress" }), el("span", { class: "muted" }));
    const importBtn = el("button", { class: "btn btn-primary", type: "submit" }, preview.kind === "sheets" ? "Import sheets" : "Import table");
    const cancelBtn = el("button", { class: "btn", type: "button" }, "Cancel");
    let importing = false;
    let job: string | null = null;
    /** The column whose type the last refusal is about (U.11a), until it is changed. */
    let invalid: HTMLSelectElement | null = null;
    const clearInvalid = () => { if (invalid) { markInvalid(invalid, false); invalid = null; } };
    let closed = false;

    const body = el("div", { class: "import-body" });
    let nameInput: HTMLInputElement | null = null;
    const selects = new Map<string, HTMLSelectElement>();
    let current: FilePreview | null = preview.kind === "table" ? preview.preview : null;
    const table = el("div", { class: "import-preview" });
    const columnsBox = el("div", { class: "import-columns" });

    if (preview.kind === "sheets") {
      body.append(
        el("p", {}, `${staged.filename} has ${preview.sheets.length} sheet${preview.sheets.length === 1 ? "" : "s"}. They become editable sheets in this workbook (Excel formulas, up to the sheet size limit).`),
        el("ul", { class: "import-sheets" }, ...preview.sheets.map((s) => el("li", {}, s.name, el("span", { class: "muted" }, s.rows ? ` · ${s.rows} × ${s.columns} cells used` : " · empty")))),
        preview.warnings.length
          ? el("div", { class: "notice", role: "note" }, el("strong", {}, "Not everything in this file comes across:"),
            el("ul", {}, ...preview.warnings.map((w) => el("li", {}, w))))
          : el("p", { class: "muted" }, "No compatibility issues were found."),
      );
    } else {
      const p = preview.preview;
      nameInput = el("input", { name: "table", value: p.suggested_name, "aria-label": "Table name" }) as HTMLInputElement;
      body.append(
        el("p", { class: "muted" }, `${p.format} · ${bytes(staged.size_bytes)} · ${p.columns.length} column${p.columns.length === 1 ? "" : "s"}. Tables hold any number of rows; check the column types below.`),
        el("label", { class: "field" }, el("span", {}, "Table name"), nameInput),
        el("h3", { class: "import-heading" }, "Columns"),
        columnsBox,
        el("h3", { class: "import-heading" }, "First rows"),
        table,
      );
      renderTable(p);
    }

    function renderTable(p: FilePreview): void {
      current = p;
      const head = el("tr", {}, ...p.columns.map((c) => {
        let select = selects.get(c.name);
        if (!select) {
          const choices = [...p.type_choices];
          if (!choices.includes(c.detected)) choices.unshift(c.detected);
          select = el("select", { "aria-label": `Type for column ${c.name}`, "data-column": c.name,
            onchange: () => { clearInvalid(); void repreview(); } },
          ...choices.map((t) => el("option", { value: t },
            `${TYPE_LABELS[t] ?? t}${t === c.detected ? " (detected)" : ""}`))) as HTMLSelectElement;
          select.value = c.sql_type;
          selects.set(c.name, select);
        }
        return el("th", { scope: "col" }, el("div", { class: "import-col" }, c.name));
      }));
      // U.19: one row per column with its type and a few values as they will be imported.
      columnsBox.replaceChildren(el("table", { class: "result-table import-column-table" },
        el("caption", { class: "sr-only" }, "Columns, their types and sample values"),
        el("thead", {}, el("tr", {}, el("th", { scope: "col" }, "Column"), el("th", { scope: "col" }, "Type"), el("th", { scope: "col" }, "Sample values"))),
        el("tbody", {}, ...p.columns.map((c, i) => {
          const kind = kindOf(c.sql_type);
          const samples = p.rows.map((r) => r[i]).filter((v) => v !== null && v !== "").slice(0, 3).map((v) => formatValue(v, kind));
          return el("tr", {}, el("th", { scope: "row" }, c.name), el("td", {}, selects.get(c.name)!),
            el("td", { class: "import-samples" }, samples.length ? samples.join(" · ") : el("span", { class: "muted" }, "(empty)")));
        }))));
      const rows = p.rows.map((r) => el("tr", {}, ...r.map((v, i) => el("td", { class: kindOf(p.columns[i].sql_type) === "text" ? "" : "num" },
        formatValue(v, kindOf(p.columns[i].sql_type))))));
      table.replaceChildren(el("table", { class: "result-table" },
        el("caption", { class: "sr-only" }, `First ${p.rows.length} rows as they will be imported`),
        el("thead", {}, head), el("tbody", {}, ...rows)));
    }

    function types(): ColumnType[] {
      if (!current) return [];
      return current.columns
        .filter((c) => selects.get(c.name) && selects.get(c.name)!.value !== c.detected)
        .map((c) => ({ name: c.name, sql_type: selects.get(c.name)!.value }));
    }

    let previewSeq = 0;
    async function repreview(): Promise<void> {
      const seq = ++previewSeq;
      error.textContent = "";
      fixes.replaceChildren();
      try {
        const p = await api.previewUpload(workbookId, staged.upload_id, { types: types() });
        if (seq === previewSeq && p.kind === "table") renderTable(p.preview);
      } catch (e) {
        if (seq === previewSeq) showError(e);
      }
    }

    function showError(e: unknown): void {
      const problem: ImportProblem | undefined = e instanceof HttpError ? e.problem : undefined;
      error.textContent = (e as Error).message;
      fixes.replaceChildren();
      if (problem?.column && problem.suggested_type && selects.has(problem.column)) {
        const column = problem.column, type = problem.suggested_type;
        fixes.append(el("button", { class: "btn", type: "button", onclick: () => {
          selects.get(column)!.value = type;
          clearInvalid();
          fixes.replaceChildren();
          submit(new Event("submit"));
        } }, `Import “${column}” as ${TYPE_LABELS[type] ?? type} and try again`));
      }
      // The column's type is the refused field: marked, described by the error, focused.
      clearInvalid();
      const select = problem?.column ? selects.get(problem.column) : undefined;
      if (select) { invalid = select; markInvalid(select, error); }
    }

    async function pollProgress(id: string): Promise<void> {
      const bar = progress.querySelector("progress") as HTMLProgressElement;
      const text = progress.querySelector("span")!;
      const started = performance.now();
      while (importing && job === id && !closed) {
        try {
          const st = await api.jobStatus(workbookId, id);
          if (st.running && st.percentage !== undefined) {
            bar.value = st.percentage;
            text.textContent = ` ${Math.round(st.percentage)}% · ${((performance.now() - started) / 1000).toFixed(0)}s`;
          } else {
            bar.removeAttribute("value");
          }
        } catch { /* keep polling */ }
        await new Promise((r) => setTimeout(r, 250));
      }
    }

    async function submit(e: Event): Promise<void> {
      e.preventDefault();
      if (importing) return;
      importing = true;
      error.textContent = "";
      fixes.replaceChildren();
      // Busy while the server imports: it cannot be pressed twice (U.11a).
      const restore = busy(importBtn);
      for (const s of selects.values()) s.disabled = true;
      if (nameInput) nameInput.disabled = true;
      job = `import-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 8)}`;
      progress.hidden = false;
      cancelBtn.textContent = "Stop import";
      const id = job;
      pollProgress(id);
      try {
        const result = await api.importUpload(workbookId, staged.upload_id, {
          table: nameInput?.value.trim() || undefined, types: types(), job: id,
        });
        finish(result);
      } catch (err) {
        importing = false;
        job = null;
        progress.hidden = true;
        cancelBtn.textContent = "Cancel";
        restore();
        for (const s of selects.values()) s.disabled = false;
        if (nameInput) nameInput.disabled = false;
        showError(err);
      }
    }

    function finish(result: Imported | null): void {
      if (closed) return;
      closed = true;
      importing = false;
      modal.close();
      if (!result) api.cancelUpload(workbookId, staged.upload_id).catch(() => {});
      resolve(result);
    }

    function cancel(): void {
      if (importing && job) {
        // Stop the running import; the dialog stays open so the user can retry or leave.
        api.cancelJob(workbookId, job).catch(() => {});
        return;
      }
      finish(null);
    }
    cancelBtn.addEventListener("click", cancel);

    const form = el("form", { class: preview.kind === "table" ? "dialog import-pane" : "dialog dialog-wide", onsubmit: submit, "data-testid": "import-dialog" },
      el("h2", {}, `Import ${staged.filename}`),
      body,
      error,
      fixes,
      progress,
      el("div", { class: "dialog-actions" }, cancelBtn, importBtn),
    );
    const modal = openModal(form, cancel);
    (nameInput ?? importBtn).focus();
  });
}
