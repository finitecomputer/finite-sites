//! `finitesitesd` — the Finite Sites server.
//!
//! Subcommands:
//!   serve     run the API + site-serving HTTP server
//!   allow     add an operator publish grant for a pubkey (hex or npub)
//!   disallow  revoke an operator publish grant
//!   allowed   list active publishing grants
//!   pre-user-reset  wipe product state during pre-user development
//!   git-post-receive  internal hook helper for Project Repositories
//!
//! All subcommands take `--data DIR`; the registry database, blob store,
//! cookie secret, and dev-mail outbox live under that directory.

pub mod api;
pub mod apps;
pub mod content_type;
pub mod git;
pub mod limiter;
pub mod llms;
pub mod mailer;
pub mod pages;
pub mod proxy;
pub mod server;
pub mod sites;

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use finitesites_blob::BlobStore;
use finitesites_engine::{Engine, EngineConfig};
use finitesites_proto::{hex, ids, npub};
use finitesites_store::{PublishGrantSource, Store};

#[derive(Debug)]
pub struct ServeOptions {
    pub data_dir: PathBuf,
    pub listen: SocketAddr,
    pub base_domain: String,
    pub api_url: String,
    pub git_base_url: String,
    pub git_hook_helper_path: PathBuf,
    pub git_auto_reconcile: bool,
    pub site_url_scheme: String,
    pub site_url_port: Option<u16>,
    /// `None` = dev mailer (outbox files). The API key for an HTTP provider
    /// comes from its environment variable, never from argv.
    pub mail_provider: Option<mailer::MailProvider>,
    pub mail_from: Option<String>,
    /// How tier-2 apps are isolated and run.
    pub app_runner_kind: AppRunnerKind,
    /// Apps with no requests for this long are stopped to free memory and
    /// woken on the next request (the density mechanism).
    pub idle_timeout_seconds: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppRunnerKind {
    /// Record app publishes but run nothing (local dev, tests).
    Disabled,
    /// systemd DynamicUser sandbox — kernel isolation (ADR-0014).
    Systemd,
    /// Kata Containers microVM — hardware isolation (ADR-0015).
    Kata,
}

pub fn run(args: Vec<String>) -> Result<(), String> {
    let Some(command) = args.first() else {
        return Err(usage());
    };
    match command.as_str() {
        "serve" => {
            let options = parse_serve_options(&args[1..])?;
            serve(options)
        }
        "allow" => allowlist_mutate(&args[1..], true),
        "disallow" => allowlist_mutate(&args[1..], false),
        "allowed" => allowlist_list(&args[1..]),
        "pre-user-reset" => pre_user_reset(&args[1..]),
        "git-post-receive" => git_post_receive(),
        "--version" | "-V" | "version" => version(&args[1..]),
        "--help" | "help" => {
            println!("{}", usage());
            Ok(())
        }
        other => Err(format!("unknown command `{other}`\n{}", usage())),
    }
}

fn usage() -> String {
    "usage:\n  finitesitesd serve --data DIR [--listen 127.0.0.1:8787] \
     [--base-domain sites.localhost] [--api-url http://127.0.0.1:8787] \
     [--git-url http://git.sites.localhost:8787] \
     [--git-hook-helper PATH] [--git-auto-reconcile true|false] \
     [--site-scheme http] [--site-port PORT|none] \
     [--mailer dev|resend|postmark] [--mail-from ADDR] \
     [--app-runner none|systemd|kata] [--app-idle-timeout SECONDS]\n  \
     finitesitesd allow --data DIR PUBKEY_OR_NPUB [--note TEXT]\n  \
     finitesitesd disallow --data DIR PUBKEY_OR_NPUB\n  \
     finitesitesd allowed --data DIR\n  \
     finitesitesd pre-user-reset --data DIR --confirm-wipe-product-data yes\n  \
     finitesitesd git-post-receive"
        .to_string()
}

fn version(args: &[String]) -> Result<(), String> {
    if !args.is_empty() {
        return Err("usage: finitesitesd --version".to_string());
    }
    println!("finitesitesd {}", env!("CARGO_PKG_VERSION"));
    Ok(())
}

type ParsedFlags = (Vec<(String, String)>, Vec<String>);

/// Tiny explicit flag parser: `--flag value` pairs plus positionals.
/// We parse by hand instead of adding a CLI dependency; the surface is small.
fn parse_flags(args: &[String]) -> Result<ParsedFlags, String> {
    let mut flags = Vec::new();
    let mut positionals = Vec::new();
    let mut index: usize = 0;
    // Bounded by argv length.
    while index < args.len() {
        let arg = &args[index];
        if let Some(name) = arg.strip_prefix("--") {
            let value = args
                .get(index + 1)
                .ok_or_else(|| format!("flag --{name} needs a value"))?;
            flags.push((name.to_string(), value.clone()));
            index += 2;
        } else {
            positionals.push(arg.clone());
            index += 1;
        }
    }
    Ok((flags, positionals))
}

fn flag_value<'a>(flags: &'a [(String, String)], name: &str) -> Option<&'a str> {
    flags
        .iter()
        .find(|(flag, _)| flag == name)
        .map(|(_, value)| value.as_str())
}

fn parse_serve_options(args: &[String]) -> Result<ServeOptions, String> {
    let (flags, positionals) = parse_flags(args)?;
    if !positionals.is_empty() {
        return Err(format!("unexpected argument `{}`", positionals[0]));
    }
    let data_dir = flag_value(&flags, "data").ok_or("--data DIR is required")?;
    let listen: SocketAddr = flag_value(&flags, "listen")
        .unwrap_or("127.0.0.1:8787")
        .parse()
        .map_err(|_| "invalid --listen address".to_string())?;
    let base_domain = flag_value(&flags, "base-domain")
        .unwrap_or("sites.localhost")
        .to_string();
    if base_domain.is_empty() || base_domain.contains(':') || base_domain.contains('/') {
        return Err("--base-domain must be a bare domain".to_string());
    }
    let api_url = flag_value(&flags, "api-url")
        .map(str::to_string)
        .unwrap_or_else(|| format!("http://{listen}"));
    if api_url.ends_with('/') {
        return Err("--api-url must not end with /".to_string());
    }
    let site_url_scheme = flag_value(&flags, "site-scheme")
        .unwrap_or("http")
        .to_string();
    // Default the site-URL port to the listen port: in local dev the same
    // process serves both planes. Behind a real proxy pass `--site-port none`.
    let site_url_port = match flag_value(&flags, "site-port") {
        None => Some(listen.port()),
        Some("none") => None,
        Some(raw) => Some(
            raw.parse::<u16>()
                .map_err(|_| "invalid --site-port".to_string())?,
        ),
    };
    let git_base_url = match flag_value(&flags, "git-url") {
        Some(raw) => {
            if raw.ends_with('/') {
                return Err("--git-url must not end with /".to_string());
            }
            raw.to_string()
        }
        None => {
            let port_part = match site_url_port {
                Some(port) => format!(":{port}"),
                None => String::new(),
            };
            format!("{site_url_scheme}://git.{base_domain}{port_part}")
        }
    };
    let git_hook_helper_path = match flag_value(&flags, "git-hook-helper") {
        Some(raw) => PathBuf::from(raw),
        None => std::env::current_exe()
            .map_err(|error| format!("cannot determine current executable: {error}"))?,
    };
    let git_auto_reconcile = match flag_value(&flags, "git-auto-reconcile") {
        None | Some("true") => true,
        Some("false") => false,
        Some(other) => {
            return Err(format!(
                "unknown --git-auto-reconcile `{other}` (true|false)"
            ));
        }
    };
    let mail_provider = match flag_value(&flags, "mailer") {
        None | Some("dev") => None,
        Some(raw) => Some(
            mailer::MailProvider::parse(raw)
                .ok_or_else(|| format!("unknown --mailer `{raw}` (dev|resend|postmark)"))?,
        ),
    };
    let mail_from = flag_value(&flags, "mail-from").map(str::to_string);
    if mail_provider.is_some() && mail_from.is_none() {
        return Err("--mailer resend|postmark requires --mail-from".to_string());
    }
    let app_runner_kind = match flag_value(&flags, "app-runner") {
        None | Some("none") => AppRunnerKind::Disabled,
        Some("systemd") => AppRunnerKind::Systemd,
        Some("kata") => AppRunnerKind::Kata,
        Some(other) => {
            return Err(format!(
                "unknown --app-runner `{other}` (none|systemd|kata)"
            ));
        }
    };
    let idle_timeout_seconds = match flag_value(&flags, "app-idle-timeout") {
        None => apps::DEFAULT_IDLE_TIMEOUT_SECONDS,
        Some(raw) => raw
            .parse::<u64>()
            .ok()
            .filter(|seconds| *seconds > 0)
            .ok_or("--app-idle-timeout must be a positive number of seconds")?,
    };
    Ok(ServeOptions {
        data_dir: PathBuf::from(data_dir),
        listen,
        base_domain,
        api_url,
        git_base_url,
        git_hook_helper_path,
        git_auto_reconcile,
        site_url_scheme,
        site_url_port,
        mail_provider,
        mail_from,
        app_runner_kind,
        idle_timeout_seconds,
    })
}

fn git_post_receive() -> Result<(), String> {
    crate::git::run_post_receive_hook_from_env()
}

fn open_store(data_dir: &Path) -> Result<Store, String> {
    std::fs::create_dir_all(data_dir)
        .map_err(|error| format!("cannot create data dir: {error}"))?;
    Store::open(&data_dir.join("registry.db"))
        .map_err(|error| format!("cannot open registry: {error}"))
}

/// Load or create the 32-byte cookie secret at `DATA/cookie-secret`.
fn load_cookie_secret(data_dir: &Path) -> Result<[u8; 32], String> {
    let path = data_dir.join("cookie-secret");
    if path.exists() {
        let raw = std::fs::read_to_string(&path)
            .map_err(|error| format!("cannot read cookie secret: {error}"))?;
        let bytes = hex::decode32(raw.trim())
            .map_err(|_| "cookie-secret file is corrupt (expected 64 hex chars)".to_string())?;
        return Ok(bytes);
    }
    let secret = ids::random_32();
    std::fs::write(&path, hex::encode(&secret))
        .map_err(|error| format!("cannot write cookie secret: {error}"))?;
    Ok(secret)
}

fn serve(options: ServeOptions) -> Result<(), String> {
    let store = open_store(&options.data_dir)?;
    let blobs = BlobStore::open(&options.data_dir.join("blobs"))
        .map_err(|error| format!("cannot open blob store: {error}"))?;
    let cookie_secret = load_cookie_secret(&options.data_dir)?;
    let engine_config = EngineConfig {
        base_domain: options.base_domain.clone(),
        site_url_scheme: options.site_url_scheme.clone(),
        site_url_port: options.site_url_port,
    };
    let engine = Engine::new(store, blobs, cookie_secret, engine_config);
    let mail: Box<dyn mailer::Mailer> = match options.mail_provider {
        None => Box::new(
            mailer::DevMailer::new(options.data_dir.join("outbox"))
                .map_err(|error| format!("cannot open outbox: {error}"))?,
        ),
        Some(provider) => {
            let env_var = provider.api_key_env_var();
            let api_key = std::env::var(env_var)
                .map_err(|_| format!("--mailer requires the {env_var} environment variable"))?;
            let from_address = options
                .mail_from
                .clone()
                .expect("mail_from is validated alongside mail_provider");
            Box::new(mailer::HttpMailer::new(provider, api_key, from_address))
        }
    };

    let app_runner: Box<dyn apps::AppRunner> = match options.app_runner_kind {
        AppRunnerKind::Disabled => Box::new(apps::DisabledRunner),
        AppRunnerKind::Systemd => Box::new(
            apps::SystemdAppRunner::new(options.data_dir.join("apps"))
                .map_err(|error| format!("cannot set up systemd app runner: {error}"))?,
        ),
        AppRunnerKind::Kata => Box::new(
            apps::KataAppRunner::new(options.data_dir.join("apps"))
                .map_err(|error| format!("cannot set up kata app runner: {error}"))?,
        ),
    };
    let supervisor = apps::Supervisor::new(app_runner, options.idle_timeout_seconds);

    let runtime =
        tokio::runtime::Runtime::new().map_err(|error| format!("cannot start runtime: {error}"))?;
    runtime.block_on(server::serve(engine, mail, supervisor, options))
}

fn allowlist_mutate(args: &[String], allow: bool) -> Result<(), String> {
    let (flags, positionals) = parse_flags(args)?;
    let data_dir = flag_value(&flags, "data").ok_or("--data DIR is required")?;
    let [key_input] = positionals.as_slice() else {
        return Err("expected exactly one PUBKEY_OR_NPUB argument".to_string());
    };
    let pubkey = npub::pubkey_from_hex_or_npub(key_input)
        .map_err(|error| format!("invalid pubkey: {error}"))?;
    let mut store = open_store(Path::new(data_dir))?;
    if allow {
        let note = flag_value(&flags, "note").unwrap_or("");
        store
            .allow_pubkey(&pubkey, note, server::now_unix())
            .map_err(|error| format!("allow failed: {error}"))?;
        println!(
            "allowed {}",
            npub::encode_npub(&pubkey).expect("valid pubkey")
        );
    } else {
        let removed = store
            .revoke_publish_access(&pubkey, PublishGrantSource::Operator, server::now_unix())
            .map_err(|error| format!("disallow failed: {error}"))?;
        if removed {
            println!(
                "disallowed {}",
                npub::encode_npub(&pubkey).expect("valid pubkey")
            );
        } else {
            println!("pubkey had no operator publishing grant");
        }
    }
    Ok(())
}

fn allowlist_list(args: &[String]) -> Result<(), String> {
    let (flags, _positionals) = parse_flags(args)?;
    let data_dir = flag_value(&flags, "data").ok_or("--data DIR is required")?;
    let store = open_store(Path::new(data_dir))?;
    let grants = store
        .list_publish_grants(server::now_unix())
        .map_err(|error| format!("list failed: {error}"))?;
    if grants.is_empty() {
        println!("publishing grant cache is empty");
        return Ok(());
    }
    // Bounded: operator/Core curated list.
    for grant in grants {
        let display = npub::encode_npub(&grant.pubkey).unwrap_or(grant.pubkey);
        let source = grant.source.as_str();
        let expires = match grant.expires_at {
            Some(expires_at) => format!(", expires_at={expires_at}"),
            None => String::new(),
        };
        if grant.note.is_empty() {
            println!("{display}  # source={source}{expires}");
        } else {
            println!("{display}  # source={source}{expires}, {}", grant.note);
        }
    }
    Ok(())
}

fn pre_user_reset(args: &[String]) -> Result<(), String> {
    let (flags, positionals) = parse_flags(args)?;
    if !positionals.is_empty() {
        return Err(format!("unexpected argument `{}`", positionals[0]));
    }
    let data_dir = flag_value(&flags, "data").ok_or("--data DIR is required")?;
    let confirmed = flag_value(&flags, "confirm-wipe-product-data") == Some("yes");
    if !confirmed {
        return Err(
            "pre-user-reset is destructive; pass --confirm-wipe-product-data yes".to_string(),
        );
    }
    let wiped = reset_product_data(Path::new(data_dir))?;
    if wiped.is_empty() {
        println!("no Finite Sites product data found under {data_dir}");
    } else {
        println!("wiped Finite Sites product data under {data_dir}:");
        // Bounded by the fixed reset path list.
        for item in wiped {
            println!("- {item}");
        }
    }
    println!("preserved host/runtime config such as cookie-secret and deployment files");
    Ok(())
}

fn reset_product_data(data_dir: &Path) -> Result<Vec<String>, String> {
    std::fs::create_dir_all(data_dir)
        .map_err(|error| format!("cannot create data dir: {error}"))?;
    let product_entries = [
        "registry.db",
        "registry.db-wal",
        "registry.db-shm",
        "blobs",
        "outbox",
        "apps",
        "git",
    ];
    let mut wiped = Vec::new();
    // Bounded by product_entries above.
    for entry in product_entries {
        let path = data_dir.join(entry);
        if !path.exists() {
            continue;
        }
        let metadata = std::fs::symlink_metadata(&path)
            .map_err(|error| format!("cannot inspect {}: {error}", path.display()))?;
        if metadata.is_dir() {
            std::fs::remove_dir_all(&path)
                .map_err(|error| format!("cannot remove {}: {error}", path.display()))?;
        } else {
            std::fs::remove_file(&path)
                .map_err(|error| format!("cannot remove {}: {error}", path.display()))?;
        }
        wiped.push(entry.to_string());
    }
    Ok(wiped)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_probe_is_read_only() {
        run(vec!["--version".to_string()]).unwrap();
        run(vec!["-V".to_string()]).unwrap();
        run(vec!["version".to_string()]).unwrap();
        assert_eq!(
            run(vec!["--version".to_string(), "extra".to_string()]).unwrap_err(),
            "usage: finitesitesd --version"
        );
    }

    #[test]
    fn pre_user_reset_wipes_product_data_and_preserves_runtime_secret() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("registry.db"), b"db").unwrap();
        std::fs::write(dir.path().join("registry.db-wal"), b"wal").unwrap();
        std::fs::create_dir(dir.path().join("blobs")).unwrap();
        std::fs::write(dir.path().join("blobs").join("blob"), b"x").unwrap();
        std::fs::create_dir(dir.path().join("git")).unwrap();
        std::fs::write(dir.path().join("cookie-secret"), b"secret").unwrap();

        let wiped = reset_product_data(dir.path()).unwrap();
        assert!(wiped.contains(&"registry.db".to_string()));
        assert!(wiped.contains(&"registry.db-wal".to_string()));
        assert!(wiped.contains(&"blobs".to_string()));
        assert!(wiped.contains(&"git".to_string()));
        assert!(!dir.path().join("registry.db").exists());
        assert!(!dir.path().join("blobs").exists());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("cookie-secret")).unwrap(),
            "secret"
        );
    }
}
