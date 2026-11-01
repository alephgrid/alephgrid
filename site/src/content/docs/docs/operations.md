---
title: Operations
description: The data directory, backups and restore, upgrades and rollback, durability guarantees, and per-workbook compute.
---

A single `alephgrid` process serves the UI and the API, and all state lives in one data
directory (`--data-dir`, default `./alephgrid-data`). No other service is required.

## What is on disk

| Path | What |
|---|---|
| `<workbook id>/` | One workbook: `workbook.json` and its `data.duckdb` |
| `.trash/` | Workbooks moved to the trash |
| `.control/control.duckdb` | Users, organizations, spaces, roles, sessions, tokens, audit log |
| `.backups/` | Online backups |
| `.uploads/`, `.exports/` | Staged uploads and finished exports (temporary) |
| `.agent-results/` | Pinned agent results and receipts (expire after 24 h) |

Back up the whole directory with the tools below, not with `cp` while the server runs:
a copy of `data.duckdb` taken mid-flight can miss the write-ahead log or catch the file
mid-checkpoint.

## Backups and restore

- **Online**: Administration → Backups → **Back up now** (instance admins), or
  `POST /api/admin/backups`. This copies every workbook as of one revision — a
  consistent snapshot even during writes — and the control database, with SHA-256
  checksums in a manifest. The newest few are kept under `.backups/`.
- **Offline**: with the server stopped:

```bash
alephgrid --data-dir DATA --backup-to /other/disk/backup-YYYYMMDD
```

Copy backups to another device or host. A backup on the same disk does not survive the
disk.

To restore, stop the server and restore into an **empty** directory:

```bash
alephgrid --data-dir /new/data --restore-from /other/disk/backup-YYYYMMDD
```

Checksums are verified and every workbook's revision checked before anything appears in
the new directory. Sessions do not carry over, so people sign in again; service tokens
and agent connections do carry over. Trash and staged uploads are not included, and
workbooks are each consistent on their own — there is no consistency across workbooks.

## Upgrades and rollback

1. Make a backup and copy it off the host.
2. Stop the server, replace the binary, start it again.
3. A workbook written in an older storage format is migrated the first time it opens,
   in one transaction, and the old file is kept next to it as `data.duckdb.v<N>.bak`.

There is no downgrade: an older version refuses files written by a newer format. To roll
back, restore the backup from step 1 with the older binary (the `.bak` file is the
per-workbook way back). Delete `.bak` files once you are satisfied with an upgrade.
Pre-1.0, storage formats change between releases, so treat step 1 as mandatory — the
[versioning policy](../reference/versioning/) says what to expect.

## What survives what

A change is **acknowledged** when the server replies `ok: true` (WebSocket) or 2xx
(HTTP import). By then the data, the next revision and its history entry have committed
in one DuckDB transaction, and DuckDB has written the commit to the workbook's
write-ahead log. A request that was not acknowledged either committed completely or not
at all; clients that retry with the same mutation id are never applied twice.

| Failure | What survives |
|---|---|
| Server crash (SIGKILL, panic, OOM kill) | Every acknowledged change — tested with kills during and between writes |
| Graceful stop (SIGTERM, Ctrl-C) | Everything acknowledged; running work gets `--shutdown-grace` seconds, then rolls back |
| Disk full or unwritable | The failing change rolls back and is reported; everything before it stays readable |
| Host crash or power loss | Expected to keep acknowledged changes if the storage honours flushes — **not yet tested** with power-cut rigs; disks that acknowledge writes before they are durable can lose recent commits |
| Loss or corruption of the storage | Only backups survive |

:::caution
Do not point two hosts at the same data directory. Ownership locks are reliable on one
host's local filesystem, but they are not a fence across hosts and are unreliable on
many network filesystems. A workbook whose ownership is uncertain is refused, never
opened twice.
:::

One process serves every workbook in this release (`shared_process`): per-workbook
DuckDB instances have their own memory, thread and spill limits, but this is not a
resource- or fault-isolation boundary between mutually untrusted tenants. Cap the
process itself with an OS limit (systemd `MemoryMax`, a container limit); Alephgrid
honours a cgroup memory limit when choosing its default budget.

## Per-workbook compute

Opening a workbook finds or starts its compute, and every collaborator is served by that
one allocation. A workbook's data, identity and revision never depend on its compute,
which can stop, restart or change size without changing them.

`--memory-budget` is divided among open workbooks as DuckDB memory limits, and
`--threads` as worker threads; `standard` workbooks share equally, never below
`--min-workbook-memory` and one thread. Fixed sizes are **profiles**:

```bash
alephgrid --profile large=4GiB:4 \
  --profile dedicated=8GiB:8:dedicated   # never closed for room
```

An organization admin grants profiles to a space; a space owner chooses one per
workbook (a dedicated profile needs an organization admin). Requests name a profile id,
never resource amounts. Changing profile is a controlled restart: the allocation drains,
a new one starts at the new size with the same data and revision, and collaborators
reconnect by themselves. If the new size does not fit, the workbook keeps running as it
was.

When the host budget cannot fit another allocation, a start waits in a first-come queue
for up to `--start-queue-timeout` (60 s); the page shows the position in line. Failures
say what to do: `capacity` (server full), `quota` (the space's limit), `incompatible`,
`owned_elsewhere`, or the startup error itself.

### Idle sleep and wake

A workbook's compute closes after `--idle-timeout` (default 10 minutes) without
meaningful activity. Operations people or programs send count, and a running job keeps
the workbook open however long it runs; open tabs, pings and status reads do not.
Closing drains and checkpoints first, so every committed change is in the file, then
releases the memory and thread reservation. Storage is kept as it is. The page shows
the workbook as paused, and any key or click (or **Resume**) reopens it, resending
changes that were not acknowledged.

## Limits

- One process, one machine. Clustered deployment is not part of this release.
- Beyond its memory limit, DuckDB spills to temporary files on disk
  (bounded by `--temp-budget`).
- Sheet grids hold up to Excel's size (1,048,576 × 16,384); tables are bounded by disk.

Runbooks for stuck workbooks, full disks, credential rotation and incident response are
in the repository's
[operations guide](https://github.com/alephgrid/alephgrid/blob/main/docs/OPERATIONS.md);
the full guarantees and their test evidence are in
[DURABILITY.md](https://github.com/alephgrid/alephgrid/blob/main/docs/DURABILITY.md).
