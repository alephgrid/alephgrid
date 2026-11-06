# Python functions

A workbook can define functions in Python and call them from sheet cells
(`=NORMALIZE_SKU(A2)`, `=SPREAD(B2:B40)`) and table columns (`=NORMALIZE_SKU([sku])`), like its
formula and SQL functions ([FUNCTIONS.md](FUNCTIONS.md)): defined, tested, published as versions, pinned. This page is the contract.

## How a call is computed

Python never runs inside the spreadsheet engine or the database, and never while the workbook
is locked.

1. **Evaluation reads a result cache.** A Python function is a pure function of its arguments,
   so each result is kept by *(function, version, runtime, exact arguments)* in the workbook
   (`_rf_fn_results`, storage format 9). The sheet engine asks the cache through a hook
   (Alephgrid patch 2 of IronCalc, see `vendor/ironcalc_base/ALEPHGRID_PATCHES.md`); a table
   column is a lookup in it (a DuckDB macro joined by DuckDB, not one query per row).
2. **A missing result shows `#CALC!` "Calculating NAME…"** and the call is queued.
3. **A background job computes what is missing in batches**: for a sheet, the calls the
   evaluation queued; for a table column, its distinct argument tuples not yet in the cache,
   read from a snapshot, at most 1,000 calls (and 4 MiB) per batch. The job runs the batches in an
   isolated worker process, outside any workbook lock.
4. **Results are committed only if they still apply**: the version used must still be the pinned
   one, and Python must still be allowed for the workbook. Otherwise they are discarded. Committing
   writes the cache, then re-evaluates sheets and refreshes table views. It is not a revision of
   the workbook: it changes no input.
5. A second request for a result that is being computed waits for that job; the same batch
   delivered twice writes the same results once.

Reopening a workbook shows its cached results at once. A table of a million rows with 100,000
distinct values makes 100 batches, not a million calls (benchmark in TEST_MATRIX.md).

## Definition

`FunctionDefinition {kind: "python", name, arguments, source, returns, volatile, documentation,
tests}`. `source` is Python that defines a function with the name in lower case
(`NORMALIZE_SKU` → `def normalize_sku(sku):`); it may import the standard library and the
packages of the runtime. `returns` is `number`, `text` or `boolean`. Tests run in the same worker
as real calls, and publishing needs them to pass, as for other functions. Test inputs can be typed
(a blank cell is `None`, exact text is `str`, an error input is not called; see
[FUNCTIONS.md](FUNCTIONS.md#test-values)), and **Cancel** kills a test's worker.

## Values in and out

| Argument | Python receives |
|---|---|
| a number in a sheet | `float` |
| an integer column (≤ 2^53 or any `BIGINT`) | `int` |
| `DOUBLE` column | `float` |
| `DECIMAL` column | `decimal.Decimal`, exact |
| text | `str` |
| logical | `bool` |
| an empty cell or `NULL` | `None` |
| a date (`DATE` column) | `datetime.date` |
| other column types (timestamps…) | `str`, as DuckDB writes them |
| a range or array (sheets) | `list` of rows, each a `list` of the values above |
| an error | not called: the result is that error |

| Return (`returns`) | Accepted | Becomes |
|---|---|---|
| `number` | `int`, `float`, `Decimal` (`bool` is refused) | a number; NaN or infinity → `#NUM!`; an integer beyond 2^53 → `#NUM!` |
| `text` | `str` | text (at most 32,767 characters, else `#VALUE!`) |
| `boolean` | `bool` | a logical |
| any | `None` | empty text in a sheet, `NULL` in a table |
| any | anything else | `#VALUE!` "the function returned a list, expected a number" |

An exception becomes `#VALUE!` with the exception's type, first line and the line of the source
where it happened (`ValueError: bad sku (line 3)`) as the cell's message, and is cached like any
result (the function is pure). A test shows the same message, and the editor marks that line. Running out of time, memory or output, or a
worker crash, becomes `#CALC!` with the reason; those are not cached for good: **Refresh**
(`refresh_functions`) tries again. In a table, a pending call or an error is `NULL`.

**Where they can be called.** In sheet cells, anywhere in a formula. In a table, as the whole
formula of a calculated column (SQL language), `=NORMALIZE_SKU([sku])`, whose arguments may be
any SQL expressions over the row; a formula that uses the call inside a larger expression is
refused, so every call can be found and computed. SQL queries can call the function too but
read only results already computed.

## Isolation and limits

* **Who may run them.** Publishing, pinning and running tests need the space's publish role.
  An organization admin can turn Python off for a space (`PATCH /api/spaces/{id}`
  `{python: false}`): open workbooks stop at once, results still being computed are not
  stored, and cells say why.
* **Isolated worker processes.** Each job runs one worker process under an OS sandbox: Seatbelt
  (`sandbox-exec`) on macOS, bubblewrap on Linux. The sandbox allows reading only the Python
  runtime and the worker's private, empty working directory, writing nothing, no network (no
  sockets, no DNS), and no new programs or processes (threads are allowed). On Linux, `/tmp` is
  empty and read-only, and the worker installs a seccomp filter before it reads the function's
  code that refuses `execve`, `fork`, `vfork` and any `clone` that is not a thread; a worker that
  cannot install it refuses to run. The environment is cleared: no
  credentials, tokens or paths of the server are inherited. A plain subprocess is not enough:
  without a sandbox available the server refuses to run Python (fail closed), unless started with
  `--python-isolation none` in `--auth local` mode, for one trusted person, with a warning.
* **Limits per job** (`--python-timeout`, `--python-memory`, `--python-output`): wall time, CPU
  time, memory (enforced by the OS where it can be, otherwise by a watchdog that samples the
  worker and kills it) and bytes of output. At most `--python-workers` workers run at once on the
  server; jobs wait their turn. Cancelling a recalculation, closing the workbook or a limit
  reached kills the worker's process group.
* Other workbooks are unaffected by a slow, crashing or hostile function: workers run outside the
  server process and outside any workbook's lock.

## Runtimes and packages

A **Python runtime** is a directory made by `alephgrid python-runtime create`: a Python
interpreter, the packages installed from a local folder of wheels (`--wheels`, offline, hashes
required) and `runtime.json` (Python version, every package with its version and SHA-256). Its id
is a hash of that manifest. The server uses one runtime (`--python-runtime DIR`) and lists it in
its `RuntimeSpec` (component `python`, capability `python`).

Each function version records the runtime id it was published with. A workbook whose versions
need another runtime still opens: cached results show, new calls show `#NAME?` "needs Python
runtime X; this server has Y". Data is never at stake.

## Volatile functions

`volatile: true` marks a function whose result may change without its arguments changing (it
reads the clock or randomness). Its results are computed once and reused by automatic
recalculation; only **Refresh** (`refresh_functions`) computes them again. Workers have no network
or files, so no function can have side effects outside its own result.

## Excel files

Python functions are not Excel functions. An XLSX export keeps formulas by default: Excel shows
`#NAME?` in cells that call a Python function, and the compatibility report lists them. The
**cached values** export option writes those cells as their current values instead.

## Limits

* Python functions return one value per call (no arrays that spill).
* The cache keeps the results of every version, so pinning an older version again does not
  compute again; there is no size-based eviction yet. Pinned versions' results are loaded into
  memory when the workbook opens.
* Throughput (debug build, Apple M-series): a column over 1,000,000 rows with 100,000 distinct
  values fills in about 12 s (100 batches, 2 workers); reading it afterwards takes about 60 ms.
* The Linux sandbox needs `bwrap` on the host (or in the image).
