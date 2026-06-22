# Audit Results: Published Sites on Box1 (ovh-fc-1) and TRF

**Date**: 2026-06-09
**Responds to**: [finitecomputer-published-sites-audit.md](finitecomputer-published-sites-audit.md)
**Inventory**: [finitecomputer-published-sites-audit-results-2026-06-09.jsonl](finitecomputer-published-sites-audit-results-2026-06-09.jsonl) (one object per published endpoint, both boxes)
**Scope note**: box1 + trf only, per request. The lat boxes were not swept.

## Method (read-only)

Source of truth is the control-plane sqlite (`published_endpoints` table) on each
box — every user publish is recorded there, and Traefik IngressRoutes are generated
from it 1:1. Per site we collected: the route + an HTTPS probe through local
Traefik; the actual listening process (nsenter into the pod netns + `ss -tlnp`,
argv/cwd verbatim from `/proc`); and a metadata-only walk of the project directory
via the host-side PVC path (file counts/sizes/mtimes, dependency manifests,
`*.db`/`.env` presence, framework hints, a boolean grep of entrypoints for
api-route/fs-write/sqlite/websocket/spawn markers). No site content, data, or env
values were copied off-box. Machines run as k3s StatefulSets (one namespace per
machine); home dirs live under `/var/lib/rancher/k3s/storage/`.

## Headline numbers

| box | A static as-is | B static after build | C dynamic simple | D dynamic complex | E dead/broken | total |
|---|---|---|---|---|---|---|
| ovh-fc-1 | 97 | 12 | 38 | 1 | 39 | 187 |
| trf | 40 | 0 | 7 | 0 | 6 | 53 |
| **both** | **137** | **12** | **45** | **1** | **45** | **240** |

- **62% (149/240) are tier-1-able today** (A+B). Five more "C" sites have server
  code that only serves static files (hand-written express/python static servers),
  so the practical migration pool is ~154.
- Status: 183 live, 45 broken, 12 stopped. Every broken route is class E except a
  few stopped C/B sites with intact projects.
- 240 hostnames ≠ 240 sites: only 169 distinct project dirs — agents routinely
  publish the same dir under multiple hostnames (see surprises).
- 45 owners total across both boxes.

### Serving-stack split within A (137 sites)

`python3 -m http.server` 94 · `npx serve` ~27 · `vite preview`/`npm run preview` ~6
· misc static 10. **A-class publishing is already overwhelmingly "point a dumb
file server at a directory"** — exactly the static Project Output shape.

### Runtime split within C/D (46 sites)

| box | node | python | other |
|---|---|---|---|
| ovh-fc-1 | 38 (incl. 1 D) | 1 | 0 |
| trf | 3 | 4 | 0 |

Node dominates box1 (express `server.js` is the universal pattern; 3 are
`next start`). TRF skews python (flask/fastapi single-file apps). No bun, no
deno, no compiled runtimes anywhere. **Python as a tier-2 hard requirement is
validated by TRF; node/express + better-sqlite3 is the single most common
dynamic shape overall.**

### Persistence (the real tier-2 C shape)

8 sites have live sqlite persistence, all single-file (plus backups):
`roberto-matty-managed-2` (retrieval.sqlite3), `celine-closet`/`celine-digital-closet`
(closet.db, same app under 2 hosts), `skyler-finite-skill-feed` (skills.db),
`lazarus-recipe-book` (data.db), `mimi-reynolds-crm` (contacts.db),
`jeremy-…-art-academies-dashboard` (art-academies.sqlite + 21 timestamped backup
copies), `jeremy-ani-global` (progress.db). Everything else that's dynamic
persists to JSON files on disk (`writes_fs` marker) or nothing.
**One process + one port + sqlite/JSON-files-in-cwd covers every C site found.**

## Tier-1 limit flags (A/B sites)

- `azadi-eliminated-leaders` / `azadi-masih-dl-v2` / `azadi-masih-video`
  (ovh-fc-1): the same 122 MiB mp4 (`masih_full_fa_v2.mp4`) sits in each served
  dir — over the 25 MiB file cap. Everything else fits comfortably.
- No A/B site exceeds 2,000 files or 512 MiB once vendor dirs are excluded.
- No hostname label is reserved (`api`, `www`, …) or >63 chars; all are valid
  single DNS labels. (Two paths contain spaces — `dev/public speaking tips`,
  `dev/Planck Interactive` — dir names, not labels; harmless but fun.)

## Auth split (matters for migration: tier 1 is public-only today)

| box | public | oauth2-proxy self | oauth2-proxy emails | oauth2-proxy org |
|---|---|---|---|---|
| ovh-fc-1 | 35 | 133 | 12 | 7 |
| trf | 24 | 27 | 2 | 0 |

**Only ~25% of published sites are public.** The default (`self` = owner-only)
dominates, mostly because publish defaults there and agents never flip it. If
tier 1 has no auth story, the migratable-today set shrinks from ~149 to the
public subset (~45 A/B sites) unless owners opt to go public.

## E sites — route-table cleanup list (45)

All 45 are `desired_process_state=external` endpoints where the process died and
no project dir is resolvable; routes still exist and return 302→auth, 404, or 502.
Full list: `jq 'select(.class=="E")' …jsonl`. Notables: `skyler-finite-ghost`
(abandoned Ghost install), `celine-celine-closet` + `skyler-finite-prayer-rule` +
`skyler-finite-braun-clock` (label-typo duplicates of live sites),
`smoke-finite-permissions-check` (test artifact). ~21% of box1's route table is
dead weight.

## Extra routed hostnames (not user sites)

46 (box1) + 16 (trf) additional IngressRoutes exist beyond `published_endpoints`:
per-machine `<name>-opencode.*` consoles plus platform routes (`auth`, `dashboard`,
`git`, `matrix`, apex). **No demo-era or ailounge user leftovers were found
routable on either box** — the route table is fully accounted for by
control-plane + platform.

## Surprises

1. **Multi-hostname single-dir publishing is a pattern, not an accident.**
   `skyler-finite:dev/finite-landing` is behind 5 hostnames (incl.
   `computer.finite.vip`); brandon's `us-map` vite dev server backs 3 different
   "sites" (mandolin-chords, us-map, princess-clock-game — two of them serving
   the *wrong* content); alex2's strikes-map backs 3 hosts on one port. Tier-2
   runspec should treat hostname→workload as N:1, and label-typo re-publishes
   explain several E entries.
2. **A published endpoint backed by the agent itself**: `jeremy-siri-webhook`
   resolves to the hermes agent process (`hermes_cli … gateway run`) listening on
   8644 — users publish their agent's own webhook gateway as a "site". That's a
   "stays on your machine" shape tier 2 should explicitly exclude.
3. **`run_cwd` in the control plane is unreliable**: agents work around it with
   `cd X && …` inside `run_command`, `--directory` flags, and absolute script
   paths. Three sites record `run_cwd=/home/node` while actually serving a
   subdir. A tier-2 runspec needs an explicit, validated workdir field.
4. **Rebuild-on-restart as deploy strategy**: several B sites encode
   `npm install && npm run build && npm run preview` as the supervised command —
   the build happens at every process restart. These migrate to tier 1 with one
   offline build.
5. **External API usage is near zero**: across all 240 projects, only
   openstreetmap tiles (6), openrouter (5), telegram (1), huggingface (1).
   Sandboxed egress for tier 2 can start very restrictive.
6. **Secrets barely exist**: 6 sites total have `.env*` files. Tier-2 secret
   injection is nice-to-have, not blocking.
7. **The only D is dead**: a stopped matrix-conduit (`skyler-finite-matrix`),
   plus the abandoned Ghost in E. Nothing currently live exceeds the
   one-process-one-port shape. Tier 2 as specced (C shape) covers 100% of live
   dynamic workloads on these two boxes.
8. **Websockets exist but lightly**: `brandon-finite-agent-challenge` (live,
   express+ws) and `skyler-finite-workspace` (live, ws dep) — tier 2 should pass
   websocket upgrades through, but nothing is websockets-*heavy*.
9. **TRF's biggest publisher is one user**: andy owns 25/53 endpoints, mostly
   python http.server one-pagers. Publishing concentration is extreme on both
   boxes (skyler 37/187 on box1).

## What this means for the asks in the request

- **A/B migration**: ~149 candidates; gate on the auth question (only ~59 are
  public) and the one 122 MiB video (×3 hosts).
- **Tier-2 runspec**: `runtime ∈ {node, python}` + `command` (verbatim argv seen
  in the wild above) + `workdir` (explicit, see surprise 3) + one `port` + a data
  dir for sqlite/JSON files. That covers all 46 C/D sites. Runspec sketches per
  site below.
- **D decisions**: nothing live to decide; matrix/ghost pattern = "stays on your
  machine".
- **E cleanup**: 45 routes deletable from the control plane today.

## Appendix: C/D runspec sketches (one line per site)

- `skyler-finite-john1.finite.vip` [C, live] — **node** · `node /home/node/dev/john1/server.js` · port 7410
- `skyler-finite-designmd.finite.vip` [C, live] — **node** · `node /home/node/dev/designmd/server.js` · port 7411 · markers: api_routes
- `skyler-finite-workspace.finite.vip` [C, live] — **node** · `node server-entry.js` · port 7412 · markers: api_routes · websocket dep present
- `skyler-finite-archive.finite.vip` [C, live] — **node** · `node /home/node/dev/john1/server.js` · port 7410 (same process as john1)
- `skyler-finite-font-preview.finite.vip` [C, live] — **node** · `node /home/node/dev/font-preview/server.js` · port 7420 · markers: api_routes
- `skyler-finite-finite-landing.finite.vip` [C, live] — **node** · `node server.js` · port 7430 · markers: api_routes
- `skyler-finite-agentlander-sky.finite.vip` [C, live] — **node** · `node server.js` · port 7430 (same process as finite-landing)
- `roberto-matty-managed-2.finite.vip` [C, stopped] — **python** · `python3 -m uvicorn services.api.app:app --host 0.0.0.0 --port 8001` · port 8001 · **data:** data/index/retrieval.sqlite3 · markers: api_routes,sqlite
- `skyler-finite-finite-landing-2.finite.vip` [C, live] — **node** · `node server.js` · port 7430 · markers: api_routes
- `prayer-rule.finite.vip` [C, live] — **node** · `node /home/node/dev/prayer-rule/server.js` · port 7422 · markers: api_routes
- `skyler-finite-shader-lab.finite.vip` [C, stopped] — **node** · `next start -p 7440` · port 7440
- `celine-hrf-goals-dashboard.finite.vip` [C, live] — **node** · `node server.js` (express) · port 3000 · markers: api_routes,writes_fs,spawns
- `skyler-finite-matrix.finite.vip` [D, stopped] — **node** · `/home/node/dev/matrix-conduit/start.sh` · port 6167 · matrix-conduit
- `skyler-finite-finite-agency.finite.vip` [C, live] — **node** · `node /home/node/dev/finite-agency/server.js` · port 7450 · markers: api_routes
- `skyler-finite-inter-sticker.finite.vip` [C, live] — **node** · `node /home/node/dev/inter-sticker/server.js` · port 4820 · markers: api_routes
- `brandon-finite-agent-challenge.finite.vip` [C, live] — **node** · `node server.js` (express+ws) · port 3741 · markers: api_routes,websocket
- `celine-closet.finite.vip` [C, live] — **node** · `next start -p 3011` (next-server 14) · port 3011 · **data:** data/closet.db
- `celine-digital-closet.finite.vip` [C, live] — same process/data as celine-closet
- `dimsum-guinness-finder.finite.vip` [C, live] — **node** · `node server.js` (express) · port 3002 · markers: api_routes
- `roshna-rihla.finite.vip` [C, live] — **node** · `next start` (next-server 16) · port 3000
- `azadi-iran-internet-watch.finite.vip` [C, live] — **node** · `node server.js` (express) · port 4242 · markers: api_routes
- `dimsum-barfinder.finite.vip` [C, live] — **node** · `node server.js` (express) · port 3000 · markers: api_routes
- `skyler-finite-skill-feed.finite.vip` [C, live] — **node** · `node server.js` (express) · port 3742 · **data:** data/skills.db · markers: api_routes,sqlite,spawns,outbound_http
- `lazarus-recipe-book.finite.vip` [C, live] — **node** · `node server.js` (express, 1060 loc) · port 3000 · **data:** data.db · markers: api_routes
- `iherbs-rwanda-voice.finite.vip` [C, live] — **node** · `node server.js` (express) · port 3000 · markers: api_routes,writes_fs
- `iherbs-phoenix-irony.finite.vip` [C, stopped] — **node** · `node server.js` · port 7341 · markers: api_routes,spawns
- `paul-finite-2-wedding-preview.finite.vip` [C, live] — **node** · `node server.js` · port 4177 · markers: api_routes,writes_fs,spawns
- `skyler-finite-noscroll.finite.vip` [C, live] — **node** · `node server.js` · port 4822
- `austin-finite-hermes-self.finite.vip` [C, live] — **node** · `node server.js` · port 3000 · markers: api_routes
- `ella-btc-isla.finite.vip` [C, live] — **node** · `node server.js` (express) · port 3100 · markers: api_routes,writes_fs,spawns
- `alex2-strikes-map.finite.vip` [C, live] — **node** · `node server.js` (express) · port 4567 · markers: api_routes,writes_fs
- `ella-pointbreak-gym.finite.vip` [C, live] — **node** · `node server.js` · port 3000 · static-file-serving only → trivially static-able
- `ella-las-santas.finite.vip` [C, live] — **node** · `node server.js` · port 3420 · static-file-serving only → trivially static-able
- `ella-isabella-santos-2.finite.vip` [C, live] — **node** · `node server.js` · port 3421 · static-file-serving only → trivially static-able
- `ella-the-source.finite.vip` [C, live] — **node** · `node server.js` (express) · port 3200 · markers: api_routes
- `alex2-strikes-map-guest.finite.vip` [C, live] — same dir/port as alex2-strikes-map
- `ella-isabella-santos.finite.vip` [C, live] — same dir/port as isabella-santos-2 · trivially static-able
- `alex2-strikes-map-prb.finite.vip` [C, live] — same dir/port as alex2-strikes-map
- `ella-instagram-tool.finite.vip` [C, live] — **node** · `node server.js` (express) · port 3200 · markers: api_routes,writes_fs,spawns,outbound_http
- `mimi-reynolds-crm.trf.finite.computer` [C, stopped] — **python** · `python main.py` (fastapi/uvicorn) · port 3000 · **data:** contacts.db · markers: api_routes,outbound_http
- `jeremy-sourdough-baking.trf.finite.computer` [C, live] — **python** · `python3 app.py` · port 4318 · static-file-serving only → trivially static-able
- `jeremy-jeremy-ani-art-academies-dashboard.trf.finite.computer` [C, live] — **node** · `node server/index.mjs` (express+better-sqlite3, vite-built front) · port 3001 · **data:** data/art-academies.sqlite (+21 backup copies) · markers: api_routes,sqlite
- `jeremy-siri-webhook.trf.finite.computer` [C, live] — **python** · hermes agent `gateway run` · port 8644 · backed by the agent process itself, not a user app
- `jeremy-ani-global.trf.finite.computer` [C, live] — **node** · `node server/index.mjs` (express+better-sqlite3) · port 5173 · **data:** server/progress.db · markers: api_routes,sqlite,spawns
- `jeremy-zoom-transcripts.trf.finite.computer` [C, live] — **node** · `node server.js` (express) · port 3456 · markers: writes_fs,outbound_http
- `andy-bruna-peru-panel.trf.finite.computer` [C, live] — **python** · `python3 api.py` · port 8091 · markers: writes_fs,spawns,outbound_http

## Raw collection artifacts

Local working dir: `finitecomputer/tmp/site-audit/` (collector scripts, raw
per-box JSONL, listener/marker scans). On-box leftovers under `/tmp` on each box
(`audit_box.py`, `site-audit-*.jsonl`, `listeners-*.json`, `markers-*.json`) —
safe to delete.
