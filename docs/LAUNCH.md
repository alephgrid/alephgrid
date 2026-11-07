# Launch v0.1.0

What is done, what is left before the first public release, what only the project owner can
do, and the announcement drafts. State as of 7 November 2026. Task IDs refer to
[IMPLEMENTATION_CHECKLIST.md](IMPLEMENTATION_CHECKLIST.md): this page is the summary, the
checklist is the record.

## 1. Done

| Area | Tasks | Where it landed |
|---|---|---|
| Security remediation (audit run-1) and the run-2 and run-3 re-audit fixes | L.01, SEC.01–SEC.14, SEC.T01 | PRs #14, #52, #66 |
| Cleanup of existing features: Analyze pane, header filters, find bar, charts, dialogs, first run | L.02, U.15–U.20, U.T04 | PRs #15, #18 |
| Sheet completeness: formatting, number formats, layout, merges, sort and filter, tabs, zoom, charts, rules, comments | L.03, S.01–S.12 | PRs #16, #17 |
| Team screens: Share, space settings, administration; state inventory; leave guard | L.05, U.02, U.08–U.11 | PRs #20, #23 |
| MCP for local workbooks: tools, receipts, approved proposals, Agent connections UI; listed in the MCP registry; checked with MCP Inspector and Claude Code | most of L.04 | PRs #19, #21, #22, #45, #46, #65 |
| Launch UI: fill handle, Mod+A region, paste-special menu, functions side panel, chart previews, disabled-command reasons, new workbooks open into a sheet, spreadsheet-first journey test; clipboard exchange with other spreadsheets; the README-screenshot bugs (L.05c) | most of L.05b, L.05c | PRs #26–#31, #56, #61–#63, #67, #68, #70 |
| CI on CircleCI, run on demand, with pr, nightly and release tiers | 9.16, part of 9.10 and 9.14 | PR #33; fixes from its first runs in #34, #36, #39, #40; tiers in #69 |
| Release pipeline: binaries, SBOMs, third-party notices, draft release, approval, then GitHub Releases, Docker, npm and PyPI | part of 9.08 and 9.11 | PRs #49–#51, #57, #59, #64; never run yet |
| Public website at alephgrid.dev: landing, install, releases, docs, blog, llms.txt, MCP manifest | L.07 (part) | PRs #35, #37, #38, #41–#44 |
| Docs: README, security and conduct policies, screenshots; review against the code and the shipped install channels | L.07 (part) | PR #32, the 6 Nov review, the docs-reconcile PR |

## 2. Left before release

### Product

* **Launch UI (L.05b, U.14 gate):** copying HTML into other spreadsheet apps checked by hand, a
  real Mod+V in Firefox and Windows browsers (F.04b); the manual screen-reader pass
  (ACCESSIBILITY_TESTING.md, U.T06); a usability review with a real user (U.T07); then the U.14
  gate.
* **MCP (L.04):** Claude Desktop and ChatGPT checks (owner, M.03); whether to bundle DuckDB's ICU
  extension so time-zone functions work (M.T02).
* **Security gate (SEC.15):** the owner decisions in the run-3 report, run-2's owner checks on a
  disposable deployment, and a green `nightly` CI run.

### Packaging and operations (L.06, phase 9)

Running the Docker volume smoke tests and both Compose examples on a Docker host (9.02, 9.03),
backup and restore on a clean second host (9.04), release-build benchmarks and a published
capacity envelope (9.05, 9.06, 9.15), the provenance review and the NOTICE copyright line (9.08),
the first CircleCI runs of the nightly and release tiers and structured logs (9.10, 9.13, 9.14),
the first real release run (9.11; signatures deferred), the 9.T01–9.T03 checks and the 9.12 gate; then the L.07 launch review and
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
* **GitHub settings:** private vulnerability reporting is on (checked 7 November). Turn on
  Discussions (Announcements, Q&A, Ideas, Show and tell); until then the README and the issue
  template send questions to issues. Change the repository description, which says "billions of
  rows", to match the measured scale in the README. Protect `main`; set topics
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
5. Publish the announcement below and pin it in Discussions (once they are on).

## 5. Repository files

| File | State |
|---|---|
| `README.md`, `CHANGELOG.md` (v0.1.0 notes under "Unreleased"), `CONTRIBUTING.md`, `SECURITY.md`, `CODE_OF_CONDUCT.md` (contact conduct@alephgrid.dev), `LICENSE` (Apache-2.0) | Present |
| `.github/ISSUE_TEMPLATE/`, `.github/pull_request_template.md`, `.circleci/config.yml` | Present |
| `docs/` | Indexed in [docs/README.md](README.md) |
| `NOTICE`, `THIRD-PARTY-NOTICES.md` | Present, generated by `tools/licenses.py`; copyright line to confirm (9.08) |
| Benchmarks | Tool present (`tools/bench/bench.mjs`, [OPERATIONS.md](OPERATIONS.md#benchmarks)); results not published (9.05) |

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

> Alephgrid is an open-source, self-hosted spreadsheet for tens of millions of rows: Excel
> formulas, charts and formatting next to big tables, `XLOOKUP` over a whole table, SQL when
> you want it, MCP for agents. One binary, Apache-2.0. https://alephgrid.dev
