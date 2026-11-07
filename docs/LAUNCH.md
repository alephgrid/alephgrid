# Launch v0.1.0

What is done, what is left before the first public release, what only the project owner can
do, and the announcement drafts. State as of 6 November 2026. Task IDs refer to
[IMPLEMENTATION_CHECKLIST.md](IMPLEMENTATION_CHECKLIST.md): this page is the summary, the
checklist is the record.

## 1. Done

| Area | Tasks | Where it landed |
|---|---|---|
| Security remediation (audit run-1) and the run-2 re-audit fixes | L.01, SEC.01–SEC.14, SEC.T01 | PRs #14, #52 |
| Cleanup of existing features: Analyze pane, header filters, find bar, charts, dialogs, first run | L.02, U.15–U.20, U.T04 | PRs #15, #18 |
| Sheet completeness: formatting, number formats, layout, merges, sort and filter, tabs, zoom, charts, rules, comments | L.03, S.01–S.12 | PRs #16, #17 |
| Team screens: Share, space settings, administration; state inventory; leave guard | L.05, U.02, U.08–U.11 | PRs #20, #23 |
| MCP for local workbooks: tools, receipts, approved proposals, Agent connections UI; listed in the MCP registry | most of L.04 | PRs #19, #21, #22, #45, #46 |
| Launch UI, first part: fill handle, Mod+A region, paste-special menu, functions side panel, chart previews, disabled-command reasons, new workbooks open into a sheet, spreadsheet-first journey test | part of L.05b | PRs #26–#31 |
| CI on CircleCI, run on demand | 9.16 | PR #33; fixes from its first runs in #34, #36, #39, #40 |
| Release pipeline: binaries, draft release, approval, then GitHub Releases, Docker, npm and PyPI | part of 9.11 | PRs #49–#51, PyPI wheels added 7 Nov (not yet run) |
| Public website at alephgrid.dev: landing, install, releases, docs, blog, llms.txt, MCP manifest | L.07 (part) | PRs #35, #37, #38, #41–#44 |
| Docs: README, security and conduct policies, screenshots; review against the code | L.07 (part) | PR #32 and this review |

## 2. Left before release

### Product

* **Bugs from the README screenshots (L.05c, a–f):** table formulas refuse mixed references
  such as `B$1`; sheet charts skip currency- and percent-formatted cells; the Analyze result
  chart is clipped; Analyze's "Refer to values as" hint goes stale; the Hub footer wraps badly;
  Mod+G does not open Go to while the grid has focus. Each fix starts with a failing test.
* **Launch UI (L.05b, U.14 gate):** shared component and state rules (U.11a); a searchable sheet
  list and a pass over every toolbar control (U.04a); the Hub loading placeholder and a test of
  the no-access state (U.03b); in the function panel, syntax highlighting and line errors, typed
  blank/empty/error test inputs (needs a protocol change), Cancel for a running test and impact
  shown before Activate (U.06b–e); filtered-row select-all, formatted paste from other apps,
  named date series and copy between sheets (F.04a–d); the U.T05 and U.T06 checks, a real
  screen-reader pass (ACCESSIBILITY_TESTING.md) and a usability review.
* **MCP (L.04):** check with real clients (MCP Inspector and at least one chat client, M.03),
  then evaluate real questions through them (M.T02).
* **Security gate (SEC.15):** run-2's seven deferred units, the audit's owner checks on a
  disposable deployment, and a green `full` CI run.

### Packaging and operations (L.06, phase 9)

Docker volume smoke tests and Compose examples (9.02, 9.03), backup and restore on a clean
second host (9.04), release-build benchmarks and a published capacity envelope (9.05, 9.06,
9.15), third-party notices (9.08), CI tiers and structured logs (9.10, 9.13, 9.14), signatures
and an SBOM (9.11), the 9.T01–9.T03 checks and the 9.12 gate; then the L.07 launch review and
the L.08 gate on a clean machine. [OPERATIONS.md](OPERATIONS.md) and
[RELEASING.md](RELEASING.md) describe the current state.

### Website

Before release day, check that every install command on the site works on its platform, that
version labels and checksums match the release, and that the site builds without broken links.
Container images, npm and PyPI stay marked as not yet published until the release exists.

## 3. Owner actions

Things only the project owner can do (accounts, settings, decisions):

* **Release secrets** ([RELEASING.md](RELEASING.md), "Once"): a granular npm token from the
  `alephgrid` account, a PyPI API token (account-wide for the first upload, which creates the
  `alephgrid` project; then a project-scoped one), a GitHub token with `repo` and
  `write:packages`, and the CircleCI `release` context holding them, restricted to maintainers.
* **PyPI name:** create the PyPI account now; the name `alephgrid` (free on 7 November) is
  claimed by the first upload, since PyPI has no reservation without one.
* **Release safety** ([CI.md](CI.md#release-safety)): keep "Pass secrets to forked PRs" off,
  add a tag ruleset for `v*`, and reserve the npm platform package names.
* **CircleCI:** apply for the open-source plan.
* **GitHub settings:** turn on private vulnerability reporting (SECURITY.md relies on it) and
  Discussions (Announcements, Q&A, Ideas, Show and tell); protect `main`; set topics
  (`spreadsheet`, `duckdb`, `big-data`, `sql`, `excel`, `self-hosted`, `rust`, `mcp`) and labels
  (`bug`, `feature`, `docs`, `good first issue`, `help wanted`, `performance`, `security`); upload
  a 1280×640 social preview image (logo and the hero screenshot).
* **Decisions:** reuse terms for the brand artwork (U.13); the CSV export default stance
  ([SECURITY.md](SECURITY.md)); whether to add a funding link, and only with a verified
  recipient.
* **After the first image is pushed:** make the `ghcr.io/alephgrid/alephgrid` package public.

## 4. Release day

1. Rename `## Unreleased` in [CHANGELOG.md](../CHANGELOG.md) to `## 0.1.0 - <date>`, set the
   version in `Cargo.toml`, merge, and push the tag `v0.1.0` on `main`.
2. Run `tools/ci.sh release v0.1.0`; check the private draft release, then approve
   `approve-publish` in CircleCI ([RELEASING.md](RELEASING.md)).
3. Update the website: in `site/src/data/releases.json`, set the published channels to
   `published: true` with their URLs and checksums, and bump `current`.
4. Add the release and CI badges and install-from-release steps to the README; regenerate the
   screenshots with `web/scripts/readme-screenshots.mjs` if the UI changed (add an Analyze
   screenshot once its chart fits).
5. Publish the announcement below and pin it in Discussions.

## 5. Repository files

| File | State |
|---|---|
| `README.md`, `CHANGELOG.md` (v0.1.0 notes under "Unreleased"), `CONTRIBUTING.md`, `SECURITY.md`, `CODE_OF_CONDUCT.md` (contact conduct@alephgrid.dev), `LICENSE` (Apache-2.0) | Present |
| `.github/ISSUE_TEMPLATE/`, `.github/pull_request_template.md`, `.circleci/config.yml` | Present |
| `docs/` | Indexed in [docs/README.md](README.md) |
| `NOTICE` and third-party notices | Missing (9.08) |
| Benchmarks page | Missing (9.05) |

## 6. Announcement drafts

### GitHub release notes

The pipeline publishes the CHANGELOG's `0.1.0` section as the release notes, so they are
maintained there.

### Discussions announcement (pinned)

> **Alephgrid 0.1.0 is out**
>
> We built Alephgrid because spreadsheets stop working at a million rows, and the tools that
> go further want your data in their cloud. Alephgrid runs where your data already is: one
> binary or one container, data in a plain directory, every feature free.
>
> Try it with `npx alephgrid --demo-rows 10000000`. Tell us what you tried, what broke and what
> you need next: S3 data read in place and multi-machine deployments are next on the
> [roadmap](https://github.com/alephgrid/alephgrid/blob/main/docs/ROADMAP.md).

### Short post (social, forums)

> Alephgrid is an open-source, self-hosted spreadsheet for hundreds of millions of rows: Excel
> formulas, charts and formatting next to big tables, `XLOOKUP` over a whole table, SQL when
> you want it, MCP for agents. One binary, Apache-2.0. https://alephgrid.dev
