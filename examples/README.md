# Examples

Working demos for each hosting tier, smallest first. Each was published to
finite.chat as part of platform validation.

## Project Repository seed

`finitechat-native-mockup` is the Project-first validation example. The
Project Apply JSON lives outside the deploy path so the committed Project
Repository source only contains the deployable mockup and its required
`finite.toml`.

The fixture grants `skyler@example.com` as the bootstrap editor. Replace that
email before applying if a different External Principal should clone and push.

```sh
fsite project apply \
  --json examples/project-applies/finitechat-native-mockup.json \
  --dry-run \
  --output json \
  --config examples/finitechat-native-mockup/finite.toml

fsite project apply \
  --json examples/project-applies/finitechat-native-mockup.json \
  --output json \
  --config examples/finitechat-native-mockup/finite.toml

fsite email-login skyler@example.com
fsite email-redeem skyler@example.com TOKEN_FROM_EMAIL
fsite auth git finitechat-native --email skyler@example.com --output json
git clone https://git.finite.chat/finitechat-native.git /tmp/finitechat-native
rsync -a --delete examples/finitechat-native-mockup/ /tmp/finitechat-native/
cd /tmp/finitechat-native
git add finite.toml index.html
git commit -m "Seed finitechat native mockup"
git push origin main
```

Pushing `main` is the publish step. Finite Sites validates committed bytes
selected by `finite.toml` and creates the immutable Version; it does not run
builds.

## Tier 1: static

- **hello-site** — plain files. Legacy site-first check:
  `fsite publish NAME examples/hello-site`.
- **spa-pushstate** — dependency-free single-page app using the history
  API. Needs `--spa` so deep links serve the shell:
  `fsite publish NAME examples/spa-pushstate --spa`.
- **react-bun-spa** — React 19 + React Router 7 bundled with Bun:
  ```sh
  cd examples/react-bun-spa
  bun install && bun run build
  fsite publish NAME dist --spa
  ```
  Bun's HTML entrypoint build (`bun build index.html --outdir=dist`)
  rewrites the script tag to the hashed bundle; `--spa` makes router
  paths refresh-safe. That's the whole recipe.

## Tier 2: server apps (in progress)

- **nextjs-demo** — idiomatic Next.js, `output: "standalone"`, run as
  `node server.js` on `$PORT`.
- **fasthtml-demo** — Python FastHTML with PEP 723 inline dependencies,
  run as `uv run app.py` on `$PORT`.

Published with `fsite publish-app NAME PATH --start "CMD"`; the platform
runs the process in a sandbox and proxies the site host to it, behind the
same private/shared/public gate as static sites.
