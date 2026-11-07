# Releasing

A release is built, tested and packaged by CircleCI from a version tag, waits in CircleCI
for a maintainer's approval, and only then becomes public. Nothing runs on a push. No release
has been made yet, so none of the channels below is published.

| Channel | What users run | Built by |
|---|---|---|
| GitHub Releases | download `alephgrid-VERSION-TARGET.tar.gz` (+ `SHA256SUMS`, SBOMs) | `release-linux-*`, `release-macos`, `release-sbom` |
| Docker | `docker run -p 7878:7878 ghcr.io/alephgrid/alephgrid` | `release-docker` (`Dockerfile.release`, from the tested binaries) |
| npm (also pnpm, yarn, bun) | `npx alephgrid` · `npm i -g alephgrid` | `release-npm` (`tools/release/npm.mjs`) |
| PyPI (also pipx, uv) | `uvx alephgrid` · `pipx install alephgrid` · `pip install alephgrid` | `release-pypi` (`tools/release/pypi.py`) |

Every archive, image, npm package and wheel holding the binary carries `LICENSE`, `NOTICE` and
`THIRD-PARTY-NOTICES.md` (written by `tools/licenses.py`; the image keeps them in
`/usr/share/doc/alephgrid/`). Archives and SBOMs are checksummed but **not yet signed**:
signatures (for example Sigstore/cosign for the image and the archives) are deferred.

Targets: Linux x86_64 and arm64 (built on Debian 11, so they need glibc 2.31 or newer: Debian
11+, Ubuntu 20.04+, RHEL 9), macOS Apple silicon and Intel (macOS 11+). Windows: Docker or WSL
until a native build exists. macOS binaries are not yet signed or notarized: the first run
needs right-click → Open (or `xattr -d com.apple.quarantine alephgrid`).

PyPI gets wheels only, no source distribution: building from source needs Rust, Node and
several minutes of DuckDB compilation, which an installer should not start. Each wheel holds the
binary as an installed script, so pip, pipx and uv put `alephgrid` on the PATH.
`tools/release/pypi.py` tags each wheel from its binary, never below the build baseline:
`manylinux_2_31_x86_64` / `manylinux_2_31_aarch64` (pip 20.3 or newer) and `macosx_11_0_arm64` /
`macosx_11_0_x86_64`, and refuses a Linux binary that links a library outside the manylinux set.
PyPI accepts files up to 100 MB by default; a larger wheel needs a limit increase from PyPI first.

## Once: accounts and secrets

1. **npm:** the packages are unscoped, so no npm organization is needed: `alephgrid` (the
   launcher) and one binary package per platform, `alephgrid-linux-x64`, `alephgrid-linux-arm64`,
   `alephgrid-darwin-arm64` and `alephgrid-darwin-x64`. They are owned by a maintainer's npm
   account (two-factor authentication is required to publish) and can be shared with the
   `alephgridhq` organization's teams. The names were claimed on 7 November 2026 with `0.0.0`
   placeholders. Create a granular access token with read and write on those five packages,
   no organization access, and "bypass two-factor authentication" (CI cannot answer a 2FA
   prompt). npm restricts tokens that bypass 2FA: direct publishing with them stops in
   January 2027, so before then the npm job must move to npm trusted publishing or another
   supported flow (open item).
2. **GitHub:** a token for the release and the image: a classic personal access token with
   `repo` and `write:packages` (GitHub Container Registry does not take fine-grained tokens).
3. **PyPI:** create a PyPI account for the project (2FA is required) and an API token. PyPI's
   trusted publishing (tokenless OIDC) supports GitHub Actions, GitLab CI/CD, Google Cloud and
   ActiveState, not CircleCI (checked 7 November 2026), so the release uses a token. A token
   scoped to one project can only be made once the project exists, and the project is created
   by its first upload: for the first release use an account-wide token, then replace it with a
   token scoped to `alephgrid` and revoke the first one. PyPI cannot reserve a name without an
   upload; `alephgrid` was free on 7 November 2026.
4. **CircleCI:** Organization settings → Contexts → create a context named **`release`** with
   `GITHUB_TOKEN`, `GITHUB_USER` (the token's GitHub username), `NPM_TOKEN` and `PYPI_TOKEN`.
   Restrict it to maintainers.
5. After the first image is pushed, make the package public: GitHub → alephgrid → Packages →
   alephgrid → Package settings → Change visibility → Public.

## Each release

1. On `main`, set the version in `Cargo.toml` (`[workspace.package] version`) and rename the
   changelog's "Unreleased" heading to `## X.Y.Z - YYYY-MM-DD`. Run
   `python3 tools/licenses.py` so `NOTICE` and `THIRD-PARTY-NOTICES.md` match the locked
   dependencies (the check's clippy job fails when they do not). Merge that pull request.
2. Run the check: `tools/ci.sh check main`. It must be green.
3. Tag and push the tag (tags are public):

   ```bash
   git switch main && git pull
   git tag -a vX.Y.Z -m "Alephgrid X.Y.Z" && git push origin vX.Y.Z
   ```

4. Start the release: `tools/ci.sh release vX.Y.Z`. CircleCI:
   * checks that the tag, `Cargo.toml` and the changelog agree;
   * builds the four binaries (about 30 minutes; DuckDB is compiled from source), runs a smoke
     test on each native build (start, version, embedded UI, create a workbook, import a CSV)
     and packages them with checksums;
   * writes CycloneDX SBOMs (`release-sbom`): `alephgrid-VERSION-TARGET.cdx.json` for the Rust
     crates linked into each binary (cargo-cyclonedx, pinned) and `alephgrid-VERSION-web.cdx.json`
     for the web bundle (`npm sbom`, production dependencies). The C/C++ libraries inside DuckDB
     are not in cargo's graph; they are listed in `THIRD-PARTY-NOTICES.md`;
   * creates a **draft** GitHub release with the archives, the SBOMs and `SHA256SUMS` (drafts
     are visible only to maintainers);
   * stops at **`approve-publish`**.
5. Check the draft on GitHub → Releases: notes, files, sizes. Download one archive and run it.
6. In CircleCI, approve `approve-publish`. Then, in parallel: the release is published, the
   image `ghcr.io/alephgrid/alephgrid:X.Y.Z` and `:latest` is pushed for amd64 and arm64, and
   the npm packages are published (the platform packages first, then `alephgrid`; already
   published versions are skipped, so the step can be re-run), and the four wheels are
   uploaded to PyPI (a wheel already there with the same bytes is skipped; any other file under
   that name stops the job). PyPI versions follow PEP 440: `X.Y.Z` stays `X.Y.Z`, `X.Y.Z-rc.N`
   becomes `X.Y.ZrcN`.
7. Check: `npx alephgrid@X.Y.Z --version`, `uvx alephgrid@X.Y.Z --version`,
   `docker run --rm ghcr.io/alephgrid/alephgrid:X.Y.Z --version`.

If a build fails before approval, fix it on `main`, move the tag only if nothing was published
(`git tag -d vX.Y.Z && git push --delete origin vX.Y.Z`, then tag again), and re-run step 4: the
draft is replaced. Once published, a version is never replaced: release X.Y.Z+1.

## Locally

The packaging scripts run anywhere, with a binary standing in for a release build:

```bash
tools/release/smoke.sh target/debug/alephgrid 0.1.0
tools/release/package.sh 0.1.0 aarch64-apple-darwin target/debug/alephgrid dist
node tools/release/npm.mjs 0.1.0 dist npm-out      # needs all four archives in dist
python3 tools/release/pypi.py 0.1.0 dist wheels --target aarch64-apple-darwin   # default: all four
python3 -m venv /tmp/ag && /tmp/ag/bin/pip install wheels/*.whl && /tmp/ag/bin/alephgrid --version
```
