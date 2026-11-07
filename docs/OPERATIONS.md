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
  `job`, `class` and `lane`; a job stopped by its deadline or a cancel is logged. Allocation
  lifecycle lines (`allocation ready`, `allocation start failed`, `allocation stopping`)
  carry `workbook`, `allocation`, `operation` (the `operation_id` of a runtime request) and
  `reason`; migration lines carry `workbook`, WebSocket lines `session`.
- Logs never carry passwords, tokens, cookies, request headers, query strings or request
  bodies, and workbook contents are not logged, except that a warning may quote an engine
  error message (DuckDB or Python), which can name a value.

## Metrics

`GET /metrics` serves Prometheus text format (version 0.0.4). It answers:

- an instance admin (session cookie). With `--auth local` (loopback only) that is everyone
  who can reach the port;
- a scraper sending `Authorization: Bearer <token>` when `--metrics-token`
  (`ALEPHGRID_METRICS_TOKEN`, at least 16 characters) is set.

Everyone else gets 401 or 403. Without a token, scraping in `--auth accounts` needs an admin
session, so set one for Prometheus:

```yaml
scrape_configs:
  - job_name: alephgrid
    scheme: https
    authorization: { credentials_file: /etc/prometheus/alephgrid-token }
    static_configs: [{ targets: ["grid.example.com"] }]
```

Label values come only from fixed sets in the code (states, reasons, job classes and lanes,
`le`), never from a request: no series names a workbook, space, user, file, SQL text or path,
and the number of series is fixed. Every counter is present from the first scrape, at 0.

| Metric | Type | Labels | What |
| --- | --- | --- | --- |
| `alephgrid_build_info` | gauge | `version` | always 1 |
| `alephgrid_start_time_seconds` | gauge | | process start (Unix time) |
| `alephgrid_ready` | gauge | | 1 when `/readyz` answers 200 |
| `alephgrid_shutting_down` | gauge | | 1 once SIGTERM was received |
| `alephgrid_workbooks_open` | gauge | | workbooks whose compute is ready |
| `alephgrid_allocations` | gauge | `state`: requested, starting, running, ready, draining | live allocations |
| `alephgrid_allocations_queued` | gauge | | starts waiting in line for host capacity |
| `alephgrid_allocation_starts_total` | counter | | starts begun |
| `alephgrid_allocation_start_failures_total` | counter | `reason`: incompatible, capacity, quota, owned_elsewhere, startup_failed, cancelled, lost | starts that did not reach ready |
| `alephgrid_allocation_start_seconds` | histogram | | request to ready |
| `alephgrid_allocation_stops_total` | counter | `reason`: deleted, evicted, requested, shutdown, restarting | evicted = idle, or closed to make room |
| `alephgrid_allocations_lost_total` | counter | | allocations of a previous process marked lost at start |
| `alephgrid_jobs_waiting` | gauge | `lane`: read, write | jobs queued in open workbooks |
| `alephgrid_jobs_running` | gauge | | jobs running |
| `alephgrid_job_wait_seconds` | histogram | `class`: interactive, bulk; `lane` | time in the workbook's queue |
| `alephgrid_job_duration_seconds` | histogram | `class`, `lane` | run time (queries, imports, exports, edits, fetches) |
| `alephgrid_jobs_total` | counter | `class`; `outcome`: ok, failed, cancelled, deadline_exceeded, rejected | rejected = the queue was full |
| `alephgrid_websocket_connections` | gauge | | open sessions |
| `alephgrid_websocket_connections_total` | counter | | sessions opened |
| `alephgrid_imports_total` | counter | `result`: ok, failed | file imports |
| `alephgrid_import_bytes_total` | counter | | bytes of files imported |
| `alephgrid_upload_staged_bytes`, `alephgrid_upload_budget_bytes` | gauge | | staged uploads and their budget |
| `alephgrid_memory_budget_bytes`, `alephgrid_memory_reserved_bytes` | gauge | | `--memory-budget` and what open workbooks reserve of it |
| `alephgrid_threads_budget`, `alephgrid_threads_reserved` | gauge | | the same for engine threads |
| `alephgrid_disk_reserve_state` | gauge | `state`: held, released, off | one of them is 1 |
| `alephgrid_disk_reserve_bytes`, `alephgrid_disk_free_bytes` | gauge | | reserve size, free disk at the data directory |
| `alephgrid_last_backup_timestamp_seconds` | gauge | | newest online backup in `.backups/` (Unix time; 0 when none). Offline backups made elsewhere are not seen |

Histogram buckets (seconds): 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1, 2.5, 5, 10, 30, 60,
300, 900, 3600.

## Alerts

Suggested Prometheus rules; tune thresholds to your workload.

| Alert | Expression | For | Means |
| --- | --- | --- | --- |
| Down | `up{job="alephgrid"} == 0` | 2m | the process does not answer (liveness) |
| NotReady | `alephgrid_ready == 0 and alephgrid_shutting_down == 0` | 2m | data directory not writable or control database not answering ([Health](#health-and-readiness)) |
| DiskReserveReleased | `alephgrid_disk_reserve_state{state="released"} == 1` | 0m | the disk filled up; follow [Disk is full](#disk-is-full) |
| DiskLow | `alephgrid_disk_free_bytes < 5 * 1024^3` | 10m | under 5 GiB free; imports and backups will start failing |
| AllocationStartFailures | `sum(increase(alephgrid_allocation_start_failures_total{reason=~"startup_failed\|owned_elsewhere\|incompatible"}[15m])) > 0` | 0m | a workbook could not be opened; see [stuck allocation](#a-workbook-will-not-open-or-its-compute-allocation-is-stuck) |
| CapacityRefusals | `sum(increase(alephgrid_allocation_start_failures_total{reason="capacity"}[15m])) > 3` | 0m | starts refused after waiting in line: raise `--memory-budget` or lower `--idle-timeout` |
| StartQueue | `alephgrid_allocations_queued > 0` | 5m | starts keep waiting for capacity |
| JobWaitP95 | `histogram_quantile(0.95, sum by (le, class) (rate(alephgrid_job_wait_seconds_bucket[10m]))) > 5` | 10m | jobs wait over 5 s for their workbook's queue (interactive ones should wait well under 1 s) |
| JobsRejectedOrTimedOut | `sum(increase(alephgrid_jobs_total{outcome=~"rejected\|deadline_exceeded"}[15m])) > 5` | 0m | overloaded workbooks or too-short timeouts |
| MemoryBudgetNearLimit | `alephgrid_memory_reserved_bytes / alephgrid_memory_budget_bytes > 0.9` | 15m | open workbooks hold over 90% of the budget; new starts will close idle ones or wait |
| BackupStale | `time() - alephgrid_last_backup_timestamp_seconds > 26 * 3600` | 0m | no online backup in 26 h (only if you back up daily with `POST /api/admin/backups`) |

## Support diagnostics

A diagnostics bundle is one JSON file to attach to a support request: version and build,
platform (OS, architecture, CPUs, memory), the effective configuration with every secret,
identity, URL and path shown only as `set` / `not set`, health, storage formats, sizes and
counts, metrics, and recent log lines.

- **From the running server:** Administration API `GET /api/admin/diagnostics` (instance
  admins; audited as `diagnostics_downloaded`). It includes the newest 200 log lines of the
  process.
- **From the data directory:** `alephgrid diagnose --data-dir DIR [--out FILE]`, with the
  same flags and environment as the server (in Docker: `docker exec <container> alephgrid
  diagnose --data-dir /data --out /data/diagnostics.json`). It reads files only, never a
  database, so it works while the server runs or is stopped; it has no log lines.

It never holds passwords, tokens, the SSO client secret or client id, the metrics token,
session cookies or emails. Outside the log lines it holds no workbook, space, user or file
names or ids, cell values, SQL text or paths. Log lines are copied as written (never request
bodies, headers or query strings) with configured secrets, `agt_`/`agc_` tokens, cookies,
bearer credentials, emails and the data directory's path cut out. They keep request paths and
workbook ids, which match your own log; a line can name a file given to `alephgrid` on its
command line, and an engine error quoted in a warning can name a value (see [Logs](#logs)).
Read the bundle before you send it.

## Docker

The image (`ghcr.io/alephgrid/alephgrid`) is published with the first release; until then,
build it from a checkout with `docker build -t ghcr.io/alephgrid/alephgrid .` (the Compose
files below then use it). It runs as the user `alephgrid`, uid and gid 10001,
never as root. Data lives in `/data` (`ALEPHGRID_DATA_DIR`); `/backups` is there for offline
backups. Both are owned by that user in the image.

- **Named volume** (`-v alephgrid-data:/data`): nothing to set up. Docker creates the volume
  from the image's `/data`, owned by 10001.
- **Host folder** (bind mount, `-v /srv/alephgrid:/data`): Docker keeps the folder's owner, so
  give it to 10001 before the first start, or the server stops with "permission denied":

  ```bash
  sudo mkdir -p /srv/alephgrid && sudo chown -R 10001:10001 /srv/alephgrid
  ```

  Or run the container as yourself, with a folder you own:
  `docker run --user "$(id -u):$(id -g)" -e HOME=/tmp -v "$PWD/data:/data" …`.

Ready-made Compose files:

| File | For |
| --- | --- |
| [deploy/localhost/compose.yaml](../deploy/localhost/compose.yaml) | one person: no sign-in, published on `127.0.0.1:7878` only, other host names refused |
| [deploy/team/compose.yaml](../deploy/team/compose.yaml) | a team: accounts, HTTPS through Caddy with `ALEPHGRID_PUBLIC_URL`, health checks on `/readyz`, memory and CPU limits, settings from `.env` ([.env.example](../deploy/team/.env.example)), a backups volume |
| [docker-compose.yml](../docker-compose.yml) | building the image from a checkout |

Both images set `HEALTHCHECK` to `alephgrid health`. In the team setup Caddy starts once
Alephgrid is healthy; JSON logs go to `docker compose logs alephgrid`. The server's DuckDB
memory follows the container's memory limit (80% of it) unless `ALEPHGRID_MEMORY_BUDGET` is set.

Offline backup with Compose (the server is stopped for it; copy the result off the host):

```bash
docker compose stop alephgrid
docker compose run --rm alephgrid --backup-to /backups/backup-$(date +%Y%m%d)
docker compose start alephgrid
docker compose cp alephgrid:/backups ./alephgrid-backups   # then move it to another machine
```

## Backups

- **Online**: Administration → Backups → **Back up now** (instance admins), or
  `POST /api/admin/backups`. This is a consistent copy of every workbook and of the control
  database. Only one backup runs at a time, and only if there is room on the disk. The newest
  few are kept under `<data dir>/.backups/` (`GET /api/admin/backups` lists them).
- **Offline**: with the server stopped, run
  `alephgrid --data-dir DATA --backup-to /other/disk/backup-YYYYMMDD` (a new directory).

A backup is a plain directory: `manifest.json` (version, revisions and SHA-256 checksums),
`control.duckdb` (accounts, roles, sessions, tokens, audit log) and one folder per workbook.
Trash, staged uploads, exports and Python scratch space are left out.

**Copy every backup to another device or host**; a backup on the same disk does not survive
the disk. Any file copy works, because restore checks every checksum and refuses a damaged copy:

```bash
rsync -a /srv/alephgrid/.backups/ backup-host:/backups/alephgrid/      # online backups
rsync -a /other/disk/backup-YYYYMMDD backup-host:/backups/alephgrid/   # an offline one
```

## Restore

On the same host or a clean one:

1. Install the same version of Alephgrid that made the backup, or a newer one (a newer one
   migrates older workbooks when they open; an older one refuses newer files).
2. Copy the backup directory to the host.
3. Restore into an empty or missing directory, with no server using it:
   `alephgrid --data-dir /srv/alephgrid --restore-from /backups/backup-YYYYMMDD`.
   Every checksum is verified and each workbook is opened and checked before anything is put
   in place; a directory that is not empty is refused. With Docker, restore into a new volume:

   ```bash
   docker run --rm -v alephgrid-data:/data -v /backups/backup-YYYYMMDD:/restore:ro \
     ghcr.io/alephgrid/alephgrid:X.Y.Z --restore-from /restore
   ```

   (the backup must be readable by uid 10001).
4. Start the server on that directory. Users, roles, spaces, service tokens and agent
   connections come back; so do sign-in sessions that had not expired when the backup was made.
   To end those sessions, disable and re-enable the users concerned, or reset passwords.

`tests/backup.rs` runs this procedure with the binary (accounts, a member, a service token, a
table and a sheet; back up elsewhere, lose the original, restore into an empty directory, sign
in and read everything back).

## Upgrades

1. Make a backup and copy it off the host.
2. Stop the server, replace the binary (or change the image tag; pin versions in Compose rather
   than using `latest`) and start it again.
3. A workbook written in an older storage format is migrated the first time it opens. The old
   file is kept next to it as `data.duckdb.v<N>.bak`; delete those once the new version has run
   well for a while.

**Rollback limits.** Going back to an older version is not supported once a workbook has been
migrated: the older version refuses files that are newer than it understands. To roll back,
stop the server, restore the backup from step 1 into an empty directory with the older binary,
and start that. Changes made after the backup are lost, so take the backup just before
upgrading. Before 1.0, a minor version may migrate the storage format (see CHANGELOG.md).

## Runbooks

### A workbook will not open, or its compute (allocation) is stuck

1. Open it. The Compute panel (toolbar → Compute) shows its state and, when it failed, why.
2. **Cancel** stops a start that has not finished (or is waiting in line for capacity).
   **Restart** stops and starts it again; work that was not acknowledged as saved is resent by
   open clients. The API equivalent is `POST /api/workbooks/{id}/runtime` with
   `{"action": "cancel"}` or `{"action": "restart"}`.
3. Administration → Workbooks and memory (`GET /api/admin/resources`) lists the open workbooks
   with their memory, threads and jobs. Administration → Sessions lists live connections.
4. If it still fails, stop the server, make an offline backup, and look at the log lines with
   that `workbook`. Do not delete its directory. On the next start, allocations left by the
   previous process are marked lost and cleaned up.

### A job is stuck

Bulk jobs (imports, queries, sorts, copies, exports) stop by themselves at `--job-timeout`
(1 hour by default), interactive requests at `--request-timeout` (30 s).

1. The person who started it cancels it in the UI, or anyone who can edit the workbook calls
   `DELETE /api/workbooks/{id}/jobs/{job}`. Only that job is interrupted. A job stopped by a
   cancel or its deadline is logged as `job stopped` with its `workbook` and `job`.
2. If it does not stop, **Restart** the workbook's compute (above). As a last resort, stop the
   server: on SIGTERM running work gets `--shutdown-grace` (30 s) and is then interrupted;
   committed changes are safe.

### Disk is full

The server keeps a small **disk reserve** (`.control/reserve`, 32 MiB by default,
`--disk-reserve`). When a change runs out of space it is rolled back, and the server deletes the
reserve at once: a rolled-back change's blocks stay inside the workbook file, so without the
reserve the disk would stay full and the server could not record state or restart. With it, the
server keeps working well enough for you to free space. Administration → Overview shows the free
disk and whether the reserve is held; `/readyz` answers `degraded` with
`"disk_reserve": "released"`, and the log says "the disk is full". The reserve comes back by
itself once there is room for it and twice as much again.

Backups refuse to start without room for them, and a workbook at its size limit
(`--max-workbook-size`) accepts only changes that remove data. Free space by doing any of:

- empty the trash;
- delete old backups (Administration → Backups), after copying them off the host;
- remove expired `.exports` and `.agent-results` files (the server also sweeps them);
- grow the volume.

Then check that `/readyz` reports `"disk_reserve": "held"` again.

### Lost admin password

Passwords are stored only as hashes, and no one can read or reset another user's password from
the UI. An instance admin who lost theirs:

1. If another instance admin can sign in, nothing is urgent; still reset the password below.
2. Stop the server (`docker compose stop alephgrid`).
3. Set a new password:

   ```bash
   ALEPHGRID_NEW_PASSWORD='a new long password' alephgrid --data-dir DATA --reset-password admin@example.com
   # Docker:
   docker compose run --rm -e ALEPHGRID_NEW_PASSWORD='a new long password' alephgrid --reset-password admin@example.com
   ```

   Without `ALEPHGRID_NEW_PASSWORD` the password is read from standard input. It refuses
   while a server uses the directory, ends the user's sessions and records `password_reset`
   (actor `cli`) in the audit log. It does not enable a disabled account.
4. Start the server and sign in. Accounts that sign in only with single sign-on have no
   password; their access is managed at the identity provider.

### Restore from a backup

1. Pick the backup (`GET /api/admin/backups`, or the copies on the backup host). Prefer the
   newest one taken before the problem.
2. Stop the server. Do not restore over the live directory: move it aside (or keep the volume)
   until the restore is checked.
3. Restore into an empty directory or new volume as in [Restore](#restore), with the same or a
   newer version, and start the server on it.
4. Check: `/readyz` is `ok`, an admin signs in, the workbooks open with the expected history.
5. To recover a single workbook instead, restore into a separate directory, start a second
   server on it (another `--port`), export what you need and import it into the live server.

### Upgrade, and roll back

1. Read the release notes (CHANGELOG.md) for storage changes.
2. Back up and copy the backup off the host, then upgrade as in [Upgrades](#upgrades).
3. Check `/readyz`, sign in and open a few workbooks.
4. To roll back: stop the server, restore the pre-upgrade backup into an empty directory with
   the older version and start that. Changes since the backup are lost; an older version
   cannot open workbooks the newer one migrated.

### Rotate credentials

- **Single sign-on client secret**: create a second secret for the client at the identity
  provider, set it as `ALEPHGRID_OIDC_CLIENT_SECRET` (in `.env` with Compose) and restart the
  server (`docker compose up -d` recreates it). Sign in with SSO once to check, then delete the
  old secret at the identity provider. People already signed in stay signed in: sessions are
  the server's own, not the identity provider's.
- **Single sign-on client**: to move to a new client id, set `ALEPHGRID_OIDC_CLIENT_ID` and the
  secret together, register `https://<your host>/api/auth/oidc/callback` as its redirect URI,
  and restart.
- **Service tokens**: create the new token (space menu → Service tokens…), switch the client,
  then revoke the old one.
- **Agent connections**: create a new connection, update the MCP client, then revoke the old
  connection.
- **Passwords**: users change their own under their account; changing it ends their other
  sessions. A lost admin password: see above.
- **A user who should lose access at once**: an instance admin disables the account, which ends
  its sessions, or an organization admin removes them from the organization.
- **Bootstrap admin password** (`ALEPHGRID_BOOTSTRAP_PASSWORD`): used only while there are no
  users; remove it from the environment after the first start.

### Incident: data looks wrong

1. Open the workbook's **History**. Every change shows who made it and when. Table changes can be
   undone as a new change unless someone has changed the same cells since.
2. The audit log (Administration → Audit log, or the organization's Activity) records imports,
   exports, sharing changes and agent tool calls.
3. If a change cannot be undone, restore the most recent good backup into a new data directory
   and copy the workbook across (see "Restore from a backup", step 5). Restoring is per data
   directory, so do not restore over the live one.

## Limits

- One process and one machine. Workbooks are distributed only in a cluster deployment (not part
  of this release).
- Memory and threads follow `--memory-limit` and `--threads`. Beyond memory, DuckDB spills to
  temporary files on disk.
- Sheet grids hold up to Excel's size (1,048,576 × 16,384). Tables are bounded by disk.

## Benchmarks

`tools/bench/bench.mjs` (Node 22+, no dependencies) measures a server binary with seeded data
and concurrent clients over the public protocol, so results can be reproduced and compared.
It never uses an existing data directory: it starts the binary on a new data directory inside
a scratch directory (`--scratch`, which must be new or empty; by default a fresh temporary
directory, deleted afterwards unless `--keep`). The server creates no data by itself; the tool
writes the CSV files and imports them through `POST /api/workbooks/{id}/import`.

```bash
cargo build --release -p alephgrid-server
node tools/bench/bench.mjs --bin target/release/alephgrid --label "what machine, why"    # smoke
node tools/bench/bench.mjs --bin target/release/alephgrid --scale full --label "..."     # full
node tools/bench/bench.mjs --help                                                         # every option
```

| | Smoke (default) | Full (`--scale full`) |
|---|---|---|
| narrow table (id, category, amount, qty, day, flag) | 50,000 rows | 10,000,000 rows |
| wide table (id + 99 numeric columns; `--wide-columns`) | 5,000 rows | 1,000,000 rows |
| text-heavy table (id, title, ~0.5 KB body, tag, city) | 20,000 rows | 2,000,000 rows |
| seconds measured per table and client count | 5 (after 1 s warm-up) | 60 |
| server memory budget (`--memory-budget`) | 512 MiB | 512 MiB unless set |

Each size can be set on its own (`--rows-narrow`, `--rows-wide`, `--rows-text`); `--seed`
fixes the data and the workload. For each table it runs 1, 5 and 20 clients (`--clients`),
each with its own connection and view, looping over a mixed workload: 50% fetches of 100 rows
at random positions (30 columns at a time on the wide table), 10% each sort, filter (and back),
column statistics over up to 10,000 rows, one-cell edit and an aggregate SQL query. It samples
the server's resident memory (RSS) every 250 ms.

Results (`--out`, default `bench-results/<time>`):

* `raw.csv`: every request: table, clients, client, operation, start, latency, error code;
* `rss.csv`: the RSS samples by phase;
* `summary.json` and `summary.csv`: per table, client count and operation: count, errors,
  operations per second, p50/p95/p99/max latency, and peak RSS; plus the settings, the
  server version, the commit and the machine.

A smoke run checks that the tool and the server work; its numbers are not capacity figures.
Published capacity limits (9.05, 9.15) need full runs of a release build on a dedicated
machine, recorded here with their `summary.json`. None are published yet.
