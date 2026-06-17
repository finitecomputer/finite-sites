# Tier 2 v0: App Bundles In systemd Sandboxes

A tier-2 app site is published as one `tar.gz` bundle plus a start
command (`fsite publish-app NAME PATH --start "CMD"`). The platform
extracts the bundle into a read-only release directory, assigns the site
a stable loopback port, and runs the command as a `finite-app@{site_id}`
systemd template instance: DynamicUser, one writable StateDirectory
(`$DATA_DIR`/`$HOME`), memory/CPU/task caps. The serving plane proxies
the site host to the port behind the same visibility gate as static
sites. finitesitesd controls instances over systemd's D-Bus API,
authorized by a polkit rule scoped to `finite-app@*` (sudo is impossible:
the daemon itself runs with NoNewPrivileges).

The contract is runtime-agnostic — the command just has to listen on
`$PORT` and write only under `$DATA_DIR`. Node/Bun and Python (uv with
PEP 723 inline deps) are the blessed lanes; Next.js standalone and
FastHTML are the reference examples.

**Isolation posture, stated plainly**: a systemd sandbox is real but it
is kernel-level, not hardware-level. It is acceptable while every
publisher has an active operator publish grant; it is not acceptable for hostile
tenants. The upgrade path (roadmap) is moving the same bundle + runspec
contract onto MicroSandbox/Kata microVMs without changing the publish
surface. Tracked in the technical debt ledger.

**Considered Options**

- File-manifest publish for apps (like static): a Next standalone output
  is thousands of files; one bundle blob keeps sessions to one upload and
  dedup at artifact granularity.
- sudo for unit control: blocked by the daemon's own NoNewPrivileges
  hardening — D-Bus + polkit is also strictly narrower.
- MicroVMs from day one: the right destination, but it gates "does the
  product work" on the hardest infrastructure; the bundle/runspec
  contract is the part that must not change later, so it shipped first.
- Plain child processes under the daemon: no isolation between apps and
  the platform's own user; rejected outright.
