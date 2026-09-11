//! Universal provider archive registry.
//!
//! Every provider declares the physical storage it needs; one provider may
//! own several sources (roles), every source snapshots independently, and
//! scanners/loaders receive archived roots rather than hardcoded live roots.
//! Existing parsers are reused unchanged.
//!
//! This module grows per migration checkpoint: file-tree providers first,
//! then SQLite/mixed, then aggregated multi-source providers, then WSL. The
//! indexer hook below is the first plug-in point (used by `storage::index` so
//! migrated providers are derived into SQLite automatically).

use std::path::{Path, PathBuf};

/// A provider's snapshot indexer: fold one completed snapshot into the
/// derived SQLite index. Returns [`IndexSupport`] so the reconciler can tell
/// "indexed" apart from "not yet supported".
pub type SnapshotIndexer = fn(
    &rusqlite::Connection,
    &crate::storage::Source,
    &crate::storage::SnapshotInfo,
) -> Result<crate::storage::index::IndexSupport, String>;

/// Indexer for a migrated provider, if one is registered.
#[must_use]
pub fn indexer_for(provider: &str) -> Option<SnapshotIndexer> {
    let _ = provider;
    // File-tree, SQLite, and aggregated providers register here as their
    // migration checkpoints land.
    None
}

/// Absolute original path → snapshot-space path for providers whose stable
/// IDs embed absolute filesystem locations. Returns `None` when no registered
/// source of `provider` covers `stable_path` or the mapped file is absent.
#[must_use]
pub fn map_stable_path(
    provider: &str,
    stable_path: &Path,
) -> Option<(
    crate::storage::Source,
    crate::storage::SnapshotInfo,
    PathBuf,
)> {
    let root = crate::storage::snapshot::data_root().ok()?;
    let sources_dir = root.join("sources");
    let entries = std::fs::read_dir(&sources_dir).ok()?;
    let mut best: Option<(usize, crate::storage::Source)> = None;
    for entry in entries.flatten() {
        let Ok(bytes) = std::fs::read(entry.path().join("source.json")) else {
            continue;
        };
        let Ok(source) = serde_json::from_slice::<crate::storage::Source>(&bytes) else {
            continue;
        };
        if source.provider != provider {
            continue;
        }
        // Remote and local sources alike: match by recorded original root.
        // (Remote inner paths are handled by map_remote_path_to_snapshot.)
        if let Some(original_root) = source.original_root.as_deref() {
            if stable_path.strip_prefix(original_root).is_ok() {
                let specificity = original_root.len();
                if best.as_ref().map_or(true, |(len, _)| specificity > *len) {
                    best = Some((specificity, source.clone()));
                }
            }
        }
    }
    let (_, source) = best?;
    let snapshot = crate::storage::latest_completed_snapshot(&source.id)?;
    let original_root = source.original_root.clone()?;
    let relative = stable_path.strip_prefix(&original_root).ok()?;
    let mapped = if relative.as_os_str().is_empty() {
        snapshot.data_path.clone()
    } else {
        snapshot.data_path.join(relative)
    };
    if mapped.exists() {
        Some((source, snapshot, mapped))
    } else {
        None
    }
}

/// Rewrite snapshot-interior absolute paths back to stable external IDs in
/// the well-known identifier fields. Only touches strings carrying the exact
/// snapshot prefix, so real user paths and message content are never altered.
pub fn rewrite_snapshot_prefix(
    value: &str,
    snapshot_data_root: &Path,
    original_root: &str,
) -> String {
    let prefix = snapshot_data_root.to_string_lossy();
    if let Some(rest) = value.strip_prefix(prefix.as_ref()) {
        format!("{original_root}{rest}")
    } else {
        value.to_string()
    }
}
