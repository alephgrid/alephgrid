# Debate verdict and synthesized sequence

Four debaters: A (connectivity-first), B (agent-gateway-first), C (engine/materialization-first), D (stack stress-test). Owner stipulation: users arrive with CSVs, Parquet, object stores, operational DBs, and warehouses from day one. After openings and cross-rebuttals, the positions converged on a shared spine; this verdict resolves the two disagreements that remained.

## What all three build-order debaters converged on (unanimous)

1. **Layer 0 ships first**: per-query observability spans (plan hash, pushdown-succeeded flag, rows scanned), source-type telemetry, persistent job outbox (meta-table, no broker), duckdb-rs insulation trait + CI against libduckdb-sys HEAD (D3 mitigation).
2. **Minimal identity gate, not full Cedar**: OIDC login, deny-by-default routes, admin-only connection management. Full embedded authz arrives with the multi-principal gateway.
3. **Engine cross-cutting work is cheap and lands early**: Arrow IPC transport, (SQL-hash, data_rev) result cache with an explicit memory budget separate from DuckDB `memory_limit`, keyset paging, InterruptHandle cancellation.
4. **Bounded materialization always-on** with revision-stamped storage, freshness records ({snapshot token, fetched_at, watermark}), watermark incremental refresh.
5. **DuckLake is a committed exit, not an option** (D4): receipts cannot time-travel-replay without a snapshot substrate; until then receipts pin persisted content-hashed result artifacts + data_rev (B's tiering), and Iceberg/Delta sources pin durable snapshot IDs natively.
6. **Stack holds** (D): embedded DuckDB over chDB/ClickHouse/DataFusion/Polars (workload mismatch; switch triggers documented), DuckDB-file-per-workbook now, Arrow IPC over HTTP/2 + zstd (no Flight SQL — browsers can't gRPC; Quack on watch, beta in v1.5.3, re-evaluate single-writer model at its v2.0 GA), server-side compute (no duckdb-wasm hybrid), in-process cache, FTS-not-VSS for catalog search.

## Resolved disagreement 1: gateway position (B vs A+C)

**Verdict: late-Q2 thin gateway, but with B's receipt tiering adopted.** Two of three ordered the gateway last, and the dependency argument is decisive: ResultHandles and receipts pin materialized artifacts, so the materialization tier must exist first or the gateway is rearchitected under live users (C-C2, A's adoption of D4). B's counter — that the niche window is open now — is answered by the thin-slice compromise: the gateway lands within the same two quarters, as a wrapper, not a rearchitecture risk. B's lasting contributions: freeze the tool-surface contract early (stability is a property of the contract, not the sequencing), and tiered receipts (content-hash verification now, DuckLake replay later) which defused A's "degraded receipts" attack.

## Resolved disagreement 2: connector breadth (A vs B+C)

**Verdict: narrow slice in Q1-Q2 — S3/httpfs + Postgres (+ sqlite/excel where near-free), full Tier-1 fleet + ADBC warehouses in Q3.** The owner's "all sources day one" sets the destination, not the Q1 scope: two of three cut breadth first, and A itself cut ADBC when capacity halved. Warehouse users bridge via Parquet-export-to-S3 (A's own interim). The Metabase-CVE checklist (structured fields, server-held secrets, session-scoped DuckDB secrets, SSRF/DNS-pinning, READ_ONLY ATTACH default) applies in full to the narrow slice — hardening depth over driver count.

## Final synthesized 2-quarter sequence

1. **Layer 0**: observability spans, source-type telemetry, persistent job outbox, duckdb-rs shim + HEAD CI.
2. **Identity gate (minimal)**: OIDC, deny-by-default, admin-only connections.
3. **Engine core**: Arrow IPC pages, revision-keyed Arrow-bytes cache with explicit budget, keyset paging, interrupt cancellation, DuckDB version bump.
4. **Connection store + narrow ATTACH**: structured per-driver allowlisted fields, encrypted server-held secrets, session-scoped in-memory DuckDB secrets, SSRF hardening; READ_ONLY ATTACH for S3/httpfs + Postgres; EXPLAIN preflight scan guardrails; raw source-dialect passthrough escape hatch.
5. **Materialization tier always-on**: revision-stamped local storage, freshness records, watermark refresh via outbox jobs.
6. **Thin read-only agent gateway (late Q2)**: frozen tool contract — schema cards with sample values, `search_catalog` (FTS), ResultHandles backed by persisted content-hashed artifacts, tiered answer receipts, recovery-hint errors.

**Q3+**: ADBC warehouse fallback (Snowflake/BigQuery/Databricks), MySQL + rest of Tier-1, DuckLake snapshot substrate, proposal-gated mutations, metric registry, public eval suite.

**If capacity halves**: cut item 6 (C's cut — nothing upstream depends on it); next cut is Postgres from item 4 (B's cut), leaving S3-only.

## Kill criteria (from the debaters' lose conditions)

- If telemetry shows users bounce on "can't connect warehouse" at high rates → pull ADBC forward from Q3.
- If a connector-less OSS rival occupies the agent-spreadsheet niche on experience alone → pull gateway forward.
- If duckdb-rs lags a DuckDB minor by >90 days twice → activate the binding swap plan.
- Quack v2.0 GA + a real multi-writer need → revisit the workbook concurrency model.
