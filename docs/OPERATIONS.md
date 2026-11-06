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

## Health and readiness

Both need no sign-in and say only which check failed (no paths, sizes or error text).

| Endpoint | Use it for | Answers |
| --- | --- | --- |
| `GET /healthz` | liveness: restart the process if it stops answering | `200 {"status":"ok"}` while the process serves HTTP; no disk or database access |
| `GET /readyz` | readiness: send traffic only when ready; alerting | `200` with `status` `ok` or `degraded`, or `503` with `unavailable`, and each check |

`/readyz` checks:

- `accepting_work`: the server is not shutting down (it turns `failed` on SIGTERM, while
  running work drains);
- `data_dir_writable`: a small file can be created, written and removed in
  `<data dir>/.control/`;
- `control_db`: the control database answers a query;
- `disk_reserve`: `held`, `off` (`--disk-reserve 0`) or `released`. Released means the disk
  filled up (or had no room for the reserve at start). The answer is then `degraded` but still
  `200`, so administrators can reach the server to free space; alert on it.

The image has no curl: `alephgrid health` requests `/readyz` on `127.0.0.1:$ALEPHGRID_PORT`
and exits 0 when ready, 1 otherwise (`--live` for `/healthz`, `--url` for another address).
Both Dockerfiles use it as their `HEALTHCHECK`.

## Logs

Logs go to standard output. `--log-format json` (or `ALEPHGRID_LOG_FORMAT=json`) writes one
JSON object per line for a log collector; the default `text` is for people. Levels follow
`RUST_LOG` (default `alephgrid=info,tower_http=warn`; `tower_http=debug` adds one line per
request with its status and latency).

- Every response carries an `x-request-id` header. A proxy's own id is kept when it is short
  and plain (letters, digits, `-`, `_`, `.`, at most 64); otherwise the server makes one.
- Lines written while handling a request carry a `request` span with `request_id`, `method`
  and `path` (the path holds the workbook id). Bulk jobs add a `job` span with `workbook`,
  `job`, `class` and `lane`; a job stopped by its deadline or a cancel is logged. Workbook
  lifecycle lines (start, migration, drain, failure) carry `workbook`, WebSocket lines
  `session`.
- Logs never carry passwords, tokens, cookies, request headers, query strings or request
  bodies, and workbook contents are not logged, except that a warning may quote an engine
  error message (DuckDB or Python), which can name a value. A support bundle is the log plus `GET /api/admin/resources` and
  `/api/admin/sessions` (instance admins).

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
