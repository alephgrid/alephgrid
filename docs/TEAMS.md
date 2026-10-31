# Teams: organizations, spaces, sign-in and roles

Phase 7 (7.01–7.06, 7.17). Code: `crates/alephgrid-server/src/auth/` (store, roles, OIDC,
routes), the guards in `http.rs` and the per-operation check in `ws.rs`. Tests:
`crates/alephgrid-server/tests/teams.rs`, `web/e2e/teams.spec.ts`.

## Objects

| Object | Lives in | Notes |
|---|---|---|
| Organization | control database | Members are `admin` or `member`. The first start creates the *Default organization*. |
| Space | control database | Belongs to one organization; holds workbooks; grants roles. The first start creates *General*. |
| Workbook | its directory | `workbook.json` names its `space`. Its data never enters the control database. |
| User | control database | Email (unique, case-insensitive), name, Argon2id password hash and/or an identity-provider identity (issuer + subject), the groups of the last sign-in. |
| Session | control database | SHA-256 of the cookie secret, user, expiry. |
| Service token | control database | One space, `viewer` or `editor`, an expiry (at most 366 days); SHA-256 of its secret. |
| Invitation | control database | Organization role, optionally a space role, an email, an expiry (1–30 days); SHA-256 of its secret. |
| Audit event | control database | Actor, action, organization/space/workbook, outcome, detail. |

The control database is `<data-dir>/.control/control.duckdb`. Backups (`backup.rs`, format 2) copy
it from one transaction after the workbooks, with its checksum in the manifest; restore verifies it.

## Migration from workspaces (7.01)

At every start `state::migrate_to_spaces` makes sure each workbook belongs to a space; once done
it changes nothing:

1. Create the default organization and its *General* space if they do not exist.
2. Each workspace of `workspaces.json` becomes a space with the same id (a clashing name gets a
   ` (2)` suffix).
3. Each workbook, live or in the trash, without a known space joins its former workspace's
   space, otherwise *General*.
4. `workspaces.json` moves to `.control/workspaces.json.migrated`.

Every step can be repeated, so a crash midway is finished by the next start. A format-1 backup
(made before spaces) restores `workspaces.json`, and the next start migrates it.

## Who is asking

`--auth local` (default) has no sign-in: every request is the *local* principal, with every right.
The server refuses to bind a non-loopback interface in this mode unless `--unsafe-no-auth`.

`--auth accounts` requires a principal on every route except `/api/config`, `/api/auth/*` and the
invitation lookup/accept routes:

* **Session cookie** `alephgrid_session`: `HttpOnly`, `SameSite=Lax`, `Secure` when `--public-url`
  is https, lasting `--session-hours` (default 12). Signing out deletes the session.
* **Service token** `Authorization: Bearer agt_<id>_<secret>`, header only. A token in a URL
  is never accepted.

Setup: with no user, the server prints a one-time setup code at start (on its own console), and
`/setup` asks for it to create the **instance admin**, who is an admin of every organization and
the only one allowed to use `/api/admin/*`, create organizations or disable users. Deployments can
instead set `ALEPHGRID_BOOTSTRAP_EMAIL` and `ALEPHGRID_BOOTSTRAP_PASSWORD`.

Password sign-in gives the same answer for a wrong email and a wrong password, takes as long for
both, and refuses an email for 15 minutes after 5 failures. Passwords have at least 10
characters.

## Roles (7.03)

A principal's role in a space is the highest of:

* `owner` for an admin of the space's organization (and for the instance admin);
* its direct role in the space;
* the roles the space grants to identity-provider groups the user was in at their last sign-in.

Only members of the space's organization receive the last two. A service token has exactly its
role in its own space and nothing elsewhere.

| Capability | viewer | editor | owner |
|---|---|---|---|
| view: see the space, open and read its workbooks | ✓ | ✓ | ✓ |
| query: run read-only SQL (still `--disable-sql`, the SQL policy) | ✓ | ✓ | ✓ |
| export: table, query and XLSX downloads | ✓ | ✓ | ✓ |
| edit: cells, tables, sheets, saved queries/views/analyses, imports, recalculation, stop a workbook | | ✓ | ✓ |
| publish: function versions and pins | | ✓ | ✓ |
| create workbooks here, duplicate, rename, star, move in | | ✓ | ✓ |
| delete workbooks (to the trash) and restore them | | ✓ | ✓ |
| purge trashed workbooks | | | ✓ |
| manage members and service tokens | | | ✓ |
| rename or delete the space | | | ✓ |

Rules:

* A workbook inherits its space's roles; there are no per-workbook grants. Share a workbook by
  putting it in a space.
* Moving a workbook needs *delete* where it is and *create* where it goes, within one organization.
* A space can be deleted only when it holds no workbooks, the trash included.
* Organization admins invite people (organization role and optionally a space role) and manage
  organization membership. Space owners grant roles to existing organization members and groups.
  An organization always keeps at least one enabled admin.
* Any organization member may create a space and becomes its owner.
* Starring is shared by everyone who can change the workbook; it is not per user yet.

## Enforcement (7.04)

* Every HTTP route names the capability it needs (`http.rs`, `auth/routes.rs`), checked before any
  work. Every WebSocket operation is listed in `ws::required_caps` without a catch-all, so a new
  operation cannot compile without a decision.
* An object of a space the principal cannot see is reported as **not found** with the same message
  as a missing one: a workbook id, a space id or a token id from elsewhere reveals nothing. A
  visible object the principal lacks the capability for is **403**.
* A workbook's change history (`history`) names who made each change by principal: `local`,
  `user:<id>` or `token:<id>`.
* Uploads, downloads and jobs are scoped to their workbook, and their workbook's access is checked
  on each request.
* Open WebSocket connections recheck access whenever identities, roles, sessions, tokens or a
  workbook's space change (`Auth::bump`), and when their credential expires. Losing access sends
  `access_revoked` and closes the connection; a changed role sends `role_changed` and applies to
  the next request. Each request also checks expiry itself.

## Browser and network protections (7.06)

* Requests carrying proxy identity headers (`X-Forwarded-User`, `X-Remote-User`, `Remote-User`,
  `X-Auth-Request-User`/`-Email`, `X-Forwarded-Email`, `X-Forwarded-Preferred-Username`,
  `X-Webauth-User`) are refused with 400. Alephgrid never trusts identities asserted by proxies;
  use OIDC.
* The `Host` header must be a loopback name, or the host of `--public-url`, unless the server is
  bound to a non-loopback interface without `--public-url` (DNS rebinding).
* A request that changes something, and every WebSocket upgrade, is refused if its `Origin` is not
  this server. A change sent with a session cookie and no `Authorization` must carry `Origin` or
  `Sec-Fetch-Site: same-origin`.
* No secret is placed in a URL: sessions are cookies, tokens are headers, invitation links carry
  their code in the fragment (`/invite#<code>`), which browsers send neither to servers nor in
  `Referer`. The setup code is shown only on the server's console. Only hashes are stored.
* Errors and the admin session list never contain cookies, tokens or hashes; error messages have
  the data directory redacted.

## Single sign-on (7.02, 7.17)

OpenID Connect authorization code flow with PKCE (S256), `state` and `nonce`
(`auth/oidc.rs`). Configure `--public-url`, `--oidc-issuer`, `--oidc-client-id` and, for a
confidential client, `ALEPHGRID_OIDC_CLIENT_SECRET`; register
`<public-url>/api/auth/oidc/callback` as the redirect URI. Discovery must report exactly the
configured issuer; endpoints must be https (http only for loopback, for tests).

The ID token must be signed with an asymmetric algorithm (RS/PS 256–512, ES256/384, EdDSA) by a key
in the issuer's JWKS (refetched once for an unknown key), and its `iss`, `aud`, `azp` (when
several audiences), `exp` (60 s leeway) and `nonce` must match. A `state` is good once and for 10
minutes.

Claim mapping:

| Claim | Use |
|---|---|
| `iss` + `sub` | The identity. A user is found only by this pair, never by email. |
| `email` + `email_verified` | Needed to create a user; matched against open invitations. Unverified emails are ignored. |
| `name` or `preferred_username` | Display name of a new user. |
| `--oidc-groups-claim` (default `groups`) | Group names (array or one string), replaced at every sign-in. |

* An existing identity signs in (unless disabled). A new one needs a verified email with an open
  invitation, or `--oidc-join any` (then it joins the default organization as a member with no
  space roles until granted). A verified email that already has a password account is refused:
  emails do not link accounts.
* Invitations for the verified email are applied at every sign-in.

Revocation: disabling a user deletes their sessions and closes their connections immediately.
Removing someone from the identity provider, or from a group, takes effect at their next sign-in
(groups are re-read then) and at the latest when their session expires (`--session-hours`);
Alephgrid does not poll the identity provider. Removing an organization member or a space role
takes effect immediately.

## Limits and not yet done

* One process serves every space (`shared_process`); roles are an authorization boundary, not a
  resource-isolation boundary (see DURABILITY.md). Quotas are 7.07.
* No per-user stars, per-workbook grants, SCIM provisioning or identity-provider back-channel logout.
* Audit records cover sign-in, setup, memberships, roles, invitations, tokens, spaces,
  organizations and workbook create/move/delete/restore/purge and backups; mutations, exports and
  function publication are 7.10.
