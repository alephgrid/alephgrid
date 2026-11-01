// Regenerates raster icons and the OpenGraph card from the vector brand
// assets in assets/brand/. Run from site/: node scripts/make-icons.mjs
import sharp from 'sharp';
import { readFileSync, writeFileSync } from 'node:fs';

const icon = readFileSync(new URL('../public/favicon.svg', import.meta.url));
const logo = readFileSync(
  new URL('../../assets/brand/alephgrid-horizontal-primary.svg', import.meta.url),
  'utf8',
);

const out = (name) => new URL(`../public/${name}`, import.meta.url).pathname;

for (const [name, size] of [
  ['app-icon-192.png', 192],
  ['app-icon-512.png', 512],
  ['apple-touch-icon.png', 180],
  ['favicon-32.png', 32],
]) {
  await sharp(icon, { density: 300 }).resize(size, size).png().toFile(out(name));
  console.log('wrote', name);
}

// OpenGraph card: paper canvas, faint worksheet rules, the wordmark centred.
const inner = logo.replace(/<svg[^>]*>/, '').replace('</svg>', '');
const cell = 42;
let rules = '';
for (let x = cell; x < 1200; x += cell) rules += `<line x1="${x}" y1="0" x2="${x}" y2="630" stroke="#E1E8E4" stroke-width="1"/>`;
for (let y = cell; y < 630; y += cell) rules += `<line x1="0" y1="${y}" x2="1200" y2="${y}" stroke="#E1E8E4" stroke-width="1"/>`;
const og = `<svg xmlns="http://www.w3.org/2000/svg" width="1200" height="630" viewBox="0 0 1200 630">
  <rect width="1200" height="630" fill="#F8FAF8"/>
  <g opacity="0.55">${rules}</g>
  <rect x="0" y="0" width="1200" height="6" fill="#047857"/>
  <g transform="translate(286 265) scale(2)">${inner}</g>
</svg>`;
writeFileSync(out('og.svg'), og);
await sharp(Buffer.from(og), { density: 150 }).resize(1200, 630).png().toFile(out('opengraph.png'));
console.log('wrote opengraph.png');
