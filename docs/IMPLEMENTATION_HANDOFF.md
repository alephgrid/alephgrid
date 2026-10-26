# Implementation handoff

Updated: 26 October 2026. Branch `claude/eager-ramanujan-lns8s1` (from `main` at `bbd4c2c`, phase 3 merged).

## State

Phase 4 (SQL workspace) is complete locally: 4.01–4.08, 4.T01, 4.T02 and the 4.09 gate are checked in
IMPLEMENTATION_CHECKLIST.md with evidence in TEST_MATRIX.md ("## Phase 4": contracts, results, baseline,
gate commands). Phases 1–3 are unchanged except for the fixes listed there (a reader left inside a failed
transaction, internal-table names accepted by table operations, a Views-menu loading race).
CI has not run this branch yet; all results are from a Linux x86_64 cloud container.

Not started: phase 5 and later. Phase 7 (identities, roles, sharing) is not implied by anything here.

Storage format is now **3** (data revisions, saved queries, materialization provenance). Older files migrate
on open with a `.v<N>.bak` copy; `tests/fixtures/v2/` is a real format-2 workbook written by the phase-3
server. `docs/SQL_AND_CUSTOM_FUNCTIONS.md`, named in the checklist header, is not in this repository.

## Verify

```bash
cargo test --workspace                                   # PROPTEST_CASES=96 in CI
ROWFINITY_SMALL_FS=<64 MB fs> cargo test --workspace --test storage_faults
cargo clippy --workspace --all-targets -- -D warnings
cd web && npm run build && cd .. && cargo build --workspace --all-targets
cd web && npx playwright test                            # 41 tests; needs Google Chrome, or:
E2E_CHROME_PATH=/opt/pw-browsers/chromium-1243/chrome-linux64/chrome npx playwright test
```
Use `--workspace` for every cargo command: `-p <crate>` resolves features differently and rebuilds DuckDB
(10+ minutes on 4 cores).

## Known gaps

- `too_large` from query → sheet carries its row limit only in the message text; the UI parses it.
- `query` on an UPDATE/DELETE names the statement kind but does not count affected rows in advance.
- Pre-existing formula columns that use functions now refused (e.g. `current_setting`) still evaluate; the
  function policy applies when a formula is added or changed.
- `runtime.rs::eviction_racing_new_work_never_serves_from_a_closed_workbook` can miss its own "≥ 10
  evictions in 60 s" precondition under heavy CPU contention (seen once at baseline, never since).
- Power loss / host failure, OS-level limits, multi-node: unchanged from DURABILITY.md.

## Temporary resources

A git worktree of `bbd4c2c` and its target directory may remain in the session scratchpad (`baseline`,
`baseline-target`); remove with `git worktree remove --force <path>`. A 64 MB tmpfs may be mounted at
`/mnt/rfsmall`. No servers are left running.
