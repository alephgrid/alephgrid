---
title: CLI and configuration
description: Alephgrid server flags, environment variables and defaults.
---

`alephgrid --help` lists every option. The ones you'll reach for:

| Flag | Env | Default |
|------|-----|---------|
| `--data-dir` | `ALEPHGRID_DATA_DIR` | `./alephgrid-data` |
| `--host` / `--port` | `ALEPHGRID_HOST` / `ALEPHGRID_PORT` | `127.0.0.1` / `7878` |
| `--auth local\|accounts` | `ALEPHGRID_AUTH` | `local`: no sign-in, loopback only |
| `--public-url` | `ALEPHGRID_PUBLIC_URL` | how people reach the server (needed for SSO; https makes cookies Secure) |
| `--oidc-issuer`, `--oidc-client-id` | `ALEPHGRID_OIDC_*` (secret: `ALEPHGRID_OIDC_CLIENT_SECRET`) | no single sign-on |
| `--session-hours` | `ALEPHGRID_SESSION_HOURS` | 12 |
| `--space-storage`, `--space-workbooks`, `--space-jobs`, `--space-open-workbooks` | `ALEPHGRID_SPACE_*` | no limits (admins can set them per space) |
| `--memory-limit` | `ALEPHGRID_MEMORY_LIMIT` | DuckDB default (80% of RAM, spills to disk) |
| `--threads` | `ALEPHGRID_THREADS` | all cores |
| `--idle-timeout` | | seconds without activity before a workbook's compute sleeps |
| `--profile name=MEM:THREADS[:dedicated]` | | extra compute sizes admins can grant to spaces |
| `--disable-sql` | `ALEPHGRID_DISABLE_SQL` | SQL allowed; when set, every operation that runs user-written SQL is refused |
| `--demo-rows N` | | generate a demo workbook |
| `alephgrid mcp --url URL --token agc_…` | `ALEPHGRID_URL`, `ALEPHGRID_AGENT_TOKEN` | connect an MCP client over stdio to a running server |

This table mirrors the repository
[README](https://github.com/alephgrid/alephgrid#options) at the time of writing; when they
disagree, `--help` on your build is authoritative.
