//! Build an app bundle (tar.gz) from a local directory for tier-2 publish.
//!
//! Unlike the static walk, app bundles include dotfiles and node_modules:
//! a Next.js standalone output *is* a node_modules tree, and apps need
//! their config files. The only exclusions are the things that must never
//! leave a workspace: `.git` and `.finite` (key material).

use std::path::Path;

use finitesites_proto::limits::MAX_APP_BUNDLE_BYTES;

use crate::CliError;

const EXCLUDED_ROOT_ENTRIES: &[&str] = &[".git", ".finite"];

/// Tar+gzip the directory. Returns the compressed bytes.
pub fn build_bundle(root: &Path) -> Result<Vec<u8>, CliError> {
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

    let entries = std::fs::read_dir(&root)
        .map_err(|error| CliError::Io(format!("cannot read {}: {error}", root.display())))?;
    let mut included: u32 = 0;
    // Bounded: top-level entries of one directory; recursion below is
    // delegated to tar's bounded append_dir_all.
    for entry in entries {
        let entry =
            entry.map_err(|error| CliError::Io(format!("cannot read directory entry: {error}")))?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy().to_string();
        if EXCLUDED_ROOT_ENTRIES.contains(&name_str.as_str()) {
            continue;
        }
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|error| CliError::Io(format!("cannot stat {}: {error}", path.display())))?;
        let result = if file_type.is_dir() {
            builder.append_dir_all(&name_str, &path)
        } else {
            builder.append_path_with_name(&path, &name_str)
        };
        result
            .map_err(|error| CliError::Io(format!("cannot bundle {}: {error}", path.display())))?;
        included += 1;
    }
    if included == 0 {
        return Err(CliError::Usage("app directory is empty".to_string()));
    }

    let bytes = builder
        .into_inner()
        .and_then(|encoder| encoder.finish())
        .map_err(|error| CliError::Io(format!("cannot finish bundle: {error}")))?;
    if bytes.len() as u64 > MAX_APP_BUNDLE_BYTES {
        return Err(CliError::Usage(format!(
            "app bundle is {} MiB, over the {} MiB limit",
            bytes.len() / (1024 * 1024),
            MAX_APP_BUNDLE_BYTES / (1024 * 1024)
        )));
    }
    assert!(!bytes.is_empty());
    Ok(bytes)
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

    fn bundle_paths(bytes: &[u8]) -> Vec<String> {
        let decoder = flate2::read::GzDecoder::new(bytes);
        let mut archive = tar::Archive::new(decoder);
        let mut paths: Vec<String> = archive
            .entries()
            .unwrap()
            .map(|e| {
                let mut entry = e.unwrap();
                let path = entry.path().unwrap().display().to_string();
                let mut sink = String::new();
                let _ = entry.read_to_string(&mut sink);
                path
            })
            .collect();
        paths.sort();
        paths
    }

    #[test]
    fn bundles_everything_except_git_and_finite() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "server.js", "x");
        write(dir.path(), ".env.example", "x");
        write(dir.path(), "node_modules/dep/index.js", "x");
        write(dir.path(), ".git/HEAD", "ref");
        write(dir.path(), ".finite/sites/x.env", "SECRET");

        let bytes = build_bundle(dir.path()).unwrap();
        let paths = bundle_paths(&bytes);
        assert!(paths.contains(&"server.js".to_string()));
        assert!(paths.contains(&".env.example".to_string()));
        assert!(paths.iter().any(|p| p.starts_with("node_modules/")));
        assert!(!paths.iter().any(|p| p.starts_with(".git")));
        assert!(!paths.iter().any(|p| p.starts_with(".finite")));
    }

    #[test]
    fn rejects_empty_directories() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(build_bundle(dir.path()), Err(CliError::Usage(_))));
    }
}
