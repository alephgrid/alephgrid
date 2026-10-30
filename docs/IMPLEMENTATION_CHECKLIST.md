# Alephgrid implementation checklist

Created: 26 October 2026.

Goal: an open-source analytical spreadsheet with SQL, user-defined formula functions, organization-owned spaces, and simple single-node or distributed hosting.

## How to track progress

Follow the delivery sequence and dependencies below. Task IDs are stable references, not a strict execution order: newly added tasks may appear before an older gate. Check an item only when its behavior and applicable test tasks are implemented and verified; partial backend support does not count as a finished user feature. Add a test, commit, PR or verification note beside completed items. Keep blocked items unchecked and record the dependency.

Existing checked tasks and their evidence are preserved. New tasks start unchecked. This enhancement is planning work, not a fresh certification of the implementation. Re-check findings against current code before changing it.

Supporting documents: [assessment](OPEN_SOURCE_ASSESSMENT.md), [SQL and custom-function design](SQL_AND_CUSTOM_FUNCTIONS.md), [enterprise architecture decision](ENTERPRISE_ARCHITECTURE_DECISION.md), [test strategy](TEST_STRATEGY.md), [test matrix](TEST_MATRIX.md). This file is the execution tracker; those documents provide rationale. The older [roadmap](ROADMAP.md) is background and may lag implementation.

## Architecture requirements

The chosen direction is **durable workbooks with automatic, policy-managed compute**. Opening a workbook is the default action. Approved profiles and dedicated compute are advanced options, not mandatory steps for every user.

- Spaces own membership, permissions and budgets; they are not machines.
- Workbooks retain stable identities, data, revisions and pinned dependencies across runtime replacement.
- Immutable `RuntimeSpec` objects describe engine/image/package versions and required capabilities. `ComputePolicy` controls profiles, quotas and isolation. `RuntimeAllocation` describes replaceable execution capacity.
- One mutable workbook has one authoritative owner at a time. Shared nodes do not imply shared application processes across untrusted tenants.
- Keep permissions, revision validation, durable commit, migrations and audit enforcement in the core. Extensions cannot bypass them.
- Preserve a useful standalone distribution. Kubernetes is an execution provider; EKS is one deployment target. Neither JupyterHub nor a cloud service becomes a mandatory dependency.
- Lease expiry alone cannot fence direct DuckDB file writes. Never start a replacement writer without enforceable exclusivity; fail closed when ownership is uncertain.

## Delivery sequence and release boundaries

| Milestone | Required work | Exit condition |
|---|---|---|
| Reliable local product | Phases 1–3, including new local runtime tasks | Verified data integrity, bounded execution and recovery |
| SQL and reusable functions | Phases 4–5; phase 6 can proceed independently | Complete user workflows and versioned calculations |
| Familiar analyst workflows | Phase F P0 and F.15 gate | Verified lookups, formulas, table integration and reproducible analysis workflows |
| Single-node team release | Phase 7 and operational tasks in phase 9 | Authorization, collaboration, quotas, restore and upgrade verified |
| Python-function release | Phase 8 after phase 7 | Isolated, bounded, versioned code execution verified; do not block a non-Python team release on this |
| Connected data | Phase 10 | Scoped credentials and reliable refresh |
| Kubernetes beta | Phase 11 launch tasks and 11.16 gate | Conservative single-owner placement and tested storage handoff |
| Cluster recovery release | All phase 11 tasks and 11.10 gate | Published durability/recovery objectives demonstrated |
| Extension ecosystem | Phase E below, after real adapters exercise contracts | External contributors can implement and verify compatible extensions |

Phase 7 design and implementation may proceed alongside phases 4–6 once phase 3 and SQL capability restrictions are ready. Phase 10 is not a prerequisite for Kubernetes; phase 11 requires the single-node operational foundations. Optional phase 12 work remains demand-driven.

## Testing and completion requirements for every feature

Use the existing TEST_STRATEGY and TEST_MATRIX; extend them rather than creating a second reporting system. Before coding, record each task's observable contract, invariants, normal/boundary/invalid/concurrent/failure cases and expected-result oracle. Add a failing regression test for confirmed bugs where feasible.

Each feature needs relevant unit/integration tests and a real browser journey for user-facing behavior. Use property/state-machine tests, differential or metamorphic checks, fault injection, fuzzing and performance tests where the behavior warrants them. Tests must assert values, persisted state and side effects, not merely successful responses. Tests comparing shared implementation paths need an additional independent oracle for critical correctness claims.

Use deterministic seeds, isolated fixtures and observable synchronization. Record commands, versions, results, failures and gaps. Do not mask flaky tests with retries or use test counts/coverage percentages as proof of correctness. Process-kill testing is not proof of power-loss durability. Run destructive fault tests only in disposable environments.

New `N.Txx` entries are required test packages mapped to implementation IDs. Complete relevant test cases alongside each feature; do not defer all verification to the phase gate. Checked legacy entries remain historical evidence and should be rerun when affected.

## 1. Fix data integrity and existing workflows

Complete before releasing an alpha that users rely on for their data.

- [x] **1.01** Align the tagged table/sheets import response with frontend types and both home/workbook upload flows.
  - [x] Test (B): `api.rs::import_returns_tagged_table_or_sheets` — CSV/xlsx shapes, failed import creates nothing.
  - [x] Test (C): `journey.spec.ts` table journey — home upload opens the imported table.
  - Evidence: TEST_MATRIX 1.01; passing 2026-10-26.
- [x] **1.02** Separate full sheet export from viewport fetching; eliminate the 5,000-row sheet-to-table truncation. Verify 4,999, 5,000, 5,001 and 20,000 rows.
  - [x] Test (B/E): `sheets.rs::sheet_to_table_keeps_every_row_around_the_fetch_cap` at 1/4,999/5,000/5,001/20,000 rows with closed-form oracles.
  - [x] Test (B): type inference, leading zeros, dates, header requirements.
  - Evidence: TEST_MATRIX 1.02; fails on `ff3e736`, passes now.
- [x] **1.03** Make append-and-edit a single transaction; failed edits leave no added rows and empty batches are no-ops.
  - [x] Test (B): `integrity.rs` failed append, empty batch, last-write-wins, reopen.
  - [x] Test (D): `props.rs::table_matches_reference_model` state machine vs `Vec` model (regression seed saved).
  - Evidence: TEST_MATRIX 1.03; mutants for validation/no-op/last-write killed.
- [x] **1.04** Make sheet bulk mutations atomic, including in-memory rollback and restart consistency after failures.
  - [x] Test (F): `sheets.rs::failed_save_rolls_memory_back_to_disk` (fault point `save_sheets`).
  - [x] Test (B): invalid batch changes nothing and keeps undo history; grouped undo/redo; reopen.
  - Evidence: TEST_MATRIX 1.04. Limitation: undo history is in memory only.
- [x] **1.05** Preserve decimals and large integers across fetch, edit, copy, SQL results and export; test integers beyond JavaScript's exact range.
  - [x] Test (A): `wire.rs` 2^53 boundary and input grammar.
  - [x] Test (B/E): `precision.rs` fetch/edit/stats/query/export, Parquet round trip via `EXCEPT`; `journey.rs` over the wire.
  - Evidence: TEST_MATRIX 1.05; frontend stats formatting fixed after the generated types exposed it.
- [x] **1.06** Make column rename/delete dependency-aware and transactional; rewrite or reject affected formulas before mutation.
  - [x] Test (A): `formula.rs` reference locations and rewrite.
  - [x] Test (B): `formulas.rs` renames (Unicode offsets, literals), refused deletes, injection/subquery rejection, reopen.
  - Evidence: TEST_MATRIX 1.06.
- [x] **1.07** Fix calculated-column visibility in SQL queries beginning with `WITH`.
  - [x] Test (A): `sql.rs::leading_with_detection`.
  - [x] Test (B/E): `tests/sql.rs` CTE forms, shadowing, joins with hand-computed oracle, materialization.
  - Evidence: TEST_MATRIX 1.07.
- [x] **1.08** Use unique temporary upload paths and clean up interrupted imports.
  - [x] Test (B): concurrent same-name uploads; raw-TCP interrupted upload; late-failing CSV leaves no table; startup cleanup.
  - [x] Test (B): non-ASCII download names (found and fixed a header panic).
  - Evidence: TEST_MATRIX 1.08. Gap: disk-full not simulated; upload size limits are 3.02.
- [x] **1.09** Make workbook deletion reject new work, drain active operations and close handles before removing storage; provide recoverable trash where appropriate.
  - [x] Test (B): delete disconnects clients, trash/restore/purge, delete during a 3M-row write, trash survives restart.
  - [x] Test (C): trash UI restore; open workbook closes when deleted elsewhere.
  - Evidence: TEST_MATRIX 1.09; fixed a session leak found by these tests.
- [x] **1.10** Add shared/generated protocol types or contract tests to catch frontend/backend drift.
  - [x] Test: `protocol.rs` generated `web/src/protocol.ts` must be current; every op has a result type.
  - [x] Test (B): `api.rs::every_operation_replies_with_its_declared_type` — all 34 ops round-trip through their declared types.
  - Evidence: TEST_MATRIX 1.10.
- [x] **1.11 — Gate:** Pass import → edit → filter → export → restart → reopen tests with exact value comparisons and failed-operation cases.
  - [x] Test (B/E): `journey.rs` table and sheet journeys with restart and xlsx round trip.
  - [x] Test (F): `crash.rs` SIGKILL between and during writes.
  - [x] Test (C): `journey.spec.ts` the same journeys through the browser UI, with SIGTERM restart and SIGKILL.
  - Evidence: TEST_MATRIX 1.11. Gap: two-user variant waits on 7.08.

## 2. Finish the local spreadsheet experience

Depends on phase 1.

- [x] **2.01** Connect existing IronCalc APIs to sheet tabs, rendering, editing and sheet change events.
  - [x] Test (C): `sheets.spec.ts` rendering, errors, live updates between two pages.
  - [x] Test (C): Enter-key regression in `journey.spec.ts`.
  - Evidence: TEST_MATRIX 2.01; passing 2026-10-26 on `phase-2-local-spreadsheet`.
- [x] **2.02** Support creating and editing a blank sheet from a new workbook.
  - [x] Test (C): empty-state and menu paths in `sheets.spec.ts`.
  - Evidence: TEST_MATRIX 2.02; passing 2026-10-26 on `phase-2-local-spreadsheet`.
- [x] **2.03** Complete XLSX import/export UI with explicit compatibility warnings and roundtrip verification.
  - [x] Test (B): `xlsx.rs` round trip, notes, copy-mode references.
  - [x] Test (C): xlsx notes in `import.spec.ts`.
  - Evidence: TEST_MATRIX 2.03; passing 2026-10-26 on `phase-2-local-spreadsheet`.
- [x] **2.04** Expose existing sheet undo/redo and verify formula recalculation after undo, redo and reopening.
  - [x] Test (C): keyboard/toolbar undo-redo and reload in `sheets.spec.ts`.
  - [x] Test (B): grouped undo in `tests/sheets.rs`.
  - Evidence: TEST_MATRIX 2.04; passing 2026-10-26 on `phase-2-local-spreadsheet`.
- [x] **2.05** Add import preview, type overrides, progress and actionable error messages.
  - [x] Test (B): `tests/import.rs`, `imports.rs` (staging, retry, cancel, progress, expiry).
  - [x] Test (C): `import.spec.ts` preview, override, fix, stop.
  - Evidence: TEST_MATRIX 2.05; passing 2026-10-26 on `phase-2-local-spreadsheet`.
- [x] **2.06** Clearly distinguish large Tables from free-form Sheets; bound table/query-to-sheet conversion and display its limits.
  - [x] Test (B): bounded copies; contract for `table_to_sheet`.
  - [x] Test (C): `kinds.spec.ts`.
  - Evidence: TEST_MATRIX 2.06; passing 2026-10-26 on `phase-2-local-spreadsheet`.
- [x] **2.07** Persist named views with sort, filter and column presentation settings; label live versus snapshot behavior.
  - [x] Test (B): `tests/views.rs` live views, hidden columns, snapshots, schema changes.
  - [x] Test (C): `views.spec.ts`.
  - Evidence: TEST_MATRIX 2.07; passing 2026-10-26 on `phase-2-local-spreadsheet`.
- [x] **2.08** Add grid disposal for window listeners, observers, timers and pending requests.
  - [x] Test (C): `disposal.spec.ts` leak counters (old code leaked 18 listeners, 6 observers).
  - Evidence: TEST_MATRIX 2.08; passing 2026-10-26 on `phase-2-local-spreadsheet`.
- [x] **2.09** Provide accessible active-cell/viewport semantics, keyboard navigation and visible focus.
  - [x] Test (C): `a11y.spec.ts` axe, ARIA grid, focus, keyboard.
  - Evidence: TEST_MATRIX 2.09; passing 2026-10-26 on `phase-2-local-spreadsheet`.
- [x] **2.10 — Gate:** A new user can create, import, edit, save and reopen both tables and sheets using the UI alone.
  - [x] Test (C): gate journey in `sheets.spec.ts`.
  - Evidence: TEST_MATRIX 2.10; passing 2026-10-26 on `phase-2-local-spreadsheet`.

## 3. Establish bounded execution and storage foundations

Depends on phase 1. Complete before shared hosting or expensive custom-function execution.

- [x] **3.01** Add bounded workbook job queues with cancellation, deadlines, progress and priority for interactive reads.
  - Test: `scheduling.rs`, `sched.rs` unit, `storage.rs::readers_are_not_blocked…`, `resilience.spec.ts` (cancel). Evidence: TEST_MATRIX 3.01.
- [x] **3.02** Bound uploads, request/response bytes, pasted cells and requested row/column counts; clamp before allocation.
  - Test: `scheduling.rs` limits, `sheets.rs::oversized_sheet_fetches…` (failed before fix). Evidence: TEST_MATRIX 3.02.
- [x] **3.03** Enforce host-wide CPU, memory and temporary-disk budgets; account for DuckDB, sheets and worker allocations.
  - Test: `runtime.rs` budget sums read back from DuckDB, `budget.rs` unit. Evidence: TEST_MATRIX 3.03. Limits are DuckDB-level (DURABILITY.md).
- [x] **3.04** Evict idle workbook handles and expire unused materialized views.
  - Test: `runtime.rs` idle eviction and eviction races, `storage.rs` view expiry. Evidence: TEST_MATRIX 3.04.
- [x] **3.05** Fetch visible column ranges and use a byte-bounded cache covering both fresh and stale blocks.
  - Test: `cache.spec.ts`, `wide.spec.ts`. Evidence: TEST_MATRIX 3.05.
- [x] **3.06** Separate stable record IDs from display order; preserve efficient positional access without renumbering an entire table tail.
  - Test: `props.rs` (record ids, 300 cases), `storage.rs`. Evidence: TEST_MATRIX 3.06.
- [x] **3.07** Add monotonic workbook revisions, idempotent mutation IDs and an atomic operation-history format.
  - Test: `storage.rs`, `scheduling.rs`, `backup.rs`. Evidence: TEST_MATRIX 3.07.
- [x] **3.08** Add versioned storage migrations and a consistent backup/restore mechanism covering workbook data and metadata.
  - Test: `storage.rs` v1 fixture migration, `backup.rs`. Evidence: TEST_MATRIX 3.08; no downgrade (DURABILITY.md).
- [x] **3.09** Handle SIGTERM: stop admission, drain work, persist/checkpoint and close cleanly.
  - Test: `shutdown.rs` (real binary). Evidence: TEST_MATRIX 3.09.
- [x] **3.11** Define workbook, immutable RuntimeSpec, ComputePolicy, RuntimeAllocation and ownership-assignment schemas; preserve workbook identity independently of allocation lifetime.
  - Test: `runtime.rs` spec pinning, data outliving compute. Evidence: TEST_MATRIX 3.11–3.14.
- [x] **3.12** Add a narrow internal RuntimeProvider contract: idempotent ensure/start, status/readiness, drain/stop, capability reporting and typed failures; distinguish requested, running and ready states.
  - Test: `runtime/mod.rs` unit, `runtime.rs`. Evidence: TEST_MATRIX 3.11–3.14.
- [x] **3.13** Implement a Local provider using the same core binary where practical; persist/reconcile allocation state after controller restart without introducing mandatory cluster services.
  - Test: `runtime.rs` reconciliation, `crash.rs` reconcile after SIGKILL. Evidence: TEST_MATRIX 3.11–3.14. Local, in-process provider only.
- [x] **3.14** Define lifecycle transitions and operation IDs for start, cancel, drain, stop and recover; prevent duplicate allocations and clean up failed startup resources.
  - Test: `runtime.rs` duplicate starts, cancel before ready, failed-start cleanup. Evidence: TEST_MATRIX 3.11–3.14.
- [x] **3.15** Define the acknowledged-edit durability contract and trust/isolation classes; document what survives process, host and storage loss before promising recovery.
  - Evidence: docs/DURABILITY.md (power loss and host failure untested, stated).
- [x] **3.T01** Test 3.01–3.05: queue saturation, fair progress, cancellation races, slow consumers, overflow-sized fetches, wide/text-heavy cache bounds, idle eviction and host-wide resource accounting.
  - Evidence: TEST_MATRIX 3.T01; pass 2026-10-26.
- [x] **3.T02** Test 3.06–3.09: randomized edits against a reference model, stable IDs, duplicate mutations, revision monotonicity, migration rollback limits and consistent backup/restore under activity.
  - Evidence: TEST_MATRIX 3.T02; pass 2026-10-26.
- [x] **3.T03** Test 3.08–3.15: injected write/disk-full/permission failures, crash during persistence, SIGTERM, duplicate starts, cancel-before-ready, controller restart and failed allocation cleanup; explicitly record unsupported storage-fault simulations.
  - Evidence: TEST_MATRIX 3.T03; real disk-full on a 64 MB filesystem; unsupported faults listed.
- [x] **3.10 — Gate:** Cancellation, overload, disk-full, crash/restart and restore tests preserve consistency and keep resource use bounded.
  - Evidence: TEST_MATRIX 3.10 gate run 2026-10-26 (local; CI not yet run). No multi-node failover implied.

Gate 3.10 also requires the local runtime/lifecycle tasks 3.11–3.15 and their tests. It does not imply multi-node failover.

## 4. Deliver the SQL workspace

Build on the existing SQL console. Depends on phases 1–3; shared query permissions depend on phase 7.

- [x] **4.01** Add a schema browser and table/column autocomplete.
  - Test: `sqlwork.rs` schema, `sqlworkspace.rs` (WS), `sql.spec.ts` tree/quoting/completion. Evidence: TEST_MATRIX 4.01.
- [x] **4.02** Support safely bound query parameters and clear parse/runtime errors.
  - Test: `sqlpolicy.rs` unit, `sqlwork.rs` parameters/errors, `sqlworkspace.rs` codes and redaction. Evidence: TEST_MATRIX 4.02.
- [x] **4.03** Show bounded result previews, explicit truncation, execution timing, cancellation and full export actions.
  - Test: `sqlwork.rs` previews/exports, `sqlworkspace.rs` cancel isolation, exports, races; `sql.spec.ts`. Evidence: TEST_MATRIX 4.03 (fixed a reader left inside a failed transaction).
- [x] **4.04** Save, rename, reopen and version queries; keep local queries private until sharing permissions exist.
  - Test: `sqlwork.rs` versions/reopen, `sqlworkspace.rs` restart, `sql.spec.ts`. Evidence: TEST_MATRIX 4.04. Saved queries are visible to anyone who can open the workbook (no identities before phase 7); the unsaved draft stays in the browser.
- [x] **4.05** Complete result-to-table and bounded result-to-sheet workflows.
  - Test: `sqlwork.rs` sheet limits, `sql.spec.ts`, `kinds.spec.ts`. Evidence: TEST_MATRIX 4.05.
- [x] **4.06** Distinguish saved SQL, live query views and materialized results; track source revisions and refresh status.
  - Test: `sqlwork.rs` materializations and data revisions, `storage.rs` v2 fixture, `crash.rs` kill during refresh, `sql.spec.ts`. Evidence: TEST_MATRIX 4.06 (storage format 3).
- [x] **4.07** Restrict SQL/formula capabilities, external readers, extension loading and internal metadata access for shared-host mode; console disabling alone is insufficient.
  - Test: `capabilities.rs`, `sqlpolicy.rs`, `sqlworkspace.rs` lockdown. Evidence: TEST_MATRIX 4.07 (fixed multi-statement execution and internal-table access). Applies to every server, not only shared hosts. Not organization authorization (phase 7).
- [x] **4.08** Route supported mutations through operations that maintain Alephgrid metadata, history and revisions.
  - Test: `capabilities.rs` reference model and refusals, `sqlworkspace.rs`, `sql.spec.ts`. Evidence: TEST_MATRIX 4.08. INSERT/UPDATE/DELETE only; other DDL/DML refused.
- [x] **4.T01** Test 4.01–4.06: schema/autocomplete correctness, bound parameters, hostile strings, joins/CTEs/calculated columns, exact values, truncated previews, cancellation and saved-result refresh/reopen in API and browser journeys.
  - Evidence: TEST_MATRIX 4.T01; pass 2026-10-26 (local).
- [x] **4.T02** Test 4.07–4.08: direct SQL and formula attempts to access files/network/internal metadata, install extensions or bypass mutation bookkeeping; verify rejection and unchanged durable state, including statements that appear read-only.
  - Evidence: TEST_MATRIX 4.T02; pass 2026-10-26 (local).
- [x] **4.09 — Gate:** Verify joins, aggregates, CTEs, calculated columns, parameters, cancellation, export and result materialization end to end.
  - Evidence: TEST_MATRIX 4.09 gate run 2026-10-26 (local Linux; CI not yet run on this branch).

## 5. Add user-defined formula functions

Depends on phases 2–4. This phase covers formula-defined and SQL functions; Python is phase 8.

- [x] **5.01** Prototype a named IronCalc LAMBDA: define `MARGIN(revenue,cost)` and call `=MARGIN(B2,C2)`.
- [x] **5.02** Define a function registry with stable IDs, signature, source, immutable version, scope, documentation, tests and execution adapter.
- [x] **5.03** Build the function editor with argument declarations, examples, test inputs and preview results.
- [x] **5.04** Register named formulas through the engine; support cell/range arguments and nested formulas without string-substitution evaluation.
- [x] **5.05** Add function autocomplete, signature help, documentation and readable errors in the formula editor.
- [x] **5.06** Pin workbook function versions; support explicit upgrade and rollback without silently changing other workbooks.
- [x] **5.07** Specify blanks/NULL, decimals, dates, errors, array outputs, spill collisions and naming conflicts.
- [x] **5.08** Add validated SQL expression functions/macros for SQL queries and calculated table columns; explicitly document which engines support each function type.
- [x] **5.09** Verify dependency recalculation, cycles, save/reopen, version changes and unsupported XLSX-export behavior.
- [x] **5.11** Bind function dependencies to immutable RuntimeSpec versions; declare which execution adapters support each signature and reject unsupported combinations before execution.
- [x] **5.T01** Test 5.01–5.05 and 5.08: define/test/call via UI, nested/range calls, SQL-function registration, malformed definitions, naming conflicts and autocomplete; check outputs against hand-computed or independent results.
- [x] **5.T02** Test 5.06–5.11: old-version reproducibility, explicit upgrade/rollback, dependency cycles, precision/null/error/spill semantics, reopen and incompatible-runtime refusal; do not silently replace missing versions.
- [x] **5.10 — Gate:** A user can define, test, call and persist their own function from the UI, with correct recalculation after argument edits.

Verification (27 October 2026): [function guide](FUNCTIONS.md), engine `tests/functions.rs`, server `tests/functions.rs` and protocol contract coverage, and browser `functions.spec.ts`. See [phase 5 test evidence](TEST_MATRIX.md#phase-5) for results and limitations. Python and space-wide sharing remain later phases.

## 6. Add analysis tools for users who do not write SQL

Depends on phases 2–4. These items can proceed alongside phase 5 once dependencies are complete.

- [x] **6.01** Add a group-by/pivot builder with refresh behavior and bounded output.
- [x] **6.02** Add charts backed by server-side aggregates rather than whole-table downloads.
- [x] **6.03** Add a join builder with key/type validation and duplicate-match visibility.
- [x] **6.04** Add null, duplicate, distinct-count and type profiling.
- [x] **6.05** Add find/replace with preview and reversible mutations.
- [x] **6.06** Add number/date/locale formats, freeze/hide columns and bounded conditional formatting.
- [x] **6.T01** Test 6.01–6.04: pivots/joins/profiling against known datasets with duplicates, skew and NULLs; enforce chart/output bounds and verify source refresh semantics.
- [x] **6.T02** Test 6.05–6.06 and the full UI journey: replacement preview/rollback, locale/timezone formatting, keyboard/accessibility, saved presentation and correct reopening.
- [x] **6.07 — Gate:** An analyst can import data, join it, summarize it, chart it and reopen the saved analysis without writing SQL.

Verification (27 October 2026): [analysis guide](ANALYSIS.md), engine/server `analysis.rs`, protocol contract tests and browser `analysis.spec.ts`. See [phase 6 test evidence](TEST_MATRIX.md#phase-6): 231 Rust tests and 49 browser tests passed. Initial bounds and supported formats are documented in the guide.

## F. Deliver familiar spreadsheet formulas at analytical scale

New planning work; phase 5 custom functions and phase 6 bounded analysis remain completed within their documented scope. This phase closes built-in compatibility, editor and large-table formula gaps. Read the [function catalog](FORMULA_CATALOG.md), [table formulas](TABLE_FORMULAS.md) and [keyboard reference](KEYBOARD.md). Registry presence is not a completion checkmark.

Order: F.01–F.04 first; F.05–F.09 next; F.10–F.14 afterwards. Depends on phases 2–6; team release also requires phases 7/9. S3, Python, MCP and Kubernetes do not block the local analyst gate. P2/deferred catalog rows are backlog proposals, not prerequisites for F.15.

- [x] **F.01** Maintain versioned function manifest from the comparison matrix: sheet/table support, signatures, argument modes, examples, limitations, evidence and unsupported behavior; generate searchable built-in help.
  - [x] Initial catalog: 562 registry-checked entries, searchable bounded help, 44 reviewed P0 signatures/examples and pinned-engine drift checks. See [catalog scope](FORMULA_CATALOG.md).
  - [x] Complete argument-mode documentation and evidence beyond the initial example baseline; retain explicit unreviewed entries. Schema 3: per-argument rules citing their cases for all 44 P0 entries, a source for every expected value, coercion inputs shown in help; 518 entries stay explicitly "Not reviewed". Tests: `tests::catalog` (12), `e2e/catalog.spec.ts` (3). Table status stays `not_supported` until F.06/F.08 add evidence.
- [x] **F.02** Certify P0 sheet functions, especially VLOOKUP/XLOOKUP/HLOOKUP, INDEX/MATCH/XMATCH, conditional aggregates, logic/error handling, text/date/math. Reuse pinned IronCalc and fix verified gaps rather than reimplementing existing functions.
  - [x] Add 121 mode-level golden cases across all 44 P0 functions, with reopen checks and searchable evidence: 121 pass with the documented local compatibility patch. See [reproducers](FORMULA_GAPS.md).
  - [x] Resolve the six pinned-engine gaps with a licensed, documented local dependency patch and enabled desired-result regressions.
  - [x] Finish the broader coercion/argument-mode matrix: 156 coercion cases (152 pass, 4 accepted gaps) over typed F1:G8 inputs, sourced from documented Excel rules; LEFT/RIGHT/MID numeric-text counts fixed in the local patch. See [gaps](FORMULA_GAPS.md).
  - [x] Resolve or explicitly accept the six remaining known gaps: IFERROR/IFNA of an empty cell corrected to 0 from observed Excel output (OASIS OFFICE-3468), now verified; dates before 1900-03-01 accepted with reasons (IronCalc date numbering, fix proposed upstream in F.14). 277 cases: 273 verified, 4 accepted gaps. See [gaps](FORMULA_GAPS.md).
- [x] **F.03** Deliver built-in autocomplete, argument help, range picking, colored references, F4 locking, named ranges and meaningful formula error locations; preserve custom-function behavior.
  - [x] Initial built-in formula-bar completion and signature help for the 44 reviewed P0 entries, preserving workbook custom functions. Unreviewed built-ins are not suggested (F.01).
  - [x] A1 reference locking with F4 in both editors; same-sheet formula-bar range picking and bounded colored reference hints. See [editor scope](FORMULA_EDITOR.md).
  - [x] Named ranges (workbook/sheet scope, collision checks against built-ins and LAMBDAs, rename rewrites formulas, undo, reopen), Go to, cross-sheet tab picking, parse-error locations that block saving, and error-origin display. Engine `sheets::editing`, `tests/api.rs`, `e2e/names.spec.ts`. Local patch: trailing-input rejection and token positions (IronCalc suite 2765/2765).
- [ ] **F.04** Add a context-aware command/shortcut registry, fill down/right, formula-aware copy/move and paste-special. Audit existing keys; test OS/browser conflicts, keyboard layouts, IME and assistive technology.
  - [x] One registry (`web/src/commands.ts`, `workbookcommands.ts`) drives grid keys, the command palette (⌘K, toolbar) and shortcut help (⌘/); conflict test; IME composition ignored; Latin layouts by printed letter, other scripts by key position. Fill down/right (⌘D/⌘R), formula-aware copy/paste, cut/move with reference updates (same sheet), paste special (all/formulas/values/formats, transpose); each one undo step and bounded. Sheet changes are sent in user order. Engine `sheets::editing`, `e2e/commands.spec.ts`; key audit in [KEYBOARD.md](KEYBOARD.md).
  - [x] Browser suite in Firefox and WebKit (Safari's engine) as well as Chrome (`E2E_BROWSER=firefox|webkit`; Linux Chrome in CI). Fixed: dialogs lost Escape/Tab when focus left them (Safari does not focus clicked buttons); panels opened while the page was still connecting failed with "not connected"; a paste right after copy/cut could miss it. Safari Tab/Option+Tab and macOS End-key behaviour documented ([keyboard](KEYBOARD.md#browsers-and-platforms)). Regression tests run in all three engines.
  - [ ] Not verified: real screen readers (VoiceOver/NVDA/JAWS), Windows browsers, physical non-US keyboards, paste into the grid in Firefox. Manual script and results table: [ACCESSIBILITY_TESTING.md](ACCESSIBILITY_TESTING.md) (not run yet). Not implemented: cross-sheet move, fill handle drag, paste special for text from other apps (pastes as values).
- [x] **F.05** Specify typed formula AST, lazy table references, row context, stable IDs/order, tagged errors, blank/NULL/coercion and precision rules. Refuse unsupported plans; no regex SQL translation or full-table cell materialization.
  - [x] Specification ([TABLE_FORMULAS.md](TABLE_FORMULAS.md)) and compiler (`table_formula`): hand-written parser with spans, static types, value/error pairs, guarded SQL, explicit refusals. Differential test: ~100 formulas × 60 typed rows agree with IronCalc after documented normalizations; goldens and refusal tests. 6 tests.
  - [x] Spreadsheet calculated columns stored with their language (storage format 6, migration tested on the v1/v2 fixtures), hidden error columns with propagation between formula columns, error marks in row blocks and the grid, rename/delete dependencies, add-column UI. `tests::table_formulas` (2), `e2e/tableformulas.spec.ts`. Whole-column references are refused until F.06/F.08 use them.
- [x] **F.06** Implement exact table XLOOKUP/VLOOKUP and multi-key lookup through batched plans with deterministic duplicate selection, missing-key handling and one output per input; no per-cell SQL queries.
  - [x] Exact `XLOOKUP(value, T[key], T[ret], [if_not_found], 0, 1|-1)` in table formulas: one grouped, ordered relation per formula left-joined on a normalized key (first/last by persisted order, one result per row), sheet-consistent matching of types, case and blanks, errors from formula columns propagate, cross-table cycles and broken references show #REF!. Differential test against IronCalc (10 lookups × 60 rows).
  - [x] `VLOOKUP(value, T, n, FALSE)` (n < 1 `#VALUE!`, past the end `#REF!`; pattern characters `#VALUE!` per row) and typed multi-key lookups `XLOOKUP(1, (T[a]=x)*(T[b]=y), …)` / `XLOOKUP(TRUE, T[a]=x, …)`, all through one grouped relation built in a single scan. Column-level dependencies across tables: renames rewrite `T[c]`/`T` everywhere, deletes (including VLOOKUP-shifting ones) are refused, mutual lookups work and circular references are refused with their path. Grouped relations cached in scratch by SQL and source data revisions (10M orders/1M customers: page 182–189 ms → 45–58 ms; `lookup_read_timings`). Tests: lookups, VLOOKUP and multi-key vs IronCalc, wildcard rule (`table_formula::tests`); source edits, duplicates, renames/deletes, mutual lookups, cycles, cache reuse/rebuild/eviction (`tests::table_formulas`, 7). See [lookups](TABLE_FORMULAS.md#lookups-f06). Not done: sorting/filtering by a lookup column still evaluates every row (3.5 s at 10M; F.09).
- [x] **F.07** Add approximate/wildcard/search modes only after semantic validation; evaluate ASOF/ordered plans, validate sort requirements and document rejected modes.
  - [x] XLOOKUP/XMATCH `match_mode` -1/1 (nearest smaller/larger in the sheet engine's comparison order, unsorted keys, first or last in search order, the engine's blank and cross-type rules) and 2 (wildcards); `VLOOKUP` without FALSE and `MATCH` 1/-1 over keys validated as sorted (otherwise `#N/A` on every row), `MATCH` 0, `XMATCH`, `INDEX(T[c], n)`/`INDEX(T, n, c)`; `VLOOKUP(…, FALSE)` now reads text values as patterns, as sheets do. Plans: an ASOF join over keys grouped once plus one cached row of extremes (cross-type values, error keys, the sort check), so a page reads only its rows (1M orders: 56 ms per page, was 390 ms with a per-row-set plan); wildcard patterns matched once per distinct pattern. Rejected with reasons: binary search (same results without sorting), HLOOKUP, LOOKUP. Tests: 24 + 28 formulas × 60 rows vs IronCalc (approximate/wildcard; sorted, MATCH, XMATCH, INDEX), unsorted-key and INDEX edge cases by hand, refusals, a workbook test (tiers, sort broken by an edit, refused deletes incl. INDEX's column, reopen); catalog: MATCH/XMATCH/INDEX supported (52). Documented gaps: sorted lookups over duplicate keys (last row; the engine's binary search is arbitrary), INDEX row 0. See [nearest, wildcard and sorted lookups](TABLE_FORMULAS.md#nearest-wildcard-and-sorted-lookups-f07).
- [x] **F.08** Add table SUMIFS/COUNTIFS/AVERAGEIFS and an allowlisted spreadsheet-style calculated-column subset with explicit error/criteria semantics and shared scans.
  - [x] `SUMIFS`/`COUNTIFS`/`AVERAGEIFS` and `SUMIF`/`COUNTIF`/`AVERAGEIF` over whole table columns, criteria exactly as the sheet engine reads them (numbers, numeric text, operators, wildcards, `"<>"`, `""`, logical, errors, blank, date text), constant criteria as one filter, equality criteria as one cached grouped scan, text criteria per distinct value; dependency tracking, renames and refused deletes via the F.06 graph. Fixed in the vendored IronCalc: `"<=x"`/`">=x"` text criteria compared the wrong way. Tests: 38 formulas vs IronCalc, hand-checked values, a workbook test (totals, counts, averages, wildcard and text criteria, a lookup of a total, edits, renames, reopen). 1M/100K: 45–57 ms per page (text criteria 347 ms). See [conditional aggregates](TABLE_FORMULAS.md#conditional-aggregates-f08).
  - [x] The allowlisted subset beyond these: `MINIFS`/`MAXIFS`, and `SUM`/`AVERAGE`/`MIN`/`MAX`/`COUNT`/`COUNTA` of whole columns (one aggregate relation per formula, joined to every row), differential-tested against sheet ranges (20 more formulas) with hand-checked values. Catalog schema 4: `table_status` is `supported` for the 49 functions table formulas compile, with `table_modes` and `table_evidence` (the tests that use each); a catalog test checks evidence both ways and that the status matches exactly what the compiler accepts; the help shows it and can filter by it.
- [x] **F.09** Add cross-engine dependencies, cycle detection, snapshot-consistent recalculation, cancellation, generation-based invalidation and atomic derived updates; keep last committed results visibly stale on failure.
  - [x] Table formulas read single sheet cells (`=[@amount]*(1+Settings!$B$1)`, quoted sheet names, `$` kept; ranges, unqualified cells, unknown and internal sheets refused). Values are saved with every sheet or formula change in `_rf_links` (storage format 7) and read from the reader's snapshot, never the live model. References follow their cells through row/column inserts and deletes, moves, sheet renames and undo/redo via anchor formulas in a very hidden sheet kept outside the undo history (local IronCalc patch: two additions); a deleted cell becomes `#REF!`; deleting a sheet that formulas read is refused; the hidden sheet is not listed, addressable or exported. A changed value changes the readers' data revision, a changed type their structure. Tests: sheet cells vs IronCalc for 7 kinds of cell value (99 formulas × 60 rows; two documented differences found: Date fractions, and AND/OR with an error after the deciding value, see [gaps](FORMULA_GAPS.md#and-and-or-with-an-error-after-the-deciding-value)), rewriting, and workbook tests (edits, inserts, undo/redo, moves, rename, refused delete, row delete, type changes, reader connections, reopen, export). IronCalc suite 2765/2765. See [sheet cells](TABLE_FORMULAS.md#sheet-cells-f09).
  - [x] Changes reach tables that read what changed: after a commit the server sends `cells_changed`/`table_changed` (`origin: 0`) for tables whose formulas read changed sheet cells and for tables that look up or aggregate a changed table, transitively; saved query results and live queries count the tables their formula columns read as sources (a snapshot of orders goes stale when a customer it looked up changes). Tests: `tests::links` (5), server `sheet_cells_read_by_tables_notify_their_readers`, `e2e/tableformulas.spec.ts`.
  - [x] Sheet cells read tables: a formula naming a table that the sheet engine does not know becomes a table cell, compiled as a table formula (whole columns, lookups, aggregates, sheet cells; bare `A1` means its own sheet), stored with a never-reused result slot that the sheet engine references, shown as typed in the grid and formula bar. Recomputed in the same transaction as any change to what it reads, in rounds until values settle; any failure rolls the whole change back. Cycles across both engines refused with their path (table formulas, lookups, sheet references, ranges, names, spills; INDIRECT/OFFSET stop at the round limit). Renames rewrite them, deletes are refused, undo/redo and reopen keep them, export writes values with a note per cell. Server sends `sheet_changed` (`origin: 0`). Tests: `tests::tablecells` (5), server `sheet_cells_that_read_tables_follow_table_edits`, `e2e/tableformulas.spec.ts`. See [sheet cells that read tables](TABLE_FORMULAS.md#sheet-cells-that-read-tables-f09).
  - [x] Stored formula results for tables of 200,000+ rows whose formulas read other tables: built from one snapshot, stamped with every table read (generation and data revision), installed atomically and only over older results; sorted/filtered views read the result set their order came from. The server recalculates stale results in the background after each commit (bulk read job, superseded by newer changes, `cancel_recalculation`/`recalculate` ops, `table_changed` when done); meanwhile and after failure or cancellation views keep the last committed results with `stale` and the grid shows a notice with Cancel/Recalculate; exports never write stale values. Fixed on the way: lookup-cache tables built inside a read snapshot were shared before it committed (a cancelled recalculation broke other readers); they are now shared after commit and checked for visibility. Tests: `tests::results` (5), server `stored_results_are_recalculated_in_the_background`, `e2e/tableformulas.spec.ts` (stale notice, cancel, recalculate). 1M orders: first sort by a lookup column 303 ms (stores results), later sorts 151 ms, filter 26 ms; 10M in F.T04. See [stored formula results](TABLE_FORMULAS.md#stored-formula-results-f09).
  - Export notes for table cells are returned by the engine but not yet shown in the download: tracked in F.13.
- [ ] **F.10** Verify P1 dynamic arrays, LET/LAMBDA and named ranges; add bounded spill/table-result routing, collision checks and source-refresh semantics. Decide volatile epoch/seed policy without silently relaxing UDF determinism.
- [ ] **F.11** Extend phase 6 pivots with multiple measures, totals/subtotals, calculated measures and bounded drill-down; add line/scatter charts, shared slicers and persisted dashboard layouts with explicit limits.
- [ ] **F.12** Add validation/dropdowns, deduplication, split/trim/type-conversion previews and formula inspection; use atomic/reversible mutations or derived result tables.
- [ ] **F.13** Add XLSX compatibility reports for unsupported formulas, arrays, styles and table references; never silently replace a live formula with a value. Publish reproducible sample workbooks.
- [ ] **F.14** Triage missing pure functions and defects against current IronCalc; prepare focused upstream proposals/tests. Track temporary patches, pinned upgrades and removal conditions; keep connectors/UI/DuckDB integration in Alephgrid.
- [ ] **F.T01** Test F.01–F.04: independent golden values per function/mode, missing/duplicate keys, first/last matches, wildcard escapes, mixed types, blanks/errors, unequal ranges, Unicode, absolute/mixed references, keyboard-only formula entry and undo/reopen.
- [ ] **F.T02** Test F.05–F.09: independent small reference evaluator versus batched plans, duplicate/cardinality invariants, stable ordering, cycles, source rename/delete/reorder, cross-engine errors, concurrent source edits, cancellation/fault rollback and stale-cache rejection.
- [ ] **F.T03** Test F.10–F.13: spill collision/size refusal, dynamic arrays, pivot/filter totals, chart bounds, validation bypass attempts, XLSX provenance fixtures and explicit incompatibility reports; include round-trip and real-browser journeys.
- [ ] **F.T04** Benchmark 100K/1M/10M-row lookup/aggregate workflows with skew, NULLs, source edits and memory spill; publish cold/warm latency, peak memory, scanned bytes, temporary disk and query counts. Set measured regression budgets; prohibit per-row query growth and full-data browser transfer.
- [ ] **F.15 — Analyst gate:** Import orders/customers, enrich via XLOOKUP, compute conditional totals, build/filter a pivot and chart, refresh changed data, reopen and export with compatibility notes without SQL. All P0 functions and relevant F.T01–F.T04 evidence must pass; report remaining modes and P1/P2 gaps explicitly.

## 7. Make organizations and spaces safe for teams

Depends on phase 3 and the SQL restrictions in phase 4. Design can begin earlier; do not expose untrusted shared hosting before this gate.

- [ ] **7.01** Implement organization → space → workbook ownership and a migration for existing workspace labels.
- [ ] **7.02** Add bootstrap admin, OIDC login, secure sessions, logout and scoped service tokens.
- [ ] **7.03** Implement organization admin, space owner, editor and viewer roles with explicit inheritance and sharing rules.
- [ ] **7.04** Enforce authorization on every HTTP route and WebSocket operation, including export, SQL, uploads and deletion.
- [ ] **7.05** Add space membership management and OIDC group mapping or invitations.
- [ ] **7.06** Remove long-lived secrets from URLs/logs; use authenticated sessions or short-lived download/connection tickets.
- [ ] **7.07** Add per-space storage, job and compute quotas plus administrative usage visibility.
- [ ] **7.08** Add stale-edit checks, event-gap detection and reconnect/resync using stable IDs and revisions.
- [ ] **7.09** Expose table operation history/undo and define its behavior under concurrent edits.
- [ ] **7.10** Record audit events for access changes, mutations, exports and function publication.
- [ ] **7.11** Enable authorized sharing of saved queries, views and function versions; keep publication, editing and execution permissions distinct.
- [ ] **7.13** Implement server-validated ComputePolicy entitlements and quota reservations for approved resource profiles; prevent concurrent starts from exceeding space budgets.
- [ ] **7.14** Make Open workbook automatically locate/start compatible capacity and route collaborators to its owner; show queued/starting/ready/draining/failed states with actionable errors.
- [ ] **7.15** Add optional profile selection and dedicated-allocation requests for authorized users; preserve workbook URL, ownership and version pins across controlled resize/restart.
- [ ] **7.16** Implement idle shutdown based on meaningful activity and active jobs; checkpoint/drain before stopping, retain storage and show its retention/cost implications. Heartbeats alone must not keep compute alive forever.
- [ ] **7.17** Define identity-provider claim mapping and revocation behavior; keep authorization in Alephgrid and reject untrusted forwarded identity headers.
- [ ] **7.T01** Test 7.01–7.07, 7.10–7.11 and 7.17: positive/negative role-operation matrix across every HTTP/WS path, cross-space object IDs, export/query/function access, expired/revoked sessions, secret redaction and audit attribution.
- [ ] **7.T02** Test 7.08–7.09: same/different-row concurrent edits, structural races, duplicate/delayed/missed events, reconnect and undo under the documented conflict policy.
- [ ] **7.T03** Test 7.13–7.16: concurrent quota admission, forged profile requests, two collaborators opening a stopped workbook, failed start, controlled resize, heartbeat-only idle and long-running jobs during idle evaluation.
- [ ] **7.12 — Gate:** Two spaces on one host cannot access each other's data; concurrent/reconnecting users cannot silently lose or misapply edits.

Gate 7.12 includes policy-managed local execution and its lifecycle tests; it does not require Kubernetes.

## 8. Add Python functions callable from cells

Core product scope. Prototype after phase 5; shared release depends on phases 3 and 7.

- [ ] **8.01** Prove an engine integration for `=NORMALIZE_SKU(A2)` and range arguments; decide on a supported upstream hook or maintained extension and document its maintenance cost.
- [ ] **8.02** Define typed worker input/output contracts and exact scalar, range and error semantics.
- [ ] **8.03** Run Python in isolated workers with no inherited credentials, denied host mounts and network disabled by default; a subprocess alone is insufficient isolation.
- [ ] **8.04** Enforce execution time, CPU, memory, output and concurrency limits with cancellation.
- [ ] **8.05** Pin runtime images and package dependencies; provide offline installation and an optional local/team worker profile.
- [ ] **8.06** Execute table calculations in bounded batches and benchmark the bridge; avoid one network/process call per cell.
- [ ] **8.07** Avoid holding the workbook lock during worker execution; commit results only when input and function revisions still match.
- [ ] **8.08** Cache pure functions by exact inputs/version; define explicit refresh semantics for volatile functions and keep side effects out of automatic recalculation.
- [ ] **8.09** Integrate Python editing, test runs, documentation, version publishing and formula autocomplete into the function library.
- [ ] **8.10** Handle worker crash, duplicate job delivery, permission revocation and unavailable functions explicitly.
- [ ] **8.11** Explain XLSX compatibility and offer an explicit cached-value export option; custom Python code does not become an Excel-native function.
- [ ] **8.13** Implement a versioned internal FunctionRuntime adapter with capability declarations, typed batch schemas and explicit runtime identity; integrate it with RuntimeSpec and ComputePolicy.
- [ ] **8.T01** Test 8.01–8.02, 8.05–8.09 and 8.11–8.13: scalar/range/batch equivalence, errors/precision, dependency versions, offline installation, caching, recalculation, reopen and export compatibility in engine/API/browser paths.
- [ ] **8.T02** Test 8.03–8.04 and 8.10: filesystem/network/credential escape attempts, child processes, timeout/memory/output exhaustion, crash, cancellation, duplicate jobs, revoked permission and stale results; assert other workbooks remain responsive and durable state is unchanged on failure.
- [ ] **8.12 — Gate:** Define → test → publish → call from cells → recalculate → reopen works; worker isolation and failure tests pass without hanging other workbooks.

## 9. Package and release the single-node product

An earlier local alpha may ship after phases 1–5 with explicit limitations. The team release requires phase 7 and this phase's operational checks. Python functionality requires phase 8 before it is enabled/advertised; it need not block a non-Python team release. Analysis features can be released incrementally.

- [ ] **9.01** Verify the core binary/container runs with one durable data directory and no mandatory cluster services.
- [ ] **9.02** Smoke-test fresh Docker named volumes and bind mounts; fix ownership and document required permissions.
- [ ] **9.03** Provide localhost and team Compose examples with required/generated credentials, HTTPS, health/readiness checks and resource limits.
- [ ] **9.04** Document and verify backup to another location, restore onto a clean host, upgrades and rollback limitations.
- [ ] **9.05** Run reproducible benchmarks for varied dataset shapes and concurrent clients; publish raw results and supported capacity limits.
- [ ] **9.06** Profile remaining bottlenecks; validate read-connection concurrency, equivalent-view reuse and incremental sheet snapshots before adopting them.
- [ ] **9.07** Prepare the public Git repository and replace placeholder clone URLs; reconcile README and roadmap with shipped behavior.
- [ ] **9.08** Review source/artifact provenance, third-party licenses and dependency notices; exclude proprietary extracted assets and private captures.
- [ ] **9.09** Add SECURITY.md, maintainer/contribution guidance, issue/PR templates, changelog and version support policy.
- [ ] **9.10** Make CI run protocol/browser journeys, authorization, reconnect, crash recovery and install/restore checks alongside builds, unit tests and linting.
- [ ] **9.11** Publish locked reproducible builds, tested platform binaries, container architectures, checksums/signatures and dependency inventory/SBOM.
- [ ] **9.13** Add correlated request/job/workbook/allocation identifiers, lifecycle metrics and structured logs without secrets; define operational alerts, health versus readiness and support diagnostics.
- [ ] **9.14** Automate PR, scheduled and release test tiers from TEST_STRATEGY; add bounded fuzz targets/corpus retention, longer state-machine runs and targeted mutation/critical-branch review. Report implemented versus actually executed checks separately.
- [ ] **9.15** Publish supported trust boundaries, capacity envelope, recovery objectives and upgrade compatibility; add incident, stuck-allocation, credential-rotation and restore runbooks.
- [ ] **9.T01** Test 9.01–9.04 and 9.11: clean-machine/bare-volume installation, ownership, no-cloud standalone start, optional Python profile, backup restore on another host, supported upgrade and rollback limitations.
- [ ] **9.T02** Verify 9.05–9.06 and 9.13–9.15: seeded narrow/wide/text datasets, 1/5/20-client mixed workloads, p50/p95/p99, RSS/spill/browser heap and leaks; validate metric/log attribution and exercise incident runbooks.
- [ ] **9.T03** Verify 9.07–9.11 and 9.14: release artifacts match versioned source, dependency notices/provenance and checksums are present, required CI tiers enforce failures and quarantined tests have explicit follow-up.
- [ ] **9.12 — Gate:** A new user can install, analyze data, create a function, back up, restore and upgrade using published instructions; organization controls remain available in open source.

## 10. Connect external data

Depends on phase 7 permissions and phase 9 operational foundations.

- [ ] **10.01** Define connector interfaces and least-privilege credential storage; never expose credentials to workbook collaborators.
- [ ] **10.02** Add read-only Postgres query/import support.
- [ ] **10.03** Add S3-compatible Parquet access/import support.
- [ ] **10.04** Add scheduled/manual refresh with progress, cancellation, failure history and last-success status.
- [ ] **10.05** Detect schema drift and define replace/append behavior without silently breaking formulas or saved queries.
- [ ] **10.06** Record source lineage and result versions; define how user edits interact with refresh.
- [ ] **10.08** Exercise a shared DataConnector contract through both initial connectors: capability/schema discovery, credential references, bounded reads, cancellation, refresh lineage and typed failures; avoid exposing raw credentials in extension metadata.
- [ ] **10.T01** Test 10.01–10.08: credential scope/rotation/revocation, unauthorized refresh, schema drift, duplicate refresh, source timeout, partial import failure, cancellation, concurrent edits and lineage; compare imported rows/types with independent fixtures using real disposable source services.
- [ ] **10.07 — Gate:** Authorized users can refresh connected data reliably while credentials and cross-space data remain isolated.

## 11. Distribute workbooks across workers

Depends on the single-node release and its durability, identity and revision foundations, including the Local RuntimeProvider and ComputePolicy. This scales multiple workbooks; it does not split one query across machines. Phase 10 connectors are not a prerequisite. Deliver the conservative Kubernetes beta before advertising stronger cluster recovery.

- [ ] **11.01** Separate gateway/auth/routing, shared control metadata and workbook-worker interfaces.
- [ ] **11.02** Add PostgreSQL-backed cluster metadata for users, permissions, jobs and workbook ownership while preserving standalone mode.
- [ ] **11.03** Assign one authoritative worker per mutable workbook and route HTTP/WebSocket traffic to it.
- [ ] **11.04** Store immutable imports, versioned snapshots and durable recovery logs in object storage; keep active workbook files on worker storage.
- [ ] **11.05** Implement enforceable ownership transfer. For the initial direct-file implementation, prove old-owner termination and exclusive storage handoff before replacement writes; lease expiry alone is insufficient. Fail closed when exclusivity cannot be established, with a documented recovery procedure.
- [ ] **11.06** Define acknowledgement durability, recovery-point and recovery-time objectives; implement replay and revision-aware reconnect.
- [ ] **11.07** Add worker admission/placement, quotas, draining, recovery and controlled rebalancing.
- [ ] **11.08** Dispatch isolated function jobs across workers with version checks and idempotent result commits.
- [ ] **11.09** Add cluster deployment examples and metrics for queue depth, resource usage, ownership and recovery.
- [ ] **11.11** Implement a Kubernetes RuntimeProvider using the same lifecycle contract as Local; delegate pod/node scheduling to Kubernetes and support capability discovery without a mandatory bespoke scheduler/operator.
- [ ] **11.12** Package a Helm deployment with least-privilege controller/service identities, runtime isolation, network policy, probes, drain behavior and persistent storage. Provide EKS/Karpenter guidance as an optional deployment target, not an engine dependency.
- [ ] **11.13** Reconcile desired and observed allocations after retries/controller restarts; deduplicate starts and bound/clean orphan resources without deleting retained workbook data.
- [ ] **11.14** Keep profile selection server-validated and cloud-neutral; define node/zone/architecture/storage compatibility and quota limits. Share nodes only within policy; never equate a namespace or shared node with adequate untrusted-code isolation.
- [ ] **11.15** Define an explicit storage lifecycle for attach, consistent checkpoint/backup, retain, transfer and restore; distinguish same-node restart, node loss and zone loss. Refuse unsafe handoffs and report pending capacity/storage constraints.
- [ ] **11.T01** Run provider conformance for 11.01–11.03 and 11.07–11.15: duplicate starts/stops, timeout, readiness failure, insufficient capacity, denied API calls, controller restart, idle drain and orphan cleanup in a disposable Kubernetes cluster.
- [ ] **11.T02** Test 11.05, 11.13 and 11.15 with an old worker still alive during lease expiry/network partition; prove the replacement cannot write and verify exclusive handoff before resuming. A mocked lease test alone does not establish storage fencing.
- [ ] **11.16 — Kubernetes beta gate:** Automatic workbook placement, shared collaboration, policy enforcement, durable retained storage and conservative handoff pass 11.T01–11.T02. Requires 11.01–11.03, 11.05, applicable 11.07/11.09 work and 11.11–11.15; does not certify remote-log recovery or zero acknowledged-edit loss across node/zone failures.
- [ ] **11.T03** Test 11.04–11.08: durable-log acknowledgement boundaries, interrupted snapshots/replay, duplicate jobs, stale function results, lost workers and restore in a new failure domain; measure achieved recovery point/time and compare with published objectives.
- [ ] **11.10 — Gate:** Kill/restart workers and interrupt networking; verify no split-brain writes, no unauthorized access and recovery within documented objectives.

Gate 11.10 is the stronger cluster recovery release gate and requires all phase 11 work. It must not be inferred from 11.16 or a successful Kubernetes deployment.

## E. Publish a tested extension ecosystem

Design internal seams in phases 3/5/7/8/10. Publish each external contract only after real adapters exercise it; phase E is not a prerequisite for the standalone release. RuntimeProvider stabilization requires Local and Kubernetes evidence; connectors require their real source adapters. Stable IDs use `E.xx` to preserve existing phase/task references.

- [ ] **E.01** Define versioned extension manifests with identity, protocol version, capabilities, runtime dependencies, permissions and compatible core versions; reject incompatible registrations.
- [ ] **E.02** Publish RuntimeProvider lifecycle schemas and a reference adapter after Local/Kubernetes conformance passes; document idempotency, readiness, cancellation, reconciliation and cleanup obligations.
- [ ] **E.03** Publish DataConnector and FunctionRuntime SDK/protocol examples with typed schemas, credential references, batching, provenance and deterministic/volatile semantics.
- [ ] **E.04** Define identity-integration contracts for validated claims and revocation while retaining core authorization; prevent extensions from granting themselves resource access.
- [ ] **E.05** Publish supported workbook APIs and events with pagination, error schemas, revision/idempotency semantics and reconnect rules for integrations and alternative clients.
- [ ] **E.06** Enforce administrator-approved extension installation, scoped capabilities and out-of-process execution for untrusted code; preserve audit, commit and migration invariants regardless of adapter.
- [ ] **E.07** Publish executable conformance suites and reference implementations; require results for each supported provider, connector and runtime version.
- [ ] **E.08** Define compatibility/deprecation and security-reporting policies, extension maintainer guidance and architecture decision review; document upgrade/migration expectations without promising an untested stable in-process ABI.
- [ ] **E.T01** Test E.01–E.06: incompatible protocol/manifests, forged capabilities, revoked credentials, duplicate/out-of-order operations, crash/restart and attempts to bypass ACL/revision/commit checks through real extension boundaries.
- [ ] **E.T02** Verify E.02–E.08 with an extension authored from published documentation without changes to core; run the conformance suite across supported version combinations and a breaking-upgrade rejection case.
- [ ] **E.09 — Gate:** A contributor can implement, package, install and test a supported extension using documented contracts; its failure cannot bypass the declared core invariants.

## 12. Evaluate later extensions

These are optional follow-on decisions, not blockers for a useful open-source release. Implement only when demand and measurements justify them.

- [ ] **12.01** Evaluate distributed execution of one large dataset; define partitioning, ordering, joins, cancellation and result snapshots before selecting an engine.
- [ ] **12.02** Evaluate additional warehouse connectors and cross-workbook queries with source-level authorization.
- [ ] **12.03** Evaluate database write-back with preview, permissions, conflict handling and recoverability.
- [ ] **12.04** Evaluate desktop packaging and a smaller-data browser-only mode.
- [ ] **12.05** Evaluate an optional local/BYOK AI assistant with explicit data access and reviewable operations.
- [ ] **12.06** Evaluate general Python notebooks and table-returning function jobs separately from pure cell functions.
- [ ] **12.07** Evaluate a Microsoft Excel add-in if users need custom Alephgrid functions inside Excel itself.
- [ ] **12.08** Evaluate optional JupyterHub integration through supported authentication/routing contracts for existing customer deployments; do not fork or require JupyterHub core.
- [ ] **12.T01** For each evaluation 12.01–12.08, record demand, decision, prototype evidence, cost/operational tradeoffs and acceptance tests if adopted. Checking an evaluation does not mean its proposed feature is implemented; add implementation tasks before advertising support.

## Progress notes

Add one row when starting work, encountering a blocker or completing a task. Keep task checkboxes above as the completion source of truth.

| Date | Task ID | Status | Evidence / next action |
|---|---|---|---|
| 2026-10-26 | Planning | Checklist created | Consolidated the assessment and SQL/custom-function requirements; implementation completion has not been reassessed. |
| 2026-10-26 | 1.01–1.11 | Complete | Engine, server, generated protocol, trash UI and browser journeys. Commands and results in TEST_MATRIX.md; new tests failed on `ff3e736` as expected. |
| 2026-10-26 | 2.01–2.10 | Complete | Import preview/overrides/progress/cancel, xlsx notes, bounded sheet copies, saved live views and snapshots, grid disposal, accessibility. Nine defects found by the new tests and fixed; see TEST_MATRIX.md. |
| 2026-10-26 | Architecture / remaining phases | Checklist enhanced | Preserved completed tasks and stable IDs; added workbook-first runtime/provider foundations, automatic placement and compute policy, conservative Kubernetes handoff versus stronger recovery gates, phase-specific test packages and extension ecosystem. Planning changes only; no new implementation completion claimed. |

| 2026-10-27 | U / 10 / P / M / 12 | Planning enhanced | Added complete light/dark product UI, S3-first connector scope, measurable performance work and mapped tests; MCP, GCS, Azure, Postgres and lakehouse expansions explicitly deferred. Preserved all existing completion marks and IDs; no product implementation performed. |

| 2026-10-27 | F | Planning enhanced | Researched Google Sheets; compared 562 names against pinned IronCalc registry; added function matrix, keyboard/upstream plan and formula-at-scale implementation/test tasks. No new product code implemented. |
| 2026-10-30 | F.01–F.05 | In progress | Commits b5f62fc (vendored IronCalc patch from the Codex handoff), 0966c18 (F.01 done; F.02 coercion matrix, 6 explicit gaps), b58943a (F.03 done; F.04 implemented, real assistive-technology runs not done), b8d5be5 (F.05 spec + compiler). Next: store `sheet`-language calculated columns (format 6), error overlay in row blocks and the grid, then F.06 lookups as grouped joins. Browser suite: 5 failures come from the uncommitted UI redesign, not phase F. Build with `CARGO_TARGET_DIR` pointing at an existing target to avoid rebuilding DuckDB. |
