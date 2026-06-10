# Finite Sites

Self-hosted site publishing for Finite Computer agents. A user says "make me
a website"; their agent builds it, claims `name.finite.chat`, and publishes
with nostr-key-signed requests. Sites are private by default, shareable with
specific emails via magic links, or public — like sharing a Google Doc.

This replaces self-hosting sites from inside agent machines (see the
AI Lounge postmortem in finitecomputer) and the nsite-based prototype in
finite-site: the claim/version/registry model carries over, the serving
substrate is now finite-owned storage behind one wildcard domain.

## What works today (v1)

- **Static sites**: manifest publish with content-addressed dedup, immutable
  versions, atomic latest-pointer flips, ETag revalidation.
- **Nostr auth**: every registry mutation is a NIP-98-signed request. The
  user identity key claims names; a per-site workspace-held key publishes.
- **Allowlist**: only operator-allowlisted npubs can claim/publish
  (payments are out of scope for now).
- **Sharing**: per-site visibility `private` / `shared` / `public`, email
  ACLs, magic-link login, host-scoped signed cookies. Revoking an email
  takes effect on the next request.
- **Agent surface**: the `fsite` CLI hides nostr/keys/manifests entirely.

Stateful sites (SQLite-backed apps) and full containers are tiers 2 and 3
behind the same publish API — see `docs/roadmap.md`.

## Layout

| Crate | What it owns |
|---|---|
| `finitesites-proto` | nostr events, NIP-98, manifests, names, limits, DTOs |
| `finitesites-blob` | content-addressed blob storage (filesystem; Garage/S3 seam) |
| `finitesites-store` | SQLite registry: sites, claims, versions, shares, tokens |
| `finitesites-engine` | all decisions: claim/publish/share/view/magic links |
| `finitesitesd` | the server: control-plane API + wildcard site serving + allowlist ops |
| `fsite-cli` | agent-facing CLI (`fsite`) |

## Local quickstart

```sh
# 1. run the server (data dir holds registry, blobs, cookie secret, outbox)
cargo run -p finitesitesd -- serve --data .dev-data

# 2. in another shell: create an identity and allowlist it
cargo run -p fsite-cli --bin fsite -- whoami
cargo run -p finitesitesd -- allow --data .dev-data <npub from whoami> --note me

# 3. claim, publish, share
cargo run -p fsite-cli --bin fsite -- claim hello
cargo run -p fsite-cli --bin fsite -- publish hello examples/hello-site
open http://hello.sites.localhost:8787/        # 401: private by default
cargo run -p fsite-cli --bin fsite -- share hello --add-email you@example.com
# request a link on the login page; the dev mailer writes it to
# .dev-data/outbox/*.txt instead of sending real email
cargo run -p fsite-cli --bin fsite -- share hello --public --yes-public
```

`*.sites.localhost` resolves to loopback in modern browsers; for curl pass
`-H "Host: hello.sites.localhost:8787"` against `127.0.0.1:8787`.

`just dev`, `just test`, `just lint` wrap the common loops.

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
