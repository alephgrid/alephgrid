# Alephgrid

An open-source spreadsheet for data that doesn't fit in a spreadsheet: open CSV, Parquet,
JSON or Excel files with hundreds of millions of rows, use Excel formulas and charts next to
them, and SQL when you want it. It runs on your machine; your data never leaves it.

```bash
npx alephgrid flights.parquet      # opens http://127.0.0.1:7878 with the file loaded
npm install -g alephgrid           # or pnpm add -g alephgrid / yarn global add alephgrid / bun add -g alephgrid
alephgrid --demo-rows 10000000     # try it on 10 million generated rows
```

This package installs the prebuilt binary for your platform (Linux x64/arm64 with glibc,
macOS Apple silicon/Intel). On Windows, use Docker or WSL:
`docker run -p 7878:7878 ghcr.io/alephgrid/alephgrid`.

Documentation, source and issues: <https://github.com/alephgrid/alephgrid>. Apache-2.0.
