//! Key material handling for the CLI.
//!
//! Two kinds of keys, per ADR-0003:
//! - the user identity key, at `~/.config/finite-sites/identity.env`
//!   (override with FINITE_SITES_IDENTITY), used to claim names;
//! - one site key per site, at `.finite/sites/NAME.env` in the workspace,
//!   used to publish and share that site.
//!
//! Key files are `KEY=hex` env-style, created with 0600 permissions, and
//! must never be committed or included in deploy artifacts.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use finitesites_proto::{event, hex, ids};

use crate::CliError;

const IDENTITY_KEY_NAME: &str = "FINITE_SITES_USER_SECRET";
const SITE_KEY_NAME: &str = "FINITE_SITE_SECRET";
const EMAIL_KEY_NAME: &str = "FINITE_SITES_EMAIL_SECRET";

pub struct KeyFile {
    pub secret: [u8; 32],
    pub pubkey: String,
}

fn parse_env_file(content: &str, wanted_key: &str) -> Option<String> {
    // Bounded: key files are a handful of lines.
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(value) = trimmed.strip_prefix(wanted_key)
            && let Some(value) = value.strip_prefix('=')
        {
            return Some(value.trim().to_string());
        }
    }
    None
}

fn load_key_file(path: &Path, key_name: &str) -> Result<KeyFile, CliError> {
    let content = std::fs::read_to_string(path)
        .map_err(|error| CliError::Io(format!("cannot read {}: {error}", path.display())))?;
    let secret_hex = parse_env_file(&content, key_name)
        .ok_or_else(|| CliError::Key(format!("{} is missing {key_name}", path.display())))?;
    let secret = hex::decode32(&secret_hex)
        .map_err(|_| CliError::Key(format!("{} has a malformed secret", path.display())))?;
    let pubkey = event::pubkey_for_secret(&secret)
        .map_err(|_| CliError::Key(format!("{} secret is not a valid key", path.display())))?;
    Ok(KeyFile { secret, pubkey })
}

fn write_key_file(path: &Path, key_name: &str, secret: &[u8; 32]) -> Result<(), CliError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            CliError::Io(format!("cannot create {}: {error}", parent.display()))
        })?;
    }
    let content = format!("{key_name}={}\n", hex::encode(secret));
    std::fs::write(path, content)
        .map_err(|error| CliError::Io(format!("cannot write {}: {error}", path.display())))?;
    set_owner_only_permissions(path)?;
    // Paired check: read the key back before trusting it was stored.
    let reread = load_key_file(path, key_name)?;
    assert!(reread.secret == *secret);
    Ok(())
}

#[cfg(unix)]
fn set_owner_only_permissions(path: &Path) -> Result<(), CliError> {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|error| CliError::Io(format!("cannot chmod {}: {error}", path.display())))
}

#[cfg(not(unix))]
fn set_owner_only_permissions(_path: &Path) -> Result<(), CliError> {
    Ok(())
}

pub fn identity_path() -> Result<PathBuf, CliError> {
    if let Ok(custom) = std::env::var("FINITE_SITES_IDENTITY") {
        return Ok(PathBuf::from(custom));
    }
    let home = std::env::var("HOME")
        .map_err(|_| CliError::Key("HOME is not set; set FINITE_SITES_IDENTITY".to_string()))?;
    Ok(PathBuf::from(home).join(".config/finite-sites/identity.env"))
}

/// Load the user identity, creating one on first use.
pub fn load_or_create_identity() -> Result<KeyFile, CliError> {
    let path = identity_path()?;
    if path.exists() {
        return load_key_file(&path, IDENTITY_KEY_NAME);
    }
    let secret = ids::random_32();
    write_key_file(&path, IDENTITY_KEY_NAME, &secret)?;
    eprintln!("created new identity at {}", path.display());
    load_key_file(&path, IDENTITY_KEY_NAME)
}

pub fn email_key_path(email: &str) -> Result<PathBuf, CliError> {
    let home = std::env::var("HOME")
        .map_err(|_| CliError::Key("HOME is not set; cannot store email key".to_string()))?;
    let digest = hex::encode(&Sha256::digest(
        email.trim().to_ascii_lowercase().as_bytes(),
    ));
    Ok(PathBuf::from(home)
        .join(".config/finite-sites/emails")
        .join(format!("{}.env", &digest[..16])))
}

pub fn load_or_create_email_key(email: &str) -> Result<KeyFile, CliError> {
    let path = email_key_path(email)?;
    if path.exists() {
        return load_key_file(&path, EMAIL_KEY_NAME);
    }
    let secret = ids::random_32();
    write_key_file(&path, EMAIL_KEY_NAME, &secret)?;
    eprintln!("created email key at {}", path.display());
    load_key_file(&path, EMAIL_KEY_NAME)
}

pub fn site_key_path(name: &str) -> PathBuf {
    PathBuf::from(".finite/sites").join(format!("{name}.env"))
}

pub fn load_site_key(name: &str) -> Result<KeyFile, CliError> {
    let path = site_key_path(name);
    if !path.exists() {
        return Err(CliError::Key(format!(
            "no site key at {}; run `fsite claim {name}` in this workspace first",
            path.display()
        )));
    }
    load_key_file(&path, SITE_KEY_NAME)
}

/// Load the site key for a name, creating it on first claim.
pub fn load_or_create_site_key(name: &str) -> Result<KeyFile, CliError> {
    let path = site_key_path(name);
    if path.exists() {
        return load_key_file(&path, SITE_KEY_NAME);
    }
    let secret = ids::random_32();
    write_key_file(&path, SITE_KEY_NAME, &secret)?;
    eprintln!(
        "created site key at {} -- keep it out of git and deploy artifacts",
        path.display()
    );
    load_key_file(&path, SITE_KEY_NAME)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_env_file_finds_key() {
        let content = "# comment\nFINITE_SITE_SECRET=abc123\nOTHER=x\n";
        assert_eq!(
            parse_env_file(content, "FINITE_SITE_SECRET").as_deref(),
            Some("abc123")
        );
        assert_eq!(parse_env_file(content, "MISSING"), None);
        // A key that is a prefix of another must not match it.
        let tricky = "FINITE_SITE_SECRET_OLD=zzz\nFINITE_SITE_SECRET=good\n";
        assert_eq!(
            parse_env_file(tricky, "FINITE_SITE_SECRET").as_deref(),
            Some("good")
        );
    }
}
