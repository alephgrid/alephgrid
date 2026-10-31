# Debater C — Opening Statement: Core Engine and Materialization First

Position: ship the performance + materialization tier in the next 1–2 quarters — Arrow IPC transport, (SQL-hash, data_rev) result cache, keyset paging, interrupt-driven cancellation, aggressive DuckDB version discipline, and a bounded materialization layer with freshness records — before broad connectors and before the agent gateway.

**C1. The report's own verdict makes materialization the load-bearing wall.** The verdict is "build federation-first, serve ingest-first": *no interactive surface — grid repaint, formula recompute, agent iteration loop — ever runs against a remote scan.* Read that literally. Day-one users arriving with S3, Postgres, and warehouses can only be served through the materialization tier. A connector without it is a demo that janks on first scroll; an agent gateway without it hands agents unbounded remote scans. Materialization is not one of six layers — it is the component the other five assume exists.

**C2. Every agent ResultHandle IS a materialized artifact.** The report's Layer-2 design — ResultHandles backed by snapshot-pinned temp tables, served by fetch_rows/aggregate/sample — is bounded materialization with a tool API on top. So is the answer receipt: "a receipt pins the materialized artifact (hash + snapshot/revision IDs)." Build the gateway first and you build an ad-hoc, un-revisioned version of this tier, then rearchitect it under live users. Build the tier first and the gateway becomes a thin, correct wrapper. Dependency order is not preference; it is topology.

**C3. The physics is uncontested — both debate sides conceded it.** Federation-first conceded "a spreadsheet grid recomputing interactively over a remote-only source is not viable." DuckDB-native storage is ~10x faster than live Postgres scans; pushdown fails silently (the documented 10-second full-scan regression vs milliseconds); MySQL pushdown is experimental and off. A spreadsheet is the most latency-intolerant analytical UI there is: every scroll is a query.

**C4. First impression = scroll feel, and that is the OSS adoption funnel.** Both rival plans court the same self-hosters who will `docker run`, load data, and drag the scrollbar within 90 seconds. Quadratic's exit left the niche open, but "open-source spreadsheet that stutters" closes it. Nobody files an issue about jank — they close the tab. Connectors and MCP tools get evaluated only by users the grid didn't lose.

**C5. The engine work is cheap, measured, and compounding; retrofit is not.** The payoffs are quantified: DuckDB version currency (500s → 35s over releases — re-bumping the crate beats any app tuning), zonemap-friendly clustering (~1.5x faster, 2.5x smaller), Arrow IPC (~39% smaller payloads, ~150ms faster large fetches), revision-keyed caching with no TTLs because data_rev already exists in functions.rs. Keyset paging and InterruptHandle cancellation are days of work each. Conversely, JSON transport, OFFSET paging, and uncached reads calcify into protocol surface, client code, and agent tool contracts — perf debt is the hardest debt to retrofit. The repo's own checklist already gates the S3 release on "measured performance"; this plan simply executes the gate.

**C6. One quarter of this tier de-risks both rivals' quarters.** Connectors-first still needs scan guardrails, freshness records, and bounded pulls (the report mandates all three ship together). Gateway-first still needs handles, receipts, and caching. My plan is the intersection of their prerequisites — the lowest-regret ordering under uncertainty about whether users arrive with warehouses or CSVs, which the report flags as the one unresolved empirical question.

**I concede:** (a) perf is invisible in marketing — nobody stars a cache, and a launch needs a headline feature; (b) with zero connectors, day-one S3/Postgres users bounce before ever feeling the fast grid; (c) the competitive window post-Quadratic is time-boxed, and a quarter of plumbing spends calendar; (d) local CSV workloads may already feel "fast enough," so some of this tier's value is latent until remote sources land.

**I win if:** materialization/caching code is demonstrably load-bearing under both the first connector and the first agent tool with no rearchitecture; grid interaction stays sub-100ms on remote-backed tables; early churn correlates with responsiveness.

**I lose if:** telemetry shows users bounce on "can't connect my warehouse" rather than feel; a thin connector + naive serving retains users fine for two quarters; or the gateway built on ad-hoc temp tables never actually needs the rework I predict.

## Round 2 — Rebuttal (C)

**Against A4 ("the materialization tier IS connectivity work").** This is a concession dressed as a collapse. Most of my tier — Arrow IPC, the (SQL-hash, data_rev) cache, keyset paging, cancellation, revision stamping — runs entirely on the local engine and is exercised by the CSV/S3 data users already have; the broker is merely one *producer* into it. The tell is A's own sequencing: materialization is item 4 of 7, behind the connection store and ADBC. Under deadline, the Metabase-CVE checklist eats the quarter and "bounded materialization always-on" degrades to a LIMIT clause — exactly the demo-that-janks I warned about. If A agrees materialization must land first *within* their milestone, A has adopted my ordering and we're arguing labels.

**Against B3 ("~70% satisfied by the existing engine").** B concedes this claim is untested in the same document — and the owner's stipulation directly contradicts it: users arrive with Postgres and warehouses *by premise*, so the gateway's catalog is thin for precisely the users B needs. Worse, B4 admits ResultHandles are snapshot-pinned temp tables — my artifacts — while B's own concession 4 admits receipts without Layer 4 are "weaker replayability." A moat made of receipts you can't replay is marketing, not governance.

**On D.** D3 genuinely gates my C5: if duckdb-rs lags a DuckDB minor, the version-currency lever stalls. I absorb D's mitigation as a Q1 work item — a thin internal connection trait plus CI building against libduckdb-sys HEAD — which is *engine-tier work only my plan prioritizes*; the lever survives with insurance, and D names the switch trigger (90-day lag, twice). The rest of D strengthens me: D4's receipt gap argues against B shipping receipts early (interim answer: handle = materialized artifact + data_rev gives bounded replayability now, DuckLake later); D9's cache/memory_limit shared budget, persistent job outbox, and per-query spans (plan hash, pushdown-succeeded, rows scanned) are all engine-tier deliverables — observability-as-Layer-0 is literally instrumentation of my query path.

**Updated position (convergence).** Final 2-quarter sequence:
1. Layer 0: per-query spans + source-type telemetry (weeks, ships first — D9).
2. Engine core: Arrow IPC, revision-keyed cache with explicit memory budget, keyset paging, InterruptHandle cancellation, duckdb-rs insulation trait + HEAD CI.
3. Minimal identity gate (A's item 6: OIDC, deny-by-default, admin-only connections).
4. Narrow connectivity slice: hardened S3/httpfs + Postgres ATTACH only — structured fields, server-held secrets, READ_ONLY — one OLTP source to exercise materialization end-to-end; full Tier-1 fleet and ADBC deferred.
5. Bounded materialization always-on: freshness records, watermark refresh, persistent job outbox.
6. Late Q2: read-only gateway thin-slice (search_catalog, ResultHandles, receipts pinning materialized artifacts).

**If capacity halves:** cut item 6, the gateway slice — it is the only item whose absence weakens nothing upstream of it.
