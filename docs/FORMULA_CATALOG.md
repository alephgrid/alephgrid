# Built-in formula catalog (phase F, first increment)

Choose **Formula help** in a workbook to search the 562-name comparison inventory. Search
matches names, categories and reviewed descriptions; availability filters distinguish names
recognized by IronCalc from unrecognized names. Lists contain at most 40 entries per page.
The help is read-only and available without SQL access.

The single versioned source is `crates/alephgrid-core/data/function_catalog.json`.
It records schema/catalog versions, the pinned engine, competitor provenance, priority,
registration status, table support, signature, argument-mode review, examples, limitations,
evidence and planned implementation/upstream routes. The browser imports this source directly;
engine tests check the same artifact against the pinned dependency and evaluate its examples.
There is no second manually maintained runtime catalog.

## What the first increment establishes

- All 562 names are checked against the English registry in IronCalc 0.9.1. Of these, 486
  are registered and 76 are not registered under the exact catalog name.
- All 44 P0 entries have reviewed signatures, original descriptions and hand-calculated
  examples. The examples use the displayed A1:C4 fixture and are checked through Alephgrid's
  actual sheet adapter before and after reopening a file-backed workbook.
- Additional lookup tests cover reverse duplicate selection, exact missing values, a next-larger
  match, wildcard matching, range/index errors, source recalculation and undo.
- The formula bar offers the 44 reviewed built-ins when editing a sheet. Tab accepts a
  suggestion; argument help appears after the opening parenthesis. Workbook functions remain
  available. Ordinary sheet text and quoted literals/references do not trigger suggestions.
- Sheet built-ins are not offered as SQL expressions; DuckDB SQL functions and custom functions
  remain separate capabilities.
- **Table support** (schema 4): `table_status` is `supported` for the 52 functions spreadsheet
  table formulas compile ([table formulas](TABLE_FORMULAS.md)), each with `table_modes` (what is
  supported, e.g. exact-match XLOOKUP only, whole columns for SUMIFS) and `table_evidence`, the
  differential or golden tests that exercise it. A catalog test checks both directions: every
  cited test exists and uses the function, and `supported` matches exactly the names the
  compiler accepts. The help shows the modes and tests, and can list only supported functions.

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
[the implementation checklist](IMPLEMENTATION_CHECKLIST.md). Formulas over large tables are
specified in [table formulas](TABLE_FORMULAS.md).

## XLOOKUP array-input gap (resolved locally)

During the first increment the browser evaluated `=XLOOKUP(2,{1;2;2},{10;20;30})` as `#N/A`;
the independent expected answer is `20`. The local `alephgrid-compat-1` patch now returns `20`
and `xlookup_inline_arrays_should_find_a_present_key` runs without `--ignored`. The patch covers
one-dimensional inline lookup arrays with equally shaped inline/range results only; see
[FORMULA_GAPS.md](FORMULA_GAPS.md). [Upstream issue 1338](https://github.com/ironcalc/IronCalc/issues/1338)
is related. No upstream issue or PR has been sent, and current upstream has not been reproduced.

The next editor increment adds [F4 reference locking, same-sheet range picking and reference hints](FORMULA_EDITOR.md). Broader F.03 capabilities remain pending.

## Mode-level certification increment (F.02)

Catalog schema 2 adds `verification_cases` to each entry. Each case has a unique ID,
mode description, formula, independently calculated expected display text, status
(`verified` or `known_gap`) and a limitation note. The existing A1:C4 fixture applies;
A5:A6 are empty. Empty expected strings mean an empty displayed result, not an absent
formula. The fixture occupies columns A–C and test formulas occupy column D.

The 121 cases span all 44 P0 functions. They cover exact/approximate lookups,
first/last duplicate matches, sorted binary-search examples, missing values and
invalid ranges; conditional aggregate criteria; error handling and lazy IF branches;
text/empty-string/Unicode BMP cases; signed rounding; leap days and month/year rollover.
Tests evaluate the actual sheet adapter and repeat assertions after reopening the
file-backed workbook. The original 44 examples and source-edit/undo tests remain.

Formula help exposes the exact cases in an expandable **Mode evidence** section,
including expected results and known gaps. A passing case certifies that input only.
In particular, the binary-search cases use correctly sorted input and unique search
keys; they do not promise duplicate tie behavior or correctness on unsorted input.
BMP examples do not certify emoji/grapheme handling. Locale/date-system parity,
complete coercion matrices, all optional arguments and broad performance certification
remain future work. F.02 and F.T01 remain unchecked overall.

The local `alephgrid-compat-1` patch resolves the six original gaps: **all 121 cases pass**. This certifies those inputs only; broader P0 mode/coercion coverage remains incomplete.
The original failures and remaining upstream work are recorded in the
[minimal reproducers and upstream triage plan](FORMULA_GAPS.md). Known gaps never
receive a verified label; each has a separately runnable desired-result regression.

## Coercion and argument-mode matrix (F.01/F.02, schema 3)

Schema 3 adds per-case `group` (`modes` or `coercion`) and `oracle` (the rule the expected
value comes from), and per-argument documentation for every P0 entry: each argument's
`rule` cites the verification cases that exercise it. The test checks that argument names
match the reviewed signature and that every cited case exists for that function. The 518
entries without a reviewed signature remain explicitly "Not reviewed" with no argument data.

Coercion inputs are F1:G8 of the shared fixture: number 2, text `'2`, TRUE, an empty cell,
`=NA()`, `=""`, `Ärger`, -3.5, and `a*b`/`a?b`/`axb`. A separate test checks these types
with ISTEXT/ISBLANK/ISLOGICAL/ISNA before and after reopen.

The 156 coercion cases cover: numbers/text/logical/blank/error in ranges versus direct
arguments for SUM, AVERAGE, COUNT, COUNTA, MIN and MAX; numeric-text and logical conversion
in scalar functions; logical conditions given text, blanks and errors; criteria with empty
strings, logical values, error text, numeric text, Unicode case folding, wildcards and `~`
escapes; text conversion of numbers, logical values and blanks; Unicode character counts;
date serial boundaries and truncated month arguments; and lookup keys that must not convert
text to numbers. 152 pass; four are accepted date-system gaps, see [FORMULA_GAPS.md](FORMULA_GAPS.md).

Totals: 277 cases (273 verified, 4 accepted gaps). Still not covered: locale-specific parsing,
the 1904 date system, all optional-argument combinations, emoji/grapheme clusters,
array/spill results of P0 functions, and numerical precision at extremes.
