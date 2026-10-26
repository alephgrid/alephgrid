# Rowfinity implementation checklist

Created: 26 October 2026.

Goal: an open-source analytical spreadsheet with SQL, user-defined formula functions, organization-owned spaces, and simple single-node or distributed hosting.

## How to track progress

Work through phases in order. Within a phase, follow the numbered task IDs. Check an item only when its behavior is implemented and verified; partial backend support does not count as a finished user feature. Add a test, commit, PR or verification note beside completed items. Keep blocked items unchecked and record the dependency.

All implementation tasks below start unchecked: they describe remaining work from the assessment, not a fresh certification of current code. Some foundations already exist. Re-check findings against current code before changing it.

Supporting documents: [assessment](OPEN_SOURCE_ASSESSMENT.md), [SQL and custom-function design](SQL_AND_CUSTOM_FUNCTIONS.md). This file is the ordered execution tracker; those documents provide rationale. The older [roadmap](ROADMAP.md) is background and may lag implementation.

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

- [ ] **2.01** Connect existing IronCalc APIs to sheet tabs, rendering, editing and sheet change events.
- [ ] **2.02** Support creating and editing a blank sheet from a new workbook.
- [ ] **2.03** Complete XLSX import/export UI with explicit compatibility warnings and roundtrip verification.
- [ ] **2.04** Expose existing sheet undo/redo and verify formula recalculation after undo, redo and reopening.
- [ ] **2.05** Add import preview, type overrides, progress and actionable error messages.
- [ ] **2.06** Clearly distinguish large Tables from free-form Sheets; bound table/query-to-sheet conversion and display its limits.
- [ ] **2.07** Persist named views with sort, filter and column presentation settings; label live versus snapshot behavior.
- [ ] **2.08** Add grid disposal for window listeners, observers, timers and pending requests.
- [ ] **2.09** Provide accessible active-cell/viewport semantics, keyboard navigation and visible focus.
- [ ] **2.10 — Gate:** A new user can create, import, edit, save and reopen both tables and sheets using the UI alone.

## 3. Establish bounded execution and storage foundations

Depends on phase 1. Complete before shared hosting or expensive custom-function execution.

- [ ] **3.01** Add bounded workbook job queues with cancellation, deadlines, progress and priority for interactive reads.
- [ ] **3.02** Bound uploads, request/response bytes, pasted cells and requested row/column counts; clamp before allocation.
- [ ] **3.03** Enforce host-wide CPU, memory and temporary-disk budgets; account for DuckDB, sheets and worker allocations.
- [ ] **3.04** Evict idle workbook handles and expire unused materialized views.
- [ ] **3.05** Fetch visible column ranges and use a byte-bounded cache covering both fresh and stale blocks.
- [ ] **3.06** Separate stable record IDs from display order; preserve efficient positional access without renumbering an entire table tail.
- [ ] **3.07** Add monotonic workbook revisions, idempotent mutation IDs and an atomic operation-history format.
- [ ] **3.08** Add versioned storage migrations and a consistent backup/restore mechanism covering workbook data and metadata.
- [ ] **3.09** Handle SIGTERM: stop admission, drain work, persist/checkpoint and close cleanly.
- [ ] **3.10 — Gate:** Cancellation, overload, disk-full, crash/restart and restore tests preserve consistency and keep resource use bounded.

## 4. Deliver the SQL workspace

Build on the existing SQL console. Depends on phases 1–3; shared query permissions depend on phase 7.

- [ ] **4.01** Add a schema browser and table/column autocomplete.
- [ ] **4.02** Support safely bound query parameters and clear parse/runtime errors.
- [ ] **4.03** Show bounded result previews, explicit truncation, execution timing, cancellation and full export actions.
- [ ] **4.04** Save, rename, reopen and version queries; keep local queries private until sharing permissions exist.
- [ ] **4.05** Complete result-to-table and bounded result-to-sheet workflows.
- [ ] **4.06** Distinguish saved SQL, live query views and materialized results; track source revisions and refresh status.
- [ ] **4.07** Restrict SQL/formula capabilities, external readers, extension loading and internal metadata access for shared-host mode; console disabling alone is insufficient.
- [ ] **4.08** Route supported mutations through operations that maintain Rowfinity metadata, history and revisions.
- [ ] **4.09 — Gate:** Verify joins, aggregates, CTEs, calculated columns, parameters, cancellation, export and result materialization end to end.

## 5. Add user-defined formula functions

Depends on phases 2–4. This phase covers formula-defined and SQL functions; Python is phase 8.

- [ ] **5.01** Prototype a named IronCalc LAMBDA: define `MARGIN(revenue,cost)` and call `=MARGIN(B2,C2)`.
- [ ] **5.02** Define a function registry with stable IDs, signature, source, immutable version, scope, documentation, tests and execution adapter.
- [ ] **5.03** Build the function editor with argument declarations, examples, test inputs and preview results.
- [ ] **5.04** Register named formulas through the engine; support cell/range arguments and nested formulas without string-substitution evaluation.
- [ ] **5.05** Add function autocomplete, signature help, documentation and readable errors in the formula editor.
- [ ] **5.06** Pin workbook function versions; support explicit upgrade and rollback without silently changing other workbooks.
- [ ] **5.07** Specify blanks/NULL, decimals, dates, errors, array outputs, spill collisions and naming conflicts.
- [ ] **5.08** Add validated SQL expression functions/macros for SQL queries and calculated table columns; explicitly document which engines support each function type.
- [ ] **5.09** Verify dependency recalculation, cycles, save/reopen, version changes and unsupported XLSX-export behavior.
- [ ] **5.10 — Gate:** A user can define, test, call and persist their own function from the UI, with correct recalculation after argument edits.

## 6. Add analysis tools for users who do not write SQL

Depends on phases 2–4. These items can proceed alongside phase 5 once dependencies are complete.

- [ ] **6.01** Add a group-by/pivot builder with refresh behavior and bounded output.
- [ ] **6.02** Add charts backed by server-side aggregates rather than whole-table downloads.
- [ ] **6.03** Add a join builder with key/type validation and duplicate-match visibility.
- [ ] **6.04** Add null, duplicate, distinct-count and type profiling.
- [ ] **6.05** Add find/replace with preview and reversible mutations.
- [ ] **6.06** Add number/date/locale formats, freeze/hide columns and bounded conditional formatting.
- [ ] **6.07 — Gate:** An analyst can import data, join it, summarize it, chart it and reopen the saved analysis without writing SQL.

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
- [ ] **7.12 — Gate:** Two spaces on one host cannot access each other's data; concurrent/reconnecting users cannot silently lose or misapply edits.

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
- [ ] **8.12 — Gate:** Define → test → publish → call from cells → recalculate → reopen works; worker isolation and failure tests pass without hanging other workbooks.

## 9. Package and release the single-node product

An earlier local alpha may ship after phases 1–5 with explicit limitations. The team release requires phases 7–8; analysis features can be released incrementally.

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
- [ ] **9.12 — Gate:** A new user can install, analyze data, create a function, back up, restore and upgrade using published instructions; organization controls remain available in open source.

## 10. Connect external data

Depends on phase 7 permissions and phase 9 operational foundations.

- [ ] **10.01** Define connector interfaces and least-privilege credential storage; never expose credentials to workbook collaborators.
- [ ] **10.02** Add read-only Postgres query/import support.
- [ ] **10.03** Add S3-compatible Parquet access/import support.
- [ ] **10.04** Add scheduled/manual refresh with progress, cancellation, failure history and last-success status.
- [ ] **10.05** Detect schema drift and define replace/append behavior without silently breaking formulas or saved queries.
- [ ] **10.06** Record source lineage and result versions; define how user edits interact with refresh.
- [ ] **10.07 — Gate:** Authorized users can refresh connected data reliably while credentials and cross-space data remain isolated.

## 11. Distribute workbooks across workers

Depends on the single-node release and its durability, identity and revision foundations. This scales multiple workbooks; it does not split one query across machines.

- [ ] **11.01** Separate gateway/auth/routing, shared control metadata and workbook-worker interfaces.
- [ ] **11.02** Add PostgreSQL-backed cluster metadata for users, permissions, jobs and workbook ownership while preserving standalone mode.
- [ ] **11.03** Assign one authoritative worker per mutable workbook and route HTTP/WebSocket traffic to it.
- [ ] **11.04** Store immutable imports, versioned snapshots and durable recovery logs in object storage; keep active workbook files on worker storage.
- [ ] **11.05** Implement ownership leases and fencing to prevent an old worker from committing after reassignment.
- [ ] **11.06** Define acknowledgement durability, recovery-point and recovery-time objectives; implement replay and revision-aware reconnect.
- [ ] **11.07** Add worker admission/placement, quotas, draining, recovery and controlled rebalancing.
- [ ] **11.08** Dispatch isolated function jobs across workers with version checks and idempotent result commits.
- [ ] **11.09** Add cluster deployment examples and metrics for queue depth, resource usage, ownership and recovery.
- [ ] **11.10 — Gate:** Kill/restart workers and interrupt networking; verify no split-brain writes, no unauthorized access and recovery within documented objectives.

## 12. Evaluate later extensions

These are optional follow-on decisions, not blockers for a useful open-source release. Implement only when demand and measurements justify them.

- [ ] **12.01** Evaluate distributed execution of one large dataset; define partitioning, ordering, joins, cancellation and result snapshots before selecting an engine.
- [ ] **12.02** Evaluate additional warehouse connectors and cross-workbook queries with source-level authorization.
- [ ] **12.03** Evaluate database write-back with preview, permissions, conflict handling and recoverability.
- [ ] **12.04** Evaluate desktop packaging and a smaller-data browser-only mode.
- [ ] **12.05** Evaluate an optional local/BYOK AI assistant with explicit data access and reviewable operations.
- [ ] **12.06** Evaluate general Python notebooks and table-returning function jobs separately from pure cell functions.
- [ ] **12.07** Evaluate a Microsoft Excel add-in if users need custom Rowfinity functions inside Excel itself.

## Progress notes

Add one row when starting work, encountering a blocker or completing a task. Keep task checkboxes above as the completion source of truth.

| Date | Task ID | Status | Evidence / next action |
|---|---|---|---|
| 2026-10-26 | Planning | Checklist created | Consolidated the assessment and SQL/custom-function requirements; implementation completion has not been reassessed. |
| 2026-10-26 | 1.01–1.11 | Complete | Engine, server, generated protocol, trash UI and browser journeys. Commands and results in TEST_MATRIX.md; new tests failed on `ff3e736` as expected. |
