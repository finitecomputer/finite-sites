//! `finitesitesd` — the Finite Sites server.
//!
//! Subcommands:
//!   serve     run the API + site-serving HTTP server
//!   allow     add a pubkey (hex or npub) to the publishing allowlist
//!   disallow  remove a pubkey from the allowlist
//!   allowed   list allowlisted pubkeys
//!
//! All subcommands take `--data DIR`; the registry database, blob store,
//! cookie secret, and dev-mail outbox live under that directory.

pub mod api;
pub mod apps;
pub mod content_type;
pub mod limiter;
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
use finitesites_store::Store;

#[derive(Debug)]
pub struct ServeOptions {
    pub data_dir: PathBuf,
    pub listen: SocketAddr,
    pub base_domain: String,
    pub api_url: String,
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
     [--site-scheme http] [--site-port PORT|none] \
     [--mailer dev|resend|postmark] [--mail-from ADDR] \
     [--app-runner none|systemd|kata] [--app-idle-timeout SECONDS]\n  \
     finitesitesd allow --data DIR PUBKEY_OR_NPUB [--note TEXT]\n  \
     finitesitesd disallow --data DIR PUBKEY_OR_NPUB\n  \
     finitesitesd allowed --data DIR"
        .to_string()
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
        site_url_scheme,
        site_url_port,
        mail_provider,
        mail_from,
        app_runner_kind,
        idle_timeout_seconds,
    })
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
            .disallow_pubkey(&pubkey)
            .map_err(|error| format!("disallow failed: {error}"))?;
        if removed {
            println!(
                "disallowed {}",
                npub::encode_npub(&pubkey).expect("valid pubkey")
            );
        } else {
            println!("pubkey was not on the allowlist");
        }
    }
    Ok(())
}

fn allowlist_list(args: &[String]) -> Result<(), String> {
    let (flags, _positionals) = parse_flags(args)?;
    let data_dir = flag_value(&flags, "data").ok_or("--data DIR is required")?;
    let store = open_store(Path::new(data_dir))?;
    let allowed = store
        .list_allowed()
        .map_err(|error| format!("list failed: {error}"))?;
    if allowed.is_empty() {
        println!("allowlist is empty");
        return Ok(());
    }
    // Bounded: operator-curated list.
    for (pubkey, note) in allowed {
        let display = npub::encode_npub(&pubkey).unwrap_or(pubkey);
        if note.is_empty() {
            println!("{display}");
        } else {
            println!("{display}  # {note}");
        }
    }
    Ok(())
}
