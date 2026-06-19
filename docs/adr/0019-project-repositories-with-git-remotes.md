# Project Repositories With Git Remotes

Finite Sites will move from site-first publishing with Source Snapshots toward
Project Repositories as the collaboration source of truth. A Project Repository
is editable git history. A Finite Site is one Project Output. Pushing committed
bytes to a Deploy Branch creates immutable Versions; Finite Sites validates and
serves those bytes, but does not run builds.

This supersedes ADR-0017 as the long-term collaboration model. Source Snapshots
remain useful bootstrap scaffolding while git-backed Project Repositories are
implemented, but they are not the final multi-agent editing primitive.

Core decisions:

- Project Repository is the parent; Project Outputs such as sites or documents
  are produced from it.
- Project Slug and Site Name are separate identifiers, even when simple flows
  default them to the same string.
- Project Config is a root `finite.toml` that describes Project Outputs.
  Deploy Branch publishing requires this file. `fsite` may generate it for
  happy paths, but the deploy system should not infer output paths.
- Milestone 1 Project Config supports:
  `project.slug`, output IDs, `kind = "site"`, `site_name`, `branch`, `path`,
  and `spa`. The CLI must document this schema through Agent-Safe describe
  workflows so agents do not guess it.
- Agents may edit Project Config directly, but should prefer Agent-Safe
  `fsite project apply --dry-run` workflows when creating or changing outputs.
- Agents own build steps and commit Deploy Outputs. Finite Sites never becomes
  Vercel-style build infrastructure for this tier.
- Pushing to a Deploy Branch auto-publishes committed output bytes as a new
  Version. Visibility and permission changes remain owner-controlled.
- Project Visibility is private by default and independent from output
  Visibility.
- Project Collaborator is the primary edit permission. Site-scoped Editors are
  bootstrap/legacy language.
- Permissions attach to Principals. Email identifies External Principals;
  npubs identify Native Principals.
- Agents and devices act through distinct Agent Keys, authorized by
  project-scoped Agent Delegations. Agents do not sign with human keys.
- Git Remotes use standard git, canonically
  `https://git.finite.chat/{project}.git` in production.
- Git smart HTTP is served by `git-http-backend` behind Finite Sites
  authentication and authorization. Finite Sites should not implement the git
  protocol or run a full forge for the first Project Repository milestone.
- Bare Project Repositories live under the Finite Sites data dir, e.g.
  `DATA_DIR/git/projects/{project_id}.git`. Disk paths use stable internal
  IDs; Git Remote URLs use Project Slugs.
- Git `post-receive` hooks record ref-change events. A Finite Sites
  reconciler interprets those durable events and creates Versions. Deploy work
  must not happen directly inside the git protocol request.
- Ref-change events and resulting Version audit records include Principal,
  Agent Key when present, and Git Credential. Email bootstrap still creates or
  links a concrete key/agent identity; pushes are not audited as email alone.
- `fsite auth git PROJECT` mints scoped HTTPS Git Credentials after email
  verification or Key Challenge. The server never receives private keys.
- GRASP/NIP-34 are not required for the first implementation. If Finite Sites
  later needs decentralized repository announcements, patches, issues,
  interoperable servers, or migration across servers, adopt those protocols
  rather than inventing equivalents.
- Existing pre-user Sites are disposable. Use Pre-User Reset to wipe Finite
  Sites product state and redeploy examples through the Project-first model
  instead of carrying adapter or migration code.
- Pre-User Reset wipes product data such as registry state, blobs, app data,
  future git repositories, outbox, tokens, sessions, grants, collaborators,
  sites, and versions. It keeps host/runtime configuration such as installed
  binaries, systemd units, Caddy/Cloudflare configuration, mail provider
  configuration, OS users, deployment scripts, and source checkouts.
- `fsite` must become Agent-Safe: structured input/output, dry-run validation,
  deterministic errors, and machine-readable descriptions of commands and
  workflows.

**Considered Options**

- Keep Source Snapshots as the collaboration model: easy and already working,
  but agents collaborate better over git and snapshots have no native merge,
  branch, or history semantics.
- Build a Vercel-like server builder: clean for users, but it makes Finite
  own dependencies, caches, logs, secrets, timeouts, and reproducibility. Agents
  can run builds and commit bytes.
- Require GRASP first: aligned with nostr-native identity and decentralized
  collaboration, but too much protocol surface before proving the hosted
  Project loop. Raw git keeps agent workflows boring and standard.
- Keep Site as the parent: simpler from the current registry, but wrong for
  projects that begin as data/logic and only later produce sites, PDFs, or
  multiple outputs.
- Migrate or partially delete existing test Sites: preserves data that does
  not matter yet and creates compatibility code before the product model is
  stable.
