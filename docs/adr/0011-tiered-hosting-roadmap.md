# Tiered Hosting Behind Project Outputs

Finite Sites is designed as three isolation tiers behind the same Project
Repository, Project Output, and sharing surface. v1 ships tier 1 only.

1. **Static** (shipped): manifest + blobs, zero per-site processes.
2. **Stateful sites**: a normal app process (e.g. Bun + a SQLite file) run
   in a gVisor-sandboxed container with a read-only rootfs, one writable
   data volume, default-deny egress, and sleep/wake on idle. One host-level
   Litestream process replicates every tenant database to object storage.
3. **Finite Machines**: arbitrary containers as Kata microVM pods on k3s,
   Fly-Machines-style start/stop, for the railway/fly use case.

The defining property: the artifact the agent built and ran on the user's
Finite Computer is the artifact that ships — a folder for tier 1, the same
process + SQLite file for tier 2, a container image for tier 3. No
platform-specific rewrite at any tier.

**Considered Options**

- Cloudflare Workers for Platforms: least ops, but stateful apps must be
  rewritten as Workers + D1 and there is no real container story.
- Machines-only (everything a microVM): uniform but wasteful for static
  sites, which are most of the volume.
- Three tiers behind one project model: each workload gets the cheapest sufficient
  isolation; chosen.
