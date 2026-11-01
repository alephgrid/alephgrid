# Alephgrid website

Public site: landing, install, releases, docs and blog. Astro + [Starlight](https://starlight.astro.build/)
(docs + blog chrome) with custom landing pages. This is **not** the application UI — that lives in `web/`.

## Develop

```bash
cd site
npm install
npm run dev        # http://localhost:4321/alephgrid/
npm run build      # static output in dist/
npm run preview
```

## Deployment and the custom domain

`.github/workflows/site.yml` builds and deploys to GitHub Pages on pushes to `main` that touch
`site/**`. Defaults assume the project-pages URL `https://alephgrid.github.io/alephgrid/`.

When the custom domain is bought, set two repository **Actions variables** — no code change:

- `SITE_URL=https://<your-domain>`
- `SITE_BASE=/`

and configure the domain under Settings → Pages. Every internal link is base-relative, so the
switch is atomic.

## Honesty rules (from docs/WEBSITE_LAUNCH_PLAN.md)

- `src/data/releases.json` is the single source of truth for install/download UI. A channel shows
  as ready **only** when `published: true` with real artifacts; never make a channel look ready by
  editing page copy.
- No fabricated stats, logos, sponsor lists or benchmark counters. Screenshots are real product
  screenshots from `assets/screenshots/`.
- The support page carries no payment link until a verified funding recipient exists.

## Content layout

- `src/pages/*.astro` — landing, install, releases, contribute, support (marketing chrome).
- `src/content/docs/docs/**` — documentation (Starlight chrome, sidebar in `astro.config.mjs`).
- `src/content/docs/blog/**` — blog posts (starlight-blog; add `title`, `date`, `authors`, `excerpt`).
- Brand: tokens in `src/styles/custom.css` (light paper/forest/emerald; dark neutral charcoal —
  green only as accent), Manrope in `src/fonts/` (OFL license alongside).
