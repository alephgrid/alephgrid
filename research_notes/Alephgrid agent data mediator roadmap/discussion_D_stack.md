# Debater D — Stress-Testing the Stack Itself

My brief: not *what to build first*, but *whether the pillars hold*. Verdict up front: the stack survives, but with two real fragilities the report never prices in — the duckdb-rs FFI pin and the absence of any snapshot substrate under "answer receipts."

## 1. Computation engine

**Steelman the alternatives.** DataFusion is pure Rust: no FFI, no `bundled` C++ build, memory-safe to the planner, and extensible enough that GlareDB built federation on it. chDB/ClickHouse own high-concurrency serving: thousands of concurrent point queries, streaming inserts, MergeTree background merges — and ClickBench is literally their benchmark. Polars gives the fastest Rust-native dataframe ops with zero embedding ceremony.

**D1.** None of these workloads is Alephgrid's. The product is few-users-per-workbook, bursty scan-heavy queries, one writer. ClickHouse wins at >100s of concurrent small queries or sustained streaming inserts — a spreadsheet has neither; its "streaming" is a user typing. ClickHouse-server also breaks the single-binary constraint outright, and chDB's embedded mode gives up the concurrent-reader MVCC story DuckDB documents.

**D2.** DataFusion loses on exactly what the report leans on: no extension ecosystem (postgres/iceberg/delta ATTACH, FTS, VSS), no MotherDuck-style hybrid precedent, no secrets manager. GlareDB died maintaining connectors *on* DataFusion — adopting it would re-run that movie. Concession: DataFusion-the-substrate was not GlareDB's failure cause; the connector treadmill was.

**D3.** The unpriced risk is **duckdb-rs itself**: a community FFI binding (not DuckDB-team-core), pinned at the odd `1.20000.0` scheme, where `bundled` builds complicate loading community extensions and every engine upgrade — the report's #1 performance lever! — is gated on a third-party crate re-releasing. Mitigation to demand: a thin internal trait over the connection so the binding is swappable, and CI that builds against libduckdb-sys HEAD.

**Verdict: DuckDB stands.** Switch triggers: duckdb-rs lags a DuckDB minor by >90 days twice; concurrent agent query load exceeds ~50 QPS sustained per host (ClickHouse territory); or DuckDB's license/governance changes (MIT today — fine).

## 2. Data/storage layer

**D4.** DuckDB-file-per-workbook is correct for v1 but it **cannot honor the receipt contract**: a receipt pins "hash + snapshot/revision IDs," yet a native DuckDB file has no time travel — old versions are gone at checkpoint. `data_rev` proves staleness; it cannot *replay*. The report defers DuckLake to "once maturity is confirmed" while making receipts a signature mechanism. That's a load-bearing IOU.

**D5.** Backup is similarly hand-waved: copying a live DuckDB file under an active writer is unsafe; the real story is CHECKPOINT-then-copy or `EXPORT DATABASE`, serialized on the writer queue. Iceberg-from-day-one is overkill (catalog service, small-file compaction, write amplification for cell edits — terrible fit for spreadsheet mutation patterns). Concession: per-file workbooks give trivially scoped sharing, deletion, and export — genuinely good properties.

**Verdict: DuckDB file now; DuckLake is the committed exit, not an option.** Switch triggers: first receipt-replay requirement ships; multi-process access needed; workbook files >~10GB where full-file backup amplification hurts.

## 3. Transport

**D6.** Arrow IPC over HTTP (zstd via content-encoding, HTTP/2 for multiplexed page fetches) is right for bulk data; keep JSON-RPC for MCP/control plane — two planes, no conflict. **Flight SQL is wrong here**: browsers can't speak gRPC natively, so it drags in a grpc-web proxy, violating single-binary for zero browser benefit. It matters only later as an *exposed* server protocol for BI tools.

**D7.** Quack, verified: released as **beta in DuckDB v1.5.3 (May 12, 2026), shipped via the core repo, explicitly subject to change, targeting stable at v2.0** ([quack page](https://duckdb.org/quack/), [docs](https://duckdb.org/docs/current/quack/overview.html), [repo](https://github.com/duckdb/duckdb-quack)). Vendor-claimed ~few-thousand writes/sec on 8 CPUs. Do not adopt pre-2.0 — but notice it erodes a core assumption: one-writer-per-file-per-process stops being a law of physics. Re-evaluate the whole workbook concurrency model when Quack goes stable.

**Verdict: HTTP(/2) + Arrow IPC + zstd; WebSockets only for revision-tick notifications; Quack on watch.** Switch trigger: Quack GA in v2.0 plus a real multi-writer requirement.

## 4. Computation placement

**D8.** All-in-server is correct *because the formula engine must have exactly one home*. duckdb-wasm hybrids (Perspective/Mosaic) are seductive, but a ~35MB WASM payload, a second engine version to keep semantics-identical with the server's SQL compilation, and Perspective's own "medium-sized datasets" caveat make client replication the wrong default at 100M rows. The cheap 80% is Mosaic-style *caching and prefetch of server results*, not client compute. Concession: client-side slicing of small pinned aggregates (pivot results <1M cells) is a legitimate later optimization — as an optimization of rendering, never of formula evaluation.

**Verdict: server-side; revisit wasm only for read-only embeds/offline viewers.** Switch trigger: telemetry shows cached-page interactions still RTT-bound for remote users.

## 5. What the report missed

**D9.** In-process cache is right (single binary kills Redis), but the Arrow-bytes cache and DuckDB's `memory_limit` compete for the same RAM with no shared accounting — set an explicit cache budget or OOM debugging becomes archaeology. Job queue: an in-memory queue loses materialization/refresh jobs on restart; persist jobs in a meta table (a SQLite/DuckDB outbox), no external broker. Agent catalog search: DuckDB's built-in FTS is fine, but the VSS extension's persisted HNSW indexes carry documented experimental/unsafe-persistence caveats — FTS first, embeddings later. Observability is absent from all six layers: per-query spans (plan hash, pushdown-succeeded flag, rows scanned) are the telemetry the report says adjudicates its own open question — it's Layer 0, not an afterthought.

**Overall: the stack is sound; the report's real exposure is operational (FFI currency, snapshots, backup, observability), not architectural.**
