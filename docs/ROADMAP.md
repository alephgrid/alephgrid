# Roadmap

## v0.1: big tables, local-first (this release)
- [x] DuckDB workbook files; import CSV/TSV/Parquet/JSON; export CSV/Parquet/JSON.
- [x] Instant open, no rehydration; viewport block cache.
- [x] Sort, filter, formula columns, SQL console, selection stats.
- [x] Live multi-user edits over WebSocket; token auth; Docker.

## v0.2: spreadsheet parity (in progress)
- [x] **Sheets**: free-form Excel-formula grids (`=SUM(A1:A10)`, `XLOOKUP`) on [IronCalc](https://github.com/ironcalc/IronCalc), undo/redo, reference shifting on row/column insert.
- [x] XLSX import/export (via IronCalc, as sheets).
- [x] SQL result → sheet, sheet → table.
- [x] Workspace manager: workspaces, starred, recent, search, sort, duplicate, move.
- [ ] Sheet formulas that read tables (`=TSUM("flights", "fare_usd")` or SQL-linked ranges that refresh).
- [ ] Sheet formatting toolbar (number formats, bold, colors, borders), backed by IronCalc styles.
  Why IronCalc rather than HyperFormula: IronCalc is Apache-2.0 and written in Rust, so it embeds in the engine next to DuckDB. HyperFormula is `GPL-3.0-only` (or a paid proprietary license), would relicense the bundled client, and keeps the whole sheet in browser memory.
- [ ] Undo/redo for tables (operation log in the workbook file) and version history.
- [ ] Number and date formats per column with locale control.
- [ ] Find and replace across a table (pushed down to DuckDB).
- [ ] Pivot tables (DuckDB `PIVOT`), group-by UI.
- [ ] Charts (aggregated server-side, rendered client-side).
- [ ] Freeze columns; hide columns; conditional formatting.

## v0.3: connected data
- [ ] Connectors through DuckDB extensions: Postgres, MySQL, SQLite, S3/GCS/R2, Iceberg, Delta, BigQuery, Snowflake. Credentials encrypted at rest and never exposed to collaborators.
- [ ] Linked tables (query in place, no import) and scheduled refresh.
- [ ] Write-back of edited rows to the source database.
- [ ] Python cells (sandboxed subprocess with Arrow handoff).

## v0.4: teams
- [ ] OIDC/SAML login, per-workbook roles (viewer/editor/owner), audit log. Free, no "SSO tax".
- [ ] Pool of read connections per workbook; per-session view schemas.
- [ ] Presence (cursors, selections) and comments.
- [ ] `allowed_directories` sandbox for SQL on shared servers.

## Beyond
- [ ] Desktop app (Tauri) using the same engine, fully offline.
- [ ] In-browser mode on DuckDB-WASM for zero-install sharing of small and medium files.
- [ ] Binary wire format (Arrow IPC) for wide tables.
- [ ] AI assistant with bring-your-own model or key, including local models.
