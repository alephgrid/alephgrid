# Contributing

Thanks for helping. Rowfinity is meant to stay simple to run and simple to hack on.

## Setup

```bash
(cd web && npm install)
cargo run -p rowfinity-server -- --demo-rows 1000000   # server on :7878
(cd web && npm run dev)                                # UI on :5173 with hot reload
```

## Before opening a PR

```bash
cargo test --workspace
cargo clippy --workspace -- -D warnings
(cd web && npm run build)
```

## Where things live

- **Engine** (`crates/rowfinity-engine`): all data logic. It is a plain library with
  no async code and no HTTP, so it can be embedded in a desktop app or a Python
  binding. Add a test in `src/tests.rs` for any behaviour change.
- **Server** (`crates/rowfinity-server`): transport only. New operations go in the
  `Op` enum in `src/ws.rs` and in `docs/PROTOCOL.md`.
- **Web** (`web/src`): `grid.ts` (drawing and input), `cache.ts` (row fetching),
  `workbook.ts` (screen), `ui.ts` (menus and dialogs).

## Principles

1. Never load a whole table into memory or send it over the wire. Push work down to DuckDB.
2. Every feature is available to everyone. No paid tiers in this repository.
3. Data stays in open formats the user can take elsewhere.
