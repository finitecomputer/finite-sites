# Roadmap

v1 (this repo, shipped): static tier, NIP-98 auth, operator publish grants,
sharing with magic links, local dev loop. This document sketches what comes
next so v1 decisions stay compatible with it.

## Production deploy (tier 1 on real metal)

A dedicated SaaS box, separate from user agent machines:

- DNS: `*.finite.chat` A record to the box; `api.finite.chat` for the
  control plane.
- Edge: Caddy (or Traefik) terminating TLS with one wildcard cert via
  DNS-01, proxying everything to `finitesitesd` with the original Host
  preserved. No per-site edge configuration, ever — the AI Lounge
  postmortem rule. Run finitesitesd with
  `--base-domain finite.chat --site-scheme https --site-port none
  --api-url https://api.finite.chat`.
- Storage: Garage for blobs (new `BlobStore` impl), Litestream replicating
  `registry.db` to Garage.
- Mail: Postmark or SES `Mailer` implementation.
- Ledger items 1, 2, 4, 7 must close before this deploy.
- Agent machines get the `fsite` binary and the publishing skill; the
  publish grant cache is the onboarding lever. Operator grants handle VIPs and
  early migrations; Core-synced grants become the paid-entitlement path.

## Tier 2: stateful sites — SHIPPED, hardware-isolated (ADR-0014, ADR-0015)

What shipped: `fsite publish-app NAME PATH --start "CMD"` — one tar.gz
bundle, stable port, proxied behind the same visibility gate. Now runs in
**Kata Containers microVMs** (Cloud Hypervisor), hardware-isolated, with a
**wake-on-request Supervisor**: idle apps are stopped (RAM freed) and woken
on the next request (~1.4s cold). Verified live on finite-lat-2 with
Bun+SQLite, Next.js standalone, and FastHTML; per-app microVMs 8–87 MiB.
The systemd runner (ADR-0014) remains for boxes without KVM. Remaining
(ledger items 9–10): websockets, an `fsite logs` surface, and optionally a
Firecracker snapshot tier for sub-second wake.

The original sketch, kept for the microVM upgrade:

- Claim/share/auth surfaces are unchanged.
- A tier-2 publish uploads the app bundle the same way (manifest + blobs)
  plus a small typed runspec (entrypoint, port). Never raw compose/YAML
  from tenants — the control plane translates a constrained spec.
- Runtime: one container per app under gVisor (runsc), read-only rootfs
  materialized from the blob store, one writable volume for the SQLite
  file, CPU/mem/pids quotas, default-deny egress, sleep on idle / wake on
  request from the serving plane.
- One host-level Litestream 0.5 process replicates `*/data/*.db` to
  Garage.
- The router gains "proxy to tenant socket" alongside "serve blob".

The point: the exact Bun+SQLite app the agent ran on the user's Finite
Computer ships unchanged. Getting sites off user machines must not require
a rewrite.

## Tier 3: finite machines (arbitrary containers)

- Kata Containers runtime class on k3s: every tenant pod a hardware
  isolated microVM (~150–300ms starts).
- Fly-Machines-style API in the control plane: create/start/stop/destroy,
  image from a registry, per-machine volumes.
- Same auth (site key = machine key), same sharing gate in front of HTTP
  machines, same `fsite`-style CLI surface (`fsite machine ...`).

## Later / optional

- **nsite export**: publish kind 15128/35128 manifests + push blobs to
  public Blossom servers as a censorship-resistance escape hatch; finite
  stays the fast canonical host (re-uses finite-site's vendored tooling).
- **Custom domains**: CNAME to the box + per-domain cert via Caddy
  on-demand TLS, gated on a registry check.
- **Billing**: BTCPay, Stripe, or agentic payments for non-VIP users. Payments
  mint Core-owned publish grants synced to the Sites registry; operator grants
  remain the VIP/comp lever.
- **Rollback**: `fsite rollback NAME` — the registry already stores
  immutable versions; this is a pointer flip plus contract questions
  (finite-site deliberately deferred it).
