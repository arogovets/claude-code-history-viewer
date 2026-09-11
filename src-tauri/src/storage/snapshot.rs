//! Snapshot paths, discovery, and the mutable `current.json` pointer.
//!
//! Completed snapshot directories are immutable. The only mutable file per
//! source is `current.json`, which itself holds no historical data.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// A completed, verified snapshot.
#[derive(Debug, Clone)]
pub struct SnapshotInfo {
    pub source_id: String,
    pub snapshot_id: String,
    /// Snapshot root (`.../snapshots/<id>/`).
    pub path: PathBuf,
    /// Preserved bytes (`.../snapshots/<id>/data/`).
    pub data_path: PathBuf,
    pub manifest: crate::storage::SnapshotManifest,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CurrentPointer {
    snapshot_id: String,
    completed_at: String,
}

pub(crate) fn data_root() -> Result<PathBuf, String> {
    let home =
        crate::utils::home_dir().ok_or_else(|| "Could not determine home directory".to_string())?;
    Ok(home.join(".claude-history-viewer").join("data"))
}

pub(crate) fn source_dir(data_root: &Path, source_id: &str) -> PathBuf {
    data_root.join("sources").join(source_id)
}

pub(crate) fn snapshots_dir(data_root: &Path, source_id: &str) -> PathBuf {
    source_dir(data_root, source_id).join("snapshots")
}

pub(crate) fn staging_root(data_root: &Path, source_id: &str) -> PathBuf {
    source_dir(data_root, source_id).join(".staging")
}

pub(crate) fn snapshot_path(data_root: &Path, source_id: &str, snapshot_id: &str) -> PathBuf {
    snapshots_dir(data_root, source_id).join(snapshot_id)
}

pub(crate) fn current_pointer_path(data_root: &Path, source_id: &str) -> PathBuf {
    source_dir(data_root, source_id).join("current.json")
}

pub(crate) fn manifest_path(snapshot_root: &Path) -> PathBuf {
    snapshot_root.join("manifest.json")
}

pub(crate) fn data_path(snapshot_root: &Path) -> PathBuf {
    snapshot_root.join("data")
}

fn read_manifest(snapshot_root: &Path) -> Option<crate::storage::SnapshotManifest> {
    let bytes = std::fs::read(manifest_path(snapshot_root)).ok()?;
    let manifest: crate::storage::SnapshotManifest = serde_json::from_slice(&bytes).ok()?;
    if manifest.status != "completed"
        || manifest.version != crate::storage::manifest::MANIFEST_VERSION
    {
        return None;
    }
    if !data_path(snapshot_root).is_dir() {
        return None;
    }
    Some(manifest)
}

/// Allocate a unique, time-sortable snapshot id.
pub(crate) fn allocate_snapshot_id() -> String {
    let ts = chrono::Utc::now().format("%Y%m%dT%H%M%S%.3fZ").to_string();
    let uniq = uuid::Uuid::new_v4().to_string();
    format!("{ts}-{}", &uniq[..8])
}

/// All completed snapshots for a source, newest first.
#[must_use]
pub fn list_snapshots(source_id: &str) -> Vec<SnapshotInfo> {
    let Ok(root) = data_root() else {
        return Vec::new();
    };
    list_snapshots_in_root(&root, source_id)
}

pub(crate) fn list_snapshots_in_root(data_root: &Path, source_id: &str) -> Vec<SnapshotInfo> {
    let dir = snapshots_dir(data_root, source_id);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(manifest) = read_manifest(&path) else {
            continue;
        };
        // Manifest must belong to this snapshot directory.
        if manifest.source_id != source_id {
            continue;
        }
        let Some(snapshot_id) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if manifest.snapshot_id != snapshot_id {
            continue;
        }
        out.push(SnapshotInfo {
            source_id: source_id.to_string(),
            snapshot_id: snapshot_id.to_string(),
            data_path: data_path(&path),
            path: path.clone(),
            manifest,
        });
    }
    out.sort_by(|a, b| b.snapshot_id.cmp(&a.snapshot_id));
    out
}

/// Newest completed snapshot, via `current.json` when valid, otherwise the
/// newest valid snapshot directory (crash-recovery fallback).
#[must_use]
pub fn latest_completed_snapshot(source_id: &str) -> Option<SnapshotInfo> {
    let root = data_root().ok()?;
    latest_completed_snapshot_in_root(&root, source_id)
}

pub(crate) fn latest_completed_snapshot_in_root(
    data_root: &Path,
    source_id: &str,
) -> Option<SnapshotInfo> {
    let snapshots = list_snapshots_in_root(data_root, source_id);
    if snapshots.is_empty() {
        return None;
    }
    let pointer_path = current_pointer_path(data_root, source_id);
    if let Ok(bytes) = std::fs::read(&pointer_path) {
        if let Ok(pointer) = serde_json::from_slice::<CurrentPointer>(&bytes) {
            if let Some(hit) = snapshots
                .iter()
                .find(|s| s.snapshot_id == pointer.snapshot_id)
            {
                return Some(hit.clone());
            }
        }
    }
    snapshots.into_iter().next()
}

pub(crate) fn write_current_pointer(
    data_root: &Path,
    source_id: &str,
    snapshot_id: &str,
) -> Result<(), String> {
    let dir = source_dir(data_root, source_id);
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("Failed to create source dir {}: {e}", dir.display()))?;
    let pointer = CurrentPointer {
        snapshot_id: snapshot_id.to_string(),
        completed_at: chrono::Utc::now().to_rfc3339(),
    };
    let bytes = serde_json::to_vec_pretty(&pointer)
        .map_err(|e| format!("Failed to encode pointer: {e}"))?;
    let path = current_pointer_path(data_root, source_id);
    let tmp = path.with_extension(format!("json.{}.tmp", uuid::Uuid::new_v4()));
    std::fs::write(&tmp, &bytes)
        .map_err(|e| format!("Failed to write current pointer staging file: {e}"))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("Failed to publish current pointer: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[serial_test::serial]
    fn latest_falls_back_when_pointer_missing() {
        let _sandbox = crate::test_utils::SandboxHome::new();
        let root = data_root().unwrap();
        let source_id = "fallback-probe";
        // No snapshots at all.
        assert!(latest_completed_snapshot_in_root(&root, source_id).is_none());
    }
}
