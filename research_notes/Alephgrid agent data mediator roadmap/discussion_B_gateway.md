# Debater B — Opening Statement: Agent-Gateway-First

**Position.** Next 1–2 quarters: ship identity-lite (OIDC, space membership, deny-by-default) plus the read-only MCP gateway — three-tier tool surface, schema cards, `search_catalog`, ResultHandles with snapshot-pinned temp tables, answer receipts — on the EXISTING local engine. The broad connector broker (Postgres/MySQL ATTACH, warehouse ADBC) comes after, behind the gateway.

## Claims

**C1 — The window is open now, and only for the gateway.** Quadratic went closed-source in March 2026; the report states flatly there is "no self-hosted, open, governed agent-data mediator in the market." That vacancy is for an *agent-native* spreadsheet, not for another connector hub — Metabase, Trino, and Airbyte already saturate "connects to everything." Spending two quarters on connectors ships us into a crowded category while the empty one closes.

**C2 — The gateway IS the differentiator; connectivity is a commodity.** The report's own conclusion: "Trino connects and DuckDB is fast" — neither is the moat. The moat is that every answer, human- or agent-produced, is a governed, reproducible, inspectable artifact. Receipts, proposal-gated mutations, and the trusted/verified-query flag are the brand. You cannot market a connector list; you can market "98–100% metric-tier accuracy with receipts" (dbt's measured semantic-layer numbers vs 84–90% text-to-SQL).

**C3 — "All source types from day one" is ~70% satisfied by the existing engine.** CSV, Parquet, and S3/httpfs already work. Files and object stores are the bulk of early self-hosted analytical workloads, and Iceberg/Parquet-on-S3 is the report's own case where federation adds least (the source is already versioned columnar storage). What day-one users cannot get anywhere else is a governed agent over that data.

**C4 — The gateway has structural leverage pass-through MCP servers can't copy.** Because Alephgrid *is* a DuckDB engine, a ResultHandle is a snapshot-pinned temp table served by `fetch_rows`/`aggregate`/`sample` — Superglue's "big results belong in a file the agent can query" solved natively. Build this once on local data and every future connector inherits it for free; build connectors first and you re-plumb them all for receipts later.

**C5 — Gateway-first is the lower-regret ordering, by the report's own asymmetry logic.** Connectivity grows incrementally *behind* a stable tool surface: each new ATTACH source is invisible to agents (same tools, same receipts). The reverse is not true — connectors without the gateway are just another BI backend, and retrofitting receipts/handles onto a shipped connector API is a breaking change.

**C6 — The broker is the riskiest component; sequencing it second de-risks it.** Metabase's CVE trail shows the connection string is the most dangerous input a mediator accepts, and GlareDB died building connectors. Shipping the broker in Q1, under deadline, on a project without hardened identity is how you earn your own CVE. Identity → gateway → broker is the report's Layer 1-2-3 order; I am defending the roadmap as written.

**Preempting the obvious attacks.** "Agents without real DBs are demos" — S3/Parquet/CSV are real customer data, and dbt's benchmark itself ran on DuckDB. "A gateway without hardened identity is dangerous" — agreed; that's why identity-lite is *inside* my first deliverable, not deferred. "Perf debt compounds" — Arrow IPC + revision-keyed cache land with Layer 2 per the report; they're cross-cutting, not broker-dependent.

## I concede
1. Postgres/MySQL live-attach demand is real; some warehouse-first prospects will churn in Q1–Q2.
2. A gateway over files-only data narrows the launch story versus "connect your warehouse."
3. The 70%-coverage claim (C3) is untested — the report says no telemetry exists on what users actually arrive with; instrumentation must ship day one.
4. Read-only + receipts without the materialization tier (Layer 4) gives weaker replayability for mutable sources than the end-state design.

## I win if
- Launch telemetry shows early users predominantly bring files/S3, and gateway adoption (agent sessions, receipts generated) outpaces connector requests.
- A connector-first competitor emerges and fails to matter because it lacks the governed-agent story.

## I lose if
- Telemetry shows most arriving users hold Postgres/warehouse credentials and bounce on "import your data first."
- The gateway sits unused because agents without live operational data answer nothing users care about — i.e., the demo critique proves out in metrics, not vibes.

## Round 2 — Rebuttal (B)

**Against A-C3 ("a pre-broker gateway returns empty catalogs").** This is A's weakest claim because it is factually false about the shipping product: CSV, Parquet, and S3/httpfs import already work, so the catalog on day one contains every file and object-store table a user loads — real customer data, the same substrate dbt's 98–100% benchmark ran on. A's stronger point is demand ("all sources arrive day one"), but the owner's stipulation is about what users *bring*, not what must be *live-attached* in Q1: an S3-Parquet user is fully served by the existing engine plus my gateway, and a Postgres user can pg_dump/CSV while the broker hardens. A also wants it both ways: C3 says the gateway must be "rebuilt post-broker," yet A's own win condition is that the later gateway "consumes connection IDs, materializations, and receipts unchanged" — that stability is a property of the gateway's *contract*, which argues for freezing the tool surface early, not late.

**Against C-C4 ("scroll feel is the adoption funnel").** Weakest in C's set because the grid already reads local DuckDB storage — the 10x-slower remote-scan jank C invokes cannot occur before connectors exist, and C concedes (d) that local CSV workloads "may already feel fast enough." C4 therefore defends a quarter of insurance against a failure mode my sequence never exposes. C's C2, by contrast, I largely accept — see below.

**Where D hits me.** D4 lands: an ephemeral temp table behind a ResultHandle cannot honor "replayable receipt," and I was leaning on it. My answer is to tier the receipt contract rather than wait for DuckLake: v1 receipts pin a **persisted, content-hashed result artifact** (the handle's table written to receipt storage with data_rev + plan + policy version) — that gives *verification* (recompute, compare hashes) immediately; *time-travel replay* arrives when DuckLake matures, exactly as D's "committed exit, not an option" demands. For Iceberg/S3 sources, receipts pin durable snapshot IDs today — no substrate needed. D3 (duckdb-rs pin) mildly helps me: a stable gateway contract insulates agents from engine-binding churn. D9 helps outright: observability-as-Layer-0 and the persistent job outbox slot under my plan unchanged, and per-query telemetry is what adjudicates A's demand claim.

**Updated position (convergence).** Two-quarter sequence:
1. Layer 0: observability spans + source-type telemetry + persistent job outbox (D9).
2. Identity-lite: OIDC, deny-by-default, admin-only secrets (A item 6 ≈ my gate — we agree).
3. Read-only MCP gateway on local engine: three tiers, schema cards, search_catalog, ResultHandles, **persisted receipt artifacts** (C's materialization point, absorbed).
4. One connector slice, not the broker: Postgres READ_ONLY ATTACH, structured fields, bounded materialization with freshness records, scan preflight.
5. Arrow IPC + revision-keyed cache (cross-cutting, with 3).
Broad broker (ADBC warehouses, MySQL, community extensions) is Q3.

**If capacity halves:** cut item 4, the Postgres slice — the gateway with receipts on local+S3 data is the product; a connector without it is a commodity.
