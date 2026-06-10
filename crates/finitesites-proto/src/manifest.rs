//! Publish manifests: the list of `(path, sha256, size)` entries that fully
//! describes one immutable site version.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::limits::{MAX_FILE_BYTES, MAX_MANIFEST_FILES, MAX_PATH_BYTES, MAX_SITE_BYTES};
use crate::{ProtoError, hex};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ManifestFile {
    /// Absolute serving path, e.g. `/index.html`. Stored exactly as served.
    pub path: String,
    /// Lowercase hex sha256 of the file bytes.
    pub sha256: String,
    /// File size in bytes; re-checked against the uploaded blob.
    pub size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PublishManifest {
    pub files: Vec<ManifestFile>,
}

impl PublishManifest {
    /// Validate every entry and the manifest-wide limits, with the static
    /// per-file ceiling.
    pub fn validate(&self) -> Result<(), ProtoError> {
        self.validate_with_max_file(MAX_FILE_BYTES)
    }

    /// Validate with an explicit per-file ceiling. App-bundle manifests use
    /// MAX_APP_BUNDLE_BYTES (one tar.gz far larger than any static asset);
    /// everything else uses MAX_FILE_BYTES via `validate`.
    pub fn validate_with_max_file(&self, max_file_bytes: u64) -> Result<(), ProtoError> {
        assert!(max_file_bytes >= MAX_FILE_BYTES);
        if self.files.is_empty() {
            return Err(ProtoError::InvalidManifest("manifest has no files"));
        }
        if self.files.len() > MAX_MANIFEST_FILES as usize {
            return Err(ProtoError::InvalidManifest("too many files"));
        }

        let mut total_bytes: u64 = 0;
        // Bounded by MAX_MANIFEST_FILES, checked above.
        for file in &self.files {
            validate_manifest_path(&file.path)?;
            if !hex::is_hex32(&file.sha256) {
                return Err(ProtoError::InvalidManifest(
                    "sha256 must be 64 lowercase hex chars",
                ));
            }
            if file.size > max_file_bytes {
                return Err(ProtoError::InvalidManifest("file exceeds max size"));
            }
            total_bytes = total_bytes.saturating_add(file.size);
        }
        if total_bytes > MAX_SITE_BYTES {
            return Err(ProtoError::InvalidManifest("site exceeds max total size"));
        }

        // Duplicate paths would make serving ambiguous.
        let mut paths: Vec<&str> = self.files.iter().map(|f| f.path.as_str()).collect();
        paths.sort_unstable();
        let had_duplicates = paths.windows(2).any(|pair| pair[0] == pair[1]);
        if had_duplicates {
            return Err(ProtoError::InvalidManifest("duplicate path"));
        }
        Ok(())
    }

    pub fn total_bytes(&self) -> u64 {
        self.files
            .iter()
            .map(|f| f.size)
            .fold(0, u64::saturating_add)
    }

    /// Stable digest of the manifest, recorded on the version row so a
    /// version's content list is tamper-evident.
    pub fn digest(&self) -> String {
        let mut entries: Vec<String> = self
            .files
            .iter()
            .map(|f| format!("{}\n{}\n{}\n", f.path, f.sha256, f.size))
            .collect();
        entries.sort_unstable();
        let mut hasher = Sha256::new();
        for entry in &entries {
            hasher.update(entry.as_bytes());
        }
        hex::encode(&hasher.finalize())
    }
}

/// Manifest paths are absolute, slash-separated, with conservative segment
/// rules. The same rules apply to incoming request paths before lookup, so
/// path traversal cannot name anything a manifest could not contain.
pub fn validate_manifest_path(path: &str) -> Result<(), ProtoError> {
    if path.is_empty() || path.len() > MAX_PATH_BYTES as usize {
        return Err(ProtoError::InvalidManifest("path empty or too long"));
    }
    if !path.starts_with('/') {
        return Err(ProtoError::InvalidManifest("path must start with /"));
    }
    if path.ends_with('/') {
        return Err(ProtoError::InvalidManifest("path must not end with /"));
    }
    let chars_are_safe = path.bytes().all(|b| {
        b.is_ascii_alphanumeric() || matches!(b, b'/' | b'.' | b'-' | b'_' | b'~' | b'@' | b'+')
    });
    if !chars_are_safe {
        return Err(ProtoError::InvalidManifest(
            "path contains unsafe character",
        ));
    }
    // Bounded: segment count is bounded by path length, checked above.
    for segment in path[1..].split('/') {
        if segment.is_empty() {
            return Err(ProtoError::InvalidManifest("path contains empty segment"));
        }
        if segment == "." || segment == ".." {
            return Err(ProtoError::InvalidManifest("path contains dot segment"));
        }
    }
    Ok(())
}

/// The single manifest path used by app-bundle publishes (tier 2).
pub const APP_BUNDLE_PATH: &str = "/app.tar.gz";

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, fill: &str, size: u64) -> ManifestFile {
        ManifestFile {
            path: path.into(),
            sha256: fill.repeat(64),
            size,
        }
    }

    #[test]
    fn valid_manifest_passes() {
        let manifest = PublishManifest {
            files: vec![
                file("/index.html", "a", 10),
                file("/css/style.css", "b", 20),
            ],
        };
        assert_eq!(manifest.validate(), Ok(()));
        assert_eq!(manifest.total_bytes(), 30);
    }

    #[test]
    fn digest_is_order_independent_and_content_sensitive() {
        let forward = PublishManifest {
            files: vec![file("/a.html", "a", 1), file("/b.html", "b", 2)],
        };
        let reversed = PublishManifest {
            files: vec![file("/b.html", "b", 2), file("/a.html", "a", 1)],
        };
        let changed = PublishManifest {
            files: vec![file("/a.html", "a", 1), file("/b.html", "b", 3)],
        };
        assert_eq!(forward.digest(), reversed.digest());
        assert_ne!(forward.digest(), changed.digest());
    }

    #[test]
    fn rejects_empty_too_many_and_duplicates() {
        let empty = PublishManifest { files: vec![] };
        assert!(empty.validate().is_err());

        let duplicate = PublishManifest {
            files: vec![file("/a.html", "a", 1), file("/a.html", "b", 2)],
        };
        assert_eq!(
            duplicate.validate(),
            Err(ProtoError::InvalidManifest("duplicate path"))
        );

        let too_many = PublishManifest {
            files: (0..=MAX_MANIFEST_FILES)
                .map(|i| file(&format!("/f{i}.txt"), "a", 1))
                .collect(),
        };
        assert_eq!(
            too_many.validate(),
            Err(ProtoError::InvalidManifest("too many files"))
        );
    }

    #[test]
    fn rejects_oversize_files_and_sites() {
        let big_file = PublishManifest {
            files: vec![file("/big.bin", "a", MAX_FILE_BYTES + 1)],
        };
        assert!(big_file.validate().is_err());

        let big_site = PublishManifest {
            files: (0..21)
                .map(|i| file(&format!("/f{i}.bin"), "a", MAX_FILE_BYTES))
                .collect(),
        };
        assert_eq!(
            big_site.validate(),
            Err(ProtoError::InvalidManifest("site exceeds max total size"))
        );
    }

    #[test]
    fn app_bundles_may_exceed_the_static_file_ceiling() {
        use crate::limits::MAX_APP_BUNDLE_BYTES;
        let bundle = PublishManifest {
            files: vec![file(APP_BUNDLE_PATH, "a", MAX_FILE_BYTES + 1)],
        };
        // Rejected under the static ceiling, accepted under the bundle one.
        assert!(bundle.validate().is_err());
        assert_eq!(bundle.validate_with_max_file(MAX_APP_BUNDLE_BYTES), Ok(()));
        let too_big = PublishManifest {
            files: vec![file(APP_BUNDLE_PATH, "a", MAX_APP_BUNDLE_BYTES + 1)],
        };
        assert!(
            too_big
                .validate_with_max_file(MAX_APP_BUNDLE_BYTES)
                .is_err()
        );
    }

    #[test]
    fn path_rules() {
        for good in ["/index.html", "/a/b/c.txt", "/.well-known/nostr.json"] {
            assert_eq!(validate_manifest_path(good), Ok(()), "{good}");
        }
        for bad in [
            "",
            "index.html",
            "/dir/",
            "//double",
            "/has space.html",
            "/back\\slash",
            "/../escape",
            "/a/../b",
            "/a/./b",
        ] {
            assert!(validate_manifest_path(bad).is_err(), "{bad}");
        }
    }
}
