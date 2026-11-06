# Roadmap

What v0.1.0 ships is in [CHANGELOG.md](../CHANGELOG.md) and, feature by feature, in
[SPREADSHEET_FEATURES.md](SPREADSHEET_FEATURES.md). Task-level status and tests are in the
[implementation checklist](IMPLEMENTATION_CHECKLIST.md); the technical constraints for the
planned work are in [ARCHITECTURE.md](ARCHITECTURE.md#planned-connected-data-clusters-and-extensions).
Nothing below is available yet.

## Next, after v0.1.0

1. **S3 data** (checklist phase 10). Register a bucket and prefix with a credential, preview and
   query Parquet in place (read-only), import a snapshot and refresh it. Credentials stay on the
   server; each space only sees its own connections.
2. **Postgres, read-only** (10.02), after the S3 gate: attach selected schemas, with scan limits
   and results served from local snapshots.
3. **Operations and performance** (phases O and P): per-query diagnostics, background jobs that
   survive restarts, measured caching and a faster wire format for wide results. Each change ships
   only with a before/after measurement.
4. **Kubernetes** (phase 11): workbooks placed on workers automatically, one owner per workbook,
   a Helm chart, and recovery that refuses writes when ownership is uncertain.
5. **Extensions** (phase E): published contracts for runtime providers, data connectors and
   function runtimes, with conformance tests.

Packaging follows the release: Homebrew and WinGet, signed macOS binaries and a native Windows
build come after v0.1.0.

## Being considered

Each needs demand and an evaluation before it is planned (checklist phase 12): Google Cloud
Storage and Azure, warehouses through ADBC, DuckLake snapshots (needed to replay agent results
against old data), writing back to databases, a desktop app, a browser-only mode for small files,
Python notebooks, and an add-in for using Alephgrid functions in Excel. An in-app AI assistant
is deliberately not in v1; agents use workbooks through [MCP](MCP.md).

## Not planned

Printing and page setup, and macros or VBA. See
[SPREADSHEET_FEATURES.md](SPREADSHEET_FEATURES.md#sheets-files-and-collaboration).
