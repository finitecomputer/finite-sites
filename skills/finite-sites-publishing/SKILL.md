---
name: finite-sites-publishing
description: Publish, update, and share websites through Finite Sites using `fsite`, without exposing users to nostr, npubs, keys, manifests, blobs, DNS, or proxies.
---

# Finite Sites Publishing

Use this skill when a human asks you to publish, deploy, update, or share a
website. Finite Sites hosts static sites at:

```text
https://NAME.finite.chat/
```

Sites are **private by default**. Sharing works like a Google Doc: private
to the owner, shared with specific email addresses, or public.

Do not explain or expose nostr, npubs, secrets, signing, manifests, blobs,
DNS, certificates, Caddy, or Traefik unless you are debugging a platform
issue. Normal publishing happens only through `fsite`.

## Prerequisites

- `fsite` is available in the runtime and `FINITE_SITES_API` points at the
  Finite Sites server.
- The site can be exported as static files (a final build output directory
  such as `dist/` — never a source tree).
- The requested name is a lowercase DNS label of 3–63 characters, such as
  `demo`, `pauls-blog`, or `launch-2026`.

If `fsite` is missing or a command is unsupported, stop and say the Finite
Sites command surface is not available in this runtime. Do not fall back to
raw nostr tooling, DNS, or proxy configuration.

If a command fails with "not allowlisted", run `fsite whoami` and tell the
human to send the npub to a Finite operator for allowlisting.

## Key Hygiene

`fsite` creates key files automatically:

- identity: `~/.config/finite-sites/identity.env`
- per-site: `.finite/sites/NAME.env` in the workspace

Never print, paste, move, commit, or deploy these files. Ensure `.finite/`
is gitignored in any repo you touch.

## Workflow

1. Identify the requested `NAME`. Check what already exists when useful:

```bash
fsite list
fsite status NAME
```

2. Build and QA the site locally first. Publish only the final artifact
   directory, never a project root (roots containing `.git`,
   `node_modules`, or `.finite` are rejected).

3. Claim and publish:

```bash
fsite claim NAME
fsite publish NAME ./dist
```

If the site is a single-page app with client-side routing (React Router,
Vue Router, etc. using history-API URLs like `/settings`), add `--spa` so
unknown paths serve the app shell instead of 404:

```bash
fsite publish NAME ./dist --spa
```

Plain multi-page sites and hash-routed apps do not need `--spa`.

Re-publishing the same name creates a new version; unchanged files upload
nothing. Tell the human the URL when the publish succeeds, and that the
site is currently private.

4. Share it the way the human asked:

```bash
fsite share NAME --add-email friend@example.com     # share with people
fsite share NAME --remove-email friend@example.com  # revoke someone
fsite share NAME --private                          # lock it down
fsite share NAME --public --yes-public              # public (see warning)
```

People shared by email sign in with a magic link sent to that address —
no account or password.

## Server Apps (tier 2)

If the site needs a server (a database, API routes, server rendering),
publish it as an app. The start command must listen on `$PORT`, and the
app may only write files under `$DATA_DIR` (everything else is
read-only):

```bash
fsite publish-app NAME ./bundle --start "node server.js"     # Next.js standalone
fsite publish-app NAME ./appdir --start "uv run app.py"      # Python (PEP 723 inline deps)
fsite publish-app NAME ./appdir --start "bun server.ts"      # Bun
```

For Next.js: set `output: "standalone"` in next.config, build, then
bundle `.next/standalone` with `.next/static` copied into it (see
examples/nextjs-demo). SQLite files belong in `$DATA_DIR`. Websockets
are not supported yet. A site is either static or an app — the kind is
fixed by its first publish.

## Public Warning

Before making any site public, warn clearly and get agreement:

```text
This will make https://NAME.finite.chat/ public. Anyone on the internet
can view it. Do not include secrets, private files, personal information,
credentials, drafts, or anything you would not want public.
```

Only after the human agrees, run the command with `--yes-public`. Never
pass `--yes-public` on your own initiative. For updates to an
already-public site, warn again only when the new content appears
personal, confidential, regulated, or otherwise sensitive.

## Out Of Scope

Rollback, deleting a site, releasing or transferring a name, and custom
domains are operator actions for now. If asked, say so and offer to note
the request for a Finite operator.
