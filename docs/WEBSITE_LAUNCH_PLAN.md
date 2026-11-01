# Alephgrid website, installation and funding plan

Research and local repository inspection: 31 October 2026; **updated 1 November 2026 against the current repository** — since the original audit the remote is `github.com/alephgrid/alephgrid`, the binary and Compose image are already named `alephgrid`, CI moved to CircleCI (`.circleci/config.yml`; the website deploys through its own GitHub Pages workflow, `.github/workflows/site.yml`), real product screenshots exist in `assets/screenshots/`, and `site/` implements W.03, W.04 (source channel + explicit unavailable states) and the W.09 first pass. Status: website implemented as a developer-preview site, no published packages. Product UI follows [UI_IMPLEMENTATION_GUIDE](UI_IMPLEMENTATION_GUIDE.md). This document covers the public site, documentation, distribution and voluntary funding.

## 1. Recommendation

Build a small, fast, documentation-led open-source website. Primary action: **Install Alephgrid**. Secondary: **View on GitHub**. No sign-up wall, sales funnel, mandatory hosted account, invented customer logos or fabricated GitHub statistics. Lead with the spreadsheet experience; explain DuckDB further down for technical readers. SQL is optional advanced access.

Use Astro for the custom landing/install/support pages and Starlight for documentation in a separate `site/` directory; do not replace the application in `web/`. Starlight supplies documentation navigation, search, code formatting and theme support [S1]. Build static output and deploy through CI to a static host; GitHub Pages is the initial low-infrastructure choice, with a custom domain once selected. Keep the build portable to other hosts. Do not put credentials or a payment backend in the public website.

The approved light visual direction is paper/white with forest headings and emerald actions. Dark is strictly neutral charcoal (#111113/#18181B), never forest-green surfaces. Reuse Manrope, vector logos, restrained grid motifs and compact install controls. A screenshot, not an abstract illustration, is the central proof of the product.

## 2. What the checkout actually contains

Inspected README.md, Dockerfile, docker-compose.yml, rust-toolchain.toml, main.rs and .github/workflows on 31 October.

| Channel | Observed locally | Public-site treatment |
|---|---|---|
| Source | Rust/Node build instructions; toolchain pinned to 1.97.0; binary is `rowfinity` | Developer setup, after clean-environment verification; don't advertise an `alephgrid` binary yet |
| Docker Compose | Builds from checkout; local `rowfinity:latest` name; shared `change-me` token; port exposed as 7878:7878 | Developer preview pending safer documented defaults; not evidence of a published registry image |
| macOS binaries / Homebrew | No release workflow or tap files found | Planned until artifacts and clean-machine install pass |
| Windows binaries / WinGet | No package manifest/release pipeline found; CI shown is Linux | Planned; Windows native support needs actual build/runtime evidence |
| Linux binary / OCI image | No publishing workflow found | Planned; build and publish versioned artifacts first |
| Helm | No chart found | Planned; depends on phase 11 and safe ownership/storage behavior |
| Funding | No funding configuration found | Support page can explain intent; activate checkout links only after recipient setup |

Resolved since the audit: the canonical repo is `https://github.com/alephgrid/alephgrid`, the binary is `alephgrid` and Compose uses `alephgrid:latest`. Still open: domain, package publisher identity and funding recipient. This audit is of the checkout, not a claim that no external release exists anywhere.

README examples mention older prerequisites, large demo data, token-in-URL access, six resolved formula gaps, and broad availability claims. Audit these against the release; website copy must not inherit them automatically. Avoid “unlimited”, complete Excel parity, SSO availability, zero cold starts or no-size-limit claims without evidence.

## 3. Site map

| Route | Purpose / main content |
|---|---|
| `/` | Understand the product, see real UI, choose installation |
| `/install/` | OS/architecture/version/channel selector; verified commands and downloads |
| `/docs/getting-started/` | Install → launch → import a small file → formula → save/export |
| `/docs/spreadsheets/` | Cells, keyboard shortcuts, formulas, clipboard, supported semantics |
| `/docs/self-hosting/` | Docker, persistence, authentication, backups, updates; Helm when ready |
| `/docs/reference/` | CLI/config, limits, file compatibility, optional SQL, versioned APIs |
| `/releases/` | Release notes, checksums, migrations, known issues, supported platforms |
| `/contribute/` | Code/docs/tests/design contributions, build/test guide, issue triage |
| `/roadmap/` | Current/next/later with links to real issues; explicit partial support |
| `/support/` | Sponsorship purpose, active providers, suggested amounts, spending updates |
| `/brand/` | Logo assets and usage/provenance once artwork reuse terms are selected |

Footer: source code, docs, license, security reporting, releases, contribute, support, privacy/telemetry statement. Link only destinations that actually exist. Version documentation alongside breaking behavior; show supported versions and archival notices.

## 4. Homepage design, top to bottom

### Header

Left: compact Alephgrid logo. Middle/right: Product, Docs, Install, Contribute, Support; GitHub link and Light/Dark/System control. Mobile uses a simple accessible menu. Avoid a large permanent sticky header over documentation.

### Hero

Headline: **Room for every row.**

Supporting copy: **An open-source spreadsheet for exploring data on your own machine or infrastructure.**

Primary: Install Alephgrid. Secondary: View on GitHub. Small adjacent release badge shows the actual version and maturity (for example Preview), sourced from release metadata. Never invent a stable release number.

Below copy: large light/dark workbook screenshot with visible formula bar and selected cells, SQL closed. A short optional caption describes the illustrated action. Until the redesigned UI ships, label concept art clearly; replace with real release screenshots for launch. No autoplay background video.

### Quick-install block

Tabbed macOS / Windows / Linux / Docker / Kubernetes; each shows readiness, prerequisites, shell, OS architecture, version, a command or download, Copy, launch instruction and “Full installation guide”. OS detection is only a default suggestion, never a forced redirect. Keyboard-accessible tabs and copy feedback are required.

On the homepage, default to a ready local channel. If none is ready, present “Developer preview — build from source” honestly. Do not enable Copy for nonexistent package commands. Kubernetes links to a full prerequisites/configuration page rather than implying a production cluster needs only one command.

### Product proof

Three restrained sections, each with a real workflow crop:
1. Familiar spreadsheet work: cells, formulas, copy/paste, filter and sort.
2. Data on your terms: local files, exports, self-hosted storage; describe telemetry/connectors accurately rather than making unconditional data-egress claims.
3. Room to analyze: documented scale/limits, pivots and charts only where shipped.

Show measured benchmarks only with linked hardware, dataset, build mode, version and reproducible script. Do not use a decorative “billions of rows instantly” counter.

### Deployment choices

Three compact cards: On your computer, On your server, On Kubernetes. Explain intended audience and readiness. Local use is easiest and first. Docker is the next self-hosting path. Kubernetes is an operator choice, not a prerequisite or default upsell.

### Open development and support

Link contribution guide, public roadmap and release notes. Show real contributors only from an auditable source; no fake counts. A quiet sponsorship strip reads “Help keep Alephgrid maintained” with Support the project. Funding does not interrupt installation or unlock core spreadsheet functionality.

## 5. Installation UX and release engineering

### Release metadata is the source of truth

Maintain a validated release manifest: version, maturity, platform, architecture, minimum OS, artifact URL, SHA-256, signature/provenance URL, package ID, published/verified status, install/run/update/uninstall commands and matching docs version. Generate website download controls from it. Fail CI on missing ready artifacts or placeholder URLs; never make a package appear ready by editing marketing copy alone.

Avoid a default `curl | sh` or `irm | iex` installer. Prefer package-manager commands and direct verified downloads. An installer script, if added later, must be inspectable, version-pinned and integrity-checked. Local server binds to loopback, prints its actual URL, handles an occupied port clearly, and doesn't create massive demo datasets by default.

### macOS

First distribute tested arm64 and x86_64 binaries; choose/document minimum versions and signing/notarization route as part of release packaging. Provide a direct download fallback. Then maintain an official Homebrew tap; taps are the documented external package channel [S2]. Test install, initial launch, import/edit/reopen, update and uninstall on clean hosts. Do not teach users to disable OS protections as normal onboarding.

Target command shape, **not a currently available package**:

```text
brew install <approved-owner>/tap/alephgrid
alephgrid
```

Only replace placeholders after ownership, binary naming and the tap are settled. If the binary remains rowfinity during migration, show that real name explicitly.

### Windows

Prove the native Windows build first, including DuckDB, Unicode paths, writable data location, browser launch and file locks. Begin with a signed installer/verified portable ZIP for x64; add ARM64 only after testing. A portable download must explain extraction and launching without requiring compilation. WinGet requires package metadata and submission to its repository [S3].

Target command shape, **not a currently available package**:

```text
winget install --id <ApprovedPublisher.Alephgrid> --exact
alephgrid
```

Include PowerShell-specific examples, upgrade/uninstall and a direct-download fallback. Do not silently call WSL support native Windows support.

### Linux and containers

Publish tested Linux architecture artifacts and versioned OCI images. Specify minimum compatibility. Docker local quickstart must use loopback port publication and persistent storage. Require a strong configured token where needed; do not ship `change-me` as a usable shared deployment. Team deployment documentation covers TLS/reverse proxy, authentication, storage ownership, backup/restore, upgrade and rollback. Current Compose source-build behavior is not equivalent to `docker pull` from a public registry.

A future example must include a real pinned image/version and volume; show shell-appropriate token setup without secrets in URLs. Never offer `down -v` as an ordinary uninstall without explaining it deletes persistent data.

### Helm / Kubernetes

Follow the JupyterHub pattern: prerequisites, explicit values file, pinned chart version, installation and operational checks [S4]. Publish a tested OCI chart [S5]. Target syntax only, not a live endpoint:

```text
helm upgrade --install alephgrid oci://<approved-registry>/<namespace>/alephgrid --version <chart-version> --namespace alephgrid --create-namespace -f values.yaml
```

Specify/test the supported Helm and Kubernetes versions rather than trusting a generic latest-version example. Helm's current OCI documentation warns of version-specific content.

Launch chart defaults: no public ingress by default, explicit persistent volume/storage class requirements, resource requests/limits, probes, secret references, backup guidance and conservative single-owner workbook placement. Do not scale several writers against one DuckDB file. A basic chart does not certify distributed compute, failover fencing or enterprise multi-tenancy. Helm beta waits for the relevant phase 7/9/11 gates; EKS is one documented environment, not a mandatory dependency.

## 6. Voluntary funding

### Recommended first channel: GitHub Sponsors

Use a maintainer or organization profile once eligibility, recipient identity and onboarding are complete. GitHub supports one-time and recurring sponsorship [S6]; receiving funds requires an eligible supported-region account [S7]. Do not infer payout eligibility from the user's current location alone. Keep checkout on the provider, not in Alephgrid.

Proposed amounts for owner review, not live tiers: $5/month supporter, $20/month contributor supporter, $100/month team supporter, custom one-time amount. Benefits can be optional acknowledgement and spending updates. No feature unlocks, private bug fixes, guaranteed roadmap votes or implied support SLA. Company logo placement requires consent and must not imply endorsement of the product.

Support-page copy: “Alephgrid is open source. Contributions help fund maintenance, release infrastructure, compatibility testing and documentation. You can also help with bug reports, tests and docs.” Include how funds will be used, who receives them, and periodic summaries when there is actual activity. Do not invent a funding goal, funded percentage or sponsor list. Do not promise tax deductibility.

After account activation add `.github/FUNDING.yml`; GitHub documents the repo sponsor-button mechanism [S8]. A planning template (do not activate placeholders):

```yaml
# Replace only with an active verified recipient.
# github: APPROVED_ACCOUNT
# open_collective: APPROVED_COLLECTIVE
```

Website Support, README and the GitHub sponsor button should point to the same canonical recipient. Verify checkout destination manually without making a payment. No account setup or financial transaction is authorized by this plan.

### Later option: Open Collective

Consider this when several maintainers need a project budget, expense reimbursement and fiscal hosting rather than funding one maintainer. Hosts set eligibility and fees; receiving/paying money requires the appropriate host/organization arrangement [S9]. Open Source Collective explains its hosted contribution process and explicitly says its contributions are not charitable tax-deductible donations [S10]. Confirm the chosen host's current terms before onboarding. Do not conflate Open Source Collective with other similarly named organizations.

Start with one clear funding route; adding several payment buttons before a community exists creates clutter. Use “Support” or “Sponsor” prominently; retain “donate” as conversational wording only where the provider's actual terms fit.

## 7. Implementation checklist and order

- [x] **W.01 (partial)** Repo (`alephgrid/alephgrid`) and binary naming (`alephgrid`) confirmed. Open: domain, package publisher identity, funding recipient. Record decisions; no guessed IDs.
- [ ] **W.02** Audit shipped claims, README prerequisites and documentation gaps against the release; classify platform/channel readiness.
- [x] **W.03** Done (1 Nov 2026): `site/` Astro + Starlight foundation, brand assets with OFL font license, neutral charcoal dark tokens, routing, static build. Deploy workflow: `.github/workflows/site.yml` (GitHub Pages; set `SITE_URL`/`SITE_BASE` repo variables when the domain is live).
- [x] **W.04** Done (1 Nov 2026): responsive homepage; install page shows the source channel and explicit Planned states driven by `site/src/data/releases.json`; copy buttons with manual-copy fallback.
- [ ] **W.05** Implement release manifest and macOS/Linux/Windows artifact pipelines, integrity metadata and clean-machine smoke tests. Enable each OS separately when it passes.
- [ ] **W.06** Publish and test Homebrew/WinGet channels; link direct-download alternatives and verified update/uninstall docs.
- [ ] **W.07** Publish versioned container images and safe local/team deployment guides; verify persistence and restore.
- [ ] **W.08** Build Helm chart and operator docs after phase 11 prerequisites; test install/upgrade/rollback/uninstall storage retention, failure and ownership behavior.
- [~] **W.09** First pass done (1 Nov 2026): getting-started, spreadsheets, big-tables, self-hosting, CLI/file/versioning reference, releases, contribute, support and blog pages exist in `site/`. Remaining: keep them synced with the release and expand shortcut/compatibility detail.
- [ ] **W.10** Create support page, activate an approved provider profile, verify recipient and then add FUNDING.yml. Owner completes identity/payout onboarding.
- [ ] **W.11** Add link/metadata/manifest validation, mobile/keyboard/theme testing, code-example smoke tests and static deployment CI. No tracker required; document any analytics chosen later.
- [ ] **W.12 — Website gate** Every enabled install command works on its advertised clean platform; destinations/checksums/version labels match the release; screenshot claims are accurate; funding links lead to the approved recipient. Planned channels remain clearly unavailable.

Suggested rollout: developer preview site → tested native binaries and Docker → package-manager convenience → Kubernetes beta. Website work can proceed before all channels exist; presenting a polished nonexistent installer cannot.

## 8. Checks and open decisions

Test homepage at narrow/mobile/desktop widths, reduced motion, keyboard-only tabs/copy buttons, both themes and 200% zoom. Verify external links, alt text, canonical/social metadata and deploy-base paths. Test failed clipboard writes with manual-copy fallback. Navigation remains usable without client JS; scripts are limited to install selection, copy, theme and docs search.

Installation acceptance includes version output, loopback launch, a tiny sample import, formula/edit, durable reopen, clean shutdown, update preservation and documented uninstall behavior. Helm tests run in a disposable cluster. No CI test creates a 100-million-row demo by default.

Owner decisions needed before public launch: canonical domain/repo, release maturity, package publisher identity, supported OS matrix, signing resources, funding recipient and original-brand artwork reuse terms. These do not block designing the site.

## Sources

All consulted 31 October 2026; recommendations and command templates above are Alephgrid proposals, not claims of existing packages.

- [S1: Astro Starlight](https://starlight.astro.build/) — static documentation foundation and bundled navigation/search/theme features.
- [S2: Homebrew taps](https://docs.brew.sh/How-to-Create-and-Maintain-a-Tap) — external formula distribution.
- [S3: Microsoft WinGet package submission](https://learn.microsoft.com/en-us/windows/package-manager/package/) — manifest and submission process.
- [S4: Zero to JupyterHub installation](https://z2jh.jupyter.org/en/stable/jupyterhub/installation.html) — Kubernetes prerequisites, chart configuration and installation structure.
- [S5: Helm OCI registries](https://helm.sh/docs/topics/registries/) — chart distribution and version-specific caveats.
- [S6: GitHub Sponsors overview](https://docs.github.com/en/sponsors/getting-started-with-github-sponsors/about-github-sponsors) — one-time/monthly sponsorship.
- [S7: Receiving GitHub sponsorships](https://docs.github.com/en/sponsors/receiving-sponsorships-through-github-sponsors/about-github-sponsors-for-open-source-contributors) — eligibility/onboarding.
- [S8: GitHub sponsor button](https://docs.github.com/en/repositories/managing-your-repositorys-settings-and-features/customizing-your-repository/displaying-a-sponsor-button-in-your-repository) — FUNDING.yml.
- [S9: Open Collective fiscal-host documentation](https://github.com/opencollective/documentation/blob/main/fiscal-hosts/fiscal-hosts.md) — hosting arrangements and host-specific policies.
- [S10: Open Source Collective contribution process](https://docs.oscollective.org/for-donors-companies-organizations-and-individuals/supporting-projects) — handling hosted contributions and tax-deductibility statement.
