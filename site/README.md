# Alephgrid website

Public site: landing, install, releases, docs and blog. Astro + [Starlight](https://starlight.astro.build/)
(docs + blog chrome) with custom landing pages. This is **not** the application UI — that lives in `web/`.

## Develop

Requires Node.js 22.23.4 or newer (Astro 7 refuses to start on older versions). Cloudflare
Workers Builds' default Node (24.x) already satisfies this.

```bash
cd site
npm install
npm run dev        # http://localhost:4321/
npm run build      # static output in dist/
npm run preview
```

## Deployment and the custom domain

Hosting is **Cloudflare Workers static assets** (`wrangler.jsonc`); the application's CI stays
on CircleCI and never builds the site.

- **Connect once** in the Cloudflare dashboard: Workers & Pages → import the GitHub repo →
  root directory `site`, build command `npm ci && npm run build`, deploy command
  `npx wrangler deploy`, **build watch paths `site/**`** — pushes that don't touch `site/`
  skip the build.
- **Manual deploy:** `cd site && npm ci && npm run build && npx wrangler deploy`.
- Until the domain exists the site serves at the Worker's `*.workers.dev` URL, at the root
  path. When the custom domain is bought (Cloudflare Registrar), add it to the Worker and set
  the build variable `SITE_URL=https://<your-domain>` — no code change.
- `SITE_BASE` defaults to `/`; set it only for a host that serves under a subpath (e.g. a
  GitHub Pages project site). Every internal link is base-relative either way.

## Honesty rules

- `src/data/releases.json` is the single source of truth for install/download UI. A channel shows
  as ready **only** when `published: true` with real artifacts; never make a channel look ready by
  editing page copy.
- No fabricated stats, logos, sponsor lists or benchmark counters. Screenshots are real product
  screenshots: copies of `assets/screenshots/` in `src/assets/`.
- The support page carries no payment link until a verified funding recipient exists.

## Content layout

- `src/pages/*.astro` — landing, install, releases, contribute, support (marketing chrome).
- `src/content/docs/docs/**` — documentation (Starlight chrome, sidebar in `astro.config.mjs`).
- `src/content/docs/blog/**` — blog posts (starlight-blog; add `title`, `date`, `authors`, `excerpt`).
- Brand: tokens in `src/styles/custom.css` (light paper/forest/emerald; dark neutral charcoal —
  green only as accent), Manrope in `src/fonts/` (OFL license alongside).
