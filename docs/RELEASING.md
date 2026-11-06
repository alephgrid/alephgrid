# Releasing

A release is built, tested and packaged by CircleCI from a version tag, waits in CircleCI
for a maintainer's approval, and only then becomes public. Nothing runs on a push.

| Channel | What users run | Built by |
|---|---|---|
| GitHub Releases | download `alephgrid-VERSION-TARGET.tar.gz` (+ `SHA256SUMS`) | `release-linux-*`, `release-macos` |
| Docker | `docker run -p 7878:7878 ghcr.io/alephgrid/alephgrid` | `release-docker` (`Dockerfile.release`, from the tested binaries) |
| npm (also pnpm, yarn, bun) | `npx alephgrid` · `npm i -g alephgrid` | `release-npm` (`tools/release/npm.mjs`) |

Targets: Linux x86_64 and arm64 (built on Debian 11, so they need glibc 2.31 or newer: Debian
11+, Ubuntu 20.04+, RHEL 9), macOS Apple silicon and Intel (macOS 11+). Windows: Docker or WSL
until a native build exists. macOS binaries are not yet signed or notarized: the first run
needs right-click → Open (or `xattr -d com.apple.quarantine alephgrid`).

## Once: accounts and secrets

1. **npm:** create the `alephgrid` organization on npmjs.com (it owns the `@alephgrid/cli-*`
   packages) and a granular access token that can publish `alephgrid` and `@alephgrid/*`.
2. **GitHub:** a token for the release and the image: a classic personal access token with
   `repo` and `write:packages` (GitHub Container Registry does not take fine-grained tokens).
3. **CircleCI:** Organization settings → Contexts → create a context named **`release`** with
   `GITHUB_TOKEN`, `GITHUB_USER` (the token's GitHub username) and `NPM_TOKEN`. Restrict it
   to maintainers.
4. After the first image is pushed, make the package public: GitHub → alephgrid → Packages →
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
   * creates a **draft** GitHub release with the archives and `SHA256SUMS` (drafts are visible
     only to maintainers);
   * stops at **`approve-publish`**.
5. Check the draft on GitHub → Releases: notes, files, sizes. Download one archive and run it.
6. In CircleCI, approve `approve-publish`. Then, in parallel: the release is published, the
   image `ghcr.io/alephgrid/alephgrid:X.Y.Z` and `:latest` is pushed for amd64 and arm64, and
   the npm packages are published (the platform packages first, then `alephgrid`; already
   published versions are skipped, so the step can be re-run).
7. Check: `npx alephgrid@X.Y.Z --version`, `docker run --rm ghcr.io/alephgrid/alephgrid:X.Y.Z --version`.

If a build fails before approval, fix it on `main`, move the tag only if nothing was published
(`git tag -d vX.Y.Z && git push --delete origin vX.Y.Z`, then tag again), and re-run step 4: the
draft is replaced. Once published, a version is never replaced: release X.Y.Z+1.

## Locally

The packaging scripts run anywhere, with a binary standing in for a release build:

```bash
tools/release/smoke.sh target/debug/alephgrid 0.1.0
tools/release/package.sh 0.1.0 aarch64-apple-darwin target/debug/alephgrid dist
node tools/release/npm.mjs 0.1.0 dist npm-out      # needs all four archives in dist
```
