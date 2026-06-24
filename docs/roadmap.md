# Roadmap

v1 (this repo, shipped): Project Repositories, static Project Outputs,
NIP-98 auth, operator publish grants, sharing with magic links, local dev
loop. This document sketches what comes next so v1 decisions stay compatible
with it.

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

What exists in the runtime: one tar.gz bundle, stable port, proxied behind
the same visibility gate. App output work must be Project-first before being
advertised to agents. The runtime now runs in
**Kata Containers microVMs** (Cloud Hypervisor), hardware-isolated, with a
**wake-on-request Supervisor**: idle apps are stopped (RAM freed) and woken
on the next request (~1.4s cold). Verified live on finite-lat-2 with
Bun+SQLite, Next.js standalone, and FastHTML; per-app microVMs 8–87 MiB.
The systemd runner (ADR-0014) remains for boxes without KVM. Remaining
(ledger items 9–10): websockets, an `fsite logs` surface, and optionally a
Firecracker snapshot tier for sub-second wake.

The original sketch, kept for the microVM upgrade:

- Project/share/auth surfaces are unchanged.
- A tier-2 Project Output deploys an app bundle plus a small typed runspec
  (entrypoint, port). Never raw compose/YAML from tenants — the control plane
  translates a constrained spec.
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
- Same Principal and Agent Delegation auth, same sharing gate in front of HTTP
  machines, same `fsite`-style CLI surface (`fsite machine ...`).

## Project Repository collaboration milestones

ADR-0019 makes Project-first git the collaboration model. The milestones
below keep the full product shape visible while letting the first
implementation stay focused.

### Milestone 1: Project Git Spine

- Pre-User Reset wipes product data so examples can be redeployed without
  compatibility adapters.
- Registry has final-shaped Principals, Agent Keys, Projects, Project
  Collaborators, Project Outputs, and Git Credentials.
- `fsite project apply --json ... --dry-run --output json` creates a Project
  with one site output.
- `fsite describe workflow publish-static-site --output json` gives agents
  the static-site happy path before they guess at removed direct publish
  commands.
- `fsite describe workflow project-config --output json` documents the
  `finite.toml` schema and example configs.
- `fsite auth git PROJECT [--email EMAIL] --store --output json` mints a scoped HTTPS
  Git Credential.
- `fsite project collaborator remove PROJECT --email EMAIL --output json`
  removes Project edit access and revokes active Git Credentials for that
  Principal.
- `git clone https://git.finite.chat/PROJECT.git` and `git push origin main`
  work with standard git through `git-http-backend` behind Finite auth.
- Pushing the Deploy Branch creates immutable Versions from committed bytes
  selected by the root `finite.toml`. The deploy system does not infer output
  paths; `fsite` workflows generate config for happy paths.
- Git `post-receive` records durable ref-change events; a reconciler creates
  Versions outside the git protocol request.
- Tests aggressively cover this chain: successful push/deploy, ignored
  non-deploy refs, invalid config failure, missing output failure, process
  crash after ref update before deploy, crash after Version creation before
  event acknowledgement, restart reconciliation, and idempotent replay.
- Generated `/llms.txt` prefers the git flow.
- Existing examples are redeployed through Project-first commands only.

### Milestone 2: Native Principals and Agent Delegations

- Native Finite users are shared to by npub/Principal, not email.
- Agents use distinct Agent Keys; humans approve project-scoped Agent
  Delegations from FiniteChat.
- Audit records both Principal and Agent Key.
- Email remains the External Principal bootstrap path for non-Finite users.

### Milestone 3: Multi-output Projects

- Registry and serving state are cut over so Project Output, not Site, owns
  visibility, shares, active versions, and version history.
- Output routing names are namespaced by output kind and serving domain, so a
  site, document, and PDF can share the same label on different domains.
- `finite.toml` supports multiple Project Outputs.
- `site` remains the first output kind. Document v0 is a narrow Markdown
  renderer output: a single Markdown file or folder of Markdown files selected
  by `finite.toml`, served through the same Project Repository and sharing
  flow.
- PDF outputs use the same Project Repository and Project Output versioning
  model once documents prove the path.
- Project Visibility remains independent from output Visibility.
- Output-level permissions exist only where they are truly needed.

### Milestone 4: Safer Collaboration Layer

- Rollback/redeploy previous Version becomes a first-class command.
- Branch or review flows are added only after the auto-publish Deploy Branch
  loop is working.
- Project history, output status, and deploy errors become machine-readable
  CLI workflows.

### Milestone 5: Protocol Interop

- Add GRASP/NIP-34 support only if Finite needs decentralized repository
  announcements, patch/issue protocol objects, interoperable servers, or repo
  migration across servers.
- Do not invent GRASP-equivalent protocol layers inside Finite.

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
