# Launch v0.1.0

What remains before the first public release, what the repository must contain, how CI and
the release are run, and the announcement. Task IDs refer to
[IMPLEMENTATION_CHECKLIST.md](IMPLEMENTATION_CHECKLIST.md); this page is the summary, the
checklist is the record. State as of 1 November 2026.

## 1. Where things stand

Done: the security remediation (L.01), cleanup of existing features (L.02), sheet completeness
with formatting, charts, rules and comments (L.03), team screens (L.05), and most of MCP for
local workbooks (L.04). The launch UI work (L.05b) is mostly merged: fill handle, functions
side panel, chart previews, disabled-command reasons, the Hub's new-workbook flow and the
spreadsheet-first journey test.

## 2. Product work left before release

### Bugs found while making the README screenshots (1 November)

| Bug | Where | Seen as |
|---|---|---|
| A table formula with a mixed reference such as `B$1` is refused (`$A2` and `$B$1` work) | table formula parser | `expected a cell such as A1 or $B$2 after the sheet name` when typing `=SUMIFS(orders[amount],orders[month],B$1)` |
| Sheet charts read the shown text, so cells formatted as currency or percent (`$1,061,840`, `24.9%`) chart as empty | `web/src/sheetcharts.ts` | A column chart over currency cells shows an empty 0–1 axis |
| The Analyze result chart is taller than the results area: bars are cut and the category labels hidden | Analyze results | Summary by region shows only the tops of the bars |
| Analyze's hint "Refer to values as [@…]" keeps the old value name after the value column changes | Analyze pane | Shows `[@sum_order_id]` after choosing `amount` |
| The Hub footer squeezes "v0.1.0 · data stays on this server" into a narrow column beside Administration | Hub sidebar | Text wraps one or two words per line |
| Mod+G does not open Go to while the grid has focus (the toolbar button works) | command registry | Seen while writing the journey test |

### Launch UI (L.05b, U.14 gate)

* U.11a: shared component and state rules (loading, empty, error, denied).
* U.04a: searchable sheet list; check every toolbar control works or says why not.
* U.03b: Hub loading placeholder; a test for the no-access screen.
* U.06b–e, function editor: syntax highlighting and line errors; typed blank, empty and error
  test inputs (needs a server change); Cancel for a running test and a resizable results
  strip; what an upgrade affects, shown before Activate.
* F.04a–d, clipboard: filtered rows; formatted paste from Excel and Google Sheets; date and
  weekday series by name; copy between sheets.
* U.T05–U.T07 usability checks, including a real screen-reader pass, then the U.14 gate.
* The "fill down and right" browser test that fails only under heavy machine load.

### MCP (L.04)

* M.03: check with real MCP clients (MCP Inspector and at least one chat client).
* M.T02: evaluate real analytical questions with verifiable answers.

### Packaging and operations (L.06, phase 9)

* 9.02 / 9.03: Docker named volumes and bind mounts; localhost and team Compose examples with
  HTTPS, health checks and resource limits.
* 9.04: backup, restore on a clean host, upgrade and rollback, verified end to end.
* 9.05 / 9.06: release-build benchmarks over varied data shapes and concurrent users; publish
  raw results and supported capacity.
* 9.08: third-party license review, a `NOTICE` file and dependency notices.
* 9.10 / 9.14 / 9.16: CI on CircleCI, on demand (section 4); the release pipeline.
* 9.11: release binaries, container images, checksums, signatures and an SBOM (section 5).
* 9.13 / 9.15: structured logs and metrics, health versus readiness, runbooks.
* 9.T01–9.T03 and the 9.12 gate; then L.07 launch review and the L.08 gate on a clean machine.

## 3. Files the repository needs

| File | State | Left to do |
|---|---|---|
| `README.md` | Rewritten 1 Nov with current screenshots | Add CI and release badges and install-from-release steps once they exist |
| `LICENSE` (Apache-2.0) | Present | — |
| `NOTICE` and third-party notices | Missing | Generate from `Cargo.lock` and `web/package-lock.json` (9.08) |
| `CHANGELOG.md` | Present, "Unreleased (launch v1)" | Turn into the 0.1.0 entry on release day |
| `CONTRIBUTING.md` | Present, links CI.md | — |
| `CODE_OF_CONDUCT.md` | Added 1 Nov (Contributor Covenant 2.1) | **Owner:** set the conduct contact address |
| `SECURITY.md` | Added 1 Nov: private reporting, response times, supported versions | **Owner:** turn on private vulnerability reporting in repository settings |
| `.github/ISSUE_TEMPLATE/` | Bug, feature, and a config that routes security reports and questions | — |
| `.github/pull_request_template.md` | Present | — |
| `.circleci/config.yml` | On-demand check, full and release runs | — |
| `docs/README.md` | Added 1 Nov: index of every page | — |
| `docs/OPERATIONS.md` | Present | Complete with 9.02–9.04 results |
| `docs/SPREADSHEET_FEATURES.md` | Present | Keep in step with each merged feature |
| Benchmarks page | Missing | Publish with 9.05 |
| `assets/screenshots/` | Regenerated 1 Nov by `web/scripts/readme-screenshots.mjs` | Rerun before release; add an Analyze screenshot once its chart fits |
| Social preview image (1280×640) | Missing | Logo plus the hero screenshot; upload in repository settings |
| Brand reuse terms | Open (U.13) | **Owner:** decide how others may use the logo |

Not for the public repository: competitor research, `reports/` and `research_notes/` should be
reviewed before the repository is made public (9.07, 9.08).

## 4. CI on CircleCI, on demand (9.16 decision, 1 November)

The owner chose CircleCI, run **only when asked**: pushes and pull requests start nothing. Setup
steps and how to run it are in [CI.md](CI.md); the configuration is `.circleci/config.yml`.

* **check** (`tools/ci.sh check`): web build, Rust tests with the protocol drift and disk-full
  checks, clippy, Chromium browser journeys.
* **full** (`tools/ci.sh full`): the check plus Firefox and WebKit, 2,000-case property tests and
  the server suites five times.
* **Release:** a tag-triggered release pipeline (binaries, images, checksums) is still to be
  added with 9.11.
* The GitHub Actions workflows were removed: they started on every push and the
  organization's Actions policy blocked them.
* **Owner:** connect the repository to CircleCI and create an API token (CI.md steps 2–5);
  apply for the open-source plan once the repository is public.

## 5. Release artifacts (9.11)

Channels for v0.1 (owner decision, 1 November): **GitHub Releases, Docker, npm** (npm also
serves pnpm, yarn and bun). Homebrew and winget wait (winget needs a Windows port). macOS
binaries ship unsigned until an Apple Developer ID exists. How it runs: [RELEASING.md](RELEASING.md).

* Binaries: Linux x86_64 and arm64 (built on Debian 11, glibc 2.31+), macOS arm64 and x86_64
  (macOS 11+), UI embedded; `SHA256SUMS`.
* Image `ghcr.io/alephgrid/alephgrid:X.Y.Z` and `:latest`, amd64 and arm64, from the tested
  binaries (`Dockerfile.release`; `/data` owned by the server's user).
* npm `alephgrid` plus `alephgrid-<platform>-<arch>` (unscoped, so no npm organization is needed).
* Nothing is public before a maintainer approves the pipeline's `approve-publish` step.
* **Owner:** npm organization and token, GitHub token, the CircleCI `release` context
  (RELEASING.md "Once"). Still open: signatures and an SBOM.

## 6. GitHub setup (owner)

* Make the repository public (9.07); keep `main` protected (required CI, reviews, no force push).
* About: description, website, topics such as `spreadsheet`, `duckdb`, `big-data`, `sql`,
  `excel`, `self-hosted`, `rust`, `mcp`.
* Upload the social preview image.
* Enable Discussions (categories: Announcements, Q&A, Ideas, Show and tell) and private
  vulnerability reporting.
* Labels: `bug`, `feature`, `docs`, `good first issue`, `help wanted`, `performance`, `security`.
* Publish release `v0.1.0` with the notes below and the section 5 artifacts; pin an
  announcement in Discussions.

## 7. Announcement drafts

### GitHub release notes: v0.1.0

> **Alephgrid 0.1.0: the first release**
>
> Alephgrid is an open-source spreadsheet for data that doesn't fit in a spreadsheet. Open
> CSV, Parquet, JSON or XLSX files with hundreds of millions of rows and work with them in
> your browser, on your laptop or your own server.
>
> **Highlights**
> - Big tables: scroll, sort, filter, edit and add calculated columns at any size; nothing is
>   loaded into memory up front.
> - Sheets: Excel formulas, formatting, merged cells, frozen panes, charts, colour rules,
>   comments, XLSX import and export.
> - Spreadsheet formulas over big tables: `XLOOKUP` and `SUMIFS` in a cell run as one query
>   over the whole table.
> - Keyboard and clipboard like a spreadsheet: fill handle, paste special, fill down and right.
> - Analyze without SQL: summaries, pivots, joins and charts; SQL when you want it.
> - Your own functions in formulas, SQL or sandboxed Python, with tests and versions.
> - Teams: spaces, roles, single sign-on, audit, quotas and backups, all free.
> - Agents: read-only MCP access to workbooks, with a receipt for every result.
>
> **Install:** download a binary below, or `docker run -p 7878:7878 ghcr.io/alephgrid/alephgrid:0.1.0`.
> **Known limits:** see docs/SPREADSHEET_FEATURES.md and docs/FORMULA_GAPS.md.
> Apache-2.0. Feedback in Discussions; security reports through SECURITY.md.

### Discussions announcement (pinned)

> **Alephgrid 0.1.0 is out**
>
> We built Alephgrid because spreadsheets stop working at a million rows, and the tools that
> go further want your data in their cloud. Alephgrid runs where your data already is: one
> binary or one container, data in a plain directory, every feature free.
>
> Try it in a minute with `alephgrid --demo-rows 100000000`. Tell us what you tried, what
> broke and what you need next: S3 data read in place and multi-machine deployments are
> next on the roadmap.

### Short post (social, forums)

> Alephgrid is an open-source, self-hosted spreadsheet for hundreds of millions of rows: Excel
> formulas, charts and formatting next to big tables, `XLOOKUP` over a whole table, SQL when
> you want it, MCP for agents. One binary, Apache-2.0. github.com/alephgrid/alephgrid
