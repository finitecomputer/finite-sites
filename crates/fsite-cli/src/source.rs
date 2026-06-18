//! Source snapshot packaging and extraction for editor handoff.
//!
//! A source snapshot is not a deploy artifact. It excludes local keys,
//! secrets, VCS metadata, dependencies, and build output so editors get the
//! project source without uploading the usual heavy or dangerous directories.

use std::path::{Path, PathBuf};

use finitesites_proto::limits::{
    MAX_SOURCE_SNAPSHOT_BYTES, MAX_SOURCE_SNAPSHOT_DIRECTORIES, MAX_SOURCE_SNAPSHOT_FILES,
};

use crate::CliError;

const EXCLUDED_DIRS: &[&str] = &[
    ".git",
    ".finite",
    "node_modules",
    "target",
    "dist",
    "build",
    ".next",
    ".turbo",
    ".cache",
];

pub fn build_source_snapshot(root: &Path) -> Result<Vec<u8>, CliError> {
    let root = root
        .canonicalize()
        .map_err(|error| CliError::Io(format!("cannot open {}: {error}", root.display())))?;
    if !root.is_dir() {
        return Err(CliError::Usage(format!(
            "{} is not a directory",
            root.display()
        )));
    }

    let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let mut builder = tar::Builder::new(encoder);
    builder.follow_symlinks(false);

    let mut stack: Vec<PathBuf> = vec![root.clone()];
    let mut directories_seen: u32 = 0;
    let mut files_seen: u32 = 0;

    // Bounded by MAX_SOURCE_SNAPSHOT_DIRECTORIES.
    while let Some(directory) = stack.pop() {
        directories_seen += 1;
        if directories_seen > MAX_SOURCE_SNAPSHOT_DIRECTORIES {
            return Err(CliError::Usage(format!(
                "more than {MAX_SOURCE_SNAPSHOT_DIRECTORIES} directories under {}",
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
            let path = entry.path();
            let file_type = entry.file_type().map_err(|error| {
                CliError::Io(format!("cannot stat {}: {error}", path.display()))
            })?;
            if file_type.is_symlink() {
                eprintln!("skipping symlink {}", path.display());
                continue;
            }
            if file_type.is_dir() {
                if should_skip_dir(&name) {
                    continue;
                }
                stack.push(path);
                continue;
            }
            if should_skip_file(&name) {
                continue;
            }
            files_seen += 1;
            if files_seen > MAX_SOURCE_SNAPSHOT_FILES {
                return Err(CliError::Usage(format!(
                    "more than {MAX_SOURCE_SNAPSHOT_FILES} source files under {}",
                    root.display()
                )));
            }
            let relative = relative_archive_path(&root, &path)?;
            builder
                .append_path_with_name(&path, &relative)
                .map_err(|error| {
                    CliError::Io(format!("cannot archive {}: {error}", path.display()))
                })?;
        }
    }
    if files_seen == 0 {
        return Err(CliError::Usage("source directory has no files".to_string()));
    }

    let bytes = builder
        .into_inner()
        .and_then(|encoder| encoder.finish())
        .map_err(|error| CliError::Io(format!("cannot finish source snapshot: {error}")))?;
    if bytes.len() as u64 > MAX_SOURCE_SNAPSHOT_BYTES {
        return Err(CliError::Usage(format!(
            "source snapshot is {} MiB, over the {} MiB limit",
            bytes.len() / (1024 * 1024),
            MAX_SOURCE_SNAPSHOT_BYTES / (1024 * 1024)
        )));
    }
    assert!(!bytes.is_empty());
    Ok(bytes)
}

pub fn extract_source_snapshot(bytes: &[u8], target: &Path) -> Result<(), CliError> {
    if target.exists() {
        let mut entries = std::fs::read_dir(target)
            .map_err(|error| CliError::Io(format!("cannot read {}: {error}", target.display())))?;
        if entries.next().is_some() {
            return Err(CliError::Usage(format!(
                "{} exists and is not empty",
                target.display()
            )));
        }
    } else {
        std::fs::create_dir_all(target).map_err(|error| {
            CliError::Io(format!("cannot create {}: {error}", target.display()))
        })?;
    }
    let decoder = flate2::read::GzDecoder::new(bytes);
    let mut archive = tar::Archive::new(decoder);
    archive
        .unpack(target)
        .map_err(|error| CliError::Io(format!("cannot extract source snapshot: {error}")))?;
    Ok(())
}

fn should_skip_dir(name: &str) -> bool {
    EXCLUDED_DIRS.contains(&name) || name.starts_with(".env")
}

fn should_skip_file(name: &str) -> bool {
    name.starts_with(".env")
}

fn relative_archive_path(root: &Path, file: &Path) -> Result<PathBuf, CliError> {
    let relative = file
        .strip_prefix(root)
        .map_err(|_| CliError::Io(format!("{} escaped the source root", file.display())))?;
    let mut out = PathBuf::new();
    // Bounded by path length.
    for component in relative.components() {
        let std::path::Component::Normal(part) = component else {
            return Err(CliError::Io(format!(
                "{} has a non-normal path component",
                file.display()
            )));
        };
        out.push(part);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read as _;

    fn write(dir: &Path, rel: &str, content: &str) {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    fn paths(bytes: &[u8]) -> Vec<String> {
        let decoder = flate2::read::GzDecoder::new(bytes);
        let mut archive = tar::Archive::new(decoder);
        let mut out: Vec<String> = archive
            .entries()
            .unwrap()
            .map(|entry| {
                let mut entry = entry.unwrap();
                let path = entry.path().unwrap().display().to_string();
                let mut sink = String::new();
                let _ = entry.read_to_string(&mut sink);
                path
            })
            .collect();
        out.sort();
        out
    }

    #[test]
    fn source_snapshot_excludes_generated_and_secret_paths() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "package.json", "{}");
        write(dir.path(), "src/main.ts", "x");
        write(dir.path(), ".env", "SECRET=1");
        write(dir.path(), ".finite/sites/x.env", "SECRET=1");
        write(dir.path(), ".git/HEAD", "ref");
        write(dir.path(), "node_modules/dep/index.js", "x");
        write(dir.path(), "dist/index.html", "x");

        let bytes = build_source_snapshot(dir.path()).unwrap();
        let paths = paths(&bytes);
        assert_eq!(paths, vec!["package.json", "src/main.ts"]);
    }

    #[test]
    fn extract_requires_empty_target() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "src/main.ts", "x");
        let bytes = build_source_snapshot(dir.path()).unwrap();
        let target = tempfile::tempdir().unwrap();
        write(target.path(), "existing.txt", "x");
        assert!(matches!(
            extract_source_snapshot(&bytes, target.path()),
            Err(CliError::Usage(_))
        ));
    }
}
