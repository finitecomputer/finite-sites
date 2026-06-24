# Project Repositories With Git Remotes

Finite Sites uses Project Repositories as the collaboration source of truth. A
Project Repository is editable git history. A Finite Site is one Project
Output. Pushing committed bytes to a Deploy Branch creates immutable Versions;
Finite Sites validates and serves those bytes, but does not run builds.

Because Finite Sites has not shipped to real users, there is no compatibility
surface for a site-first publishing model. The supported agent-facing flow is
Project Repositories plus Project Outputs, exercised through Pre-User Reset and
example redeploys rather than adapters.

Core decisions:

- Project Repository is the parent; Project Outputs such as sites, documents,
  and PDFs are produced from it.
- Project Output is the serving, visibility, sharing, active-version, and
  version-history primitive. Site, document, and PDF names are routing names
  for specific output kinds, not separate permission or versioning surfaces.
- Output routing names are namespaced by output kind and serving domain. A
  Site Name, Document Name, and PDF Name may use the same DNS label because
  they resolve under different wildcard domains.
- Project Slug and Site Name are separate identifiers, even when simple flows
  default them to the same string.
- Project Config is a root `finite.toml` that describes Project Outputs.
  Deploy Branch publishing requires this file. `fsite` may generate it for
  happy paths, but the deploy system should not infer output paths.
- Document Outputs also require an explicit Project Config entry. Pushing a
  lone Markdown file never creates or exposes a document by inference; single
  file documents are supported by pointing an output path at that Markdown
  file.
- Document Output `path` may point to either one Markdown file or one
  directory. File-vs-directory is a property of the committed tree, not a
  separate output kind. Directory documents may declare an `entry`; if omitted,
  the entry is `index.md`. Single-file documents use their source file as the
  entry.
- Document v0 is intentionally small product surface: `kind = "document"` in
  Project Config selects one Markdown file or a folder of Markdown files from
  the Project Repository, and Finite renders those files. It does not add a
  separate document editor, document collaborator model, document workflow, or
  hand-maintained navigation product.
- Document Output versions store exact authored Markdown bytes selected from
  the active Project Repository commit. Finite renders viewer HTML server-side
  in Rust. Agents do not commit generated HTML for Document Outputs, and
  rendered HTML is not the durable document artifact.
- Document rendering is a strict renderer subset, not a heavy validation
  regime. The v0 renderer must handle ordinary Markdown, frontmatter, folder
  `_index.md` files, and Obsidian-style wikilinks well enough for llm-wiki
  style folders. It should not reject a document because prose quality,
  unknown frontmatter, or uncommon Markdown extensions are imperfect; content
  outside the subset may render plainly, be ignored, or become a warning.
- Document Navigation is derived from Markdown files in v0. Markdown files
  under the Document Root become routes; `_index.md` is ordinary Document
  Markdown and may provide directory landing content or ordering hints, but v0
  does not require authors to hand-maintain a nav tree before a document can
  render.
- llm-wiki compatibility does not mean adopting llm-wiki's whole operational
  schema. `raw/`, `wiki/`, `output/`, `inventory/`, `datasets/`, `.sessions/`,
  `config.md`, and `log.md` are just Markdown/files from Finite's perspective
  unless a future Document Component or workflow gives them product meaning.
- PDF Outputs use the same Project Output/version model. The agent or user
  generates the PDF and commits it; Finite Sites stores and serves the selected
  PDF bytes, but does not generate PDFs in the server.
- Milestone 1 Project Config supports:
  `project.slug`, output IDs, `kind = "site"`, `site_name`, `branch`, `path`,
  and `spa`. The CLI must document this schema through Agent-Safe describe
  workflows so agents do not guess it.
- `fsite describe workflow publish-static-site --output json` is the canonical
  first command for agents creating a static site Project Output. It must
  explain the mental model: Project Repository is source, `finite.toml` selects
  the served output path, authorized collaborators clone the whole source tree,
  Finite serves committed bytes under the output path to viewers, and there is
  no direct bundle upload command in the current model.
- If an agent tries a removed site-first command such as `fsite publish`, the
  CLI should fail with guidance to the Project Repository workflow rather than
  a bare unknown-command error.
- Agents may edit Project Config directly, but should prefer Agent-Safe
  `fsite project apply --dry-run` workflows when creating or changing outputs.
- Agents own build steps and commit Deploy Outputs. Finite Sites never becomes
  Vercel-style build infrastructure for this tier.
- Pushing to a Deploy Branch auto-publishes committed output bytes as a new
  Version. Visibility and permission changes remain owner-controlled.
- Project Visibility is private by default and independent from output
  Visibility.
- Project Collaborator is the edit permission.
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
  verification for External Principals or User-Key authentication for Native
  Principals. `--store` is the agent-preferred path: it writes the scoped
  credential to Git's credential helper instead of printing the password. The
  server never receives private keys.
- GRASP/NIP-34 are not required for the first implementation. If Finite Sites
  later needs decentralized repository announcements, patches, issues,
  interoperable servers, or migration across servers, adopt those protocols
  rather than inventing equivalents.
- Existing pre-user Sites are disposable. Use Pre-User Reset to wipe Finite
  Sites product state and redeploy examples through the Project-first model
  instead of carrying adapter or migration code.
- Generated Agent Handoff Files must use Project Repository instructions.
- Pre-User Reset wipes product data such as registry state, blobs, app data,
  future git repositories, outbox, tokens, sessions, grants, collaborators,
  sites, and versions. It keeps host/runtime configuration such as installed
  binaries, systemd units, Caddy/Cloudflare configuration, mail provider
  configuration, OS users, deployment scripts, and source checkouts.
- `fsite` must become Agent-Safe: structured input/output, dry-run validation,
  deterministic errors, and machine-readable descriptions of commands and
  workflows.

**Considered Options**

- Build a Vercel-like server builder: clean for users, but it makes Finite
  own dependencies, caches, logs, secrets, timeouts, and reproducibility. Agents
  can run builds and commit bytes.
- Require GRASP first: aligned with nostr-native identity and decentralized
  collaboration, but too much protocol surface before proving the hosted
  Project loop. Raw git keeps agent workflows boring and standard.
- Keep Site as the parent: simpler from the current registry, but wrong for
  projects that begin as data/logic and only later produce sites, PDFs, or
  multiple outputs.
- Add document/PDF-specific sharing and version tables: narrow in the moment,
  but it creates multiple product surfaces for the same permission and history
  concepts. Project Output is the stable abstraction.
- Use one global output-name namespace: simpler uniqueness checks, but it
  makes multi-output projects fight themselves for names. Separate domains
  make the artifact kind legible from the URL.
- Store generated document HTML: quick to serve, but it collapses documents
  back into static sites and weakens future annotation, Markdown companion URL,
  and agent-editing flows. Server-side rendering keeps authored Markdown as
  the semantic document source.
- Migrate or partially delete existing test Sites: preserves data that does
  not matter yet and creates compatibility code before the product model is
  stable.
