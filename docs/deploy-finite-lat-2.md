# Deploying Finite Sites To finite-lat-2

Finite Sites runs on its own box, **finite-lat-2 (64.34.80.19)**: Caddy in
front of one `finite-saas-sites` systemd unit. Agent machines live on
finite-lat-1; keeping tenant-facing serving off the Core box removes the
shared blast radius entirely. Cloudflare holds the `finite.chat` zone and
proxies both names, hiding the box IP and absorbing floods.

Unit, Caddyfile, and env example live in `deploy/finite-lat-2/`.

**Status (2026-06-09): FULLY LIVE.** Box setup (3–4), the Cloudflare zone
(proxied `*` and `api` A records), the Origin CA cert (installed at
`/etc/finite-saas/certs/finite-chat-origin.{pem,key}`, zone on Full
strict), and outbound mail (finite.chat verified at Resend, unit running
`--mailer resend` with the send-only key in `/etc/finite-saas/sites.env`)
are all done. Validation gates passed: Project Repository apply, git auth,
clone/push through the proxy, public serving, API-host dispatch, a real magic
link delivered through Resend, and restart durability. The only standing
operational TODO is the backup scope (section 6).

## 0. Local operator SSH alias

Operator machines should have an SSH config entry for the production box.
Do this once per machine so rollout commands never depend on remembering the
raw IP or login user:

```sshconfig
Host finite-lat-2
  HostName 64.34.80.19
  User ubuntu
  IdentityFile ~/.ssh/id_ed25519
  IdentitiesOnly yes
```

Before a rollout, verify the alias and principal:

```sh
ssh finite-lat-2 'hostname && whoami'
```

The expected output is:

```text
finite-lat-2
ubuntu
```

## 1. Cloudflare zone setup (one time)

In the `finite.chat` zone:

1. **DNS records** (both Proxied / orange cloud):
   - `A  *    64.34.80.19`
   - `A  api  64.34.80.19`
   - optional explicit `A  git  64.34.80.19` if you do not want to rely on
     the wildcard record for the Git Remote host.
   (The apex `finite.chat` is free for marketing/redirect use; sites, Git,
   and the API do not need it.)
2. **SSL/TLS -> Overview**: set encryption mode to **Full**. The box
   currently serves Caddy-internal certs, which Full accepts. To upgrade to
   **Full (strict)** later: SSL/TLS -> Origin Server -> Create Certificate
   for `finite.chat, *.finite.chat`, install as
   `/etc/finite-saas/certs/finite-chat-origin.{pem,key}` (key mode 0600),
   replace `tls internal` with the cert paths in `/etc/caddy/Caddyfile`,
   reload Caddy, then flip the zone to Full (strict).
3. Optional but recommended: **Email Routing** for inbound, forwarding
   `abuse@finite.chat` and `links@finite.chat` replies to a real mailbox.

Notes:
- Universal SSL covers exactly one wildcard level (`a.finite.chat`, never
  `a.b.finite.chat`) — matching the platform's one-label site names.
- Cloudflare's proxy body limit (100 MB on free) clears the 25 MiB blob cap.
- Because Cloudflare proxies, `CF-Connecting-IP` is trustworthy; the
  login-link rate limiter uses it.

## 2. Outbound mail (Resend)

Cloudflare Email Routing is inbound-only; magic links are sent through
Resend (or Postmark — both are wired in `--mailer`):

1. Create a Resend account, add the `finite.chat` domain, and add the DKIM
   and Return-Path records Resend lists into the Cloudflare zone
   (DNS-only/grey cloud, as instructed by Resend).
2. Wait for the domain to verify, then create an API key.
3. Put the key in `/etc/finite-saas/sites.env` as `RESEND_API_KEY=...`.
4. Switch the unit from the bootstrap dev mailer to Resend: edit
   `/etc/systemd/system/finite-saas-sites.service`, replacing
   `--mailer dev` with
   `--mailer resend --mail-from "Finite Sites <links@finite.chat>"`,
   then `sudo systemctl daemon-reload && sudo systemctl restart
   finite-saas-sites`.

Until then the dev mailer writes links to
`/var/lib/finite-sites/outbox/`, which is enough for operator testing but
means shared-visibility sites cannot deliver links to real viewers.

## 3. Box setup (done on finite-lat-2)

```sh
sudo apt-get install -y caddy build-essential pkg-config rsync
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal

# source synced to ~/finite-sites (rsync, excluding target/.git/.dev-data)
cd ~/finite-sites && cargo build --release

sudo install -m 0755 target/release/finitesitesd target/release/fsite /usr/local/bin/
sudo useradd --system --home /var/lib/finite-sites --shell /usr/sbin/nologin finite-sites
sudo install -d -o finite-sites -g finite-sites /var/lib/finite-sites
sudo install -d /etc/finite-saas
echo "# RESEND_API_KEY=" | sudo tee /etc/finite-saas/sites.env && sudo chmod 0640 /etc/finite-saas/sites.env
sudo install -m 0644 deploy/finite-lat-2/finite-saas-sites.service /etc/systemd/system/
# /etc/caddy/Caddyfile from deploy/finite-lat-2/Caddyfile-sites (tls internal bootstrap)
sudo systemctl daemon-reload && sudo systemctl enable --now finite-saas-sites && sudo systemctl reload caddy
```

The installed unit currently uses `--mailer dev` (see section 2 for the
flip). The repo copy of the unit shows the production mailer flags.

NIP-98 binds signatures to the exact URL, so on-box smoke tests with
`FINITE_SITES_API=http://127.0.0.1:8787` fail closed against the
production `--api-url https://api.finite.chat` ("url mismatch"). For
pre-DNS smoke testing, temporarily sed the unit's `--api-url` to the local
address and restore it afterwards; once Cloudflare DNS is live, test the
real URL from anywhere.

## 4. Publish grant and runtime template

```sh
sudo -u finite-sites finitesitesd allow --data /var/lib/finite-sites <npub> --note "paul"
```

Agent runtimes (on finite-lat-1) need two things staged by
`prepare-runtime-template` (finitecomputer side): the `fsite` binary on
PATH, and the `finite-sites-publishing` skill from `skills/`. The released
CLI defaults to `https://api.finite.chat`; set `FINITE_SITES_API` only for a
local or self-hosted API.

## 4b. Tier-2 app hosting (installed 2026-06-10)

App sites run as `finite-app@{site_id}` systemd template instances. Box
requirements, all in place:

- runtimes on the root filesystem (apps cannot read /home): node (apt),
  `/usr/local/bin/bun`, `/usr/local/bin/uv`
- `deploy/finite-lat-2/finite-app@.service` ->
  `/etc/systemd/system/finite-app@.service`
- polkitd installed, `deploy/finite-lat-2/50-finite-sites.rules` ->
  `/etc/polkit-1/rules.d/` (lets the finite-sites user manage
  `finite-app@*` units over D-Bus; sudo cannot work because the daemon
  runs with NoNewPrivileges)
- `/var/lib/finite-sites/apps/` owned by finite-sites
- the service runs with `--app-runner systemd`

App state lives in `/var/lib/finite-app/{site_id}` (StateDirectory) —
add it to the backup scope alongside `/var/lib/finite-sites`. App logs:
`journalctl -u finite-app@{site_id}`.

## 4c. Tier-2 Kata microVM runner (installed 2026-06-10, ADR-0015)

The production runner is **Kata Containers microVMs** (Cloud Hypervisor),
hardware-isolated. This is what `--app-runner kata` uses; the systemd
runner (4b) stays available for KVM-less boxes. Box requirements, all in
place on finite-lat-2 (KVM present, nested virt on):

- containerd (apt) running; `sudo systemctl enable --now containerd`.
- **kata-static** release extracted to `/opt/kata`, with
  `containerd-shim-kata-v2` and `kata-runtime` symlinked into
  `/usr/local/bin`, and Cloud Hypervisor selected:
  `cp /opt/kata/share/defaults/kata-containers/configuration-clh.toml
  /etc/kata-containers/configuration.toml`. Verify with
  `kata-runtime check` ("System can currently create Kata Containers").
- **nerdctl** binary in `/usr/local/bin` and **CNI plugins** in
  `/opt/cni/bin` (both from upstream release tarballs). App images
  (node:22-slim, oven/bun:1, astral-sh/uv) are pulled from public
  registries on first use; no image build, so no buildkit.
- App data dirs: `/var/lib/finite-sites/apps/{site}/data` (bind-mounted as
  `$DATA_DIR`, survives stop/start) — this is the Kata-runner backup scope.
- **Privilege wiring** (the one delicate part): the Kata runner shells
  `sudo nerdctl` because CNI bridge setup needs CAP_NET_ADMIN. Install
  `deploy/finite-lat-2/finite-sites-nerdctl-sudoers` ->
  `/etc/sudoers.d/finite-sites-nerdctl`, and the drop-in
  `deploy/finite-lat-2/finite-saas-sites-kata.conf` ->
  `/etc/systemd/system/finite-saas-sites.service.d/kata.conf` (relaxes the
  daemon's own fs sandbox so nerdctl can run; tenant isolation is now the
  microVM boundary). Then the unit's ExecStart uses `--app-runner kata`.

App logs under Kata: `sudo nerdctl logs finite-app-{site_id}`. Inspect the
fleet: `sudo nerdctl ps` / `sudo nerdctl stats --no-stream`.

**Density:** the Supervisor stops apps idle past `--app-idle-timeout`
(default 900s) and wakes them on the next request (~1.4s cold, ~0.3s
warm), so idle tenants cost ~0 RAM. Resident app microVMs measured
8–87 MiB each.

## 5. Validation gates

Box-local gates (passed 2026-06-09 with a temporary local `--api-url`):

- `/api/v1/healthz` returns `{"ok":true}` through Caddy TLS.
- project apply → git push → share serves a Project Output through Caddy.
- `https://api.finite.chat/` classifies as the API plane, not a site page
  (dispatch regression gate).
- `https://git.finite.chat/PROJECT.git` routes to the Git plane; an editor
  can `fsite auth git`, clone, commit, and push `main` through
  `git-http-backend`.
- Restarting `finite-saas-sites` loses nothing.

Tier-2 runtime gates (passed 2026-06-10): bun+SQLite, FastHTML (uv inline
deps), and Next.js standalone app bundles serve through the proxy with their
visibility gates, persist data in `$DATA_DIR`, and come back up via reconcile
after a daemon restart. App outputs need a Project-first agent surface before
they are re-advertised.

Remaining gates once Cloudflare DNS is live:

- `curl https://api.finite.chat/api/v1/healthz` from anywhere.
- `fsite project apply` + `fsite auth git` + `git push origin main` from a
  real agent workspace (proves NIP-98 URL matching and git smart HTTP through
  the proxy — closes debt ledger item 7).
- A magic link arrives at a real inbox (after the Resend flip), logs the
  viewer in, and removing the email revokes access on refresh.

## 5a. Routine server rollout

Run these commands from the repo root after local tests pass. They sync the
current checkout to the production source checkout, build on the box, install
the two production binaries, restart the service, and smoke test the public
control and serving planes.

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check

ssh finite-lat-2 'install -d ~/finite-sites'
rsync -az --delete \
  --exclude .git \
  --exclude target \
  --exclude .dev-data \
  --exclude .finite \
  --exclude '.env*' \
  --exclude node_modules \
  --exclude .direnv \
  ./ finite-lat-2:~/finite-sites/

ssh finite-lat-2 \
  'rm -rf ~/finite-sites/.finite ~/finite-sites/.dev-data ~/finite-sites/node_modules ~/finite-sites/.direnv && rm -f ~/finite-sites/.env ~/finite-sites/.env.*'

ssh finite-lat-2 \
  'cd ~/finite-sites && PATH="$HOME/.cargo/bin:$PATH" cargo build --release'
ssh finite-lat-2 \
  'cd ~/finite-sites && sudo install -m 0755 target/release/finitesitesd target/release/fsite /usr/local/bin/'
ssh finite-lat-2 \
  'sudo systemctl daemon-reload && sudo systemctl restart finite-saas-sites'

curl https://api.finite.chat/api/v1/healthz
curl -I https://finitechat-native-mockup.finite.chat/
curl https://finitechat-native-mockup.finite.chat/llms.txt
```

If the rollout changes Caddy files or systemd unit files, install those files
explicitly before `daemon-reload`, then reload Caddy after the service restart:

```sh
ssh finite-lat-2 \
  'cd ~/finite-sites && sudo install -m 0644 deploy/finite-lat-2/finite-saas-sites.service /etc/systemd/system/'
ssh finite-lat-2 \
  'cd ~/finite-sites && sudo install -m 0644 deploy/finite-lat-2/Caddyfile-sites /etc/caddy/Caddyfile'
ssh finite-lat-2 \
  'sudo systemctl daemon-reload && sudo systemctl restart finite-saas-sites && sudo systemctl reload caddy'
```

If a deploy fails after install, use the journal first; the service owns all
control-plane, Git, and serving-plane state transitions:

```sh
ssh finite-lat-2 'journalctl -u finite-saas-sites -n 120 --no-pager'
```

## 5b. Project-first reset and example redeploy

Pre-User Reset is intentionally destructive. Use it only during pre-user
development, after an operator explicitly confirms that product data can be
wiped. It removes registry state, blobs, git repositories, app data, outbox,
tokens, grants, collaborators, sites, Versions, and other state under
`/var/lib/finite-sites`; it preserves systemd units, Caddy configuration,
environment files, installed binaries, OS users, certificates, and source
checkouts.

```sh
sudo systemctl stop finite-saas-sites
sudo -u finite-sites finitesitesd pre-user-reset \
  --data /var/lib/finite-sites \
  --confirm-wipe-product-data yes
sudo systemctl start finite-saas-sites

sudo -u finite-sites finitesitesd allow \
  --data /var/lib/finite-sites \
  OWNER_NPUB \
  --note "pre-user project reset bootstrap"
```

Redeploy examples through Project Repositories, not legacy site-first publish
commands:

The example fixture grants `skyler@example.com` as the bootstrap editor.
Replace that email before applying if another External Principal should mint a
Git Credential and push.

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
```

Use the returned Git Credential with standard git:

```sh
git clone https://git.finite.chat/finitechat-native.git /tmp/finitechat-native
rsync -a --delete examples/finitechat-native-mockup/ /tmp/finitechat-native/
cd /tmp/finitechat-native
git add finite.toml index.html
git commit -m "Seed finitechat native mockup"
git push origin main
```

Verify the reset path by checking:

```sh
curl https://api.finite.chat/api/v1/healthz
curl https://finitechat-native-mockup.finite.chat/
curl https://finitechat-native-mockup.finite.chat/llms.txt
```

The generated `/llms.txt` should describe `fsite auth git`, cloning
`https://git.finite.chat/finitechat-native.git`, editing committed source,
and pushing the Deploy Branch.

## 6. Backups

Add `/var/lib/finite-sites` to the offsite backup scope. Registry + blobs
are the whole state; the cookie secret file invalidates viewer logins if
lost (acceptable) but should be backed up to keep sessions across
restores. Litestream for `registry.db` is debt ledger item 4 and should
land shortly after go-live.
