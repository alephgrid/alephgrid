# Operating Alephgrid

A single `alephgrid` process serves the UI and the API. All state lives in one data directory
(`--data-dir`, default `./alephgrid-data`). No other service is required. The guarantees behind
these steps are in [DURABILITY.md](DURABILITY.md), the security model in [SECURITY.md](SECURITY.md)
and roles in [TEAMS.md](TEAMS.md).

## What is in the data directory

| Path | What |
| --- | --- |
| `<workbook id>/` | One workbook: `workbook.json` and its `data.duckdb` |
| `.trash/` | Workbooks moved to the trash |
| `.control/control.duckdb` | Users, organizations, spaces, roles, sessions, service tokens, agent connections, audit log |
| `.backups/` | Online backups made from Administration or `POST /api/admin/backups` |
| `.uploads/`, `.exports/` | Staged uploads and finished exports (temporary) |
| `.python/` | Scratch space of Python functions (when a Python runtime is configured) |
| `.agent-results/` | Pinned agent results and their receipts (expire after 24 h) |

Back up the whole directory with the tools below, not with `cp` while the server runs.

## Backups

- **Online**: Administration → Backups → **Back up now** (instance admins), or
  `POST /api/admin/backups`. This is a consistent copy of every workbook and of the control
  database. Only one backup runs at a time, and only if there is room on the disk. The newest
  few are kept under `.backups/`.
- **Offline**: with the server stopped, run `alephgrid --data-dir DATA --backup-to /other/disk/backup-YYYYMMDD`.

Copy backups to another device or host. A backup on the same disk does not survive the disk.

## Restore

1. Stop the server.
2. Restore into an empty directory:
   `alephgrid --data-dir /new/data --restore-from /other/disk/backup-YYYYMMDD`.
   Checksums are verified, and the restore refuses a directory that is not empty.
3. Start the server on the new directory. Sessions do not carry over, so people sign in again.
   Service tokens and agent connections do carry over.

## Upgrades

1. Make a backup and copy it off the host.
2. Stop the server, replace the binary (or image) and start it again.
3. A workbook written in an older storage format is migrated the first time it opens. The old
   file is kept next to it as `data.duckdb.v<N>.bak`.

Going back to an older version is not supported once a workbook has been migrated: the older
version refuses files that are newer than it understands. To roll back, restore the backup from
step 1 with the older binary.

## Runbooks

### A workbook will not open or is stuck starting

1. Open it. The Compute panel (toolbar → Compute) shows its state and, when it failed, why.
2. **Cancel** stops a start that has not finished. **Restart** stops and starts it again; work that
   was not acknowledged as saved is resent by open clients.
3. Administration → Workbooks and memory lists the open workbooks with their memory, threads and
   jobs. Administration → Sessions lists live connections.
4. If it still fails, stop the server, make an offline backup, and look at the log line naming the
   workbook. Do not delete its directory.

### Disk is full

The server keeps a small **disk reserve** (`.control/reserve`, 32 MiB by default,
`--disk-reserve`). When a change runs out of space it is rolled back, and the server deletes the
reserve at once: a rolled-back change's blocks stay inside the workbook file, so without the
reserve the disk would stay full and the server could not record state or restart. With it, the
server keeps working well enough for you to free space. Administration → Overview shows the free
disk and whether the reserve is held; "released" means the disk filled up. The reserve comes back
by itself once there is room for it and twice as much again.

Backups refuse to start without room for them, and a workbook at its size limit
(`--max-workbook-size`) accepts only changes that remove data. Free space by doing any of:

- empty the trash;
- delete old backups (Administration → Backups);
- remove expired `.exports` and `.agent-results` files (the server also sweeps them).

### Rotate credentials

- **Service tokens**: create the new token (space menu → Service tokens…), switch the client,
  then revoke the old one.
- **Agent connections**: create a new connection, update the MCP client, then revoke the old
  connection.
- **Single sign-on secret**: set the new `ALEPHGRID_OIDC_CLIENT_SECRET` and restart the server.
- **A user who should lose access at once**: an instance admin disables the account, which ends
  its sessions, or an organization admin removes them from the organization.

### Incident: data looks wrong

1. Open the workbook's **History**. Every change shows who made it and when. Table changes can be
   undone as a new change unless someone has changed the same cells since.
2. The audit log (Administration → Audit log, or the organization's Activity) records imports,
   exports, sharing changes and agent tool calls.
3. If a change cannot be undone, restore the most recent good backup into a new data directory
   and copy the workbook across. Restoring is per data directory, so do not restore over the live
   one.

## Limits

- One process and one machine. Workbooks are distributed only in a cluster deployment (not part
  of this release).
- Memory and threads follow `--memory-limit` and `--threads`. Beyond memory, DuckDB spills to
  temporary files on disk.
- Sheet grids hold up to Excel's size (1,048,576 × 16,384). Tables are bounded by disk.
