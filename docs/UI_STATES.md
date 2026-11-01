# UI states and navigation

U.11 lists the states each screen shows, in the words people see; U.02 maps how people move
between objects. Both describe shipped behaviour. The tests named under each heading check
it.

## Connection and save state (every workbook)

Shown next to the workbook name (`renderStatus` in `web/src/workbook.ts`). The first matching
line wins:

| State | Text | Meaning |
| --- | --- | --- |
| Save failed | **Not saved** | A change was lost while the connection was down; the tooltip asks to check and redo the last edits |
| Unsent changes | **Unsaved changes** | Kept in the page and sent again when the connection returns |
| Paused | **Paused** | The workbook's compute sleeps; continuing resumes it |
| Connecting | **Connecting…** | First connection, or after a server restart |
| Disconnected | **Reconnecting…** | Retrying; edits are queued |
| Sending | **Saving…** | Changes are on their way |
| Acknowledged | **Saved** | Shown only after the server has acknowledged every change |

"Saved" never appears before durable acknowledgement. Unacknowledged edits are resent after a
reconnect and applied at most once (`e2e/savestate.spec.ts`, `e2e/resilience.spec.ts`).

## Per screen

| Screen | Empty | Loading | Working | Failed / refused |
| --- | --- | --- | --- | --- |
| Hub | "No workbooks here yet. Drop a file on this page to start."; no spaces: "Ask an admin to add you to one."; search: "No workbooks match …" | — | — | Unauthenticated → sign-in page, then back to the same path |
| Grid (tables) | Empty table with the append row | Placeholder bars in cells being fetched | Busy overlay for long view changes | Error toast; the view keeps its last good state |
| Grid (sheets) | "Empty sheet" in the status bar | Placeholder bars in cells being fetched | — | Refused edits explain why (allowed values, covered merged cells, a stale layout, someone else's change) and keep the cell's value |
| Formula bar | — | — | — | "Formula not saved: … Fix it, press Escape to discard…" with the draft kept |
| Import | — | Upload progress | Import progress bar with **Stop import**; the pane stays open to retry | The problem and, when known, a one-click fix ("Import “col” as … and try again"); cancelling leaves no staged file |
| SQL workspace, Analyze | Hints for a first query or analysis | Running indicator with Cancel | Queued and running jobs | The error with its position; results stay until replaced |
| Compute | Sleeping is described as normal | Starting / waiting in line | Running, resizing (restart) | Failed with the reason, Cancel and Restart |
| Agent requests | — | — | A request that outlives its wait is a job (`job_status`) | Tool errors carry a recovery hint; cancelled jobs are discarded |
| Dialogs and panes | "No … yet." lines (rules, comments, connections, proposals, backups) | "Loading…" | Buttons disabled while a change is in flight | Errors in the dialog's alert line; input kept |

Permission states: objects a person cannot see are reported as **not found**, never as
forbidden, so an id from another space reveals nothing. Actions the role does not allow are
disabled in menus (for example Rename for viewers) and refused by the server (`tests/teams.rs`,
`e2e/teams.spec.ts`).

## Navigation and object identity (U.02)

```
Hub (/)                 All workbooks · Recent · Starred · Spaces (sidebar) · Trash
 └─ Space               sidebar entry; menu: New workbook, Library, Members, Service tokens,
                        Agent connections, Settings, Rename, Delete
     └─ Workbook        /w/<id>; title, connection/save state, Share
         ├─ Tables      tabs; header menus, filters, views (saved per table)
         ├─ Sheets      tabs; colour, hide, duplicate, reorder
         ├─ Queries     SQL workspace (saved and live queries, versions)
         └─ Functions   Functions panel (versions, pins, publication)
```

- A workbook is addressed by its id. Renaming or moving it keeps its link. Deep links survive
  sign-in: the login page returns to the path that sent it (`nextPath`).
- Tables and sheets are addressed by name inside a workbook. The active tab is marked
  (`aria-selected`), and the explorer highlights the same object.
- Sheets keep a stable internal id. Comments, rules and charts are attached to it, so they follow
  renames and tab moves.
- Unsent changes are kept in the page and resent after a reconnect. The save state shows
  **Unsaved changes** until they are acknowledged. Leaving the workbook before then asks first,
  whether through the brand or menu links or the browser's Back button. Closing the tab gets the
  browser's own warning. Staying keeps the changes (`e2e/savestate.spec.ts`).
