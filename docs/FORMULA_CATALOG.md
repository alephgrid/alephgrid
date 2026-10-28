# Built-in formula catalog (phase F, first increment)

Choose **Formula help** in a workbook to search the 562-name comparison inventory. Search
matches names, categories and reviewed descriptions; availability filters distinguish names
recognized by IronCalc from unrecognized names. Lists contain at most 40 entries per page.
The help is read-only and available without SQL access.

The single versioned source is `crates/rowfinity-engine/data/function_catalog.json`.
It records schema/catalog versions, the pinned engine, competitor provenance, priority,
registration status, table support, signature, argument-mode review, examples, limitations,
evidence and planned implementation/upstream routes. The browser imports this source directly;
engine tests check the same artifact against the pinned dependency and evaluate its examples.
There is no second manually maintained runtime catalog.

## What the first increment establishes

- All 562 names are checked against the English registry in IronCalc 0.9.1. Of these, 486
  are registered and 76 are not registered under the exact catalog name.
- All 44 P0 entries have reviewed signatures, original descriptions and hand-calculated
  examples. The examples use the displayed A1:C4 fixture and are checked through Rowfinity's
  actual sheet adapter before and after reopening a file-backed workbook.
- Additional lookup tests cover reverse duplicate selection, exact missing values, a next-larger
  match, wildcard matching, range/index errors, source recalculation and undo.
- The formula bar offers the 44 reviewed built-ins when editing a sheet. Tab accepts a
  suggestion; argument help appears after the opening parenthesis. Workbook functions remain
  available. Ordinary sheet text and quoted literals/references do not trigger suggestions.
- Sheet built-ins are not offered as SQL/table expressions. Table formula compilation is future
  phase F work. Existing DuckDB SQL functions and custom functions remain separate capabilities.

**This is not complete P0 certification.** A registry entry does not certify every argument mode
or Excel/Google Sheets parity. An example proves only its specified case. Unreviewed signatures
remain absent and their help says so; those entries are excluded from built-in autocomplete.
The catalog's table status refers to spreadsheet syntax over DuckDB table references, not the
availability of similarly named SQL functions.

## Updating the catalog

1. Change schema_version when the data contract changes; increment catalog_version for an
   evidence/content change. Review engine upgrades explicitly against Cargo.lock.
2. Update competitor listings from official sources, keeping absent listings separate from
   a claim that the competitor does not support the function.
3. Add signature, modes and examples only after review. Examples need independently derived
   expected values and the documented shared fixture; do not copy outputs from the evaluator
   and treat them as an independent oracle.
4. Run the catalog engine tests. Record limitations; fix actual mismatches or mark the entry
   partial rather than changing expected values merely to match the engine.
5. Run browser coverage after changing help/completion. Preserve custom-function suggestions,
   quoted-text behavior, accessibility and the sheet/table boundary.

Broader argument-mode coverage, reference picking/locking, named ranges, command shortcuts,
large-table compilation, cross-engine recalculation and the analyst gate remain unchecked in
[the implementation checklist](IMPLEMENTATION_CHECKLIST.md). Research context is in the
[comparison matrix](FORMULA_COMPATIBILITY_MATRIX.md) and [execution design](SPREADSHEET_COMPETITIVENESS.md).

## Known XLOOKUP array-input gap

During this increment the browser evaluated `=XLOOKUP(2,{1;2;2},{10;20;30})` as `#N/A`;
the independent expected answer is `20`. Range-based lookup over the same values passes.
The desired-result regression `xlookup_inline_arrays_should_find_a_present_key` is explicitly
ignored in the passing baseline and can be run with `--ignored` during upstream triage.
It is not evidence of supported behavior and must not be removed just to obtain a green suite.
[Upstream issue 1338](https://github.com/ironcalc/IronCalc/issues/1338) is related; test current
upstream before deciding whether to extend that report or propose a separate fix. No issue or
upstream PR was sent as part of this increment. F.02 remains incomplete.

The next editor increment adds [F4 reference locking, same-sheet range picking and reference hints](FORMULA_EDITOR.md). Broader F.03 capabilities remain pending.
