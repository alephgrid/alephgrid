# Debate: What should Alephgrid build first? — Debater A, Round 1 (Opening)

**Position: Connectivity-first.** Ship the connector broker (S3/Parquet/CSV native, Postgres ATTACH, ADBC warehouse fallback) with structured-field connections and server-held secrets before the agent/MCP gateway and before deep performance work.

## Claims

**C1 — The owner's constraint resolves the report's one open question, and it resolves it in my favor.** The report's conclusion flags exactly one unresolved empirical issue: "whether Alephgrid's first thousand users arrive holding warehouse credentials or CSV files," and says that uncertainty "determines how much of the federation spine gets exercised." The product owner has now answered it: all source types, day one. The single contingency that justified deferring connectivity is gone. A roadmap built under uncertainty should not survive the removal of that uncertainty unchanged.

**C2 — Data access is the adoption funnel; everything else is conversion.** A self-hosted analytical spreadsheet that cannot reach the user's Postgres, S3 bucket, or warehouse in minute one is a CSV viewer. Metabase became the most-deployed self-hosted analytics tool on "connect and ask" — not on query speed or AI. "Connect your data in 2 minutes" is the demo that earns stars, deployments, and the telemetry the report says to instrument at launch. You cannot instrument source mix without connectors to instrument.

**C3 — An agent gateway without diverse connected data is a demo, not a product.** The report's three-tier tool surface presupposes a catalog worth searching: `search_catalog` over schema cards with sample values, ResultHandles over snapshot-pinned temp tables. All of these take connected, materialized sources as input. Sequencing the gateway first means shipping tools that return empty candidate lists. Worse, a gateway built pre-broker must be rebuilt post-broker: opaque connection IDs, READ_ONLY attachment semantics, and materialization artifacts are the primitives the gateway's receipts and handles pin against. Build the primitives first; the gateway then lands once, on stable ground.

**C4 — The materialization tier the report demands IS connectivity work.** The report's deepest synthesis is that agent safety (bounded blast radius), UX (sub-100ms grid), and trust (replayable receipts) "all independently demand the same component — default-on bounded materialization over live-attached sources." That component is Layers 3+4 of its own roadmap. Building the broker + materialization first therefore pre-pays the hardest requirement of both the agent layer and the perf layer. Connectivity-first is not instead of those goals; it is their shared foundation.

**C5 — The build is tractable in 1–2 quarters because it is a thin broker, not a connector fleet.** GlareDB died building N native connectors; the report's verdict is to orchestrate DuckDB's own core-signed extensions (postgres, mysql, sqlite, httpfs/S3, iceberg, delta, excel) plus an ADBC fallback for Snowflake/BigQuery. Tier-1 coverage is ATTACH statements plus credential plumbing — small-team-sized. The risk profile of this work is low precisely because DuckDB Labs runs the treadmill.

**C6 — The security surface is the best-mapped part of the whole roadmap.** Metabase's CVE trail (H2 connection string → pre-auth RCE; JDBC params → file read; Snowflake config → RCE) is not an open research problem — it is a finished checklist: structured per-driver-allowlisted fields, never raw URIs; server-held secrets, clients get opaque IDs; session-scoped in-memory DuckDB secrets; READ_ONLY ATTACH default; SSRF/DNS-pinning controls; community extensions off. We know exactly what to build because others bled for it. Contrast the agent layer, where prompt injection via query results is — per the report — a gap "in every existing suite." Ship the solved-hard thing first; defer the unsolved-hard thing.

## Minimal architecture (next 1–2 quarters)

1. Connection store: server-side rows, structured fields, per-driver option allowlists; secrets encrypted at rest, never in any API response.
2. Tier-1 core extensions only (postgres, mysql, sqlite, httpfs/S3, iceberg, delta, excel), default READ_ONLY ATTACH; writes an explicit audited capability. Community extensions deferred.
3. ADBC driver-manager fallback for Snowflake/BigQuery/Databricks (broker composes source-dialect SQL; no pushdown assumed).
4. Bounded materialization always-on: every remote result lands in revision-stamped local DuckDB storage with freshness records ({snapshot token, fetched_at, watermark}); grid and future agents read only materializations.
5. Guardrails: per-source connection caps, EXPLAIN preflight scan estimates, `postgres_query()`-style raw passthrough escape hatch, SSRF hardening.
6. Minimal identity gate (not full Cedar): OIDC login, admin-only connection management, deny-by-default routes. Enough to hold secrets safely; the embedded-authz engine arrives with the gateway.
7. Source-type telemetry from release one.

## Preempting the counterarguments

- **"The empty AI-spreadsheet niche won't stay empty."** True — but Quadratic's vacated niche is *open, self-hosted, governed*. A rival shipping an agent demo over paste-in CSVs does not occupy it; whoever first lets a team attach their actual Postgres/S3/warehouse under server-held secrets does. And our gateway ships one quarter later on finished primitives rather than first on scaffolding that must be rebuilt.
- **"Connector security is the hardest part to get right."** It is hard the way bridges are hard: load-bearing, with known engineering. The CVE corpus is a spec. Meanwhile a connectors-first release with no agent and READ_ONLY defaults has strictly smaller blast radius than an agent gateway reaching through immature connectors.
- **"Prior research sequenced identity/authz first."** Its stated reason was the gateway: "a gateway without per-principal policy is just a credential with extra steps." Defer the gateway and the full Cedar layer can shrink to item 6 above. Opaque connection IDs + server-held secrets are themselves the first authorization primitive.

## I concede

1. Identity cannot be skipped entirely; the minimal gate (item 6) is non-negotiable and ships inside the broker milestone.
2. The agent gateway is the long-term differentiator (semantic-layer accuracy 98–100% vs 84–90% text-to-SQL); I argue ordering, not importance.
3. Community extensions (gsheets, ODBC) and CDC sidecars rightly stay out of the first 1–2 quarters.
4. If real telemetry later shows a 90%-CSV user base, parts of the federation spine were over-built — the owner's stipulation is what rules this out today.

## I win if / I lose if

- **I win if:** at the end of 2 quarters, a fresh deployment can attach one of each source class (file, S3/Parquet, Postgres, warehouse-via-ADBC) in under ~5 minutes with zero plaintext secrets client-side; materializations with freshness records serve all grid reads; and the subsequently built agent gateway consumes connection IDs, materializations, and receipts unchanged.
- **I lose if:** a competitor captures the open niche in that window on agent experience alone despite weak connectivity (proving the funnel ran through the agent, not the data); or a connector-path security incident occurs that full Cedar-first sequencing would have prevented; or the minimal auth gate proves so inadequate that multi-tenant authz must be retrofitted at greater cost than building it first.

## Round 2 — Rebuttal (A)

**(1) Weakest claims.**

**B-C3** ("all source types is ~70% satisfied by the existing engine") is refuted by the debate's own premise. The owner stipulated users arrive with Postgres, MySQL, and warehouses on day one; B's plan answers them with "import your data first" — which is, verbatim, B's own stated lose condition. B even concedes C3 is untested. A gateway whose `search_catalog` cannot see the user's operational data doesn't demonstrate the moat; it demonstrates its absence. And B-C1's "connectivity is a crowded category" conflates standalone hubs with connectivity *inside* the governed workspace — Airbyte existing doesn't make a spreadsheet without connectors viable.

**C-C4** ("first impression = scroll feel") inverts the funnel: a user cannot feel jank on data they couldn't connect. The local grid already scrolls; the 90-second `docker run` test fails earlier, at "where's my Postgres?" Worse, C-C5 undercuts C's own ask — if keyset paging, InterruptHandle, and the Arrow cache are "days of work each" and the report says they "can land with Layer 2," they are cross-cutting items inside a milestone, not a quarter's headline.

**(2) D's findings mostly help me, and I adopt four.** D4 (no snapshot substrate under receipts) is my strongest new ammunition against B: receipts are the gateway's signature mechanism and their storage substrate doesn't exist yet — shipping the gateway first means shipping degraded receipts, then rearchitecting onto DuckLake under live users. D3 (duckdb-rs FFI pin) hurts C most, since version-currency is C's #1 lever; for me it's a cheap mitigation I adopt (internal trait over the connection + CI against libduckdb-sys HEAD). From D9 I pull **three things into my first milestone**: the persistent job outbox (materialization/refresh jobs are exactly what restarts must not lose — a broker without it silently serves stale data), the explicit cache-vs-`memory_limit` budget, and observability as Layer 0 — per-query spans with a pushdown-succeeded flag are the measurement arm of my own preflight guardrails and the telemetry that adjudicates source mix. D6 confirms no Flight SQL scope creep.

**(3) Updated position — convergence with C.** C-C1/C2 and my C4 describe the same component; the real spine is **broker + materialization as one deliverable**, with the gateway landing as a thin wrapper at the end. Final 2-quarter sequence:

1. Layer 0: observability spans (plan hash, pushdown flag, rows scanned), source-type telemetry, duckdb-rs trait shim + CI.
2. Minimal identity gate: OIDC, admin-only connection management, deny-by-default routes.
3. Connection store: structured per-driver fields, server-held encrypted secrets, session-scoped DuckDB secrets, SSRF/DNS-pinning.
4. Tier-1 READ_ONLY ATTACH: httpfs/S3, postgres, sqlite, iceberg, delta, excel (mysql best-effort).
5. Materialization tier: revision-stamped storage, freshness records, persistent job outbox, revision-keyed Arrow-bytes cache with explicit budget, keyset paging + interrupt cancellation; DuckLake as the *committed* snapshot exit (per D4), not an option.
6. ADBC fallback for Snowflake/BigQuery.
7. Late Q2: read-only agent gateway as a thin wrapper over materializations — schema cards, `search_catalog` (FTS, no VSS), ResultHandles pinning materialized artifacts; receipts carry data_rev staleness now, DuckLake snapshots when the exit lands.

**(4) First cut if capacity halves:** item 6, the ADBC warehouse fallback — warehouse users can export Parquet to S3 in the interim, while nothing else on the list has a workaround.
