# Agents and MCP

Alephgrid lets an AI agent read workbooks through the [Model Context Protocol](https://modelcontextprotocol.io).
Agents use the same services as people, under the same authorization; there is no separate
execution path. In this release agents only read; the one change they can ask for, saving a query, waits for a person to approve it.

## Connect an agent

1. In the hub, open a space's menu (right-click it, or **⋯**) and choose **Agent connections…**
   (space owners). Name the connection, choose whether it may read every workbook of the space or
   only some, and how many days it lasts (1–366).
2. Copy the token (`agc_…`). It is shown once.
3. Configure the client with one of the setups shown:

   **Streamable HTTP** (clients that speak it, such as Claude Code):

   ```sh
   claude mcp add --transport http alephgrid http://127.0.0.1:7878/mcp \
     --header "Authorization: Bearer agc_…"
   ```

   **stdio** (Claude Desktop and other clients that start a program): the `alephgrid` program
   forwards stdio to a running server.

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

Revoking a connection ends its access at once, results included. **Activity** lists every tool
call: what the agent asked for, how many rows it got, the result handle, or the error.

What an agent reads goes to its model provider under that provider's terms. Give a connection
only the workbooks it needs.

## Tools

| Tool | What it does |
| --- | --- |
| `list_workbooks` | Workbooks the connection may read, with their tables and sheets |
| `search_catalog` | Tables, columns, sheets, saved queries and analyses matching words |
| `describe_workbook` | Schema cards: columns, types, row counts and five sample values per column; sheets; saved queries; analyses |
| `preview_table` | Up to 500 rows of a table from an offset, with chosen columns, filters and sort |
| `read_sheet` | Shown values of a sheet range (at most 5,000 cells) and its formulas |
| `run_query` | Read-only DuckDB SQL (up to 10,000 rows kept behind the handle) |
| `run_saved_query`, `run_analysis` | A saved query or analysis, by name or id |
| `fetch_result` | More rows of a result, by handle |
| `verify_result` | Recompute a result and compare it with its receipt |
| `job_status`, `cancel_job` | Requests still running after `wait_seconds` (default 20) |
| `propose_saved_query`, `proposal_status` | Ask to save a query; it is saved only when a person approves it (Agent connections → Proposals), as their change noted as proposed by the agent. `operation_id` makes retries safe |

Resources: `alephgrid://guide` and `alephgrid://workbooks/{id}` (the schema cards).

Replies show the first 100 rows with the row count, a truncation flag and a **handle**.

## Receipts

Every result an agent sees is stored for 24 hours with a receipt:

- the SHA-256 of the result bytes and of the request;
- the workbook revision and source tables;
- the engine versions (Alephgrid, DuckDB);
- a digest of the connection's access (scope, SQL setting, expiry);
- the row count, the truncation flag, and the issue and expiry times.

Fetching a page needs all of the following:

- the same connection, before the result expires;
- the same access as when the result was issued;
- the workbook still readable;
- stored bytes that match the hash.

`verify_result` recomputes the result. It reports **stale** when the workbook has changed since
the result was issued.

## Limits and safety

- Connections read as a viewer. The only change an agent can ask for is a saved query, written when a person with edit rights approves it. Changing statements, file access and network access in SQL
  are refused by the server's SQL policy. `--disable-sql` turns `run_query` off.
- Agent tokens work only at `/mcp`. Session cookies and service tokens are refused there. Pages
  from other sites cannot post to it.
- Cell contents and query results are data. The server's instructions tell agents so, and
  nothing in a cell can change what a connection may do.
- At most 4 requests run at once per connection. Jobs are forgotten after an hour.
  Cancelling a job discards its result; a query that is already running finishes within the
  server's time limit.
- Catalog search matches words in names and SQL. It does not use DuckDB's full-text search
  extension, because extension installs are refused (see SECURITY.md).

## Protocol

`POST /mcp` takes one JSON-RPC message per request (no batches) and replies with JSON.

- Protocol versions: 2025-06-18, 2025-03-26 and 2024-11-05. An unknown `MCP-Protocol-Version`
  header is refused with 400.
- `initialize` returns an `Mcp-Session-Id`.
- `GET /mcp` returns 405: the server sends no messages of its own.
