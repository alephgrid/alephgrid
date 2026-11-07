# Alephgrid documentation

The website, [alephgrid.dev](https://alephgrid.dev/docs/getting-started/), has the guided docs.
These pages are the detailed reference and the contributor documentation.

## Using Alephgrid

| Page | What it covers |
|---|---|
| [SPREADSHEET_FEATURES.md](SPREADSHEET_FEATURES.md) | Everything a spreadsheet user can do, and what is partial, planned or not planned |
| [KEYBOARD.md](KEYBOARD.md) | Keyboard shortcuts, browser conflicts and per-browser notes |
| [FORMULA_CATALOG.md](FORMULA_CATALOG.md) | Built-in sheet functions, their test evidence, and the formula editor |
| [FORMULA_GAPS.md](FORMULA_GAPS.md) | Known formula differences from Excel, with reproducible inputs |
| [TABLE_FORMULAS.md](TABLE_FORMULAS.md) | Spreadsheet formulas over whole tables (`XLOOKUP`, `SUMIFS`, …): the full contract |
| [FUNCTIONS.md](FUNCTIONS.md) | Your own functions in formulas or SQL: tests, versions, pins |
| [PYTHON_FUNCTIONS.md](PYTHON_FUNCTIONS.md) | Python functions and their sandbox |
| [ANALYSIS.md](ANALYSIS.md) | Summaries, pivots, joins, charts, dashboards and clean-up without SQL |
| [COLLABORATION.md](COLLABORATION.md) | Editing together: conflicts, reconnects, undo and history |
| [MCP.md](MCP.md) | Connecting AI agents to workbooks over the Model Context Protocol |

## Running a server

| Page | What it covers |
|---|---|
| [OPERATIONS.md](OPERATIONS.md) | Installing, backups, restore, upgrades, runbooks, limits |
| [TEAMS.md](TEAMS.md) | Organizations, spaces, roles, sign-in, single sign-on, libraries, quotas, audit |
| [COMPUTE.md](COMPUTE.md) | How workbooks get compute: profiles, queueing, resizing, idle shutdown |
| [SECURITY.md](SECURITY.md) | The security model: untrusted files, browser protections, bounded work |
| [DURABILITY.md](DURABILITY.md) | When a change is durable, what survives which failure, isolation classes |

## Contributing

| Page | What it covers |
|---|---|
| [../CONTRIBUTING.md](../CONTRIBUTING.md) | Setup, checks before a pull request, where code lives |
| [ARCHITECTURE.md](ARCHITECTURE.md) | How the engine, server and client fit together, and the design of planned work |
| [PROTOCOL.md](PROTOCOL.md) | The WebSocket operations and HTTP routes |
| [DESIGN.md](DESIGN.md) | Interface design: principles, tokens, layout, states, brand assets |
| [TEST_STRATEGY.md](TEST_STRATEGY.md) | How changes are tested: layers, oracles, fault injection, tiers |
| [TEST_MATRIX.md](TEST_MATRIX.md) | Historical per-task test evidence for phases 1–8 and F |
| [ACCESSIBILITY_TESTING.md](ACCESSIBILITY_TESTING.md) | Manual screen-reader and keyboard-layout checks |
| [CI.md](CI.md) | CircleCI, run on demand: setup and how to start a run |
| [RELEASING.md](RELEASING.md) | Building and publishing a release: binaries, Docker image, npm, PyPI |
| [UPSTREAM.md](UPSTREAM.md) | IronCalc defects, the local patch set and proposals for upstream |
| [samples/](samples/README.md) | A reproducible sample workbook used by the tests |

## Planning

| Page | What it covers |
|---|---|
| [ROADMAP.md](ROADMAP.md) | What comes after v0.1.0 |
| [IMPLEMENTATION_CHECKLIST.md](IMPLEMENTATION_CHECKLIST.md) | Every task, its tests and its status |
| [LAUNCH.md](LAUNCH.md) | Status of the v0.1.0 launch: what is left, owner actions, release day |
