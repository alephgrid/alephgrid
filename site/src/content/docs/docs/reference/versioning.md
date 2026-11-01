---
title: Versioning policy
description: How Alephgrid versions releases, storage formats and documentation.
---

Alephgrid follows [Semantic Versioning](https://semver.org).

## Before 1.0

- A **minor** version (0.x → 0.y) may change the workbook storage format. Such versions
  migrate workbooks on open and keep the old file, so you can roll back. Details in the
  [operations guide](https://github.com/alephgrid/alephgrid/blob/main/docs/OPERATIONS.md).
- A **patch** version never changes the storage format.
- Only the latest release receives fixes.

## Release channels

Each release publishes its notes on the [releases page](../../../releases/) and in the
repository [CHANGELOG](https://github.com/alephgrid/alephgrid/blob/main/CHANGELOG.md). A
distribution channel (binaries, Homebrew, WinGet, container image, Helm) is listed as ready
only when its artifacts exist with checksums and have passed a clean-machine install.

## Documentation

Behavioral documentation on this site describes the latest release. Where a feature is
pre-release or partial, the page says so explicitly rather than describing the roadmap as the
present.
