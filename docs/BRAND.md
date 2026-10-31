# Brand assets: provenance

What the product ships for its identity, and where each piece comes from (checklist U.13).

| Asset | In the repository | Origin |
|---|---|---|
| Mark (open cell with fill handle) | `web/src/icons.ts` `brandMark`, `web/public/favicon.svg`, `assets/brand/` | The selected-cell concept was supplied by the project owner; the SVG geometry was refined in brand kit v1 (`alephgrid-brand/brand-kit-v1`, October 2026). The app draws the kit's favicon geometry without its tile, colored by theme tokens. |
| Wordmark ("alephgrid") | `assets/brand/*.svg` (README), text in the app | Brand kit v1; logo lettering is outlined from Manrope. In the app the wordmark is live text in Manrope. |
| Typeface | `web/public/fonts/Manrope-variable.woff2` with `OFL-Manrope.txt` | Manrope, SIL Open Font License 1.1, from google/fonts (retrieved 30 October 2026). Keep the OFL notice with the font. |
| Interface icons | `web/src/icons.ts` | Drawn for Alephgrid on a 24-unit grid; not copied from an icon set. |
| Colors | `web/src/styles.css`, [DESIGN.md](DESIGN.md) | Brand kit v1 tokens (forest, emerald, mint, paper, ink) and the UI direction v2 neutral dark palette. |

## Not shipped

The UI direction v2 boards (`alephgrid-brand/ui-kit-v2/*.png`) and the brand kit's `concepts/`
are AI-generated design proposals. They guide the design; they are not assets, and they are not
evidence of shipped behaviour. README screenshots are taken from the running product.

## Open decision

Reuse terms for the original brand artwork (outside the font's OFL) are the project owner's
decision and have not been chosen. Choose them before publishing a brand-assets download.
