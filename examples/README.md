# Examples

Working demos for each hosting tier, smallest first. Each was published to
finite.chat as part of platform validation.

## Tier 1: static

- **hello-site** — plain files. `fsite publish NAME examples/hello-site`.
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
