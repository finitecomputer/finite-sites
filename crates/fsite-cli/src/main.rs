//! `fsite` — the agent-facing CLI for Finite Sites.
//!
//! Commands hide nostr, keys, manifests, and blob mechanics; the agent only
//! sees names, paths, emails, and URLs:
//!
//!   fsite whoami
//!   fsite claim NAME
//!   fsite publish NAME PATH [--spa]
//!   fsite status NAME
//!   fsite list
//!   fsite share NAME [--shared|--private] [--public --yes-public]
//!                    [--add-email E]... [--remove-email E]...
//!
//! Server address comes from FINITE_SITES_API (default http://127.0.0.1:8787).

mod api;
mod bundle;
mod keys;
mod walk;

use std::path::PathBuf;
use std::process::ExitCode;

use thiserror::Error;

use finitesites_proto::dto::SharingRequest;
use finitesites_proto::limits::MAX_EMAILS_PER_SHARING_REQUEST;
use finitesites_proto::npub;

#[derive(Debug, Error)]
pub enum CliError {
    #[error("{0}")]
    Usage(String),
    #[error("key error: {0}")]
    Key(String),
    #[error("io error: {0}")]
    Io(String),
    #[error("server error: {0}")]
    Api(String),
    #[error("network error: {0}")]
    Http(String),
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("fsite: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &[String]) -> Result<(), CliError> {
    let Some(command) = args.first() else {
        return Err(CliError::Usage(usage()));
    };
    match command.as_str() {
        "whoami" => whoami(),
        "claim" => claim(&args[1..]),
        "publish" => publish(&args[1..]),
        "publish-app" => publish_app(&args[1..]),
        "status" => status(&args[1..]),
        "list" => list(),
        "share" => share(&args[1..]),
        "--help" | "help" => {
            println!("{}", usage());
            Ok(())
        }
        other => Err(CliError::Usage(format!(
            "unknown command `{other}`\n{}",
            usage()
        ))),
    }
}

fn usage() -> String {
    "usage:\n  fsite whoami\n  fsite claim NAME\n  fsite publish NAME PATH [--spa]\n  \
     fsite publish-app NAME PATH --start \"CMD\"\n  \
     fsite status NAME\n  fsite list\n  fsite share NAME [--shared|--private] \
     [--public --yes-public] [--add-email EMAIL]... [--remove-email EMAIL]..."
        .to_string()
}

fn whoami() -> Result<(), CliError> {
    let identity = keys::load_or_create_identity()?;
    let display =
        npub::encode_npub(&identity.pubkey).map_err(|error| CliError::Key(error.to_string()))?;
    println!("npub:   {display}");
    println!("pubkey: {}", identity.pubkey);
    println!("file:   {}", keys::identity_path()?.display());
    println!();
    println!("ask a finite operator to allowlist this npub before claiming sites");
    Ok(())
}

fn claim(args: &[String]) -> Result<(), CliError> {
    let [name] = args else {
        return Err(CliError::Usage("usage: fsite claim NAME".to_string()));
    };
    let identity = keys::load_or_create_identity()?;
    let site_key = keys::load_or_create_site_key(name)?;
    let client = api::Client::from_env();
    let response = client.claim(&identity, name, &site_key.pubkey)?;
    if response.already_claimed {
        println!("{} was already yours", response.name);
    } else {
        println!("claimed {}", response.name);
    }
    println!("url:    {}", response.url);
    println!(
        "status: {} (publish with: fsite publish {} PATH)",
        response.status, response.name
    );
    Ok(())
}

fn publish(args: &[String]) -> Result<(), CliError> {
    let (positionals, flags): (Vec<&String>, Vec<&String>) =
        args.iter().partition(|arg| !arg.starts_with("--"));
    let mut spa = false;
    // Bounded by argv length.
    for flag in flags {
        match flag.as_str() {
            "--spa" => spa = true,
            other => return Err(CliError::Usage(format!("unknown flag `{other}`"))),
        }
    }
    let [name, path] = positionals.as_slice() else {
        return Err(CliError::Usage(
            "usage: fsite publish NAME PATH [--spa]".to_string(),
        ));
    };
    let site_key = keys::load_site_key(name)?;
    let outcome = walk::build_manifest(&PathBuf::from(path))?;
    if outcome.skipped_hidden > 0 {
        eprintln!(
            "skipped {} hidden file(s)/folder(s)",
            outcome.skipped_hidden
        );
    }

    let client = api::Client::from_env();
    let begun = client.begin_publish(&site_key, name, &outcome.manifest, spa)?;
    let total = outcome.manifest.files.len();
    let to_upload = begun.missing.len();
    eprintln!(
        "{total} file(s) in manifest, {to_upload} new blob(s) to upload \
         ({} already on the server)",
        total - to_upload.min(total)
    );

    // Bounded by MAX_MANIFEST_FILES via manifest validation.
    for (index, sha256) in begun.missing.iter().enumerate() {
        let source = outcome
            .sources
            .get(sha256)
            .ok_or_else(|| CliError::Api(format!("server asked for unknown blob {sha256}")))?;
        let bytes = std::fs::read(source)
            .map_err(|error| CliError::Io(format!("cannot read {}: {error}", source.display())))?;
        client.upload_blob(&site_key, &begun.publish_id, sha256, &bytes)?;
        eprintln!("uploaded {}/{to_upload} {}", index + 1, source.display());
    }

    let finalized = client.finalize_publish(&site_key, &begun.publish_id)?;
    println!("published {name} version {}", finalized.version_number);
    println!(
        "{} file(s), {} bytes",
        finalized.path_count, finalized.total_bytes
    );
    println!("url: {}", finalized.url);
    println!();
    println!("the site is PRIVATE by default; use `fsite share {name} ...` to share it");
    Ok(())
}

/// Tier 2: bundle a directory and publish it as a server app. The start
/// command runs in the platform sandbox and must listen on `$PORT`.
fn publish_app(args: &[String]) -> Result<(), CliError> {
    let mut positionals: Vec<&String> = Vec::new();
    let mut start: Option<String> = None;
    let mut index: usize = 0;
    // Bounded by argv length.
    while index < args.len() {
        match args[index].as_str() {
            "--start" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| CliError::Usage("--start needs a command".to_string()))?;
                start = Some(value.clone());
                index += 2;
            }
            other if other.starts_with("--") => {
                return Err(CliError::Usage(format!("unknown flag `{other}`")));
            }
            _ => {
                positionals.push(&args[index]);
                index += 1;
            }
        }
    }
    let ([name, path], Some(start)) = (positionals.as_slice(), start) else {
        return Err(CliError::Usage(
            "usage: fsite publish-app NAME PATH --start \"CMD\" \
             (the command must listen on $PORT)"
                .to_string(),
        ));
    };

    let site_key = keys::load_site_key(name)?;
    eprintln!("bundling {path} ...");
    let bundle_bytes = bundle::build_bundle(&PathBuf::from(path))?;
    let sha256 = {
        use sha2::Digest as _;
        finitesites_proto::hex::encode(&sha2::Sha256::digest(&bundle_bytes))
    };
    let manifest = finitesites_proto::PublishManifest {
        files: vec![finitesites_proto::ManifestFile {
            path: finitesites_proto::manifest::APP_BUNDLE_PATH.to_string(),
            sha256: sha256.clone(),
            size: bundle_bytes.len() as u64,
        }],
    };

    let client = api::Client::from_env();
    let begun = client.begin_publish_app(&site_key, name, &manifest, &start)?;
    if begun.missing.is_empty() {
        eprintln!("bundle already on the server (unchanged)");
    } else {
        eprintln!(
            "uploading bundle ({} MiB) ...",
            bundle_bytes.len() / (1024 * 1024)
        );
        client.upload_blob(&site_key, &begun.publish_id, &sha256, &bundle_bytes)?;
    }
    let finalized = client.finalize_publish(&site_key, &begun.publish_id)?;
    println!("published app {name} version {}", finalized.version_number);
    println!("url: {}", finalized.url);
    println!();
    println!("the app is starting; it must listen on $PORT to serve");
    println!("the site is PRIVATE by default; use `fsite share {name} ...` to share it");
    Ok(())
}

fn status(args: &[String]) -> Result<(), CliError> {
    let [name] = args else {
        return Err(CliError::Usage("usage: fsite status NAME".to_string()));
    };
    let key = actor_key_for(name)?;
    let client = api::Client::from_env();
    let summary = client.site_status(&key, name)?;
    print_summary(&summary);
    Ok(())
}

fn list() -> Result<(), CliError> {
    let identity = keys::load_or_create_identity()?;
    let client = api::Client::from_env();
    let response = client.list_sites(&identity)?;
    if response.sites.is_empty() {
        println!("no sites yet; claim one with `fsite claim NAME`");
        return Ok(());
    }
    // Bounded by MAX_SITES_PER_OWNER.
    for site in &response.sites {
        let version = site
            .active_version
            .map(|v| format!("v{v}"))
            .unwrap_or_else(|| "unpublished".to_string());
        println!(
            "{:<24} {:<8} {:<12} {:<8} {}",
            site.name, site.kind, site.visibility, version, site.url
        );
    }
    Ok(())
}

fn share(args: &[String]) -> Result<(), CliError> {
    let Some((name, flags)) = args.split_first() else {
        return Err(CliError::Usage(
            "usage: fsite share NAME [--shared|--private] [--public --yes-public] \
             [--add-email EMAIL]... [--remove-email EMAIL]..."
                .to_string(),
        ));
    };

    let mut visibility: Option<String> = None;
    let mut confirm_public = false;
    let mut add_emails: Vec<String> = Vec::new();
    let mut remove_emails: Vec<String> = Vec::new();
    let mut index: usize = 0;
    // Bounded by argv length.
    while index < flags.len() {
        match flags[index].as_str() {
            "--public" => {
                visibility = Some("public".to_string());
                index += 1;
            }
            "--shared" => {
                visibility = Some("shared".to_string());
                index += 1;
            }
            "--private" => {
                visibility = Some("private".to_string());
                index += 1;
            }
            "--yes-public" => {
                confirm_public = true;
                index += 1;
            }
            "--add-email" | "--remove-email" => {
                let value = flags
                    .get(index + 1)
                    .ok_or_else(|| CliError::Usage(format!("{} needs a value", flags[index])))?;
                if flags[index] == "--add-email" {
                    add_emails.push(value.clone());
                } else {
                    remove_emails.push(value.clone());
                }
                index += 2;
            }
            other => {
                return Err(CliError::Usage(format!("unknown flag `{other}`")));
            }
        }
    }

    if visibility.as_deref() == Some("public") && !confirm_public {
        return Err(CliError::Usage(
            "making a site public exposes it to the whole internet. \
             Confirm with the user first, then re-run with --yes-public"
                .to_string(),
        ));
    }
    if add_emails.len() + remove_emails.len() > MAX_EMAILS_PER_SHARING_REQUEST as usize {
        return Err(CliError::Usage(format!(
            "at most {MAX_EMAILS_PER_SHARING_REQUEST} email changes per command"
        )));
    }
    if visibility.is_none() && add_emails.is_empty() && remove_emails.is_empty() {
        return Err(CliError::Usage(
            "nothing to change; pass --shared/--private/--public and/or email flags".to_string(),
        ));
    }
    // Adding emails to a site implies shared visibility unless stated.
    if visibility.is_none() && !add_emails.is_empty() {
        visibility = Some("shared".to_string());
    }

    let key = actor_key_for(name)?;
    let client = api::Client::from_env();
    let response = client.set_sharing(
        &key,
        name,
        &SharingRequest {
            visibility,
            confirm_public,
            add_emails,
            remove_emails,
        },
    )?;
    println!("visibility: {}", response.visibility);
    if response.shared_emails.is_empty() {
        println!("shared with: nobody");
    } else {
        println!("shared with: {}", response.shared_emails.join(", "));
    }
    Ok(())
}

fn print_summary(summary: &finitesites_proto::dto::SiteSummary) {
    println!("name:       {}", summary.name);
    println!("url:        {}", summary.url);
    println!("status:     {}", summary.status);
    println!("kind:       {}", summary.kind);
    println!("visibility: {}", summary.visibility);
    match summary.active_version {
        Some(version) => println!("version:    v{version}"),
        None => println!("version:    unpublished"),
    }
    if !summary.shared_emails.is_empty() {
        println!("shared:     {}", summary.shared_emails.join(", "));
    }
}

/// Prefer the site key (workspace-scoped) and fall back to the identity for
/// commands that accept either signer.
fn actor_key_for(name: &str) -> Result<keys::KeyFile, CliError> {
    if keys::site_key_path(name).exists() {
        keys::load_site_key(name)
    } else {
        keys::load_or_create_identity()
    }
}
