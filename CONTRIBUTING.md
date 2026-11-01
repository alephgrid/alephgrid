# Contributing

Thanks for helping. Alephgrid is meant to stay simple to run and simple to hack on.

## Setup

```bash
(cd web && npm install)
cargo run -p alephgrid-server -- --demo-rows 1000000   # server on :7878
(cd web && npm run dev)                                # UI on :5173 with hot reload
```

## Before opening a PR

```bash
cargo test --workspace                          # unit, integration, property, crash, protocol drift
cargo clippy --workspace --all-targets -- -D warnings
(cd web && npm run build)                       # type check + bundle
cargo build -p alephgrid-server && (cd web && npm run e2e)   # browser journeys (needs Google Chrome)
```

CI on CircleCI runs only when asked (`tools/ci.sh check`); see [docs/CI.md](docs/CI.md).

If you change a type that crosses the wire, regenerate the TypeScript bindings:
`UPDATE_BINDINGS=1 cargo test -p alephgrid-server --test protocol`.

Every behavior change needs tests proportional to what can go wrong; see
[docs/TEST_STRATEGY.md](docs/TEST_STRATEGY.md) and record them in
[docs/TEST_MATRIX.md](docs/TEST_MATRIX.md). Bug fixes start with a test that fails.

## Where things live

- **Engine** (`crates/alephgrid-core`): all data logic. It is a plain library with
  no async code and no HTTP, so it can be embedded in a desktop app or a Python
  binding. Engine tests live in `src/tests/` (`faults::fail_next` injects failures).
- **Server** (`crates/alephgrid-server`): transport only. New operations go in the
  `Op` enum in `src/ws.rs`, their result type in `protocol::RESULTS`, and
  `docs/PROTOCOL.md`. Integration tests in `tests/` run the real router in-process.
- **Web** (`web/src`): `grid.ts` (drawing and input), `cache.ts` (row fetching),
  `workbook.ts` (screen), `ui.ts` (menus and dialogs).

## Principles

1. Never load a whole table into memory or send it over the wire. Push work down to DuckDB.
2. Every feature is available to everyone. No paid tiers in this repository.
3. Data stays in open formats the user can take elsewhere.
