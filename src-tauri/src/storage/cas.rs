//! Content-addressed blob reuse across snapshots and sources.
//!
//! `data/cas/<aa>/<64-hex>` holds one immutable copy per observed SHA-256.
//! Candidate assembly hardlinks from the CAS (or the previous snapshot) instead
//! of re-storing or re-downloading bytes. This is a reuse optimization only:
//! snapshots remain self-contained trees and never depend on the CAS for
//! readability.
//!
//! Deliberately minimal: no GC (never delete history), no compression, no
//! cross-machine sharing.

use std::path::{Path, PathBuf};

use crate::storage::hash::{is_plausible_hash, sha256_hex};

/// Path of a blob in the CAS. Returns `None` for malformed hashes.
#[must_use]
pub fn cas_blob_path(data_root: &Path, sha256: &str) -> Option<PathBuf> {
    if !is_plausible_hash(sha256) {
        return None;
    }
    Some(data_root.join("cas").join(&sha256[..2]).join(sha256))
}

/// Whether the CAS already holds these bytes.
#[must_use]
pub fn cas_contains(data_root: &Path, sha256: &str) -> bool {
    cas_blob_path(data_root, sha256).is_some_and(|p| p.is_file())
}

/// Store `bytes` in the CAS (no-op when present). Returns the content hash.
pub fn cas_insert_bytes(data_root: &Path, bytes: &[u8]) -> Result<String, String> {
    let hash = sha256_hex(bytes);
    let Some(dest) = cas_blob_path(data_root, &hash) else {
        return Err("Refusing to address malformed content hash".to_string());
    };
    if dest.is_file() {
        return Ok(hash);
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create CAS directory {}: {e}", parent.display()))?;
    }
    // Write-then-rename so a crash never leaves a partial blob behind. A
    // concurrent writer winning the race is fine: same hash means same bytes.
    let tmp = dest.with_extension(format!("tmp-{}", &uuid::Uuid::new_v4().to_string()[..8]));
    std::fs::write(&tmp, bytes).map_err(|e| format!("Failed to write CAS staging blob: {e}"))?;
    match std::fs::rename(&tmp, &dest) {
        Ok(()) => Ok(hash),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            if dest.is_file() {
                Ok(hash)
            } else {
                Err(format!("Failed to publish CAS blob: {e}"))
            }
        }
    }
}

/// Hardlink a CAS blob onto `dest`. Returns `false` (not an error) when the
/// blob is absent or the filesystem refuses, letting the caller fall back to
/// a plain copy.
#[must_use]
pub fn cas_link_into(data_root: &Path, sha256: &str, dest: &Path) -> bool {
    let Some(blob) = cas_blob_path(data_root, sha256) else {
        return false;
    };
    if !blob.is_file() {
        return false;
    }
    if let Some(parent) = dest.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return false;
        }
    }
    std::fs::hard_link(&blob, dest).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[serial_test::serial]
    fn cas_roundtrip_and_reuse() {
        let _sandbox = crate::test_utils::SandboxHome::new();
        let data_root = tempfile::tempdir().unwrap().path().join("data");
        let hash = cas_insert_bytes(&data_root, b"hello").unwrap();
        assert!(cas_contains(&data_root, &hash));
        // Idempotent re-insert.
        assert_eq!(cas_insert_bytes(&data_root, b"hello").unwrap(), hash);
        let dest = data_root.join("out").join("f.bin");
        assert!(cas_link_into(&data_root, &hash, &dest));
        assert_eq!(std::fs::read(&dest).unwrap(), b"hello");
        // Malformed hashes never resolve.
        assert!(!cas_contains(&data_root, "nope"));
        assert!(cas_blob_path(&data_root, "../escape").is_none());
    }
}
