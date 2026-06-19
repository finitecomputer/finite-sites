# Context

Glossary for Finite Sites. Code, docs, and prompts should use these words
with exactly these meanings.

- **Finite Site**: one published website living at `{name}.{base domain}`,
  owned by one User Key, with an immutable version history.
- **Principal**: the human-facing identity permissions attach to. A Principal
  may be represented by an email address during bootstrap and by verified key
  identities once available.
- **Native Principal**: a Principal known by npub inside Finite surfaces, such
  as a chat participant. Native shares can target this Principal directly.
- **External Principal**: a Principal identified by email because they are not
  yet a Finite user. External shares use email verification.
- **Project Repository**: the editable git history for a project. It may begin
  with data, grow logic around that data, and later produce one or more Project
  Outputs. A Project Repository may exist before any public-facing UI exists.
- **Project Slug**: the stable URL-safe identifier for a Project Repository.
  It is separate from Site Name, though simple projects may default them to
  the same string.
- **Project Output**: a user-facing artifact produced from a Project
  Repository, such as a Finite Site or a generated document.
- **Deploy Output**: committed files selected from a Project Repository and
  materialized as a Version. Agents produce Deploy Outputs; Finite Sites
  validates and serves them.
- **Deploy Branch**: the Project Repository branch whose pushed commits create
  new Versions automatically. Pushing to a Deploy Branch updates content but
  does not change visibility or permissions.
- **Project Visibility**: who may read a Project Repository. It is private by
  default and independent from the Visibility of any Project Output.
- **Site Name**: a lowercase DNS label (3–63 chars), globally unique,
  first-come, claimed before any upload. Reserved names are rejected.
- **Pre-User Reset**: a destructive operator action that wipes Finite Sites
  product state during pre-user development so examples can be redeployed
  through the current model without legacy adapters.
- **User Key / Owner**: the user's nostr keypair (npub). It claims names,
  lists sites, and may change sharing. The publish grant cache is keyed on it.
- **Owner Email**: the human-facing email label for a site's owner. It may
  publish through a verified Email Key, but it does not replace the User Key.
- **Project Collaborator**: an email address or key identity granted edit
  rights to a Project Repository. Project collaboration is the default edit
  permission; individual Project Outputs may add narrower rules later.
- **Agent Key**: a distinct npub controlled by an agent or device and linked
  to a Principal. Agent Keys authenticate work without making the agent the
  human owner.
- **Agent Delegation**: a Principal-approved authorization that lets one Agent
  Key act for that Principal on one Project Repository, with bounded
  capabilities.
- **Git Remote**: the standard git clone/push endpoint for a Project
  Repository, canonically `https://git.finite.chat/{project}.git` in
  production. Agents use normal git commands against it; Finite Sites maps
  authenticated pushes to Project Repository permissions.
- **Git Credential**: a revocable, scoped HTTPS credential minted after an
  email verification or Key Challenge. It lets standard git clients clone or
  push one Project Repository according to the Principal's permissions.
- **Agent-Safe CLI**: a command surface that agents can inspect and operate
  without out-of-band documentation. It provides structured input/output,
  dry-run validation, and machine-readable descriptions of available commands
  and workflows.
- **Project Config**: a project-level configuration file, conventionally
  `finite.toml`, describing Project Outputs such as sites or documents.
- **Key Challenge**: proof of control for a nostr key. The private key never
  leaves the user's machine; the actor signs a bounded challenge instead.
- **Site Key**: a per-site nostr keypair generated in the agent workspace at
  `.finite/sites/NAME.env`, registered at claim time. It signs publishes and
  sharing changes for that one site. Never committed, never uploaded.
- **Email Key**: a local nostr keypair verified for one email address by a
  single-use email token. It signs email-keyed publishes without exposing npubs.
- **Editor**: an email address granted publish rights for one site. Editors may
  create Versions but do not become Owners and do not gain viewer access.
- **Publish Grant Cache**: the local registry table deciding whether a User
  Key may claim and publish. Operator grants stand in for billing in v1;
  Core grants become the paid-entitlement path. If no active, unexpired grant
  exists, claim/publish fails closed.
- **Allowlist**: the deployed operator command surface for adding/removing
  `operator` publish grants. De-allowlisting an owner only removes the
  operator grant; a separate active Core grant can still allow publishing.
- **Publish Session**: a pending upload: a validated manifest plus the set
  of blobs the server still needs. Finalizing it creates a Version.
- **Manifest**: the list of `(path, sha256, size)` entries describing one
  complete site version. Paths are absolute and conservatively validated.
- **Blob**: immutable bytes stored by sha256, deduplicated across all sites
  and versions. Uploads are verified against the hash they claim.
- **Version**: an immutable snapshot created by a finalized publish. The
  site serves its **Active Version**; the pointer flip is atomic.
- **Source Snapshot**: an optional immutable `tar.gz` source archive attached
  to a Version. It is for editor handoff and is never served as site content.
- **Agent Handoff File**: `/llms.txt` on a Finite Site. A user-authored file
  is ordinary site content. If absent, the platform may synthesize one for
  editable static sites with Source Snapshots so agents can discover the
  source-pull and email-keyed publish flow.
- **Visibility**: `private` (nobody), `shared` (emails on the Share list),
  or `public`. Sites are born private. Making a site public requires an
  explicit confirmation from the human, relayed as `confirm_public`.
- **Share**: one `(site, email)` row granting view access. Removing it
  revokes access on the next request, even for live cookies.
- **Magic Link**: a single-use, 15-minute login token mailed to a shared
  email. Redeeming it sets a Viewer Cookie on the site's own host.
- **Viewer Cookie**: an HMAC-signed `(site, email, expiry)` proof, scoped to
  one site host. It proves login; the Share table decides access.
- **Control Plane**: the NIP-98-authenticated API (claim, publish, share,
  status). **Serving Plane**: anonymous-or-cookie HTTP on site subdomains.
  One process serves both in v1, split by Host header.
- **Base Domain**: the wildcard domain under which sites live —
  `sites.localhost` in development, `finite.chat` in production.
- **Outbox**: the dev mailer's output directory; each would-be email is a
  text file containing the magic link.
