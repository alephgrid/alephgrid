# Security policy

## Reporting a vulnerability

Please do not open a public issue for a security problem.

Report it privately through GitHub: on the repository's **Security** tab choose
**Report a vulnerability**. Include what you found, the version or commit, and the steps or a
small workbook that shows it. Please give us a reasonable chance to fix it before you publish.

What to expect:

* An acknowledgement within 3 working days.
* An assessment and a planned fix date within 10 working days.
* A fix in the next release, published with a GitHub security advisory that credits you
  unless you prefer otherwise.

## Supported versions

Alephgrid is before 1.0. Only the latest release receives security fixes; upgrade to it to get
them. Upgrades migrate workbooks on open and keep the old file (see
[docs/OPERATIONS.md](docs/OPERATIONS.md)).

## What is in scope

Anything that lets a person or a file do more than its role allows: reading or changing a
workbook without access, escaping the SQL, Python or file-access limits, running code on the
server, taking the server down with one request or one file, or leaking secrets or session
tokens. A server run with `--auth local` has one fully trusted user by design.

How Alephgrid treats untrusted workbook files, requests and code, and the limits that apply,
is described in [docs/SECURITY.md](docs/SECURITY.md).
