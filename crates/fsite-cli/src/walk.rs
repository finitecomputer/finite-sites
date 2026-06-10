//! Build a publish manifest from a local artifact directory.
//!
//! The walk is iterative (explicit stack, no recursion) and bounded by the
//! manifest limits. The directory must be a final build artifact: roots
//! containing `.git`, `node_modules`, or `.finite` are rejected outright so
//! an agent cannot accidentally publish a source tree or key material.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use finitesites_proto::limits::{MAX_FILE_BYTES, MAX_MANIFEST_FILES};
use finitesites_proto::manifest::validate_manifest_path;
use finitesites_proto::{ManifestFile, PublishManifest, hex};

use crate::CliError;

const UNSAFE_ROOT_ENTRIES: &[&str] = &[".git", "node_modules", ".finite"];

/// Directories scanned are bounded separately from files so a deep empty
/// tree cannot spin the walk.
const MAX_DIRECTORIES: u32 = 5_000;

pub struct WalkOutcome {
    pub manifest: PublishManifest,
    /// sha256 -> absolute file path, for the upload phase.
    pub sources: HashMap<String, PathBuf>,
    pub skipped_hidden: u32,
}

pub fn build_manifest(root: &Path) -> Result<WalkOutcome, CliError> {
    let root = root
        .canonicalize()
        .map_err(|error| CliError::Io(format!("cannot open {}: {error}", root.display())))?;
    if !root.is_dir() {
        return Err(CliError::Usage(format!(
            "{} is not a directory",
            root.display()
        )));
    }
    reject_unsafe_root(&root)?;

    let mut files: Vec<ManifestFile> = Vec::new();
    let mut sources: HashMap<String, PathBuf> = HashMap::new();
    let mut skipped_hidden: u32 = 0;
    let mut directories_seen: u32 = 0;
    let mut stack: Vec<PathBuf> = vec![root.clone()];

    // Bounded: every iteration pops one directory, and directories_seen is
    // capped below.
    while let Some(directory) = stack.pop() {
        directories_seen += 1;
        if directories_seen > MAX_DIRECTORIES {
            return Err(CliError::Usage(format!(
                "more than {MAX_DIRECTORIES} directories under {}",
                root.display()
            )));
        }
        let entries = std::fs::read_dir(&directory).map_err(|error| {
            CliError::Io(format!("cannot read {}: {error}", directory.display()))
        })?;
        for entry in entries {
            let entry = entry
                .map_err(|error| CliError::Io(format!("cannot read directory entry: {error}")))?;
            let file_name = entry.file_name();
            let name = file_name.to_string_lossy();
            if name.starts_with('.') {
                // Dotfiles are never published: they are where secrets live.
                skipped_hidden += 1;
                continue;
            }
            let path = entry.path();
            let file_type = entry.file_type().map_err(|error| {
                CliError::Io(format!("cannot stat {}: {error}", path.display()))
            })?;
            if file_type.is_symlink() {
                // Symlinks could point outside the artifact; skip loudly.
                eprintln!("skipping symlink {}", path.display());
                continue;
            }
            if file_type.is_dir() {
                stack.push(path);
                continue;
            }
            if files.len() >= MAX_MANIFEST_FILES as usize {
                return Err(CliError::Usage(format!(
                    "more than {MAX_MANIFEST_FILES} files under {}",
                    root.display()
                )));
            }
            let manifest_path = manifest_path_for(&root, &path)?;
            validate_manifest_path(&manifest_path).map_err(|error| {
                CliError::Usage(format!(
                    "{} cannot be published ({error}); rename it and rebuild",
                    path.display()
                ))
            })?;
            let metadata = entry.metadata().map_err(|error| {
                CliError::Io(format!("cannot stat {}: {error}", path.display()))
            })?;
            if metadata.len() > MAX_FILE_BYTES {
                return Err(CliError::Usage(format!(
                    "{} is larger than the {} MiB file limit",
                    path.display(),
                    MAX_FILE_BYTES / (1024 * 1024)
                )));
            }
            let bytes = std::fs::read(&path).map_err(|error| {
                CliError::Io(format!("cannot read {}: {error}", path.display()))
            })?;
            let sha256 = hex::encode(&Sha256::digest(&bytes));
            files.push(ManifestFile {
                path: manifest_path,
                sha256: sha256.clone(),
                size: bytes.len() as u64,
            });
            sources.insert(sha256, path);
        }
    }

    let manifest = PublishManifest { files };
    manifest.validate().map_err(|error| {
        CliError::Usage(format!("artifact directory is not publishable: {error}"))
    })?;
    assert!(manifest.files.len() <= MAX_MANIFEST_FILES as usize);
    Ok(WalkOutcome {
        manifest,
        sources,
        skipped_hidden,
    })
}

fn reject_unsafe_root(root: &Path) -> Result<(), CliError> {
    for entry_name in UNSAFE_ROOT_ENTRIES {
        if root.join(entry_name).exists() {
            return Err(CliError::Usage(format!(
                "{} contains {entry_name}; publish a final build output directory \
                 (like dist/ or build/), not a project root",
                root.display()
            )));
        }
    }
    Ok(())
}

fn manifest_path_for(root: &Path, file: &Path) -> Result<String, CliError> {
    let relative = file
        .strip_prefix(root)
        .map_err(|_| CliError::Io(format!("{} escaped the walk root", file.display())))?;
    let mut out = String::new();
    // Bounded: component count is bounded by path length.
    for component in relative.components() {
        let std::path::Component::Normal(part) = component else {
            return Err(CliError::Io(format!(
                "{} has a non-normal path component",
                file.display()
            )));
        };
        out.push('/');
        out.push_str(&part.to_string_lossy());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, rel: &str, content: &str) {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    #[test]
    fn walks_nested_files_and_skips_dotfiles() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "index.html", "<h1>hi</h1>");
        write(dir.path(), "css/style.css", "body{}");
        write(dir.path(), ".env", "SECRET=1");
        write(dir.path(), ".hidden/inner.txt", "x");

        let outcome = build_manifest(dir.path()).unwrap();
        let mut paths: Vec<&str> = outcome
            .manifest
            .files
            .iter()
            .map(|f| f.path.as_str())
            .collect();
        paths.sort_unstable();
        assert_eq!(paths, vec!["/css/style.css", "/index.html"]);
        assert_eq!(outcome.skipped_hidden, 2);
        assert_eq!(outcome.sources.len(), 2);
    }

    #[test]
    fn rejects_project_roots() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "index.html", "x");
        std::fs::create_dir(dir.path().join("node_modules")).unwrap();
        let result = build_manifest(dir.path());
        assert!(matches!(result, Err(CliError::Usage(_))));
    }

    #[test]
    fn rejects_empty_directories() {
        let dir = tempfile::tempdir().unwrap();
        let result = build_manifest(dir.path());
        assert!(matches!(result, Err(CliError::Usage(_))));
    }

    #[test]
    fn rejects_unpublishable_names() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "has space.html", "x");
        let result = build_manifest(dir.path());
        assert!(matches!(result, Err(CliError::Usage(_))));
    }
}
