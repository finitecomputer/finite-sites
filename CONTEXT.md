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
  Repository, such as a Finite Site, Document Output, or PDF Output. Project
  Outputs own serving visibility, sharing, active version pointers, and version
  history.
- **Output Routing Name**: the globally unique DNS label for a Project Output
  within that output kind's serving namespace. Output Routing Names are not
  global across all kinds; a site, document, and PDF may share the same label
  because they live on different serving domains.
- **Document Output**: a read-only Project Output whose source is authored as
  Markdown in a Project Repository and viewed as a rendered document. It is for
  collaborative writing and review, not only software documentation.
- **PDF Output**: a read-only Project Output whose served artifact is a PDF
  committed to a Project Repository. An agent or user generates the PDF before
  pushing; Finite Sites stores and serves the committed PDF bytes as immutable
  output versions.
- **PDF Name**: the globally unique, stable URL-safe identifier for a PDF
  Output, served under the PDF base domain. It is separate from Site Name and
  Document Name.
- **PDF Base Domain**: the serving-plane wildcard domain under which PDF
  Outputs live. It does not host Project Repository control-plane APIs.
- **Document Visibility**: the Visibility of a Document Output. It uses the
  same private, shared, and public meanings as other Project Outputs and is
  independent from Project Visibility.
- **Document Name**: the globally unique, stable URL-safe identifier for a
  Document Output, served under the document base domain. It is separate from
  Site Name.
- **Document Base Domain**: the serving-plane wildcard domain under which
  Document Outputs live, canonically `docs.finite.chat` in production. It does
  not host Project Repository control-plane APIs.
- **Document Source Path**: the project-relative path declared for a Document
  Output. It points either to one Document Markdown file or to a Document Root
  directory.
- **Document Root**: the directory in a Project Repository that contains the
  Markdown files for a directory-shaped Document Output.
- **Document Entry**: the Markdown file inside a Document Root that opens when
  a viewer visits the Document Output root URL. Directory Documents default to
  `index.md`; Single-File Documents use the source file as the entry.
- **Single-File Document**: a Document Output whose source is one Document
  Markdown file. The file is the Document Entry and renders at the Document
  Output root.
- **Document Project Output Config**: the `finite.toml` output entry that
  declares a Document Output. It uses the same Project Repository, Deploy
  Branch, and collaborator model as other Project Outputs.
- **Document Directory Index**: an optional `_index.md` file inside a
  Document Root directory. It is ordinary Document Markdown and may provide
  navigation or ordering hints for that directory, matching common llm-wiki
  folder conventions; it is not required and is not a generated cache.
- **Document Markdown**: the Markdown source for a Document Output. Finite
  Sites stores the authored text and promises a strict Document Renderer
  Subset; content outside that subset may render plainly or be ignored.
- **Document Frontmatter**: optional YAML metadata at the top of a Document
  Markdown file. Recognized fields may shape document presentation and
  navigation; unknown fields remain source metadata and are ignored by the
  renderer.
- **Document Renderer Subset**: the bounded Markdown features the Rust
  renderer must handle predictably for v0: headings, paragraphs, emphasis,
  lists, blockquotes, code spans and fences, links, images, tables,
  frontmatter, directory indexes, and Document Wikilinks. Raw HTML and richer
  blocks are outside the subset until they become explicit Document
  Components.
- **Document Component**: an explicit, allowlisted rich block or inline element
  in a Document Output. Document Components are product features, not arbitrary
  raw HTML or JavaScript.
- **Document Route**: the viewer-facing path for a Markdown file in a
  Document Output. Document Routes are clean URLs derived from Markdown paths
  inside the Document Root.
- **Document Navigation**: the viewer navigation for a Document Output,
  derived from the Document Snapshot unless a later document feature gives
  authors explicit navigation control.
- **Document Wikilink**: an Obsidian-style link inside Document Markdown,
  such as `[[Page]]` or `[[Page|label]]`. Document Wikilinks are a
  compatibility feature resolved within one Document Root; standard Markdown
  links remain the canonical link format.
- **Document Snapshot**: the exact authored Document Markdown selected for one
  deployed Document Output version. Finite Sites renders from that Markdown;
  rendered HTML is not the source of truth.
- **Document Warning**: a non-blocking issue found in a Document Snapshot,
  such as a broken internal link or unresolved Document Wikilink. Document
  Warnings do not prevent a Document Output from being served.
- **Deploy Output**: committed files selected from a Project Repository and
  materialized as a Version. Agents produce Deploy Outputs; Finite Sites
  validates and serves them.
- **Deploy Branch**: the Project Repository branch whose pushed commits create
  new Versions automatically. Pushing to a Deploy Branch updates content but
  does not change visibility or permissions.
- **Project Visibility**: who may read a Project Repository. It is private by
  default and independent from the Visibility of any Project Output.
- **Site Name**: the Output Routing Name for a Finite Site. It is a lowercase
  DNS label (3–63 chars), globally unique within the Site Base Domain,
  first-come, allocated by a Project Output before any Version is deployed.
  Reserved names are rejected.
- **Pre-User Reset**: a destructive operator action that wipes Finite Sites
  product state during pre-user development so examples can be redeployed
  through the current model without legacy adapters.
- **User Key / Owner**: the user's nostr keypair (npub). It owns Project
  Repositories, lists outputs, and may change output sharing. The publish
  grant cache is keyed on it.
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
- **Email Key**: a local nostr keypair verified for one email address by a
  single-use email token. It signs email-keyed project git credential requests
  without exposing npubs.
- **Publish Grant Cache**: the local registry table deciding whether a User
  Key may create Projects, allocate Project Outputs, and deploy new Versions.
  Operator grants stand in for billing in v1; Core grants become the
  paid-entitlement path. If no active, unexpired grant exists, project apply
  and git deploy fail closed.
- **Allowlist**: the deployed operator command surface for adding/removing
  `operator` publish grants. De-allowlisting an owner only removes the
  operator grant; a separate active Core grant can still allow publishing.
- **Publish Session**: a pending upload: a validated manifest plus the set
  of blobs the server still needs. Finalizing it creates a Version.
- **Manifest**: the list of `(path, sha256, size)` entries describing one
  complete site version. Paths are absolute and conservatively validated.
- **Blob**: immutable bytes stored by sha256, deduplicated across all sites
  and versions. Uploads are verified against the hash they claim.
- **Version**: an immutable Project Output snapshot created from a Deploy
  Branch push. The Project Output serves its **Active Version**; the pointer
  flip is atomic.
- **Agent Handoff File**: `/llms.txt` on a Project Output. A user-authored
  file is ordinary output content. If absent, the platform may synthesize one
  for editable outputs so agents can discover the supported edit flow.
- **Agent Full Context File**: `/llms-full.txt` on a Document Output. It is a
  bounded Markdown concatenation of the Document Snapshot for agents that want
  one fetch; oversized documents fall back to the Agent Handoff File index.
- **Document Agent Links**: machine-discoverable links on a rendered Document
  Route that point agents to the Agent Handoff File and to the page's
  Markdown companion URL. Document Agent Links obey the Document Output's
  Visibility.
- **Markdown Companion URL**: the raw Document Markdown representation of a
  Document Route, exposed by appending `.md` to the human-facing route shape
  instead of using a separate platform namespace. It returns the exact authored
  Document Markdown for that page. Directory index pages use the route-shaped
  companion URL; the Document Output root uses `/index.md`.
- **Visibility**: `private` (nobody), `shared` (emails on the Share list),
  or `public`. Sites are born private. Making a site public requires an
  explicit confirmation from the human, relayed as `confirm_public`.
- **Share**: one `(Project Output, Principal)` row granting view access to a
  served output. Removing it revokes access on the next request, even for live
  cookies.
- **Magic Link**: a single-use, 15-minute login token mailed to a shared
  email. Redeeming it sets a Viewer Cookie on the site's own host.
- **Viewer Cookie**: an HMAC-signed `(site, email, expiry)` proof, scoped to
  one site host. It proves login; the Share table decides access.
- **Control Plane**: the NIP-98-authenticated API (project apply, git auth,
  sharing, status). **Serving Plane**: anonymous-or-cookie HTTP on site
  subdomains. One process serves both in v1, split by Host header.
- **Base Domain**: the wildcard domain under which sites live —
  `sites.localhost` in development, `finite.chat` in production.
- **Outbox**: the dev mailer's output directory; each would-be email is a
  text file containing the magic link.
