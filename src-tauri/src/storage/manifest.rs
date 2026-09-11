//! Snapshot manifest: what a completed snapshot preserves.
//!
//! Format v2 (current): every entry carries a SHA-256 content hash and
//! presence metadata. Snapshot equality is decided on `(path, sha256,
//! present)` — size and mtime are optimization metadata only, because a file
//! can change bytes while keeping the same size and second-level mtime.
//!
//! Format v1 (f94c008 era): `{path, size, mtime_secs}` without hashes. v1
//! snapshots remain readable; their entries carry an empty hash until a v2
//! sync re-hashes the preserved bytes. v1 manifests are never mutated.

use serde::{Deserialize, Serialize};

/// Current manifest format version.
pub const MANIFEST_VERSION: u32 = 2;
/// Oldest manifest format version still accepted for reads.
pub const MANIFEST_VERSION_MIN: u32 = 1;

fn default_present() -> bool {
    true
}

/// One preserved file, relative to the snapshot `data/` root with `/` separators.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ManifestFileEntry {
    /// Relative path with `/` separators (e.g. `projects/foo/session.jsonl`).
    pub path: String,
    pub size: u64,
    /// Source mtime at copy time (seconds since epoch, 0 when unknown).
    pub mtime_secs: i64,
    /// Hex SHA-256 of the preserved bytes. Empty only for entries inherited
    /// from v1 manifests before their first v2 re-hash.
    #[serde(default)]
    pub sha256: String,
    /// Whether the file was present upstream when this snapshot was taken.
    /// `false` entries are preserved history for files deleted upstream.
    #[serde(default = "default_present")]
    pub present: bool,
    /// Unix seconds when the file was last observed upstream (0 = unknown).
    #[serde(default)]
    pub last_seen_secs: i64,
}

/// Identity of one entry for snapshot-equality purposes.
#[must_use]
pub fn entry_fingerprint(entry: &ManifestFileEntry) -> (String, String, bool) {
    (entry.path.clone(), entry.sha256.clone(), entry.present)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SnapshotManifest {
    pub version: u32,
    pub source_id: String,
    pub snapshot_id: String,
    pub created_at: String,
    pub provider: String,
    pub source_kind: String,
    pub original_root: Option<String>,
    pub endpoint: Option<String>,
    /// Always `"completed"` on disk; staging never has a manifest.
    pub status: String,
    pub files: Vec<ManifestFileEntry>,
    pub file_count: usize,
    pub total_bytes: u64,
    /// Hex SHA-256 over the canonical entry fingerprint list. Lets a remote
    /// aggregator compare snapshot content without downloading anything.
    /// Empty for v1 manifests.
    #[serde(default)]
    pub content_hash: String,
}

impl SnapshotManifest {
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn completed(
        source: &crate::storage::Source,
        snapshot_id: &str,
        created_at: &str,
        mut files: Vec<ManifestFileEntry>,
    ) -> Self {
        files.sort_by(|a, b| a.path.cmp(&b.path));
        let file_count = files.len();
        let total_bytes = files.iter().map(|f| f.size).sum();
        let content_hash = content_hash_for(snapshot_id, &files);
        Self {
            version: MANIFEST_VERSION,
            source_id: source.id.clone(),
            snapshot_id: snapshot_id.to_string(),
            created_at: created_at.to_string(),
            provider: source.provider.clone(),
            source_kind: source.kind.to_string(),
            original_root: source.original_root.clone(),
            endpoint: source.endpoint.clone(),
            status: "completed".to_string(),
            files,
            file_count,
            total_bytes,
            content_hash,
        }
    }
}

/// Canonical content hash: SHA-256 over `snapshot_id` plus every
/// `path + sha256 + present` fingerprint in sorted order.
#[must_use]
pub fn content_hash_for(snapshot_id: &str, files: &[ManifestFileEntry]) -> String {
    let mut canonical = String::with_capacity(snapshot_id.len() + files.len() * 80);
    canonical.push_str(snapshot_id);
    canonical.push('\n');
    let mut fps: Vec<(String, String, bool)> = files.iter().map(entry_fingerprint).collect();
    fps.sort();
    for (path, hash, present) in fps {
        canonical.push_str(&path);
        canonical.push('\n');
        canonical.push_str(&hash);
        canonical.push('\n');
        canonical.push_str(if present { "1" } else { "0" });
        canonical.push('\n');
    }
    crate::storage::hash::sha256_hex_str(&canonical)
}
