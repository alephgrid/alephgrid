---
title: Collaboration and teams
description: Live multi-user editing with history and undo, plus accounts, organizations, spaces, roles, SSO, quotas and the audit log.
---

Several people — or tabs, or agents — can edit one workbook at once. Every change runs
on the workbook's single writer, one at a time, and commits as one revision. The rule
throughout: a change made from an out-of-date view is refused, never misapplied, and
nothing is merged behind anyone's back.

## Editing together

Changes are checked against what you were looking at when you made them. A table cell
edit is addressed by its row's record id — which never changes and is never reused — so
it lands on the same row however rows were inserted, deleted or sorted since. It carries
the value you saw, and is refused if the cell holds another value now, its row was
deleted, or its column renamed or removed. Structural changes (inserting or deleting
rows, sheet layout changes) are checked against the table's structure generation or the
sheet's layout revision instead.

A refusal changes nothing — a request is all or nothing. When it means someone else
changed the same cell, the browser refreshes, shows you their value, and offers
**Overwrite**, which resends your change without the check. Different cells never
conflict, even in the same row. Area changes (paste, fill, find and replace, SQL
`UPDATE`/`DELETE`) replace what is in their area by design; they are refused only when
the area itself moved, and history and undo keep what they replaced.

Every committed revision is announced to every open connection in order, so everyone's
view follows along. A client that missed events refreshes what it shows; a reconnecting
client refetches its views and resends changes that were not acknowledged, each with its
original id, so a change that had already committed is not applied twice.

## History and undo

**History** lists everyone's recent changes — who made each one and when. The last 50
undoable table changes are kept, across restarts:

- Taking a change back is a new change, in the history, that restores what it replaced:
  rows it added are deleted, rows it deleted come back with their ids and places, and
  cells it set get their old values.
- If anything the undo would take back has changed since, it is refused and nothing
  changes. Later work by others is never undone as a side effect.
- Any change still kept can be undone from History by revision, not only the latest.
  An undone change records which revision took it back.
- ⌘Z and ⌘⇧Z take back your own latest change and redo your latest undo. Sheets keep
  their own shared undo history; undo there takes back only your own step.

A change touching more than 1,000,000 rows is made but cannot be undone. Column, table
and sheet-structure changes are not undoable from History.

## Accounts mode

On your own machine, `--auth local` (the default) has no sign-in. For a team, start the
server with `--auth accounts` — see [self-hosting](../self-hosting/) for the full
command. People sign in with a password or with single sign-on over OpenID Connect
(authorization code flow with PKCE); configure `--oidc-issuer` and `--oidc-client-id`,
and the identity provider's groups claim can grant roles directly. SSO users are matched
by issuer and subject, never by email, and a new SSO user needs a verified email with an
open invitation (or `--oidc-join any`).

With no user yet, the server prints a one-time setup code on its console, and `/setup`
uses it to create the **instance admin** — the only account that can create
organizations, disable users, or use the administration APIs.

### Organizations, spaces and roles

An **organization** holds members (`admin` or `member`) and **spaces**; a space holds
workbooks and grants roles. A workbook inherits its space's roles — there are no
per-workbook grants, so you share a workbook by putting it in a space. Your role in a
space is the highest of: owner (if you administer its organization), your direct role,
and the roles the space grants to your identity-provider groups.

| Capability | viewer | editor | owner |
|---|---|---|---|
| Open and read workbooks, run read-only SQL, export | ✓ | ✓ | ✓ |
| Edit cells, tables, sheets, saved queries; import; publish functions | | ✓ | ✓ |
| Create, duplicate, rename, move, delete and restore workbooks | | ✓ | ✓ |
| Purge the trash; manage members and service tokens; rename or delete the space | | | ✓ |

Organization admins invite people (an invitation carries an organization role and
optionally a space role, and expires after 1–30 days). Space owners grant roles to
existing organization members and to groups. Any organization member may create a space
and becomes its owner.

### Service tokens

A **service token** gives a script or integration a `viewer` or `editor` role in exactly
one space, for at most 366 days. It is sent as a header, never in a URL:

```sh
curl -H "Authorization: Bearer agt_<id>_<secret>" \
  https://grid.example.com/api/workbooks   # one space only
```

Only hashes of tokens, sessions and invitation codes are stored. Open connections
recheck access whenever roles, sessions or tokens change: losing access closes the
connection immediately.

### Quotas

Each space has five limits, set per space by an organization admin or falling back to
server defaults: storage bytes, workbook count, concurrent bulk jobs, open workbooks,
and guaranteed compute memory. A space at its storage limit still accepts small
interactive edits, so people can delete data to get back under it. Admins see usage
against limits under *Usage and limits…* in the account menu.

### Audit log

Every sign-in (and failed sign-in), role grant, invitation, token, workbook
create/move/delete, import, export, function publication and committed workbook change
is recorded with its actor, action, space, workbook and outcome. Organization admins
read their organization's log; the instance admin reads everything. Workbook changes are
batched within 250 ms so they do not delay the change itself; the workbook's own history
names every change's actor in the same transaction regardless.

### Administration

Instance admins get an Administration screen: backups, the audit log, open workbooks
with their memory, threads and jobs, live sessions, and instance-wide usage. Day-to-day
procedures — backups, restores, upgrades — are on the [operations](../operations/) page.

:::note[Not yet done]
Per-user stars, per-workbook grants, SCIM provisioning, identity-provider back-channel
logout, and audit-log retention limits and export are not implemented yet. Roles are an
authorization boundary, not a resource-isolation one: one process serves every space.
:::
