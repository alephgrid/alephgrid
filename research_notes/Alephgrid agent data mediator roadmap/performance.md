# Performance and Efficiency Enhancements for Alephgrid (DuckDB engine, protocol, UI layers)

Grounding: Alephgrid pins `duckdb = "1.20000.0"` with `bundled`, `parquet`, `json` features (`/Users/sparsh/.codex/worktrees/2f32/Rowfinity/Cargo.toml`); the engine already reuses long-lived connections per workbook (one writer, readers in `workbook.rs`), sets `TimeZone='UTC'` per connection, sandboxes guest connections with `SET enable_external_access=false; SET memory_limit='64MB'; SET threads=1` (`functions.rs:462`), and pages reads via `LIMIT` in `reads.rs`. The web grid is already a canvas-virtualized renderer (`web/src/grid.ts`). Note the docs site now redirects "stable" to `docs/2.0/`, i.e., DuckDB 2.0 docs are current as of late 2026 — version-sensitive behaviors below are flagged.

## DuckDB-level: which settings and patterns matter most?

### Takeaway
The highest-leverage, documented levers are: cap `memory_limit` below the 80% default on shared hosts, set `temp_directory` so blocking operators spill instead of OOM, disable `preserve_insertion_order` for big imports/exports, keep connections long-lived (already done), pre-sort tables on commonly filtered columns to exploit zonemaps (measured 1.5x faster, 2.5x smaller), and avoid ART indexes except for highly selective point lookups because index memory is not buffer-managed. Prepared statements pay off precisely in Alephgrid's regime (repeated sub-100 ms grid queries).

### Cited Findings
- `memory_limit` defaults to 80% of system RAM; if the OS/OOM-reaper kills the process, DuckDB's OOM guide recommends lowering it to ~50–60% of total RAM because some operations bypass the buffer manager and can exceed the limit — [DuckDB OOM guide](https://duckdb.org/docs/preview/guides/performance/oom)
- `SET preserve_insertion_order = false` is the documented fix for OOM during larger-than-memory Parquet/CSV import/export; it lets DuckDB reorder results lacking `ORDER BY`, reducing memory use — [How to Tune Workloads](https://duckdb.org/docs/2.0/guides/performance/how_to_tune_workloads.html)
- Spilling (out-of-core) covers `GROUP BY`, joins, `ORDER BY`, and window functions, but not `string_agg()`; default temp dir is `<dbfile>.tmp`, changeable via `SET temp_directory = '...'` — [How to Tune Workloads](https://duckdb.org/docs/2.0/guides/performance/how_to_tune_workloads.html)
- Parallelism granularity: default row-group size is 122,880 rows, so a query needs at least `k × 122,880` rows to use `k` threads; a smaller row-group size can be set at attach time (`ATTACH ... (ROW_GROUP_SIZE 16384)`). Too many threads (HyperThreading) can slow queries; cap with `SET threads = X` — [How to Tune Workloads](https://duckdb.org/docs/2.0/guides/performance/how_to_tune_workloads.html)
- Prepared statements are documented as worthwhile when the same query is run repeatedly with different parameters and each execution takes under ~100 ms (parse/plan caching dominates); DuckDB "is not designed for many small concurrent queries; it favors fewer, larger queries" — [How to Tune Workloads](https://duckdb.org/docs/2.0/guides/performance/how_to_tune_workloads.html)
- Reuse one connection rather than reconnecting per query; DuckDB's in-memory metadata cache is discarded when the last connection closes. A connection pool helps mainly when something other than CPU (e.g., network) is the bottleneck — [How to Tune Workloads](https://duckdb.org/docs/2.0/guides/performance/how_to_tune_workloads.html)
- Zonemaps (min-max indexes) are automatic for all general-purpose types and power row-group skipping, but only pay off when data is (even partially) ordered on filtered columns. Measured in the docs: ordered `DATETIME` data = 1.3 GB storage and 0.6 s query vs 3.3 GB and 0.9 s unordered (~2.5x smaller, ~1.5x faster). Ordered INTEGER keys beat random UUIDs for selective filters — [Indexing guide](https://duckdb.org/docs/2.0/guides/performance/indexing.html)
- ART indexes: help only highly selective equality/`IN` filters on a single column (multi-column/expression indexes are not eligible for index scans); they slow writes, don't affect joins/aggregations/sorts, and their memory is registered with but **not managed by** the buffer manager (never evicted under pressure). Index-scan threshold defaults to `MAX(2048, 0.001 × table_cardinality)`, tunable via `index_scan_percentage` / `index_scan_max_count`. Indexes load lazily on attach, so re-attaching frees index memory. Create indexes after bulk load, not before — [Indexing guide](https://duckdb.org/docs/2.0/guides/performance/indexing.html)
- Compression is on by default only for on-disk databases, not in-memory tables; on TPC-H Q1 SF30 a compressed database ran in ~0.55 s vs 4.22 s uncompressed in-memory (~8x faster, better cache behavior); `ATTACH ':memory:' AS db (COMPRESS)` enables it for in-memory — [How to Tune Workloads](https://duckdb.org/docs/2.0/guides/performance/how_to_tune_workloads.html)
- Profiling: `EXPLAIN` for the physical plan; `EXPLAIN ANALYZE` runs the query and reports CPU time per operator (per-step times sum above wall-clock under multithreading). Red flags to look for: nested-loop joins, missing filter pushdown, join orders that explode cardinality — [How to Tune Workloads](https://duckdb.org/docs/2.0/guides/performance/how_to_tune_workloads.html)
- Checkpointing: `CHECKPOINT` flushes the WAL to the DB file; automatic checkpoints are triggered by WAL size. Behavior change: before v1.4 `FORCE CHECKPOINT` aborted in-progress transactions; from v1.4 it waits on the checkpoint lock. Checkpoints merge row groups with many deletes, partially reclaiming space — [CHECKPOINT statement docs](https://duckdb.org/docs/stable/sql/statements/checkpoint). A third-party post claims `checkpoint_threshold` defaults to 16 MB (`SET checkpoint_threshold='100MB'` as a tuning example) — [DEV Community post](https://dev.to/sharmaprash/duckdb-how-to-fine-tune-parameters-39h8) (secondary source; verify against the configuration reference).

### Inferences
- For Alephgrid's revisioned mutations: because deletes/updates leave garbage merged only at checkpoint time, scheduling an explicit `CHECKPOINT` after large revision batches (and before snapshot reads of heavily edited tables) should keep scans tight; raising `checkpoint_threshold` on write-heavy sessions trades durability lag for fewer stalls.
- Given the one-writer/two-reader model, a per-workbook `memory_limit` set to (host RAM × ~0.5) / active workbooks is safer than the 80% default; the existing `apply settings` hook (`workbook.rs:163`) already supports this.
- Avoid adding ART indexes to user tables for grid filtering; instead keep the internal `_rf` row-id/order columns physically clustered (insertion-ordered) so zonemaps prune range reads — this matches the docs' ordered-integer finding and costs no extra memory.
- Alephgrid formula columns are compiled to SQL and re-run frequently with identical shapes — exactly the <100 ms repeated-query regime where cached prepared statements (duckdb-rs `Connection::prepare_cached`) are documented to pay off.

### Gaps
- No authoritative source found for the `checkpoint_threshold` default (official configuration page not retrieved; only a secondary blog claims 16 MB).
- No DuckDB doc/blog with measured numbers for ENUM/dictionary types' query-speed impact was retrieved in this pass; the ENUM data-type docs exist but were not fetched, so no citation is offered.

## Result transport: Arrow IPC vs JSON, paging strategy, result caching

### Takeaway
Columnar transport (Arrow IPC) measurably shrinks payloads and skips serialize/parse overhead versus JSON rows, and every sub-second DuckDB dashboard tool (Mosaic, Perspective, Rill, Evidence) relies on some combination of Arrow transport, query caching/consolidation, pre-aggregation, and prefetch rather than raw per-request speed. Keyset pagination beats OFFSET at depth in OLTP engines, but in DuckDB the bigger win is zonemap-prunable range predicates on an ordered key column.

### Cited Findings
- DuckDB supports producing/consuming the Arrow IPC serialization format via the `arrow` community extension; the DuckDB team argues Arrow "eliminates encoding and decoding overheads," yielding faster, more efficient interchange for query-result transfer (no published numbers in the post) — [Arrow IPC Support in DuckDB (May 2025)](https://duckdb.org/2025/05/23/arrow-ipc-support-in-duckdb)
- A real-world migration (Hotdata CLI PR #103) from JSON to Arrow IPC result fetching reported ~39% smaller payloads and ~150 ms faster fetches on large result sets (author's own measurements, not independent) — [hotdata-cli PR #103](https://github.com/hotdata-dev/hotdata-cli/pull/103)
- Mosaic's coordinator implements query caching, query consolidation (merging compatible queries into one issued query), and speculative prefetching (clients publish queries before results are needed); it builds pre-aggregated materialized-view tables when the cursor enters a view; the authors claim real-time interaction "under 100 ms, often 1–10 ms" over millions-to-billions of records (author claim) — [Why Mosaic](https://idl.uw.edu/mosaic/why-mosaic); [Mosaic SIGMOD paper](https://rmarcus.info/dbscholar/papers/hfdc63bfd1fe58702); [Mosaic Selections (arXiv 2025)](https://arxiv.org/pdf/2507.19690)
- Rill's DuckDB-backed metrics views cache results with configurable invalidation: default rule invalidates when the newest timeseries value changes; a custom scalar `key_sql` re-evaluated at most once per `key_ttl` acts as the cache key; per-user security attributes are part of the key — [Rill metrics-view docs](https://docs.rilldata.com/developers/build/metrics-view/underlying-model). A Rill FOSDEM 2024 talk states each dashboard interaction can fan out to up to ~100 parallel queries, motivating this caching — [FOSDEM 2024: Aerodynamic Data Models](https://fosdem.org/schedule/event/fosdem-2024-3296-aerodynamic-data-models-flying-fast-at-scale-with-duckdb)
- Evidence uses DuckDB at build time and DuckDB-WASM in the browser so interactions re-query client-side without server round trips; instantiating DuckDB-WASM requires Web Workers so queries don't block the UI thread (reported by MotherDuck, a vendor source, not Evidence first-party docs) — [MotherDuck on Evidence](https://motherduck.com/glossary/evidence/); [MotherDuck DuckDB-WASM post](https://motherduck.com/blog/duckdb-wasm-in-browser/)
- Perspective (FINOS) client/server-replicated mode: server holds tables in memory, browsers hold full WebAssembly-local copies, synchronized via Apache Arrow over WebSockets; client-side interactive performance is then "identical to client-only"; intended for medium-sized, real-time, editable datasets with many concurrent users — [Perspective client/server architecture](https://perspective.finos.org/guide/explanation/architecture/client_server.html); [Perspective data binding](https://perspective.finos.org/docs/server)
- OFFSET pagination cost grows with offset because skipped rows are still read and discarded; keyset pagination filters on the last-seen sort key and becomes an index seek when a composite index matches the ORDER BY; OFFSET also yields gaps/duplicates under concurrent writes; keyset cannot jump to arbitrary page N and needs a tie-breaker on non-unique keys (directional blog/vendor sources, no cross-engine timings) — [MotherDuck OFFSET glossary](https://motherduck.com/glossary/offset/); [Stacksync keyset post](https://www.stacksync.com/blog/keyset-cursors-postgres-pagination-fast-accurate-scalable); [resumelens keyset-vs-offset](https://www.resumelens.org/blog/sql/sql-pagination-keyset-vs-offset)

### Inferences
- Concrete adoption path for Alephgrid (Rust + TS, single binary): serialize grid pages as Arrow IPC streams from axum (the `duckdb` crate exposes Arrow record batches natively via arrow-rs) and decode with `apache-arrow` JS (or flechette) in a worker. The Hotdata figure (~39% smaller, no row-wise JSON encode/parse) is the best available directly comparable measurement; expect the win to grow with page width and numeric density.
- Keyset caveat for DuckDB specifically: DuckDB has no B-tree, so keyset's classic "index seek" advantage maps instead to zonemap pruning — paging `WHERE ord > ? ORDER BY ord LIMIT n` on the physically ordered `ORD`/row-id column prunes to a few row groups, whereas `OFFSET k` scans and discards k rows. Since Alephgrid grids are sorted views, a (sort-key, row-id) keyset cursor is the right default; keep OFFSET only for arbitrary scrollbar jumps on small results.
- Result caching keyed by `(normalized-SQL hash, table data_rev/revision)` is the Rill `key_sql` pattern made exact: Alephgrid already tracks `data_rev` per table (`functions.rs:414`), so a cache entry is valid precisely while every referenced table's revision is unchanged — no TTLs needed. Cache at the Arrow-IPC-bytes level so hits skip both execution and serialization.
- Mosaic-style query consolidation applies directly: when a formula edit invalidates several visible panels/column stats, batch their queries into one round trip (or one UNION ALL query) per revision tick.

### Gaps
- No independent, rigorous Arrow-IPC-vs-JSON transport benchmark was found — only the DuckDB team's qualitative claim and one practitioner PR's self-reported numbers.
- No published timings comparing keyset vs OFFSET on DuckDB itself (all pagination sources are Postgres/MySQL-oriented).

## Incremental recomputation of formula columns/views on edit

### Takeaway
DuckDB core has no materialized views or built-in IVM, so the practical pattern is an application-level dependency graph over formula columns plus revision-keyed memoization, recomputing only dirty columns via `UPDATE ... FROM` or `CREATE OR REPLACE` of scratch tables; research-grade DuckDB IVM extensions (OpenIVM) exist but are not production-pinnable.

### Cited Findings
- DuckDB does not support `CREATE MATERIALIZED VIEW` or automatic view refresh; the standard workaround is rebuilding a table with `CREATE OR REPLACE TABLE ... AS SELECT` — [MotherDuck materialized-view glossary](https://motherduck.com/glossary/materialized-view/)
- OpenIVM is a DuckDB extension for incremental view maintenance supporting grouped aggregates, inner/left/full-outer joins, UNION ALL, and DISTINCT (full outer join handled by merge + recompute), with refresh pragmas and a cost estimate comparing incremental vs full recompute — [OpenIVM wiki](https://github.com/ila/openivm/wiki)
- CWI's earlier `ivm-extension` maintains views via `PRAGMA ivm('view')` using a delta table with a multiplicity column, supports only SUM/COUNT aggregates, no joins/HAVING/nested subqueries, treats UPDATE as delete+insert; last pushed 2023-10-24 (likely stale) — [cwida/ivm-extension](https://awesome.ecosyste.ms/projects/github.com%2Fcwida%2Fivm-extension); [CWI publication](https://ir.cwi.nl/pub/34287/View%20at%20GitHub)
- Mosaic demonstrates the materialized-scratch-table pattern at interaction time: it builds smaller pre-aggregated tables (materialized views) lazily, "when the mouse cursor enters a view," to make selection updates fast — [Why Mosaic](https://idl.uw.edu/mosaic/why-mosaic)
- DuckDB appends never conflict (even on the same table) and transactions hold no locks, so recompute jobs that only append/replace scratch tables can overlap safely with reads — [Analytics-Optimized Concurrent Transactions (DuckDB blog, Oct 2024)](https://duckdb.org/2024/10/30/analytics-optimized-concurrent-transactions)

### Inferences
- The pragmatic architecture for Alephgrid: (1) maintain a column-level dependency DAG from the formula compiler (it already knows referenced columns); (2) on edit, mark downstream formula columns dirty and bump their tables' `data_rev`; (3) recompute each dirty column with a single set-based `UPDATE t SET c = expr FROM ...` (the engine already does exactly this shape in `analysis.rs:411`), scoped with a `WHERE id IN (affected rows)` predicate when the edit's row set is known — row-scoped recompute is the cheap "incremental" 80% case without any IVM machinery. Full-column recompute remains the fallback for aggregate-dependent formulas (SUMIF over the whole column, etc.).
- This is salsa-style memoization in spirit (durable revision counter + per-derived-value validation against input revisions) applied at column granularity; Alephgrid's existing per-table `data_rev` is the revision clock. No external source measured this pattern on DuckDB; treat as design inference.
- `CREATE OR REPLACE TABLE` for derived/scratch tables is transactional and append-only from readers' perspective, so snapshot reads on the two reader connections never see partial recomputes — consistent with the documented no-lock MVCC model.
- Do not adopt OpenIVM as a dependency in a single-binary pinned-crate product: it is a research extension, and the bundled duckdb-rs build complicates loading community extensions; use its join/aggregate delta rules as a design reference only.

### Gaps
- No published benchmark quantifying incremental vs full recompute on DuckDB (OpenIVM's wiki mentions a cost estimator but no retrieved numbers).
- salsa-rs applicability is my inference; no source discussing salsa-style recomputation over SQL engines was found.
- dbt incremental-model docs were not fetched this pass; the pattern (merge new/changed partitions into a materialized table) is well known but uncited here.

## Frontend: canvas grid virtualization at 100M+ rows

### Takeaway
The settled pattern — used by Glide Data Grid, Perspective, and already by Alephgrid's grid.ts — is: draw only visible cells to canvas (DOM virtualization alone bottlenecks on scroll), pull cell data through an async windowed data source, keep sorting/filtering in the database, and move data handling off the main thread via Web Workers; Perspective additionally keeps a full Arrow-synchronized WASM replica client-side for zero-latency interaction.

### Cited Findings
- Glide Data Grid: scales to "millions of rows" with lazy, on-demand cell rendering; the maintainers originally virtualized both axes with react-virtualized and moved to canvas because "the problem was simply scrolling performance"; the grid takes only column defs, a row count, and a `getCellContent` callback, and recommends sorting/filtering be done by the data source (e.g., a database query) — [glide-data-grid README](https://github.com/glideapps/glide-data-grid)
- Perspective browser client: `worker()` spawns the engine in a Web Worker; in client/server-replicated mode the browser holds a full local WASM copy of server tables synchronized via Apache Arrow over WebSockets, making interactive performance "identical to client-only" — [Perspective architecture](https://perspective.finos.org/guide/explanation/architecture.html); [client/server page](https://perspective.finos.org/guide/explanation/architecture/client_server.html)
- DuckDB-WASM-based tools run the database in a Web Worker because "complex analytical queries" would otherwise block the main UI thread — [MotherDuck DuckDB-WASM post](https://motherduck.com/blog/duckdb-wasm-in-browser/)
- Mosaic clients speculatively publish queries before results are needed (prefetch), and the coordinator caches and consolidates them — the browser-side half of debounced-scroll + prefetch — [Why Mosaic](https://idl.uw.edu/mosaic/why-mosaic)

### Inferences
- Alephgrid's grid.ts already implements the canvas + visible-window pattern; the documented next wins are (a) overscan prefetch: request ±1–2 pages beyond the viewport in scroll direction (Mosaic's speculative-publish pattern), (b) decode Arrow pages and hold the row cache in a Web Worker, posting only the visible slice (or a transferable/SharedArrayBuffer-backed typed-array view) to the render thread, and (c) debounce scroll-driven queries but render immediately from cache with a "loading" shimmer for missing rows — Glide's getCellContent model shows the renderer must tolerate async cells.
- SharedArrayBuffer note: no retrieved source measured SAB vs postMessage for grids; Arrow IPC buffers are transferable (zero-copy postMessage), which captures most of the benefit without COOP/COEP header requirements. Treat SAB as optional.
- Perspective-style full client replication is the wrong default at 100M+ rows (it assumes "medium-sized" datasets per its own docs); Alephgrid's server-windowed model is the correct architecture, with replication worth considering only for small result sets (pivots/aggregates).

### Gaps
- finos/regular-table specifics (its DOM-recycling approach, perf numbers) were not retrieved this pass.
- No independent benchmark of canvas vs DOM grids, nor published numbers on prefetch window sizing or debounce intervals; these remain engineering heuristics.

## Concurrency: readers, job queue, cancellation

### Takeaway
Within one process DuckDB supports concurrent readers and writers via MVCC + optimistic concurrency with no locks — read-only transactions never conflict and appends never conflict — so Alephgrid's one-writer/N-reader layout is sound and can safely grow reader connections for parallel grid pages; cancellation is first-class in duckdb-rs via `InterruptHandle` from another thread.

### Cited Findings
- Official concurrency model: within a single process, multiple writer threads are supported via MVCC + optimistic concurrency control; conflicts arise only when two transactions update/delete the same row (second one fails with a conflict error; rerun is the documented recovery); "executing many small transactions is not a primary design goal" — [DuckDB concurrency docs (LTS)](https://duckdb.org/docs/lts/connect/concurrency)
- "Transactions do not hold locks – they can always read and write to any row in any table... read-only transactions can never conflict with one another"; appends never conflict even on the same table — [Analytics-Optimized Concurrent Transactions (DuckDB blog)](https://duckdb.org/2024/10/30/analytics-optimized-concurrent-transactions)
- Multi-process: read-only access from multiple processes is supported; concurrent multi-process read-write is not (the preview docs point to a beta "Quack" client-server protocol as of v1.5.2 and to DuckLake-with-Postgres-catalog for stable multi-instance read-write) — [DuckDB concurrency docs (preview)](https://www.duckdb.org/docs/preview/connect/concurrency.html)
- Connection-count guidance: multiple connections help "mainly when another resource, such as network transfer, is the bottleneck rather than CPU" — each query already parallelizes across threads internally — [How to Tune Workloads](https://duckdb.org/docs/2.0/guides/performance/how_to_tune_workloads.html)
- Cancellation in Rust: `Connection::interrupt_handle()` returns `Arc<InterruptHandle>` (Send + Sync); calling `interrupt()` from another thread makes the running query fail with `Error::DuckDBFailure`; interrupting after the connection is dropped is a no-op — [DuckDB Rust client docs](https://duckdb.org/docs/current/clients/rust/profiling); [InterruptHandle docs.rs](https://docs.rs/spiceai_duckdb_fork/latest/duckdb/struct.InterruptHandle.html)

### Inferences
- Queue sizing: since one query saturates all cores at ≥ threads × 122,880 rows, the reader pool should be sized for latency hiding (overlap network/serialization with execution), not throughput — 2–4 readers per workbook with per-connection `SET threads` partitioned across them prevents two big scans from oversubscribing cores. No source gives a formula; this combines the row-group parallelism rule with the connection-pool guidance above.
- Cancellation latency: wire every grid-paging job to an `InterruptHandle`; on scroll, interrupt the superseded page query before enqueuing the next. The docs don't quantify interrupt latency (DuckDB checks interrupts between pipeline tasks), so measure it; no retrieved source states the check granularity.
- The single-writer discipline Alephgrid already enforces sidesteps optimistic-conflict retries entirely; keep it, and run formula recomputes as writer-queue jobs rather than opening more writer connections.

### Gaps
- No documented figure for interrupt/cancellation latency (how often DuckDB polls the interrupt flag).
- No official guidance on reader-connection pool sizing for one process; the 2–4 figure is inference.
- Whether Alephgrid's pinned duckdb-rs `1.20000.0` maps to a libduckdb with the v1.4+ `FORCE CHECKPOINT` (waiting) semantics was not verified — check `libduckdb-sys`'s bundled version.

## Quantified wins: published benchmarks justifying choices

### Takeaway
DuckDB's own maintained benchmarks show large, sustained engine-level gains (full H2O.ai db-benchmark suite: ~500 s on v0.2.7 in 2021 down to <35 s on current DuckDB) and the docs carry concrete micro-measurements (8x from compression, 1.5x/2.5x from physical ordering), so Alephgrid's biggest quantified levers are staying current on the DuckDB version and controlling data layout — ahead of any application-level tuning.

### Cited Findings
- DuckDB Labs revived and now maintains the H2O.ai db-benchmark (group-by/join vs data.table, Polars, dplyr, ClickHouse, etc.); in their 2023 re-run on c6id.metal, "DuckDB and DuckDB-latest perform very well among all dataset sizes and variations" — [DuckDB H2O.ai post (2023)](https://duckdb.org/2023/04/14/h2oai); [db-benchmark update (Nov 2023)](https://duckdb.org/2023/11/03/db-benchmark-update)
- Version-over-version: current DuckDB completes one full db-benchmark run in under 35 seconds vs nearly 500 seconds for v0.2.7 in June 2021 (~14x, DuckDB's own measurement) — [Benchmarking Ourselves over Time (2024)](https://duckdb.org/2024/06/26/benchmarks-over-time)
- Docs-level measured wins (both first-party): compressed vs uncompressed database ~8x on TPC-H Q1 SF30 (0.55 s vs 4.22 s); ordering a table by its filter column: 0.6 s vs 0.9 s query and 1.3 GB vs 3.3 GB storage — [How to Tune Workloads](https://duckdb.org/docs/2.0/guides/performance/how_to_tune_workloads.html); [Indexing guide](https://duckdb.org/docs/2.0/guides/performance/indexing.html)
- Transport: ~39% smaller payloads and ~150 ms faster large-result fetches moving JSON→Arrow IPC (single practitioner report) — [hotdata-cli PR #103](https://github.com/hotdata-dev/hotdata-cli/pull/103)
- Interaction layer: Mosaic authors claim <100 ms (often 1–10 ms) interaction latency over millions–billions of rows via caching + consolidation + pre-aggregation (author claim, SIGMOD-reviewed) — [Mosaic paper](https://rmarcus.info/dbscholar/papers/hfdc63bfd1fe58702)
- Caution on third-party numbers: a vendor (Stratum) self-benchmarks against DuckDB v1.4.4 on H2O group-by and ClickBench claiming single-threaded wins on 35/46 queries — vendor-run, single-threaded only, treat skeptically — [Stratum benchmarks](https://cljdoc.org/d/org.replikativ/stratum/0.3.84/doc/benchmarks)

### Inferences
- Priority order for maintainers, by evidence strength: (1) track DuckDB releases aggressively (largest measured, zero-effort gains — the pinned crate should be re-bumped each DuckDB minor); (2) physical ordering of tables on the grid's row-order key (zonemap numbers above); (3) Arrow IPC transport + revision-keyed result cache (Hotdata/Rill/Mosaic evidence); (4) incremental recompute scoping (unquantified but structurally obvious); (5) micro-settings (`preserve_insertion_order` on import, `memory_limit`, checkpoint cadence).
- ClickBench itself (benchmark.clickhouse.com) lists DuckDB but the official leaderboard entry wasn't retrieved; it remains the right neutral reference for "DuckDB is competitive on single-node analytics," while H2O.ai db-benchmark (duckdb.org-maintained) is the better-documented one.

### Gaps
- Official ClickBench leaderboard numbers for current DuckDB were not retrieved (search surfaced only DuckDB's H2O.ai posts and a vendor comparison).
- No published end-to-end benchmark of a DuckDB-backed grid/spreadsheet product (Rill publishes no reproducible numbers); any Alephgrid targets will need in-house measurement.
