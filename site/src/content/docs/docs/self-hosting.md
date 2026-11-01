---
title: Self-hosting
description: Run Alephgrid for a team with accounts, roles, SSO, quotas, audit and backups.
---

The same single binary that runs on your laptop runs on a server. With `--auth accounts`,
people sign in with a password or single sign-on (OpenID Connect), and spaces grant them
owner, editor or viewer roles.

:::note[Pre-release]
Published container images and a Helm chart are planned after v0.1.0 — the
[releases page](../../releases/) tracks channel readiness. Today, self-hosting builds from source.
:::

## The short version

```bash
./target/release/alephgrid --auth accounts --public-url https://grid.example.com
```

- `--auth local` (the default) means no sign-in and loopback only — for your own machine.
- `--auth accounts` enables sign-in, spaces, roles, invitations, quotas, audit log and backups.
- `--public-url` is how people reach the server; an `https` URL makes cookies Secure. Put a
  TLS-terminating reverse proxy in front.
- Single sign-on: `--oidc-issuer` and `--oidc-client-id` (secret via
  `ALEPHGRID_OIDC_CLIENT_SECRET`).

## Operations

Running it for others — backups, upgrades, restores, storage ownership — is covered by the
repository's [operations guide](https://github.com/alephgrid/alephgrid/blob/main/docs/OPERATIONS.md).
Teams, roles and identity-provider groups:
[teams guide](https://github.com/alephgrid/alephgrid/blob/main/docs/TEAMS.md).

## Agents (MCP)

Read-only agent connections per space or workbook over Streamable HTTP (`/mcp`) or stdio
(`alephgrid mcp`): schema cards, catalog search, reads, read-only SQL, saved queries and
analyses, content-hashed receipts, and per-connection activity. See the
[MCP guide](https://github.com/alephgrid/alephgrid/blob/main/docs/MCP.md).
