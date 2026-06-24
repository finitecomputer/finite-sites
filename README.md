# Finite Sites

Self-hosted site publishing for Finite Computer agents. A user says "make me
a website"; their agent creates a Project Repository, commits deploy bytes,
and pushes the Deploy Branch for `name.finite.chat`. Sites are private by
default, shareable with specific emails via magic links, or public — like
sharing a Google Doc.

This replaces self-hosting sites from inside agent machines (see the
AI Lounge postmortem in finitecomputer) and the nsite-based prototype in
finite-site: agents collaborate in git, while the serving substrate is
finite-owned storage behind one wildcard domain.

## What works today (v1)

- **Static sites**: Project Repository pushes produce content-addressed
  immutable Versions with atomic latest-pointer flips and ETag revalidation.
- **Nostr auth**: every registry mutation is a NIP-98-signed request. The
  user identity key owns Projects and output sharing.
- **Project Repositories**: a Project is a git repo plus one or more
  `finite.toml` Project Outputs. Standard git clone/push works through
  `git-http-backend` behind Finite auth, and pushes to a Deploy Branch create
  immutable Versions from committed bytes.
- **Agent handoff**: project-backed editable outputs get a generated
  `/llms.txt` unless the user published that path themselves. The generated
  file gives agents git-first instructions.
- **Publish grant cache**: only npubs with an active operator or Core grant can
  create Project Outputs and publish Versions. The deployed allowlist commands
  manage operator grants; payments/Core sync are the next source.
- **Sharing**: per-site visibility `private` / `shared` / `public`, email
  ACLs, magic-link login, host-scoped signed cookies. Revoking an email
  takes effect on the next request.
- **Agent surface**: the `fsite` CLI hides nostr/keys/manifests entirely.

Stateful sites (SQLite-backed apps) and full containers are tiers 2 and 3
behind the same Project Repository model — see `docs/roadmap.md`.

## Layout

| Crate | What it owns |
|---|---|
| `finitesites-proto` | nostr events, NIP-98, manifests, names, limits, DTOs |
| `finitesites-blob` | content-addressed blob storage (filesystem; Garage/S3 seam) |
| `finitesites-store` | SQLite registry: publish grants, projects, outputs, versions, shares, tokens |
| `finitesites-engine` | all decisions: project apply/git deploy/share/view/magic links |
| `finitesitesd` | the server: control-plane API + wildcard site serving + grant ops |
| `fsite-cli` | agent-facing CLI (`fsite`) |

## Install `fsite`

Download the matching binary from the latest GitHub release:

```text
https://github.com/finitecomputer/finite-sites/releases/latest
```

Release assets are named `fsite-linux-x86_64.tar.gz`,
`fsite-macos-x86_64.tar.gz`, and `fsite-macos-aarch64.tar.gz`.

Or build it from source:

```sh
cargo install --git https://github.com/finitecomputer/finite-sites --package fsite-cli --bin fsite
```

The CLI defaults to production Finite Sites at `https://api.finite.chat`.
Set `FINITE_SITES_API` only when targeting a local or self-hosted server.

## Local quickstart

```sh
# 1. run the server (data dir holds registry, blobs, cookie secret, outbox)
cargo run -p finitesitesd -- serve --data .dev-data

# 2. in another shell: create an identity and grant publishing access
export FINITE_SITES_API=http://127.0.0.1:8787
cargo run -p fsite-cli --bin fsite -- whoami
cargo run -p finitesitesd -- allow --data .dev-data <npub from whoami> --note me

# 3. create a Project Repository and site output
cargo run -p fsite-cli --bin fsite -- project apply \
  --json examples/project-applies/finitechat-native-mockup.json \
  --dry-run \
  --output json \
  --config examples/finitechat-native-mockup/finite.toml
cargo run -p fsite-cli --bin fsite -- project apply \
  --json examples/project-applies/finitechat-native-mockup.json \
  --output json \
  --config examples/finitechat-native-mockup/finite.toml

# 4. verify the collaborator email, clone, commit deploy bytes, and push
cargo run -p fsite-cli --bin fsite -- email-login skyler@example.com
# copy TOKEN_FROM_EMAIL from .dev-data/outbox/*.txt
cargo run -p fsite-cli --bin fsite -- email-redeem skyler@example.com TOKEN_FROM_EMAIL
cargo run -p fsite-cli --bin fsite -- auth git finitechat-native --email skyler@example.com --store --output json
git clone http://git.sites.localhost:8787/finitechat-native.git /tmp/finitechat-native
rsync -a --delete examples/finitechat-native-mockup/ /tmp/finitechat-native/
cd /tmp/finitechat-native
git add finite.toml index.html
git commit -m "Seed finitechat native mockup"
git push origin main

open http://finitechat-native-mockup.sites.localhost:8787/        # 401: private by default
cargo run -p fsite-cli --bin fsite -- share finitechat-native-mockup --shared --add-email you@example.com --send-invite
# request a link on the login page; the dev mailer writes it to
# .dev-data/outbox/*.txt instead of sending real email
cargo run -p fsite-cli --bin fsite -- share finitechat-native-mockup --public --yes-public
```

`*.sites.localhost` resolves to loopback in modern browsers; for curl pass
`-H "Host: hello.sites.localhost:8787"` against `127.0.0.1:8787`.

`just dev`, `just test`, `just lint` wrap the common loops.

## Project shape

Finite projects should be organized so the source remains useful before and
after a website exists. Start with durable data when that is the foundation;
add logic around that data when the project needs computation; produce a
website, PDF, or other output only when there is something useful to present.

The deployed site is a Deploy Output: committed bytes selected from the
Project Repository by `finite.toml` and served as a Version. Finite Sites
validates and serves committed bytes; agents own any build step that produces
those bytes. Use a dedicated output directory such as `site/` or `dist/` for
generated static files unless the whole Project Repository is intentionally a
deploy-only tree.

## Collaborative editing

Project Repositories are the preferred collaboration path. Create or update
the Project and its site output through agent-safe JSON:

```sh
fsite describe workflow publish-static-site --output json
fsite describe workflow project-config --output json
fsite project apply --json project.json --dry-run --output json
fsite project apply --json project.json --send-invite --output json
```

Minimal `finite.toml`:

```toml
[project]
slug = "finitechat-native"

[outputs.mockup]
kind = "site"
site_name = "finitechat-native-mockup"
branch = "main"
path = "."
spa = false
```

An editor verifies their email, mints a scoped Git Credential, clones, edits,
commits deploy bytes, and pushes the Deploy Branch:

```sh
fsite email-login editor@example.com
fsite email-redeem editor@example.com TOKEN_FROM_EMAIL
fsite auth git finitechat-native --email editor@example.com --store --output json
git clone https://git.finite.chat/finitechat-native.git
cd finitechat-native
# edit, test, build if needed, commit deploy bytes
git push origin main
```

If the local User Key is already a native Project Collaborator, omit
`--email` and no email verification round trip is needed:

```sh
fsite auth git finitechat-native --store --output json
git clone https://git.finite.chat/finitechat-native.git
```

Owners can remove a Project Collaborator through the same agent-facing
surface. This revokes that Principal's active Git Credentials for the Project
and is safe to replay:

```sh
fsite project collaborator remove finitechat-native --email editor@example.com --output json
```

Project Repository access is separate from output Visibility. If the email
should also lose view access to a site output, remove the Share row too:

```sh
fsite share finitechat-native-mockup --remove-email editor@example.com
```

Pushing to a Project Deploy Branch updates committed output bytes; Finite
Sites does not run builds.

There is no direct static bundle upload command in the current model. If an
agent reaches for one, use `fsite describe workflow publish-static-site
--output json` and then commit/push the selected output path.

Owners can also email a view invite for a Project Output. This is separate
from Project Repository edit access:

```sh
fsite share finitechat-native-mockup --shared --add-email viewer@example.com --send-invite
```

For project-backed editable static sites, Finite Sites serves a virtual
`/llms.txt` with git instructions when the active version did not publish
`/llms.txt` itself. That lets an owner send a site link to another person and
have their agent discover the edit flow without scraping rendered HTML as
source.

## Agent-first CLI

Agents should be able to learn `fsite` by interrogating `fsite` itself. Every
capability exposed by the CLI should be discoverable through machine-readable
help or describe commands, and every mutating project command should support
structured JSON input, structured JSON output, and dry-run validation.
`fsite --help` must point agents at the static-site happy path, and
`fsite describe workflow publish-static-site --output json` is the canonical
first command for creating a new static site Project Output.

Human-friendly commands remain useful, but they should be thin convenience
paths over the same agent-safe operations.

## Production shape

On a dedicated box (finite-lat-2, with agent machines on finite-lat-1) as one more systemd unit behind
the existing Caddy, with Cloudflare proxying `*.finite.chat` and
`api.finite.chat` (edge TLS + DDoS absorption; origin uses a Cloudflare
Origin CA cert — no ACME, see ADR-0012). Magic-link mail goes out through
Resend/Postmark (`--mailer resend --mail-from ...`). Publishing never
touches host configuration — it is registry + blob writes only.

The full runbook, Caddyfile fragment, and systemd unit live in
`docs/deploy-finite-lat-2.md` and `deploy/finite-lat-2/`. Remaining
pre-deploy items are in `docs/technical-debt-ledger.md`.

## Docs

- `CONTEXT.md` — glossary; use these words in code and prompts
- `AGENTS.md` — prompting contract and repo commands
- `docs/engineering-style.md` — the rules this code is written to
- `docs/adr/` — decisions and their alternatives
- `docs/roadmap.md` — tiers 2/3 and the path to production
- `docs/technical-debt-ledger.md` — accepted shortcuts with delete conditions
- `skills/finite-sites-publishing/SKILL.md` — agent skill for publishing
