//! Content-addressed blob storage on the local filesystem.
//!
//! Blobs are immutable files keyed by the sha256 of their bytes, laid out as
//! `root/ab/cd/abcd…` to keep directories small. Writes go through a temp
//! file + rename so a crash never leaves a partial blob at a final path.
//!
//! This is the storage seam: a Garage/S3 implementation replaces this crate
//! behind the same four operations when the platform moves to object storage
//! (see docs/adr/0007-content-addressed-blob-store.md).

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use thiserror::Error;

use finitesites_proto::hex;
use finitesites_proto::limits::MAX_FILE_BYTES;

#[derive(Debug, Error)]
pub enum BlobError {
    #[error("blob too large: {size} bytes")]
    TooLarge { size: u64 },
    #[error("blob hash mismatch: expected {expected}, computed {computed}")]
    HashMismatch { expected: String, computed: String },
    #[error("blob not found: {sha256}")]
    NotFound { sha256: String },
    #[error("blob storage corrupt: {0}")]
    Corrupt(&'static str),
    #[error("blob io error: {0}")]
    Io(#[from] std::io::Error),
}

pub struct BlobStore {
    root: PathBuf,
}

impl BlobStore {
    pub fn open(root: &Path) -> Result<BlobStore, BlobError> {
        fs::create_dir_all(root)?;
        assert!(root.is_dir());
        Ok(BlobStore {
            root: root.to_path_buf(),
        })
    }

    /// Store bytes, verifying they hash to `expected_sha256`. Idempotent:
    /// storing an existing blob succeeds without rewriting it. `max_bytes`
    /// is the caller's ceiling: MAX_FILE_BYTES for static assets,
    /// MAX_APP_BUNDLE_BYTES for app bundles.
    pub fn put(
        &self,
        expected_sha256: &str,
        bytes: &[u8],
        max_bytes: u64,
    ) -> Result<(), BlobError> {
        assert!(hex::is_hex32(expected_sha256));
        assert!(max_bytes >= MAX_FILE_BYTES);
        if bytes.len() as u64 > max_bytes {
            return Err(BlobError::TooLarge {
                size: bytes.len() as u64,
            });
        }
        let computed = hex::encode(&Sha256::digest(bytes));
        if computed != expected_sha256 {
            return Err(BlobError::HashMismatch {
                expected: expected_sha256.to_string(),
                computed,
            });
        }

        let final_path = self.blob_path(expected_sha256);
        if final_path.exists() {
            // Content-addressed: an existing file with this name already has
            // these exact bytes, so the write is a no-op replay.
            return Ok(());
        }
        let parent = final_path
            .parent()
            .ok_or(BlobError::Corrupt("blob path has no parent"))?;
        fs::create_dir_all(parent)?;
        let mut temp = tempfile::NamedTempFile::new_in(parent)?;
        temp.write_all(bytes)?;
        temp.flush()?;
        temp.persist(&final_path)
            .map_err(|e| BlobError::Io(e.error))?;

        // Paired assertion: verify the committed file before trusting it.
        let written = fs::metadata(&final_path)?;
        if written.len() != bytes.len() as u64 {
            return Err(BlobError::Corrupt("committed blob has wrong size"));
        }
        Ok(())
    }

    pub fn has(&self, sha256: &str) -> bool {
        assert!(hex::is_hex32(sha256));
        self.blob_path(sha256).is_file()
    }

    /// Read a whole blob. Blob sizes are bounded by MAX_FILE_BYTES at write
    /// time, so a full read is a bounded allocation.
    pub fn get(&self, sha256: &str) -> Result<Vec<u8>, BlobError> {
        assert!(hex::is_hex32(sha256));
        let path = self.blob_path(sha256);
        if !path.is_file() {
            return Err(BlobError::NotFound {
                sha256: sha256.to_string(),
            });
        }
        let bytes = fs::read(&path)?;
        let computed = hex::encode(&Sha256::digest(&bytes));
        if computed != sha256 {
            // Disk corruption or tampering: do not serve wrong bytes.
            return Err(BlobError::Corrupt("stored blob fails hash check"));
        }
        Ok(bytes)
    }

    /// Filesystem location of a stored blob, for callers that stream large
    /// blobs (app bundle extraction) instead of loading them.
    pub fn file_path(&self, sha256: &str) -> PathBuf {
        self.blob_path(sha256)
    }

    fn blob_path(&self, sha256: &str) -> PathBuf {
        assert!(hex::is_hex32(sha256));
        self.root
            .join(&sha256[0..2])
            .join(&sha256[2..4])
            .join(sha256)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest_of(bytes: &[u8]) -> String {
        hex::encode(&Sha256::digest(bytes))
    }

    #[test]
    fn put_get_roundtrip_and_replay() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::open(dir.path()).unwrap();
        let bytes = b"hello finite sites";
        let sha = digest_of(bytes);

        store.put(&sha, bytes, MAX_FILE_BYTES).unwrap();
        assert!(store.has(&sha));
        assert_eq!(store.get(&sha).unwrap(), bytes);
        // Replay is fine.
        store.put(&sha, bytes, MAX_FILE_BYTES).unwrap();
        assert_eq!(store.get(&sha).unwrap(), bytes);
    }

    #[test]
    fn put_rejects_wrong_hash() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::open(dir.path()).unwrap();
        let wrong = digest_of(b"other bytes");
        let result = store.put(&wrong, b"hello", MAX_FILE_BYTES);
        assert!(matches!(result, Err(BlobError::HashMismatch { .. })));
        assert!(!store.has(&wrong));
    }

    #[test]
    fn get_missing_is_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::open(dir.path()).unwrap();
        let sha = digest_of(b"never stored");
        assert!(matches!(store.get(&sha), Err(BlobError::NotFound { .. })));
    }

    #[test]
    fn corrupted_blob_is_detected_on_read() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::open(dir.path()).unwrap();
        let bytes = b"original";
        let sha = digest_of(bytes);
        store.put(&sha, bytes, MAX_FILE_BYTES).unwrap();

        let path = dir.path().join(&sha[0..2]).join(&sha[2..4]).join(&sha);
        fs::write(&path, b"tampered").unwrap();
        assert!(matches!(store.get(&sha), Err(BlobError::Corrupt(_))));
    }

    #[test]
    fn survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = b"persisted";
        let sha = digest_of(bytes);
        {
            let store = BlobStore::open(dir.path()).unwrap();
            store.put(&sha, bytes, MAX_FILE_BYTES).unwrap();
        }
        let reopened = BlobStore::open(dir.path()).unwrap();
        assert_eq!(reopened.get(&sha).unwrap(), bytes);
    }
}
