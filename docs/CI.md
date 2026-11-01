# Continuous integration (CircleCI, on demand)

CI runs **only when someone asks for it**. A push or pull request makes CircleCI create a
pipeline with every parameter off, so no workflow runs and no credits are used. A run is
started from the CircleCI app or with `tools/ci.sh`. Before a pull request, the local checks
in [CONTRIBUTING.md](../CONTRIBUTING.md) still apply.

| Run | Parameter | What it does |
|---|---|---|
| **check** | `run_check = true` | web type check and bundle; `cargo test --workspace` (with the protocol drift check and the disk-full tests); clippy with warnings as errors; browser journeys in Chromium |
| **full** | `run_full = true` | everything in check, plus browser journeys in Firefox and WebKit, 2,000-case property tests, and the server's integration and crash suites five times |

The configuration is [.circleci/config.yml](../.circleci/config.yml). One job compiles everything
(`rust-build`); the Rust tests (run with `--no-fail-fast`, so one run reports every failure),
clippy and the browser tests then run side by side, so one failure does not hide the others. Rust jobs run on an Ubuntu
24.04 VM (the disk-full tests mount a tmpfs, and Python function tests need bubblewrap). The
Cargo registry and `target/` are cached by `Cargo.lock` and `rust-toolchain.toml`, so DuckDB's
long C++ build happens once per dependency change. Playwright traces and screenshots of failing
browser tests are kept as artifacts.

## Setting it up (once)

1. **Merge the pull request that adds `.circleci/config.yml`**, so it is on `main`.
2. **Sign in to CircleCI with GitHub** at <https://circleci.com>, choosing the `alephgrid`
   organization. If the organization restricts third-party access, an organization owner
   approves CircleCI under GitHub → organization Settings → Third-party access.
3. **Add the project.** In CircleCI open *Projects*, find `alephgrid`, choose *Set Up Project*,
   and pick *Use the `.circleci/config.yml` in my repo* on branch `main`.
4. **Check that pushes run nothing.** Push any branch: the pipeline appears with no workflows.
   That is expected. To hide even those empty pipelines:
   * Projects connected through the **CircleCI GitHub App**: Project settings → *Triggers*,
     remove the GitHub push trigger. Runs are then started only by hand or through the API.
   * Projects connected through the **GitHub OAuth app**: leave it; empty pipelines cost nothing.
5. **Create a personal API token** for `tools/ci.sh`: CircleCI → User settings → *Personal API
   Tokens* → *Create New Token*. Put it in your shell profile, not in the repository:

   ```bash
   echo 'export CIRCLECI_TOKEN=<the token>' >> ~/.zshrc && source ~/.zshrc
   ```

   For a GitHub App project, also add the two values from Project settings → *Overview* and
   *Pipelines*:

   ```bash
   export CIRCLECI_PROJECT_SLUG=circleci/<organization id>/<project id>
   export CIRCLECI_DEFINITION_ID=<pipeline definition id>
   ```

6. **Start the first run** (it builds DuckDB from scratch, so it is the slowest):

   ```bash
   tools/ci.sh check main
   ```

7. **When the repository becomes public**, apply for CircleCI's open-source plan for more free
   credits.

## Running it

```bash
tools/ci.sh check              # the current branch (pushed), Chromium only
tools/ci.sh full my-branch     # everything, on a named branch
tools/ci.sh status             # workflows of the latest pipeline, with a link
tools/ci.sh status 42          # of pipeline 42
```

Or in the CircleCI app: open the project, choose *Trigger Pipeline*, pick the branch, add a
parameter of type boolean named `run_check` (or `run_full`) with value `true`, and start it.

## Cost notes

Each run uses credits: a VM with 4 CPUs for the Rust and browser jobs. The first run, and the
first after `Cargo.lock` changes, compiles DuckDB and takes longest; later runs restore it from
the cache. Use **check** for pull requests and **full** before a release or after changes to
storage, collaboration or recovery.
