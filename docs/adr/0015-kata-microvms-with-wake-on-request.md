# Tier 2 Isolation: Kata microVMs + Wake-On-Request Supervisor

App sites run as **Kata Containers microVMs** (containerd + the
`io.containerd.kata.v2` runtime, Cloud Hypervisor VMM), managed by a thin
**Supervisor** in finitesitesd that wakes apps on the first request and
stops them when idle. This supersedes ADR-0014's systemd DynamicUser
sandbox: isolation moves from kernel-level to hardware-level, and idle
apps cost ~0 memory.

## Why this stack (and not microsandbox or k8s)

We weighed microsandbox (libkrun) and Kata. microsandbox is a *library for
ephemeral, per-agent, run-untrusted-snippet sandboxes* — it gives the one
primitive we'd get from Kata anyway (a microVM) while missing everything
that makes *hosting* tractable (scheduling, restart, fleet networking,
snapshot/suspend, resource accounting), and it has been renamed across orgs
twice while self-describing as beta. Building a supervisor over raw
libkrun is the "reinvent a worse Kubernetes" trap.

Kata-under-containerd threads the needle: **containerd supplies the
battle-tested image/rootfs/lifecycle primitives**, **Kata supplies the
microVM isolation transparently**, and we skip Kubernetes' operational
surface. The orchestration we write is only what we already owned —
placement, ingress, restart — because finitesitesd is already the proxy
and supervisor in front of every app. We are not reinventing the lifecycle
engine.

## The density mechanism is wake-on-request, not overcommit

The decisive finding: the way to "run lots of tiny VMs" is **not keeping
idle ones resident**. Because the proxy fronts every app, the Supervisor
stops an app after an idle window (default 15 min) — tearing down its
microVM and freeing its RAM — and starts it again on the next request,
waiting for the port. Verified on finite-lat-2: idle apps reaped, a cold
request woke a stopped microVM and served in ~1.4s, warm ~0.3s; resident
app microVMs measured 8–87 MiB each. Idle tenants trend to ~0 RAM, so a
box "hosts" far more apps than fit resident at once. This is Fly's
suspend/resume model, minus the snapshot tier (a future optimization).

## Mechanics

- Runner trait (`deploy`/`ensure_started`/`stop`/`is_running`) abstracts
  isolation; `DisabledRunner`, `SystemdAppRunner` (ADR-0014, retained),
  and `KataAppRunner` implement it. Selected with `--app-runner`.
- KataAppRunner drives `sudo nerdctl` (CNI bridge setup needs
  CAP_NET_ADMIN). A public runtime image is chosen per start command
  (node/bun/uv) so no image build is needed — the only daemons are
  containerd and the Kata shim. Bundle is bind-mounted read-only; the
  app's `$DATA_DIR` is a host directory surviving stop/start; the proxy
  forwards to the microVM's bridge IP.
- The Supervisor tracks per-app last-access in unix seconds (so reaping is
  unit-tested with an injected clock), wakes on request, and a 60s reaper
  task stops idle apps.

## Tradeoffs accepted

- The Kata config relaxes the daemon's own filesystem sandbox so nerdctl
  can run (drop-in). Justified: tenant isolation is now the microVM
  boundary (stronger), and the daemon stays unprivileged-user with a
  single nerdctl-scoped sudo path.
- KSM/templating for shared-page density is deliberately NOT used — it
  weakens isolation between mutually-untrusting tenants.

**Considered Options**

- microsandbox/libkrun + own supervisor: wrong shape (sandbox SDK), beta,
  and the reinvent-k8s trap.
- k3s + Kata RuntimeClass: least orchestration code, but full k8s
  operational surface against the "small surface" goal.
- containerd + Kata + our thin supervisor: best fit for a density-first
  single operator; chosen.
- Firecracker snapshot suspend/resume for idle: the strongest idle-cost
  primitive; deferred — stop/start already gets idle cost to ~0, and
  snapshots add a tier we can layer on later.
