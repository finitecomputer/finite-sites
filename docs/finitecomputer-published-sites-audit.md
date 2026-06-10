# Audit Request: Published Sites On Finite Computer Boxes

**From**: finite-sites
**To**: finitecomputer operators/agents
**Why now**: Finite Sites tier 1 (static hosting at `*.finite.chat`) is
live. We are designing tier 2 — sandboxed server processes with persistent
data, language-agnostic (Python is required, not just Node/Bun). Before we
fix the runspec contract, we need ground truth about what users have
actually published from their agent machines: how many sites are static or
trivially static-able (migrate to tier 1 now), and what the non-static
ones really run.

## Scope

Every published site/route on every box that has published user sites:
`ovh-fc-1` (finite.vip), `trf`, the lat boxes, and any ailounge/demo-era
leftovers still routable. Include sites that are claimed/configured but
currently broken or stopped — "dead" is a finding, not an exclusion.

Sources of truth to sweep, per box:

- control-plane manifests (`/var/lib/finitecomputer/control-plane`) and
  whatever records `finitec publish` left;
- Traefik route/router configs (each published hostname);
- the process side: what actually backs each route (static file server?
  app process in the user pod? supervised by what?);
- inside user pods, read-only: the published project directory itself.

**Privacy posture**: this is operator-level access to user workspaces.
Collect tech-stack metadata only — file names, dependency manifests,
entrypoints, sizes. Do not copy site content, data files, or env values
out of pods; record that secrets/data *exist*, never what they contain.

## Per-site record (emit JSONL, one object per published site)

```json
{
  "box": "ovh-fc-1",
  "host": "example.finite.vip",
  "owner": "<user/machine id>",
  "auth": "public | oauth2-proxy | other",
  "serving": "static-dir | process | proxy | broken",
  "runtime": "static | node | bun | python | deno | other:<what>",
  "framework_hints": ["vite", "react", "fastapi", "streamlit", "..."],
  "entrypoint": "how it starts, if a process (command line / script)",
  "port": 3000,
  "build_artifact": "dist/ | build/ | none | unbuilt-source",
  "persistence": {
    "sqlite_files": [{"path": "data/app.db", "bytes": 123456}],
    "other_data": "description or none"
  },
  "env_or_secrets_referenced": true,
  "external_apis": ["hits openrouter", "none", "unknown"],
  "file_count": 42,
  "total_bytes": 1234567,
  "largest_file_bytes": 234567,
  "last_modified": "2026-05-30",
  "status": "live | stopped | broken",
  "notes": "anything odd"
}
```

Detection heuristics (adapt as needed):

- `package.json` / `bun.lock` / `pnpm-lock.yaml` → node/bun; check
  `scripts.start` vs `scripts.build` to tell served-source from
  built-artifact.
- `pyproject.toml` / `requirements.txt` / `uv.lock` → python; grep deps
  for `fastapi`, `flask`, `streamlit`, `gradio`, `django`.
- A route backed only by files with no process → static-dir.
- A route backed by a long-running process → record its argv verbatim.
- `*.db` / `*.sqlite*` anywhere in the project → persistence.

## Classification (the headline numbers we need)

- **A — static as-is**: served files only; would publish to tier 1 today
  with `fsite publish`.
- **B — static after build**: a Vite/Next-export/etc. project currently
  served by a dev server or from source, where `build` produces a static
  artifact with no server code. Tier-1-able with one build step.
- **C — dynamic, simple**: one process + optional SQLite/file persistence,
  one port, no other services. The tier 2 target shape. Note the runtime
  split within C (node/bun vs python vs other).
- **D — dynamic, complex**: anything beyond C — multiple processes, other
  databases, background jobs, websockets-heavy, GPU, etc. These shape (or
  get explicitly excluded from) tier 2.
- **E — dead/broken/abandoned**: route exists, nothing meaningful behind it.

## Deliverables

1. The JSONL inventory (one file, all boxes).
2. Counts: sites per class per box; runtime breakdown within C/D.
3. For every C and D site: a one-line runspec sketch
   (`runtime + entrypoint + port + data paths`) — this is the direct input
   to the tier-2 runspec design.
4. Flags against tier-1 limits for A/B sites: any file > 25 MiB, any site
   > 2,000 files or > 512 MiB total, any hostname label that is reserved
   on finite.chat (`api`, `www`, `captions`, … — full list in
   `finitesites-proto/src/names.rs`) or longer than 63 chars / not a
   single DNS label.
5. Anything that surprised you.

## What we will do with it

- A/B sites: candidates for migration to `*.finite.chat` now (agents
  re-publish with `fsite`; no infra work).
- C sites: define the tier-2 runspec around the runtimes that actually
  appear (Python is already a hard requirement; this tells us what else),
  and pick the sandbox runtime (MicroSandbox vs gVisor) knowing the real
  workload shapes.
- D sites: explicit decisions per pattern — support, defer, or "stays on
  your machine".
- E sites: cleanup list for the route table.
