# Rowfinity compatibility patch set 1

Base: crates.io ironcalc_base 0.9.1, copied from the locally cached published crate.
License: MIT OR Apache-2.0; original license files and third-party notices retained.
Owner: Rowfinity maintainers. Status: local patch, not an upstream-approved release.

Changes:
- COUNTIFS checks both range dimensions against the first criteria range.
- Shared IFS aggregation checks criteria dimensions against the result range (SUMIFS,
  AVERAGEIFS, MINIFS and MAXIFS).
- TEXTJOIN with ignore_empty skips scalar and range empty strings. This is Rowfinity's
  explicit contract; it is not a claim of independently measured Excel parity.
- VLOOKUP distinguishes nonpositive indexes (#VALUE!) from out-of-bounds indexes (#REF!).
- XLOOKUP accepts array-valued lookup vectors with same-shape array/range results,
  reusing the existing search algorithms. Existing range lookup behavior is retained.

Regression evidence lives in rowfinity-engine tests::catalog. Keep the upstream
source diff small; do not format or change unrelated dependency code.
Related XLOOKUP issue: https://github.com/ironcalc/IronCalc/issues/1338
No issue/PR has been submitted and current upstream has not yet been reproduced.

Removal: adopt a pinned upstream release/commit only when all desired-result and
source-edit/undo/reopen regressions pass without this patch, review the upstream
source diff, then remove this directory and the root Cargo patch declaration.
Review this patch before each dependency upgrade and before the F.15 release gate.

Original crates.io archive checksum recorded in the pre-patch lockfile:
`ecf55ea346dcda5e0f0034d8300dc2d40ffbcb64e7c4db23bfa1d8a0cf11c738`.
This identifies the original archive, not the patched directory.
