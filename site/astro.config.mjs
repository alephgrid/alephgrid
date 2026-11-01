// @ts-check
import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';
import starlightBlog from 'starlight-blog';

// Cloudflare Workers static hosting: the site serves at the root of
// https://alephgrid.dev (SITE_URL overrides for previews; SITE_BASE exists
// only for hosts that serve under a subpath, e.g. GitHub Pages project sites).
const site = process.env.SITE_URL || 'https://alephgrid.dev';
const base = process.env.SITE_BASE || '/';

export default defineConfig({
  site,
  base,
  integrations: [
    starlight({
      title: 'Alephgrid',
      description:
        'An open-source spreadsheet for data that doesn’t fit in a spreadsheet. Hundreds of millions of rows, real Excel formulas, SQL when you want it. Runs on your machine; your data never leaves it.',
      logo: {
        light: './src/assets/alephgrid-horizontal-primary.svg',
        dark: './src/assets/alephgrid-horizontal-inverse.svg',
        replacesTitle: true,
      },
      favicon: '/favicon.svg',
      customCss: ['./src/styles/custom.css'],
      social: [
        { icon: 'github', label: 'GitHub', href: 'https://github.com/alephgrid/alephgrid' },
      ],
      editLink: {
        baseUrl: 'https://github.com/alephgrid/alephgrid/edit/main/site/',
      },
      lastUpdated: true,
      plugins: [
        starlightBlog({
          title: 'Blog',
          authors: {
            alephgrid: {
              name: 'The Alephgrid maintainers',
              url: 'https://github.com/alephgrid/alephgrid',
            },
          },
        }),
      ],
      sidebar: [
        {
          label: 'Getting started',
          items: [
            { label: 'Install and first workbook', slug: 'docs/getting-started' },
          ],
        },
        {
          label: 'Guides',
          items: [
            { label: 'Spreadsheets and formulas', slug: 'docs/spreadsheets' },
            { label: 'Big tables', slug: 'docs/big-tables' },
            { label: 'Self-hosting', slug: 'docs/self-hosting' },
          ],
        },
        {
          label: 'Reference',
          items: [
            { label: 'CLI and configuration', slug: 'docs/reference/cli' },
            { label: 'File compatibility', slug: 'docs/reference/files' },
            { label: 'Versioning policy', slug: 'docs/reference/versioning' },
          ],
        },
      ],
      head: [
        { tag: 'meta', attrs: { property: 'og:image', content: `${site}${base === '/' ? '' : base}/opengraph.png` } },
        { tag: 'meta', attrs: { name: 'twitter:card', content: 'summary_large_image' } },
      ],
    }),
  ],
});
