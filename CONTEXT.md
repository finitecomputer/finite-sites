# Context

Glossary for Finite Sites. Code, docs, and prompts should use these words
with exactly these meanings.

- **Finite Site**: one published website living at `{name}.{base domain}`,
  owned by one User Key, with an immutable version history.
- **Site Name**: a lowercase DNS label (3–63 chars), globally unique,
  first-come, claimed before any upload. Reserved names are rejected.
- **User Key / Owner**: the user's nostr keypair (npub). It claims names,
  lists sites, and may change sharing. The publish grant cache is keyed on it.
- **Site Key**: a per-site nostr keypair generated in the agent workspace at
  `.finite/sites/NAME.env`, registered at claim time. It signs publishes and
  sharing changes for that one site. Never committed, never uploaded.
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
