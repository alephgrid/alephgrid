# Compute for workbooks

Opening a workbook is the default action: the server finds or starts its compute, and every
collaborator is served by that one allocation (checklist 7.13–7.16). Spaces own budgets and
entitlements; they are not machines. A workbook's data, identity, revision and pinned runtime
never depend on its compute, which can stop, restart or change size without changing them.

This page is the contract. Runtime objects (`RuntimeSpec`, `ComputePolicy`, `RuntimeAllocation`,
`OwnershipAssignment`) are described in ARCHITECTURE.md; HTTP shapes in PROTOCOL.md.

## Profiles (7.13, 7.15)

A **profile** is a named amount of compute an allocation reserves out of the host budget.

| Profile | Reserves | Defined by |
|---|---|---|
| `standard` | an equal share of what fixed profiles leave, never below `--min-workbook-memory` and one thread | built in; every workbook may use it |
| any other, e.g. `large` | exactly its memory and threads, whatever else is open | `--profile large=4GiB:4` (repeatable) |
| dedicated, e.g. `dedicated` | as above, and it is never closed to make room for another workbook | `--profile dedicated=8GiB:8:dedicated` |

* **Entitlements are checked by the server.** An organization admin grants profiles to a space
  (`PUT /api/spaces/{id}/profiles`); `standard` is always granted. Choosing a profile for a
  workbook needs the space's owner role, and a dedicated profile needs an organization admin.
  Requests name a profile id and nothing else: a request carrying resource amounts, an unknown
  profile, a profile not granted to the space or a role that may not choose it is refused (400 or
  403) and changes nothing. Clients never say how much memory they get.
* **The workbook keeps its profile** (`workbook.json`). If the profile is later removed from the
  server or its grant is withdrawn, the workbook still opens, with `standard`; the status says so.
* **Space compute budget** (`compute_memory_bytes` quota, `--space-compute-memory`): the memory the
  space's live allocations are guaranteed, summed. `standard` counts `--min-workbook-memory`; other
  profiles count their memory. With the existing open-workbooks quota it is checked and reserved in
  one step, under the controller's lock, together with the allocation record: concurrent starts can
  never pass a space's budget, and a refused start reserves nothing. Refusals are 429 / `quota`.

## Opening, queueing and routing (7.14)

* The workbook page asks for the workbook's status first (`GET /api/workbooks/{id}/runtime`, which
  starts nothing), then starts it without waiting (`POST … {action: "start", wait: false}`) and
  shows each state until it is ready:

  | State | Shown | Actions |
  |---|---|---|
  | `queued` (record `requested`, `queue_position` set) | the server is full; position in line; it opens by itself | Cancel |
  | `requested`, `starting`, `running` | opening (migrating if the storage format is older) | Cancel |
  | `ready` | the workbook | |
  | `draining` | closing or restarting; collaborators see why | |
  | `failed` | the server's reason and what to do (below) | Try again, Back |
  | `stopped` after idle | paused, data kept, compute released | Resume |

* **Queue.** When the host budget cannot fit another allocation and no workbook can be closed to
  make room, a start waits in a first-come queue for up to `--start-queue-timeout` (60 s), then fails
  with `capacity`. Space quotas are not queued: they are refused at once, since waiting would not
  free them.
* **One owner, every collaborator routed to it.** All opens of a workbook go through one
  single-flight start: the second collaborator waits for the first one's allocation and joins it,
  never starting another. The allocation's holder (`ownership.holder`, `host:pid` here) is the only
  process that writes the workbook; another process holding it is refused (`owned_elsewhere`), never
  opened twice.
* **Failures say what to do.** `capacity`: the server is full, try later or close workbooks;
  `quota`: the space reached a limit, close one or ask an admin; `incompatible`: the workbook needs
  another server version; `owned_elsewhere`: another server process has it open; `startup_failed`:
  the reason, and Try again.

## Changing profile: controlled restart (7.15)

`POST /api/workbooks/{id}/runtime {action: "restart", profile}` checks the entitlement and that the
new size fits (host and space budget, counting the current allocation as released) before touching
anything; if it does not fit the workbook keeps running as it was. Otherwise:

1. The allocation drains (new requests wait, running work finishes, the database checkpoints);
   collaborators get `workbook_closed {reason: "restarting"}` and reconnect by themselves.
2. A new allocation starts with the new profile and the next epoch. The ownership lock is handed
   from the old allocation to the new one, so no other process can take the workbook in between.
3. Same URL and workbook id, same data and revision, same pinned runtime spec and function
   versions. Changes a client had not had acknowledged are resent (COLLABORATION.md).

If the new allocation fails to start, the workbook is started again with its previous profile and
the request reports the failure. A plain `restart` without `profile` restarts at the same size.

## Idle shutdown (7.16)

A workbook's compute closes after `--idle-timeout` (10 minutes) without **meaningful activity**:

* **Counts:** every operation a person or program sends (WebSocket operations, HTTP requests on the
  workbook), and running or queued work (queries, imports, exports, recalculations). A job keeps
  the workbook open however long it runs; the idle time starts when it ends.
* **Does not count:** an open connection, WebSocket ping/pong, reading the runtime status. Open
  tabs alone do not keep compute running.

Closing drains and checkpoints first, so every committed change is in the workbook file; then the
lock and the reservation are released. Storage is kept as it is: the status and the paused screen
show the workbook's size on disk, that nothing it reserves is held while it is paused, and that
reopening takes a moment. Clients get `workbook_closed {reason: "idle"}`; the page shows the
workbook as paused and reconnects when the user acts (Resume, or any key or click), resending
unacknowledged changes.

## Limits

* One host: the Local provider runs allocations inside the server process (`shared_process`).
  Profiles bound DuckDB's memory and threads per workbook, not the process's whole memory, which an
  OS limit must cap (see `runtime/budget.rs`).
* Resizing is a stop-and-restart; there is no live resize.
* There are no prices: the status shows reserved memory and threads and the idle timeout, not cost
  in money.
