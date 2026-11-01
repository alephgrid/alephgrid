# Durability and isolation

What Alephgrid promises about your data, what each promise rests on, and how far it has been
tested. Read this before relying on a deployment for data you cannot recreate.

## When a change is acknowledged

A change is **acknowledged** when the server replies `ok: true` to it (WebSocket) or returns a
2xx status (HTTP import). By then:

1. The change, the next workbook revision and its history entry have committed in **one DuckDB
   transaction** (`Workbook::mutate`). There is no state in which data, revision and history
   disagree.
2. DuckDB has written the commit to the workbook's write-ahead log (`data.duckdb.wal`), which is
   folded into the database file at checkpoints (on idle close, shutdown, and as DuckDB decides).
3. Sheet changes are part of the same transaction (the IronCalc model is stored in the file); the
   in-memory model is reloaded from the file if any part of the transaction fails.
4. Metadata (`workbook.json`, runtime records) is written to a temporary file, synced,
   renamed over the old file, and the directory is synced.

A request that was **not** acknowledged (the connection dropped, the server stopped, the request
was cancelled or timed out) either committed completely or not at all. Clients that retry should
send the same `mutation_id`: a mutation id found in the history is not applied again (the last
10,000 revisions are kept for this), and the reply says `replayed: true`.

Revisions start at 0 and increase by exactly one per committed change, with no gaps. Every
revision is announced to open connections in order, and table changes keep what undo needs in the
same transaction as the change (storage format 8: `_rf_undo` and its row copies). Overlapping
edits follow [COLLABORATION.md](COLLABORATION.md).

## What survives which failure

| Failure | What survives | Evidence | Caveats |
|---|---|---|---|
| Server process crash (`SIGKILL`, panic, OOM kill) | Every acknowledged change. Unacknowledged ones are complete or absent. | `crash.rs` (SIGKILL during and between writes), `shutdown.rs` (SIGTERM during edits and imports), restart reconciliation in `crash.rs`/`runtime.rs` | The OS page cache survives a process crash, so these tests do not exercise the disk. |
| Graceful stop (`SIGTERM`, Ctrl-C) | Everything acknowledged; running work gets `--shutdown-grace` seconds, then is interrupted and rolled back; files are checkpointed; allocations are recorded as stopped. A second signal exits immediately (committed changes are still safe). | `shutdown.rs` | |
| Disk full, unwritable files | The failing change is rolled back and reported; everything acknowledged before it remains readable; uploads leave no partial file. The rolled-back change's blocks stay in the workbook file (DuckDB does not shrink it), so the server gives back its disk reserve (`--disk-reserve`) at once and can still record state and restart. | `storage_faults.rs` against a real 64 MB filesystem; `reserve.rs` unit tests; injected faults in `runtime.rs`, `scheduling.rs`, engine `storage.rs` | Checked on HFS+ (macOS disk image) and on tmpfs in CircleCI. The first tmpfs run (1 Nov 2026) found the restart failing for lack of space; the reserve fixed it. Other filesystems may report errors differently. |
| Host crash or power loss | **Expected** to keep acknowledged changes, *if* the storage honours flushes: DuckDB syncs its WAL on commit and Alephgrid syncs its metadata writes. | **Not tested.** No VM-level power-cut or `dm-flakey` testing has been done. | Disks or virtual disks that acknowledge writes before they are durable (some cloud volumes, consumer SSDs without power-loss protection, `nobarrier` mounts) can lose recent commits or corrupt the file. |
| Loss or corruption of the storage itself | Nothing on that storage. Only backups survive. | `backup.rs`: consistent snapshot backups during writes, checksummed, restore verified | Backups are as good as where they are kept and how recently they were taken. Back up to a different device. |
| Bugs that corrupt data in place | Backups taken before the bug. | — | Keep several backups; restore is per data directory. |

### Backups

`POST /api/admin/backups` (online) or `alephgrid --backup-to DIR` (offline) copies each workbook as
of one revision (DuckDB `COPY FROM DATABASE` inside one transaction), with a manifest of
revisions and SHA-256 checksums. `alephgrid --restore-from DIR --data-dir NEW` verifies the
checksums, opens every workbook and checks its revision before anything appears in `NEW`, which must
be empty. A copy of `data.duckdb` taken with `cp` while the server runs is **not** a backup: it can
miss the WAL or catch the file mid-checkpoint.

Trash, staged uploads and runtime records are not included. Workbooks are each consistent; there
is no consistency across workbooks (they are independent).

### Upgrades and migrations

Opening a workbook written by an older storage format migrates it in one transaction after copying
the original to `data.duckdb.v<N>.bak`. There is **no downgrade**: an older Alephgrid refuses files
written by a newer format, and the `.bak` copy (or a backup) is the only way back. Delete `.bak`
files once you are satisfied with an upgrade.

## Ownership

One mutable workbook has one owner at a time. The Local provider enforces this with an exclusive
`flock` on `<workbook>/.owner.lock` held for the allocation's lifetime, plus a lock on the data
directory (`.control/controller.lock`) so two servers cannot share it. These locks are reliable on
local filesystems of one host. They are **not** a fence across hosts and are unreliable on many
network filesystems; do not point two hosts at the same data directory. When ownership is uncertain
(another process holds the lock) the workbook is refused (`423 Locked`), never opened twice. A
lease or a SIGKILL test is not evidence of distributed fencing.

## Isolation classes

| Class | What it separates | What it does not | Available |
|---|---|---|---|
| `shared_process` | Workbooks' data by the server's own code; per-workbook DuckDB instances with their own memory/thread/spill limits | A bug, a crash, or a hostile query affects every workbook in the process; user SQL cannot reach files, URLs or extensions (SQL policy plus DuckDB external access disabled per workbook, phase 4), but a DuckDB defect would not be contained; limits are DuckDB-level, not OS-level | Yes (Local provider) |
| `process` | A crash stays in its process | Same user, same file access, unless the OS confines it further; a subprocess is not a security boundary by itself | No |
| `container` | Filesystem view, cgroup CPU/memory limits | Kernel shared with other containers | No |
| `virtual_machine` | Kernel and memory | Shared hardware and hypervisor | No |

Use `shared_process` for one trusted user or team. Mutually untrusted tenants need a stronger
class, which later providers will offer.

## Resource limits, precisely

`--memory-budget` is divided among open workbooks as DuckDB `memory_limit` (buffer manager), shrunk
before a new workbook opens so the sum never exceeds the budget; sheet memory is estimated and taken
out of DuckDB's share. This does **not** cap the process's resident memory (allocations outside
DuckDB's buffer manager, the server itself). `--threads` is divided as DuckDB worker threads.
`--temp-budget` bounds DuckDB spill files (half) and staged uploads (half). Cap the process with an
OS limit (systemd `MemoryMax`, a container limit) for a hard bound; Alephgrid honours a cgroup
memory limit when choosing its default budget.
