//! Tier 2 app supervision (ADR-0014, ADR-0015).
//!
//! An app site's finalized version is one `tar.gz` bundle. A `Supervisor`
//! owns the density policy over a pluggable `AppRunner`: it wakes an app on
//! the first request and stops it when idle, so idle tenants cost ~0 RAM
//! and a box hosts far more apps than fit resident.
//!
//! Two runners implement isolation:
//! - `KataAppRunner` (production): each app is a Kata Containers microVM
//!   (Cloud Hypervisor) via `nerdctl`, hardware-isolated. A public runtime
//!   image is chosen per start command; the bundle is bind-mounted
//!   read-only and `$DATA_DIR` is a host directory surviving stop/start.
//! - `SystemdAppRunner` (KVM-less fallback): a `finite-app@{site}` systemd
//!   template instance — DynamicUser, read-only code, private
//!   StateDirectory, resource caps — driven over systemd's D-Bus API.

use std::collections::HashMap;
use std::io::Write as _;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

use finitesites_engine::AppDeploy;

/// Bundle extraction bounds: a 256 MiB gzip could expand much larger, so
/// the unpacked entry count and total bytes are capped explicitly.
const MAX_BUNDLE_ENTRIES: u32 = 100_000;
const MAX_UNPACKED_BYTES: u64 = 2 * 1024 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum AppRunnerError {
    #[error("app io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("bundle invalid: {0}")]
    BundleInvalid(&'static str),
    #[error("runner command failed: {0}")]
    Command(String),
}

/// One runner backs every app site. Implementations supply isolation
/// (systemd sandbox, Kata microVM); the `Supervisor` above them owns the
/// density policy (wake on request, stop when idle).
///
/// All methods are idempotent and keyed by the app's site id so the
/// supervisor can call them without threading runner-specific handles.
pub trait AppRunner: Send + Sync {
    /// Materialize a finalized version (extract bundle, write config) and
    /// start it. Called on publish and at boot reconcile.
    fn deploy(&self, deploy: &AppDeploy, bundle_path: &Path) -> Result<(), AppRunnerError>;

    /// Start an already-deployed app if it is stopped, returning the
    /// address the proxy should forward to (loopback for systemd, the
    /// microVM's bridge IP for Kata). Called to wake an app on a request;
    /// the supervisor waits for readiness afterwards.
    fn ensure_started(&self, deploy: &AppDeploy) -> Result<SocketAddr, AppRunnerError>;

    /// Stop a running app, freeing its memory. Called by idle reaping.
    fn stop(&self, site_id: &str) -> Result<(), AppRunnerError>;

    /// Whether the app is currently running.
    fn is_running(&self, site_id: &str) -> Result<bool, AppRunnerError>;

    /// Can this runner execute the start command at all? Called at publish
    /// time so an unrunnable command is a 400 to the publisher instead of a
    /// silently dead site. Default: anything goes (systemd runs from PATH).
    fn validate_start(&self, _start_command: &str) -> Result<(), AppRunnerError> {
        Ok(())
    }
}

fn loopback(port: u16) -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)
}

/// Used when the host has no app runtime (local dev, tests). App publishes
/// still succeed and are recorded; nothing runs.
pub struct DisabledRunner;

impl AppRunner for DisabledRunner {
    fn deploy(&self, deploy: &AppDeploy, _bundle_path: &Path) -> Result<(), AppRunnerError> {
        eprintln!(
            "app runner disabled: {} v({}) recorded but not started",
            deploy.site_id, deploy.version_id
        );
        Ok(())
    }
    fn ensure_started(&self, deploy: &AppDeploy) -> Result<SocketAddr, AppRunnerError> {
        Ok(loopback(deploy.port))
    }
    fn stop(&self, _site_id: &str) -> Result<(), AppRunnerError> {
        Ok(())
    }
    fn is_running(&self, _site_id: &str) -> Result<bool, AppRunnerError> {
        Ok(false)
    }
}

pub struct SystemdAppRunner {
    apps_root: PathBuf,
}

impl SystemdAppRunner {
    pub fn new(apps_root: PathBuf) -> Result<SystemdAppRunner, AppRunnerError> {
        std::fs::create_dir_all(&apps_root)?;
        Ok(SystemdAppRunner { apps_root })
    }

    fn unit_name(site_id: &str) -> String {
        // Site ids are generated `site_<hex>`; they are also the systemd
        // instance name and must never contain shell or unit metacharacters.
        assert!(
            site_id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        );
        format!("finite-app@{site_id}.service")
    }

    fn systemctl(&self, action: &str, unit: &str) -> Result<String, AppRunnerError> {
        let output = Command::new("systemctl").args([action, unit]).output()?;
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if output.status.success() {
            Ok(stdout)
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            Err(AppRunnerError::Command(format!(
                "systemctl {action} {unit}: {stdout} {stderr}"
            )))
        }
    }

    /// Materialize the release directory and flip the `current` symlink,
    /// shared by deploy. Does not start the unit.
    fn materialize(&self, deploy: &AppDeploy, bundle_path: &Path) -> Result<(), AppRunnerError> {
        let site_dir = self.apps_root.join(&deploy.site_id);
        let release_dir = site_dir.join("releases").join(&deploy.version_id);
        if !release_dir.is_dir() {
            extract_bundle(bundle_path, &site_dir, &release_dir)?;
        }
        write_env_file(&site_dir, deploy)?;

        // Atomic symlink flip: build aside, rename over.
        let current_link = site_dir.join("current");
        let staging_link = site_dir.join("current.next");
        let _ = std::fs::remove_file(&staging_link);
        std::os::unix::fs::symlink(&release_dir, &staging_link)?;
        std::fs::rename(&staging_link, &current_link)?;
        assert!(std::fs::read_link(&current_link)? == release_dir);
        Ok(())
    }
}

impl AppRunner for SystemdAppRunner {
    fn deploy(&self, deploy: &AppDeploy, bundle_path: &Path) -> Result<(), AppRunnerError> {
        self.materialize(deploy, bundle_path)?;
        // restart (not start) so a new version replaces a running old one.
        self.systemctl("restart", &Self::unit_name(&deploy.site_id))?;
        Ok(())
    }

    fn ensure_started(&self, deploy: &AppDeploy) -> Result<SocketAddr, AppRunnerError> {
        if !self.is_running(&deploy.site_id)? {
            self.systemctl("start", &Self::unit_name(&deploy.site_id))?;
        }
        Ok(loopback(deploy.port))
    }

    fn stop(&self, site_id: &str) -> Result<(), AppRunnerError> {
        // StateDirectory (the app's $DATA_DIR) persists across stop, so
        // idle reaping never loses tenant data.
        self.systemctl("stop", &Self::unit_name(site_id))?;
        Ok(())
    }

    fn is_running(&self, site_id: &str) -> Result<bool, AppRunnerError> {
        let state = self
            .systemctl("is-active", &Self::unit_name(site_id))
            .unwrap_or_default();
        Ok(state == "active" || state == "activating")
    }
}

// ---- kata runner: hardware-isolated microVMs ------------------------------

/// Each app runs as a Kata Containers microVM managed through `nerdctl`
/// (containerd + the `io.containerd.kata.v2` runtime, Cloud Hypervisor VMM).
/// The app's bundle is bind-mounted read-only; its `$DATA_DIR` is a
/// read-write host directory that survives stop/start; networking is a CNI
/// bridge and the proxy forwards to the microVM's IP. No custom image build
/// is required — a public runtime image is chosen per start command, so the
/// only daemons are containerd and the Kata shim.
pub struct KataAppRunner {
    apps_root: PathBuf,
}

impl KataAppRunner {
    pub fn new(apps_root: PathBuf) -> Result<KataAppRunner, AppRunnerError> {
        std::fs::create_dir_all(&apps_root)?;
        Ok(KataAppRunner { apps_root })
    }

    fn container_name(site_id: &str) -> String {
        assert!(
            site_id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        );
        format!("finite-app-{site_id}")
    }

    /// Public runtime image for a start command, by its first token. The
    /// image supplies only the interpreter; app code and dependencies live
    /// in the bundle (`/app`) or are fetched at runtime into `$DATA_DIR`.
    fn image_for(start_command: &str) -> Result<&'static str, AppRunnerError> {
        let first = start_command.split_whitespace().next().unwrap_or("");
        match first {
            "node" | "npm" | "npx" => Ok("docker.io/library/node:22-slim"),
            "bun" | "bunx" => Ok("docker.io/oven/bun:1"),
            "uv" | "uvx" | "python" | "python3" => {
                Ok("ghcr.io/astral-sh/uv:python3.12-bookworm-slim")
            }
            _ => Err(AppRunnerError::Command(format!(
                "no runtime image for start command starting with `{first}` \
                 (supported: node, bun, uv/python)"
            ))),
        }
    }

    /// nerdctl drives containerd + CNI, and CNI bridge setup needs
    /// CAP_NET_ADMIN, so it runs through `sudo -n` (a narrow sudoers rule
    /// scoped to nerdctl). All argv here is daemon-constructed; the only
    /// tenant-controlled value is the start command, validated to printable
    /// ascii and passed as one argv element into the guest's shell.
    fn nerdctl(&self, args: &[&str]) -> Result<String, AppRunnerError> {
        let output = Command::new("sudo")
            .arg("-n")
            .arg("nerdctl")
            .args(args)
            .output()?;
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if output.status.success() {
            Ok(stdout)
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            Err(AppRunnerError::Command(format!(
                "nerdctl {}: {stdout} {stderr}",
                args.join(" ")
            )))
        }
    }

    fn data_dir(&self, site_id: &str) -> PathBuf {
        self.apps_root.join(site_id).join("data")
    }

    /// Create and start a fresh container for a deploy, replacing any
    /// existing one. Memory/CPU caps keep one tenant from starving the box.
    fn run_fresh(&self, deploy: &AppDeploy, release_dir: &Path) -> Result<(), AppRunnerError> {
        let name = Self::container_name(&deploy.site_id);
        let image = Self::image_for(&deploy.start_command)?;
        let data_dir = self.data_dir(&deploy.site_id);
        std::fs::create_dir_all(&data_dir)?;
        make_world_readable(&data_dir)?;

        let _ = self.nerdctl(&["rm", "-f", &name]); // ignore "no such container"

        let port = deploy.port.to_string();
        let port_env = format!("PORT={port}");
        let start_env = format!("FINITE_APP_START={}", deploy.start_command);
        let release_mount = format!("{}:/app:ro", release_dir.display());
        let data_mount = format!("{}:/data", data_dir.display());
        self.nerdctl(&[
            "run",
            "-d",
            "--name",
            &name,
            "--runtime",
            "io.containerd.kata.v2",
            "--restart",
            "no",
            "--memory",
            "512m",
            "--cpus",
            "1",
            "-w",
            "/app",
            "-e",
            &port_env,
            "-e",
            "DATA_DIR=/data",
            "-e",
            "HOME=/data",
            "-e",
            "NODE_ENV=production",
            "-e",
            &start_env,
            "-v",
            &release_mount,
            "-v",
            &data_mount,
            image,
            // No host shell: a single argv element carries the (validated,
            // printable-ascii) command into the guest's /bin/sh.
            "sh",
            "-lc",
            &deploy.start_command,
        ])?;
        Ok(())
    }

    /// The microVM's bridge IP, read back after start.
    fn container_ip(&self, site_id: &str) -> Result<IpAddr, AppRunnerError> {
        let name = Self::container_name(site_id);
        let raw = self.nerdctl(&[
            "inspect",
            "-f",
            "{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}",
            &name,
        ])?;
        raw.trim()
            .parse::<IpAddr>()
            .map_err(|_| AppRunnerError::Command(format!("no IP for {name}: `{raw}`")))
    }
}

impl AppRunner for KataAppRunner {
    fn deploy(&self, deploy: &AppDeploy, bundle_path: &Path) -> Result<(), AppRunnerError> {
        let site_dir = self.apps_root.join(&deploy.site_id);
        let release_dir = site_dir.join("releases").join(&deploy.version_id);
        if !release_dir.is_dir() {
            extract_bundle(bundle_path, &site_dir, &release_dir)?;
        }
        make_parents_traversable(&site_dir)?;
        self.run_fresh(deploy, &release_dir)?;
        Ok(())
    }

    fn ensure_started(&self, deploy: &AppDeploy) -> Result<SocketAddr, AppRunnerError> {
        if self.is_running(&deploy.site_id)? {
            return Ok(SocketAddr::new(
                self.container_ip(&deploy.site_id)?,
                deploy.port,
            ));
        }
        // A stopped container can be restarted (fast: same VM config); a
        // missing one must be recreated from the deploy.
        let name = Self::container_name(&deploy.site_id);
        if self.nerdctl(&["start", &name]).is_err() {
            let release_dir = self
                .apps_root
                .join(&deploy.site_id)
                .join("releases")
                .join(&deploy.version_id);
            self.run_fresh(deploy, &release_dir)?;
        }
        Ok(SocketAddr::new(
            self.container_ip(&deploy.site_id)?,
            deploy.port,
        ))
    }

    fn stop(&self, site_id: &str) -> Result<(), AppRunnerError> {
        // Stopping tears down the microVM and frees its RAM; the data
        // bind-mount on the host persists.
        self.nerdctl(&["stop", &Self::container_name(site_id)])?;
        Ok(())
    }

    fn is_running(&self, site_id: &str) -> Result<bool, AppRunnerError> {
        let name = Self::container_name(site_id);
        let status = self
            .nerdctl(&["inspect", "-f", "{{.State.Status}}", &name])
            .unwrap_or_default();
        Ok(status == "running")
    }

    fn validate_start(&self, start_command: &str) -> Result<(), AppRunnerError> {
        Self::image_for(start_command).map(|_| ())
    }
}

/// Extract a tar.gz bundle with explicit bounds, staged then renamed so a
/// failed extraction never leaves a half-built release directory.
fn extract_bundle(
    bundle_path: &Path,
    site_dir: &Path,
    release_dir: &Path,
) -> Result<(), AppRunnerError> {
    let staging = site_dir.join("staging");
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging)?;

    let file = std::fs::File::open(bundle_path)?;
    let decoder = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(decoder);

    let mut entry_count: u32 = 0;
    let mut unpacked_bytes: u64 = 0;
    // Bounded by MAX_BUNDLE_ENTRIES, checked inside.
    for entry in archive.entries()? {
        let mut entry = entry?;
        entry_count += 1;
        if entry_count > MAX_BUNDLE_ENTRIES {
            return Err(AppRunnerError::BundleInvalid("too many entries"));
        }
        unpacked_bytes = unpacked_bytes.saturating_add(entry.size());
        if unpacked_bytes > MAX_UNPACKED_BYTES {
            return Err(AppRunnerError::BundleInvalid("unpacked size too large"));
        }
        // unpack_in refuses paths that escape the target directory.
        let unpacked = entry.unpack_in(&staging)?;
        if !unpacked {
            return Err(AppRunnerError::BundleInvalid("entry escapes bundle root"));
        }
    }
    if entry_count == 0 {
        return Err(AppRunnerError::BundleInvalid("bundle is empty"));
    }

    // The sandbox runs as a dynamic user: code must be world-readable
    // (and traversable/executable where the bundle says so).
    make_world_readable(&staging)?;

    if let Some(parent) = release_dir.parent() {
        std::fs::create_dir_all(parent)?;
        make_parents_traversable(site_dir)?;
    }
    std::fs::rename(&staging, release_dir)?;
    Ok(())
}

/// chmod the tree so the dynamic app user can read it: dirs a+rx, files
/// keep their exec bit but gain a+r. Iterative walk, bounded by the entry
/// caps enforced during extraction.
fn make_world_readable(root: &Path) -> Result<(), AppRunnerError> {
    use std::os::unix::fs::PermissionsExt as _;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755))?;
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                stack.push(entry.path());
            } else if file_type.is_file() {
                let mode = entry.metadata()?.permissions().mode();
                let readable = if mode & 0o111 != 0 { 0o755 } else { 0o644 };
                std::fs::set_permissions(entry.path(), std::fs::Permissions::from_mode(readable))?;
            }
        }
    }
    Ok(())
}

fn make_parents_traversable(site_dir: &Path) -> Result<(), AppRunnerError> {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(site_dir, std::fs::Permissions::from_mode(0o755))?;
    let releases = site_dir.join("releases");
    if releases.is_dir() {
        std::fs::set_permissions(&releases, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

/// systemd EnvironmentFile for one app. Values are double-quoted with
/// backslash escaping; start commands are validated printable-ascii
/// upstream, so newline injection is impossible.
fn write_env_file(site_dir: &Path, deploy: &AppDeploy) -> Result<(), AppRunnerError> {
    let path = site_dir.join("app.env");
    let escaped = deploy
        .start_command
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    let state_dir = format!("/var/lib/finite-app/{}", deploy.site_id);
    let mut file = std::fs::File::create(&path)?;
    writeln!(file, "PORT={}", deploy.port)?;
    writeln!(file, "FINITE_APP_START=\"{escaped}\"")?;
    // Many runtimes (uv, bun, npm) want a writable HOME for caches; the
    // unit's StateDirectory is the app's only writable path.
    writeln!(file, "HOME={state_dir}")?;
    writeln!(file, "DATA_DIR={state_dir}")?;
    writeln!(file, "NODE_ENV=production")?;
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))?;
    Ok(())
}

// ---- supervisor: density policy over any runner ----------------------------

/// Default idle timeout: an app with no requests for this long is stopped,
/// freeing its memory. 15 minutes balances "wake is rare for a live site"
/// against "idle apps cost nothing." Tunable per box.
pub const DEFAULT_IDLE_TIMEOUT_SECONDS: u64 = 15 * 60;

/// The Supervisor turns a runner into a density-managing platform: it wakes
/// apps on the first request and stops them when idle, so idle tenants cost
/// ~0 memory and a box can "host" far more apps than fit resident at once.
/// This is the Fly suspend/resume model; the proxy already fronts every app,
/// so wake-on-request is a natural fit.
///
/// Access times are unix seconds (not `Instant`) so reaping is exercised by
/// plain unit tests with an injected clock, matching the store's `now_unix`.
pub struct Supervisor {
    runner: Box<dyn AppRunner>,
    idle_timeout_seconds: u64,
    /// site_id -> last access (unix seconds). Present means "managed and
    /// recently touched"; absent means never accessed since daemon start.
    last_access: Mutex<HashMap<String, u64>>,
    /// Cache of believed-running apps and their forward addresses, so the
    /// request hot path never shells out to the runner. Source of truth is
    /// runner state; invalidation triggers are idle reaping (stop) and a
    /// failed proxy connection (`invalidate`); stale-read behavior is one
    /// failed request, after which the next request re-wakes the app.
    endpoints: Mutex<HashMap<String, SocketAddr>>,
    /// Serializes cold wakes so two concurrent first requests cannot race
    /// the runner into creating the same container twice. Warm requests
    /// bypass this via the endpoint cache.
    wake_lock: Mutex<()>,
}

impl Supervisor {
    pub fn new(runner: Box<dyn AppRunner>, idle_timeout_seconds: u64) -> Supervisor {
        assert!(idle_timeout_seconds > 0);
        Supervisor {
            runner,
            idle_timeout_seconds,
            last_access: Mutex::new(HashMap::new()),
            endpoints: Mutex::new(HashMap::new()),
            wake_lock: Mutex::new(()),
        }
    }

    /// Publish-time check that the runner can execute this command at all.
    pub fn validate_start(&self, start_command: &str) -> Result<(), AppRunnerError> {
        self.runner.validate_start(start_command)
    }

    /// Drop the cached endpoint after a failed proxy connection; the next
    /// request takes the cold path and re-wakes the app.
    pub fn invalidate(&self, site_id: &str) {
        self.endpoints
            .lock()
            .expect("supervisor mutex never poisoned")
            .remove(site_id);
    }

    pub fn runner(&self) -> &dyn AppRunner {
        self.runner.as_ref()
    }

    fn touch(&self, site_id: &str, now: u64) {
        self.last_access
            .lock()
            .expect("supervisor mutex never poisoned")
            .insert(site_id.to_string(), now);
    }

    /// Deploy a finalized version (publish or boot reconcile), cache its
    /// forward address, and mark it active so reaping leaves it alone until
    /// it has had a full idle window.
    pub fn deploy(
        &self,
        deploy: &AppDeploy,
        bundle_path: &Path,
        now: u64,
    ) -> Result<(), AppRunnerError> {
        self.runner.deploy(deploy, bundle_path)?;
        // ensure_started on a running app is a cheap read that yields the
        // forward address (the IP can change across redeploys).
        let address = self.runner.ensure_started(deploy)?;
        self.endpoints
            .lock()
            .expect("supervisor mutex never poisoned")
            .insert(deploy.site_id.clone(), address);
        self.touch(&deploy.site_id, now);
        Ok(())
    }

    /// Wake an app for an incoming request: record the access, start it if
    /// stopped, and return the address to forward to. Warm requests are one
    /// map lookup; only cold wakes touch the runner, serialized by the wake
    /// lock. Readiness waiting is the caller's job (it is async).
    pub fn note_request_and_start(
        &self,
        deploy: &AppDeploy,
        now: u64,
    ) -> Result<SocketAddr, AppRunnerError> {
        self.touch(&deploy.site_id, now);
        if let Some(address) = self
            .endpoints
            .lock()
            .expect("supervisor mutex never poisoned")
            .get(&deploy.site_id)
            .copied()
        {
            return Ok(address);
        }
        let _wake = self
            .wake_lock
            .lock()
            .expect("supervisor mutex never poisoned");
        // Double-check under the lock: a concurrent wake may have won.
        if let Some(address) = self
            .endpoints
            .lock()
            .expect("supervisor mutex never poisoned")
            .get(&deploy.site_id)
            .copied()
        {
            return Ok(address);
        }
        let address = self.runner.ensure_started(deploy)?;
        self.endpoints
            .lock()
            .expect("supervisor mutex never poisoned")
            .insert(deploy.site_id.clone(), address);
        Ok(address)
    }

    pub fn is_running(&self, site_id: &str) -> Result<bool, AppRunnerError> {
        self.runner.is_running(site_id)
    }

    /// Stop every running app whose last access is older than the idle
    /// timeout. Apps never accessed since boot are seeded as accessed at
    /// reconcile, so they get a full idle window before the first reap.
    /// Returns the site ids stopped, for logging.
    pub fn reap_idle(&self, deploys: &[AppDeploy], now: u64) -> Vec<String> {
        let mut stopped = Vec::new();
        // Bounded by the number of app sites (bounded by the port range).
        for deploy in deploys {
            let last = {
                let map = self.last_access.lock().expect("supervisor mutex");
                map.get(&deploy.site_id).copied()
            };
            let idle_since = match last {
                Some(at) => now.saturating_sub(at),
                None => 0, // unseen: treat as just-active, reap next cycle
            };
            if last.is_none() {
                self.touch(&deploy.site_id, now);
                continue;
            }
            if idle_since < self.idle_timeout_seconds {
                continue;
            }
            match self.runner.is_running(&deploy.site_id) {
                Ok(false) => continue, // already stopped
                Ok(true) => {}
                Err(error) => {
                    eprintln!("reap: cannot check {}: {error}", deploy.site_id);
                    continue;
                }
            }
            match self.runner.stop(&deploy.site_id) {
                Ok(()) => {
                    self.last_access
                        .lock()
                        .expect("supervisor mutex")
                        .remove(&deploy.site_id);
                    self.invalidate(&deploy.site_id);
                    stopped.push(deploy.site_id.clone());
                }
                Err(error) => eprintln!("reap: cannot stop {}: {error}", deploy.site_id),
            }
        }
        stopped
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn deploy_fixture() -> AppDeploy {
        AppDeploy {
            site_id: "site_0123abcd".into(),
            version_id: "ver_0123abcd".into(),
            bundle_sha256: "ab".repeat(32),
            start_command: r#"node server.js --flag "quoted""#.into(),
            port: 21000,
        }
    }

    fn make_bundle(entries: &[(&str, &str)]) -> Vec<u8> {
        let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        let mut builder = tar::Builder::new(encoder);
        for (path, content) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(content.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(&mut header, path, content.as_bytes())
                .unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap()
    }

    #[test]
    fn extracts_bundle_and_applies_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let bundle = make_bundle(&[("server.js", "console.log('hi')"), ("lib/util.js", "x")]);
        let bundle_path = dir.path().join("bundle.tar.gz");
        std::fs::write(&bundle_path, bundle).unwrap();

        let site_dir = dir.path().join("site");
        let release_dir = site_dir.join("releases").join("ver_1");
        extract_bundle(&bundle_path, &site_dir, &release_dir).unwrap();
        assert!(release_dir.join("server.js").is_file());
        assert!(release_dir.join("lib/util.js").is_file());
        assert!(!site_dir.join("staging").exists());

        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(release_dir.join("server.js"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0o044, "world readable");
    }

    #[test]
    fn rejects_escaping_and_empty_bundles() {
        let dir = tempfile::tempdir().unwrap();
        let site_dir = dir.path().join("site");

        let empty = make_bundle(&[]);
        let empty_path = dir.path().join("empty.tar.gz");
        std::fs::write(&empty_path, empty).unwrap();
        let result = extract_bundle(&empty_path, &site_dir, &site_dir.join("releases/v"));
        assert!(matches!(result, Err(AppRunnerError::BundleInvalid(_))));

        // tar::Builder refuses to create `..` paths, so forge the header
        // bytes directly to simulate a hostile archive.
        let escaping = {
            let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
            let mut builder = tar::Builder::new(encoder);
            let mut header = tar::Header::new_gnu();
            let evil_path = b"../outside.txt";
            {
                let gnu = header.as_gnu_mut().unwrap();
                gnu.name[..evil_path.len()].copy_from_slice(evil_path);
            }
            header.set_size(4);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append(&header, &b"evil"[..]).unwrap();
            builder.into_inner().unwrap().finish().unwrap()
        };
        let escaping_path = dir.path().join("escape.tar.gz");
        std::fs::write(&escaping_path, escaping).unwrap();
        let result = extract_bundle(&escaping_path, &site_dir, &site_dir.join("releases/v2"));
        assert!(result.is_err());
        assert!(!dir.path().join("outside.txt").exists());
    }

    #[test]
    fn env_file_escapes_quotes() {
        let dir = tempfile::tempdir().unwrap();
        write_env_file(dir.path(), &deploy_fixture()).unwrap();
        let content = std::fs::read_to_string(dir.path().join("app.env")).unwrap();
        assert!(content.contains("PORT=21000"));
        assert!(content.contains(r#"FINITE_APP_START="node server.js --flag \"quoted\"""#));
        assert!(content.contains("HOME=/var/lib/finite-app/site_0123abcd"));
    }

    #[test]
    fn unit_names_are_constrained() {
        assert_eq!(
            SystemdAppRunner::unit_name("site_0a1b"),
            "finite-app@site_0a1b.service"
        );
    }
}

#[cfg(test)]
mod supervisor_tests {
    use super::*;

    use std::sync::Arc;

    #[derive(Default)]
    struct FakeRunner {
        running: Arc<Mutex<HashMap<String, bool>>>,
        stops: Arc<Mutex<u32>>,
        starts: Arc<Mutex<u32>>,
    }

    impl AppRunner for FakeRunner {
        fn deploy(&self, deploy: &AppDeploy, _bundle: &Path) -> Result<(), AppRunnerError> {
            self.running
                .lock()
                .unwrap()
                .insert(deploy.site_id.clone(), true);
            Ok(())
        }
        fn ensure_started(&self, deploy: &AppDeploy) -> Result<SocketAddr, AppRunnerError> {
            let mut map = self.running.lock().unwrap();
            if !*map.get(&deploy.site_id).unwrap_or(&false) {
                map.insert(deploy.site_id.clone(), true);
                *self.starts.lock().unwrap() += 1;
            }
            Ok(loopback(deploy.port))
        }
        fn stop(&self, site_id: &str) -> Result<(), AppRunnerError> {
            self.running
                .lock()
                .unwrap()
                .insert(site_id.to_string(), false);
            *self.stops.lock().unwrap() += 1;
            Ok(())
        }
        fn is_running(&self, site_id: &str) -> Result<bool, AppRunnerError> {
            Ok(*self.running.lock().unwrap().get(site_id).unwrap_or(&false))
        }
    }

    fn deploy(id: &str) -> AppDeploy {
        AppDeploy {
            site_id: id.into(),
            version_id: "ver_1".into(),
            bundle_sha256: "ab".repeat(32),
            start_command: "node server.js".into(),
            port: 21000,
        }
    }

    const NOW: u64 = 1_750_000_000;

    #[test]
    fn idle_apps_are_reaped_after_the_timeout() {
        let runner = Box::new(FakeRunner::default());
        let sup = Supervisor::new(runner, 600);
        let app = deploy("site_a");

        sup.deploy(&app, Path::new("/dev/null"), NOW).unwrap();
        assert!(sup.is_running("site_a").unwrap());

        // Within the window: not reaped.
        assert!(
            sup.reap_idle(std::slice::from_ref(&app), NOW + 599)
                .is_empty()
        );
        assert!(sup.is_running("site_a").unwrap());

        // Past the window: reaped.
        let stopped = sup.reap_idle(std::slice::from_ref(&app), NOW + 601);
        assert_eq!(stopped, vec!["site_a".to_string()]);
        assert!(!sup.is_running("site_a").unwrap());
    }

    #[test]
    fn a_request_wakes_a_reaped_app_and_resets_the_clock() {
        let runner = Box::new(FakeRunner::default());
        let sup = Supervisor::new(runner, 600);
        let app = deploy("site_a");
        sup.deploy(&app, Path::new("/dev/null"), NOW).unwrap();
        sup.reap_idle(std::slice::from_ref(&app), NOW + 601);
        assert!(!sup.is_running("site_a").unwrap());

        // A request wakes it and returns the forward address.
        let addr = sup.note_request_and_start(&app, NOW + 700).unwrap();
        assert_eq!(addr.port(), 21000);
        assert!(sup.is_running("site_a").unwrap());

        // The fresh access resets the idle clock: not reaped just after.
        assert!(
            sup.reap_idle(std::slice::from_ref(&app), NOW + 800)
                .is_empty()
        );
        assert!(sup.is_running("site_a").unwrap());
    }

    #[test]
    fn warm_requests_never_touch_the_runner() {
        let runner = FakeRunner::default();
        let starts = runner.starts.clone();
        let stops = runner.stops.clone();
        let running = runner.running.clone();
        let sup = Supervisor::new(Box::new(runner), 600);
        let app = deploy("site_a");
        sup.deploy(&app, Path::new("/dev/null"), NOW).unwrap();

        // Many warm requests: the endpoint cache answers; the runner is
        // never asked to start anything (deploy left the app running).
        for offset in 0..50 {
            sup.note_request_and_start(&app, NOW + offset).unwrap();
        }
        assert_eq!(*starts.lock().unwrap(), 0);
        assert_eq!(*stops.lock().unwrap(), 0);

        // Simulate a crash: the app dies and the cache is invalidated
        // (what the proxy does on an unreachable upstream). The next
        // request takes the cold path and actually starts the app.
        running.lock().unwrap().insert("site_a".into(), false);
        sup.invalidate("site_a");
        let addr = sup.note_request_and_start(&app, NOW + 100).unwrap();
        assert_eq!(addr.port(), 21000);
        assert_eq!(*starts.lock().unwrap(), 1);
    }

    #[test]
    fn freshly_deployed_apps_get_a_full_idle_window() {
        let runner = Box::new(FakeRunner::default());
        let sup = Supervisor::new(runner, 600);
        let app = deploy("site_a");
        sup.deploy(&app, Path::new("/dev/null"), NOW).unwrap();
        // Just under the window from deploy: still running.
        assert!(
            sup.reap_idle(std::slice::from_ref(&app), NOW + 500)
                .is_empty()
        );
        assert!(sup.is_running("site_a").unwrap());
    }
}

#[cfg(test)]
mod kata_tests {
    use super::*;

    #[test]
    fn runtime_image_selected_by_start_command() {
        assert_eq!(
            KataAppRunner::image_for("node server.js").unwrap(),
            "docker.io/library/node:22-slim"
        );
        assert_eq!(
            KataAppRunner::image_for("bun run start.ts").unwrap(),
            "docker.io/oven/bun:1"
        );
        assert_eq!(
            KataAppRunner::image_for("uv run app.py").unwrap(),
            "ghcr.io/astral-sh/uv:python3.12-bookworm-slim"
        );
        assert!(KataAppRunner::image_for("./mystery-binary").is_err());
    }

    #[test]
    fn container_names_are_constrained() {
        assert_eq!(
            KataAppRunner::container_name("site_0a1b"),
            "finite-app-site_0a1b"
        );
    }
}
