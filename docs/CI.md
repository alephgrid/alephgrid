# Continuous integration (CircleCI, on demand)

CI runs **only when someone asks for it**. A push or pull request makes CircleCI create a
pipeline with every parameter off, so no workflow runs and no credits are used. A run is
started from the CircleCI app or with `tools/ci.sh`. Before a pull request, the local checks
in [CONTRIBUTING.md](../CONTRIBUTING.md) still apply.

The tiers are those of [TEST_STRATEGY.md](TEST_STRATEGY.md#tiers). Every one fails on any
failing test: nothing is retried or allowed to fail, and `tools/test_gate.py` (run in every
tier) refuses unlisted skips and forgiving test commands.

| Tier (workflow) | Started by | What it does |
|---|---|---|
| **check** | `run_check = true` | web type check and bundle; `cargo test --workspace` (with the protocol drift check and the disk-full tests); clippy with warnings as errors; licence notices up to date (`tools/licenses.py --check`); browser journeys in Chromium |
| **release** | `run_release = true`, `release_version`, on a tag | binaries, SBOMs, draft release, then (after approval) GitHub release, Docker image, npm and PyPI; see [RELEASING.md](RELEASING.md) |
| **full** | `run_full = true` | everything in check, plus browser journeys in Firefox and WebKit, 2,000-case property tests of the engine, and every `alephgrid-server` test five times |
