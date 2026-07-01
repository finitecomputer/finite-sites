# Technical Debt Ledger

Tolerated shortcuts. Each item has an observed source, a risk, the first
proof of the shortcut in code, and a delete condition. A shortcut without a
delete condition is unfinished design, not accepted debt.

## 1. RESOLVED — real mailer implemented

`HttpMailer` (Resend/Postmark) ships behind the `Mailer` trait, selected
with `--mailer` + `--mail-from`, key via env var. Remaining work is
configuration, tracked in `docs/deploy-finite-lat-2.md` (domain
verification + a real-inbox validation gate). The dev mailer remains the
local default.

## 2. Login-link rate limiting only; no platform-wide limits

- **Source**: closed the login-link half (per-(site,email) and per-IP
  budgets in `crates/finitesitesd/src/limiter.rs`, applied in
  `request_link`); general request limiting deliberately deferred because
  Cloudflare's proxy fronts the serving plane in the planned deploy.
- **Risk**: API-plane brute force (NIP-98 makes this low-value) and
  origin-direct floods if Cloudflare is bypassed.
- **Proof**: only `request_link` consults `login_limiter`.
- **Delete condition**: per-IP budgets on the API plane (project init attempts and
  git deploys per pubkey per hour) before registration opens beyond the operator
  publish grant gate; Cloudflare rate-limiting rules on `/_finite/*` as
  belt-and-braces when the zone goes live.

## 3. One mutex around the engine; blocking IO in async handlers

- **Source**: rusqlite connections are not Sync; v1 traffic is tiny.
- **Risk**: a slow blob write head-of-line blocks every request.
- **Proof**: `AppState.engine: Mutex<Engine>` in
  `crates/finitesitesd/src/server.rs`.
- **Delete condition**: read/write split or connection pool plus
  `spawn_blocking` around blob IO, when p95 serve latency on the target
  box exceeds ~50ms under expected load.

## 4. Filesystem blob store and unreplicated registry

- **Source**: local v1; no object storage running.
- **Risk**: single-disk durability for all site content and the registry.
- **Proof**: `crates/finitesites-blob/src/lib.rs` writes under `--data`.
- **Delete condition**: Garage/S3 `BlobStore` implementation and a
  Litestream replication unit for `registry.db` in the production deploy
  definition.

## 5. Global blob dedup leaks hash existence

- **Source**: ADR-0007 chose global dedup.
- **Risk**: low — a publisher can learn whether some exact file already
  exists on the platform by watching the missing list.
- **Proof**: `Store::missing_blobs` consults a global `blobs` table.
- **Delete condition**: revisit before opening registration beyond the
  operator/Core publish grant gate; either accept formally in the ADR or scope
  dedup per owner.

## 6. No site delete / name release / key rotation surface

- **Source**: out of v1 contract (mirrors finite-site's v1 scope).
- **Risk**: abuse handling and mistakes need operator SQL today.
- **Proof**: `status IN ('disabled','deleted')` exists in the schema with
  no mutation path.
- **Delete condition**: operator commands for disable/delete/release with
  audit events, before non-VIP users receive publishing grants.

## 7. RESOLVED — NIP-98 URL matching verified through the live proxy

`https://api.finite.chat` is pinned end to end and the signed-call gate
passed on 2026-06-09 and later updated to the Project Repository flow:
project init plus git push from a remote machine through Cloudflare
succeeded against finite-lat-2. The residual behavior (a
misconfigured `--api-url` fails closed with "url mismatch") is documented
in `docs/deploy-finite-lat-2.md` along with the on-box smoke procedure.

## 8. RESOLVED — tier-2 runs in Kata microVMs

ADR-0015: app sites run as Kata Containers microVMs (Cloud Hypervisor) on
finite-lat-2, hardware-isolated from each other and the host (verified:
guest kernel 6.18.28 vs host 7.0.0). The systemd runner remains for boxes
without KVM. Residual: KSM/templating intentionally unused; Firecracker
snapshot suspend deferred (stop/start already gets idle RAM to ~0).

## 9. App proxy gaps: no websockets, no user-facing logs

- **Source**: tier-2 scope cut. (Idle sleep/wake now SHIPPED — the
  Supervisor stops idle apps and wakes them on request, ADR-0015.)
- **Risk**: websocket apps fail opaquely; users cannot see their own app
  logs (operators read `nerdctl logs finite-app-{site}` /
  `journalctl -u finite-app@{site}`).
- **Proof**: `crates/finitesitesd/src/proxy.rs` (no upgrade handling);
  no log surface in the API.
- **Delete condition**: websocket upgrade support and an `fsite logs NAME`
  surface before tier 2 is announced to users.

## 10. Kata config relaxes the daemon filesystem sandbox

- **Source**: ADR-0015; `sudo nerdctl` needs containerd/CNI filesystem and
  privilege access that the daemon's ProtectSystem=strict blocked.
- **Risk**: a finitesitesd compromise reaches root via `sudo nerdctl`
  (true regardless of ProtectSystem once that sudo path exists). The
  daemon still runs as the unprivileged finite-sites user.
- **Proof**: `deploy/finite-lat-2/finite-saas-sites-kata.conf`,
  `finite-sites-nerdctl-sudoers`.
- **Delete condition**: drive containerd via its gRPC API from the daemon
  (no nerdctl, no sudo) with a privilege-separated networking helper, if
  the daemon's own attack surface ever warrants it.

## 11. Wake-path gaps: no admission cap, first-wake latency, guest-root files

- **Source**: ADR-0015 wake-on-request scope cuts.
- **Risk**: (a) a request storm against many idle apps wakes them all at
  once with no global cap on resident microVMs; (b) the very first start
  of a uv app resolves dependencies and can exceed the 20s wake timeout
  (one 502, then fine — the cache persists in `$DATA_DIR`); (c) apps run
  as the image's default user (often root *inside the VM*), so files in
  the host-side data dirs can be root-owned, complicating cleanup by the
  unprivileged daemon.
- **Proof**: no admission control in `Supervisor::note_request_and_start`;
  `WAKE_TIMEOUT` in `crates/finitesitesd/src/proxy.rs`; bind-mounted data
  dirs in `KataAppRunner::run_fresh`.
- **Delete condition**: (a) a resident-VM budget with LRU eviction before
  app counts approach memory limits; (b) warm the dependency cache during
  deploy (run the start command once before flipping live) before tier 2
  is announced; (c) a fixed in-guest uid mapping or a root-owned cleanup
  helper when app deletion ships.

## 12. RESOLVED — Project Repository pushes use durable post-receive events

Project Repositories now install a `hooks/post-receive` helper that records
bounded durable git ref-change events before the Git client sees success.
`finitesitesd` reconciles pending events after receive-pack and at daemon
startup. Tests cover real `git clone`/`git push`, ignored non-deploy refs,
missing output failure, restart reconciliation after a ref update before
deploy, and idempotent replay after Version creation before event
acknowledgement.
