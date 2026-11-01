---
title: Agents over MCP
description: Read-only agent connections per space or workbook, with schema cards, read-only SQL, content-hashed receipts and per-connection activity.
---

Alephgrid lets an AI agent read workbooks through the
[Model Context Protocol](https://modelcontextprotocol.io). Agents use the same services
as people, under the same authorization — there is no separate execution path. In this
release agents only read; the one change they can ask for, saving a query, waits for a
person to approve it.

## Connect an agent

1. In the hub, open a space's menu (right-click it, or **⋯**) and choose
   **Agent connections…** (space owners). Name the connection, choose whether it may
   read every workbook of the space or only some, and how many days it lasts (1–366).
2. Copy the token (`agc_…`). It is shown once.
3. Configure the client with one of the setups shown:

   **Streamable HTTP** (clients that speak it, such as Claude Code):

   ```sh
   claude mcp add --transport http alephgrid http://127.0.0.1:7878/mcp \
     --header "Authorization: Bearer agc_…"
   ```

   **stdio** (Claude Desktop and other clients that start a program): the `alephgrid`
   program forwards stdio to a running server.

   ```json
   {
     "mcpServers": {
       "alephgrid": {
         "command": "alephgrid",
         "args": ["mcp", "--url", "http://127.0.0.1:7878"],
         "env": { "ALEPHGRID_AGENT_TOKEN": "agc_…" }
       }
     }
   }
   ```

Revoking a connection ends its access at once, results included. **Activity** lists
every tool call: what the agent asked for, how many rows it got, the result handle, or
the error.

:::caution
What an agent reads goes to its model provider under that provider's terms. Give a
connection only the workbooks it needs.
:::

## Tools

| Tool | What it does |
|---|---|
| `list_workbooks` | Workbooks the connection may read, with their tables and sheets |
| `search_catalog` | Tables, columns, sheets, saved queries and analyses matching words |
| `describe_workbook` | Schema cards: columns, types, row counts and five sample values per column; sheets; saved queries; analyses |
| `preview_table` | Up to 500 rows of a table from an offset, with chosen columns, filters and sort |
| `read_sheet` | Shown values of a sheet range (at most 5,000 cells) and its formulas |
| `run_query` | Read-only DuckDB SQL (up to 10,000 rows kept behind the handle) |
| `explain_query` | The engine's plan for a read-only query, without running it |
| `run_saved_query`, `run_analysis` | A saved query or analysis, by name or id |
| `fetch_result` | More rows of a result, by handle |
| `verify_result` | Recompute a result and compare it with its receipt |
| `job_status`, `cancel_job` | Requests still running after `wait_seconds` (default 20) |
| `propose_saved_query`, `proposal_status` | Ask to save a query; saved only when a person approves it |

Replies show the first 100 rows with the row count, a truncation flag and a **handle**
for fetching more. The server also serves two resources: `alephgrid://guide` and
`alephgrid://workbooks/{id}` (the schema cards).

## Receipts

Every result an agent sees is stored for 24 hours with a receipt:

- the SHA-256 of the result bytes and of the request;
- the workbook revision and source tables;
- the engine versions (Alephgrid, DuckDB);
- a digest of the connection's access (scope, SQL setting, expiry);
- the row count, the truncation flag, and the issue and expiry times.

Fetching a page of a result needs the same connection with the same access as when the
result was issued, before it expires, a workbook that is still readable, and stored
bytes that match the hash. `verify_result` recomputes the result and reports **stale**
when the workbook has changed since the result was issued.

## Limits and safety

- Connections read as a viewer. The only change an agent can ask for is a saved query,
  written when a person with edit rights approves it (Agent connections → Proposals), as
  that person's change noted as proposed by the agent.
- Changing statements, file access and network access in SQL are refused by the server's
  SQL policy. `--disable-sql` turns `run_query` off.
- Agent tokens work only at `/mcp`; session cookies and service tokens are refused
  there. Cell contents and query results are data: nothing in a cell can change what a
  connection may do.
- At most 4 requests run at once per connection. Jobs live in the server process and are
  forgotten after an hour or on restart; stored results and their handles survive.
- Time-zone functions (`AT TIME ZONE` and other ICU functions) are not available, since
  DuckDB extensions cannot be loaded; work in UTC with `epoch_us`. Catalog search
  matches words in names and SQL, without full-text search, for the same reason.

The protocol details — `POST /mcp` takes one JSON-RPC message per request, supported
protocol versions, session ids — are in the repository's
[MCP guide](https://github.com/alephgrid/alephgrid/blob/main/docs/MCP.md). Roles and
service tokens for non-agent integrations are on the [teams](../teams/) page.

## Discovery

Machine-readable descriptions of this MCP interface, for agent tooling and registries:

| URL | What |
|---|---|
| `https://alephgrid.dev/.well-known/mcp/server.json` | MCP Registry [`server.json`](https://static.modelcontextprotocol.io/schemas/2025-07-09/server.schema.json) manifest (also served at `/.well-known/mcp.json`) |
| `https://alephgrid.dev/llms.txt` | [llms.txt](https://llmstxt.org/) index of this documentation |
| `https://alephgrid.dev/llms-full.txt` | The complete documentation as one file |

The manifest's remote URL is a template — Alephgrid is self-hosted, so the endpoint is
`<your server origin>/mcp` with your own `agc_…` token; there is no hosted endpoint at
`alephgrid.dev`.
