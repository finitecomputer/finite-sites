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

- **hello-site** — plain files. Commit the directory as a Project Output and
  push the configured Deploy Branch.
- **spa-pushstate** — dependency-free single-page app using the history
  API. Set `spa = true` for that Project Output so deep links serve the
  shell.
- **react-bun-spa** — React 19 + React Router 7 bundled with Bun:
  ```sh
  cd examples/react-bun-spa
  bun install && bun run build
  # commit dist/ as the configured Project Output path, then git push
  ```
  Bun's HTML entrypoint build (`bun build index.html --outdir=dist`)
  rewrites the script tag to the hashed bundle; `spa = true` makes router
  paths refresh-safe.

## Tier 2: server apps (in progress)

- **nextjs-demo** — idiomatic Next.js, `output: "standalone"`, run as
  `node server.js` on `$PORT`.
- **fasthtml-demo** — Python FastHTML with PEP 723 inline dependencies,
  run as `uv run app.py` on `$PORT`.

These are future app-output fixtures. Server apps are not part of the current
agent-facing publish surface; current Project Outputs deploy committed static
bytes.
