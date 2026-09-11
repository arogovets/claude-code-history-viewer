//! Snapshot manifest: what a completed snapshot preserves.

use serde::{Deserialize, Serialize};

pub const MANIFEST_VERSION: u32 = 1;

/// One preserved file, relative to the snapshot `data/` root with `/` separators.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ManifestFileEntry {
    /// Relative path with `/` separators (e.g. `projects/foo/session.jsonl`).
    pub path: String,
    pub size: u64,
    /// Source mtime at copy time (seconds since epoch, 0 when unknown).
    pub mtime_secs: i64,
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
        }
    }
}
