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
| **pr** | `run_check = true` (`tools/ci.sh check`) | web type check and bundle; `cargo test --workspace` (protocol drift, authorization, reconnect, crash, backup/restore, disk-full on a tmpfs, property and fuzz targets at their per-change bounds); clippy with warnings as errors; licence notices up to date (`tools/licenses.py --check`); the test gate; browser journeys in Chromium |
| **nightly** | the CircleCI schedule (below), or `run_nightly = true` (`tools/ci.sh nightly`; `run_full` still works) | everything in pr, plus browser journeys in Firefox and WebKit; the install check (`tools/release/smoke.sh` on the built binary: empty data directory, UI served, workbook created, CSV imported); 2,000-case property tests; fuzz targets up to 200,000 cases or 5 minutes each; every `alephgrid-server` test five times; targeted mutation testing (`tools/mutants.py`) |
| **release** | `run_release = true`, `release_version`, on a tag (`tools/ci.sh release vX.Y.Z`) | every nightly job on the tagged commit, then binaries, SBOMs and the artifact check (`tools/release/verify.sh`); the draft release waits for all of them, then (after approval) GitHub release, Docker image, npm and PyPI; see [RELEASING.md](RELEASING.md) |

The configuration is [.circleci/config.yml](../.circleci/config.yml). After the web build, one job compiles
everything (`rust-build`); the Rust tests (run with `--no-fail-fast`, so one run reports every failure),
clippy and the browser tests then run side by side, so one failure does not hide the others. Rust jobs run on an Ubuntu
24.04 VM (the disk-full tests mount a tmpfs, and Python function tests need bubblewrap). The
Cargo registry and `target/` are cached by `Cargo.lock` and `rust-toolchain.toml`, so DuckDB's
long C++ build happens once per dependency change. Playwright traces and screenshots of failing
browser tests are kept as artifacts, and JUnit results show per test in CircleCI's *Tests* tab.
Shrunk failing inputs of the property and fuzz targets (`proptest-regressions/`) are kept as
artifacts of the long runs; commit them so every later run replays them.

## Setting it up (once)

1. **Sign in to CircleCI with GitHub** at <https://circleci.com>, choosing the `alephgrid`
   organization. If the organization restricts third-party access, an organization owner
   approves CircleCI under GitHub → organization Settings → Third-party access.
2. **Add the project.** In CircleCI open *Projects*, find `alephgrid`, choose *Set Up Project*,
   and pick *Use the `.circleci/config.yml` in my repo* on branch `main`.
3. **Check that pushes run nothing.** Push any branch: the pipeline appears with no workflows.
   That is expected. To hide even those empty pipelines:
   * Projects connected through the **CircleCI GitHub App**: Project settings → *Triggers*,
     remove the GitHub push trigger. Runs are then started only by hand or through the API.
   * Projects connected through the **GitHub OAuth app**: leave it; empty pipelines cost nothing.
4. **Create a personal API token** for `tools/ci.sh`: CircleCI → User settings → *Personal API
   Tokens* → *Create New Token*. Put it in your shell profile, not in the repository:

   ```bash
   export CIRCLECI_TOKEN=<the token>   # in your shell profile
   ```

   For a GitHub App project, also add the two values from Project settings → *Overview* and
   *Pipelines*:

   ```bash
   export CIRCLECI_PROJECT_SLUG=circleci/<organization id>/<project id>
   export CIRCLECI_DEFINITION_ID=<pipeline definition id>
   ```

5. **Start the first run** (it builds DuckDB from scratch, so it is the slowest):

   ```bash
   tools/ci.sh check main
   ```

6. **Turn on the nightly tier** (optional; it uses credits every night). In CircleCI open
   Project settings → *Triggers* (projects connected through the GitHub App: Project
   settings → *Project Setup* → *Add trigger* → *Schedule*), add a schedule on branch `main`,
   once a day at a quiet hour, and give it the pipeline parameter `run_nightly` = `true`.
   The `nightly` workflow also starts for any pipeline whose `pipeline.trigger_source` is
   `scheduled_pipeline`. A schedule cannot be set in `.circleci/config.yml`. Check the first
   scheduled pipeline shows the `nightly` workflow. Failures are reported by CircleCI's
   usual notifications (Project settings → *Notifications*).

7. **Apply for CircleCI's open-source plan** (the repository is public) for more free credits.

## Running it

```bash
tools/ci.sh check              # PR tier on the current branch (pushed), Chromium only
tools/ci.sh nightly my-branch  # the nightly tier, on a named branch
tools/ci.sh status             # workflows of the latest pipeline, with a link
tools/ci.sh status 42          # of pipeline 42
```

Or in the CircleCI app: open the project, choose *Trigger Pipeline*, pick the branch, add a
parameter of type boolean named `run_check` (or `run_nightly`) with value `true`, and start it.

## Cost notes

Each run uses credits: a VM with 4 CPUs for the Rust and browser jobs. The first run, and the
first after `Cargo.lock` changes, compiles DuckDB and takes longest; later runs restore it from
the cache. Use **check** for pull requests, and **nightly** (or the schedule) after changes to storage,
collaboration or recovery. The release tier repeats the nightly checks on the tagged commit.

## What has actually run

Implemented is not the same as executed. As of 7 November 2026:

| Tier | Implemented | Executed on CircleCI |
|---|---|---|
| pr (`check`) | yes | yes, the earlier `check` workflow on several PRs (9.16); not yet with the test gate and the fuzz targets added on 7 Nov |
| nightly | yes (7 Nov) | not yet: no schedule set up and not started by hand since these jobs were added |
| release | yes (test jobs and artifact check added 7 Nov) | never (needs the `release` context and a tag) |

Run locally on 7 Nov: the test gate, `tools/mutants.py` (9 killed, 1 known survivor), the fuzz targets (64 cases each; then up to 5,000 cases
or 2 minutes each), `tools/release/verify.sh` against packaged stand-in archives (and its
failure on a tampered archive). Update this table when a tier first runs.

## Release safety

The release workflow holds publishing tokens, so a few things are checked in the repository
and a few must be set by the owner once.

In the repository:

* `tools/ci.sh release vX.Y.Z` releases only if the tag on GitHub names the same commit as
  the tag in your checkout, and that commit is on `main`.
* Third-party images in the release jobs run by digest, before the registry token is given to
  docker: `tonistiigi/binfmt` (arm64 emulation) and `moby/buildkit` (the image builder). To
  update one, look up the new digest (`docker buildx imagetools inspect tonistiigi/binfmt:latest`)
  and change it in `.circleci/config.yml` in its own pull request.
* `release-sbom` installs `cargo-cyclonedx` at a pinned version with `--locked` and holds no
  secrets. Update the version in its own pull request.
* npm: a version already on npm is skipped only when its `dist.integrity` equals this build's
  package (a re-run). Anything else under that name and version stops the job before the main
  `alephgrid` package is published, so it can never depend on a package someone else uploaded.
  Reserve the platform package names before the first release.
* PyPI: `release-pypi` installs `twine` at a pinned version (its own dependencies are not
  pinned) and runs `twine check --strict` before uploading. A wheel already on PyPI is skipped
  only when its sha256 equals this build's (a re-run; `SOURCE_DATE_EPOCH` from the tagged commit
  makes the wheels byte-identical); any other file under that name stops the job. Update the
  twine version in its own pull request.

Once, by the owner (not visible in the repository):

* CircleCI → Organization settings → Contexts → `release` → restrict it to a security group
  holding only the maintainers.
* CircleCI → Project settings → Advanced: *Pass secrets to builds from forked pull requests*
  off.
* GitHub → Settings → Rules: a tag ruleset on `refs/tags/v*` so only maintainers can create,
  move or delete release tags.
* Prefer a token that can only push packages (`write:packages`) for the image, and a
  fine-grained token with *Contents: write* on this repository for the GitHub release, over
  one classic token with `repo` scope.
