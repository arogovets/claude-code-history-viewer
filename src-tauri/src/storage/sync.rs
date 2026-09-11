//! Directory sync into immutable snapshots.
//!
//! Crash-safe ordering:
//! ```text
//! 1. allocate unique snapshot id
//! 2. create staging directory
//! 3. copy/sync source files into staging
//! 4. generate manifest
//! 5. verify copied data enough to consider snapshot usable
//! 6. atomically publish/finalize the snapshot
//! 7. only after filesystem snapshot succeeds, parse/index it into SQLite
//! ```
//!
//! Staging lives at `sources/<id>/.staging/<snapshot-id>/` and is renamed to
//! `sources/<id>/snapshots/<snapshot-id>/`. Failed staging directories are
//! deliberately left behind for inspection, never promoted.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use once_cell::sync::Lazy;

use crate::storage::snapshot;
use crate::storage::{ManifestFileEntry, SnapshotManifest, Source};

/// Outcome of a sync attempt.
#[derive(Debug, Clone)]
pub enum SyncOutcome {
    /// A new immutable snapshot was published.
    Created(crate::storage::SnapshotInfo),
    /// Source unchanged since the latest snapshot; no new snapshot needed.
    Unchanged(crate::storage::SnapshotInfo),
}

/// Per-source status for UI/logging.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncStatus {
    pub source_id: String,
    pub provider: String,
    pub latest_snapshot_id: Option<String>,
    pub snapshot_count: usize,
    pub reachable: bool,
    pub last_error: Option<String>,
}

static SOURCE_LOCKS: Lazy<Mutex<HashMap<String, Arc<Mutex<()>>>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

fn lock_for(source_id: &str) -> Arc<Mutex<()>> {
    let mut map = SOURCE_LOCKS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    map.entry(source_id.to_string())
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

/// Run `f` under the per-source lock so concurrent UI refreshes and background
/// work cannot race on the same source.
pub fn with_source_lock<T>(source_id: &str, f: impl FnOnce() -> T) -> T {
    let lock = lock_for(source_id);
    let _guard = lock
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    f()
}

/// Persist a human-readable `source.json` next to snapshots (informational;
/// the id itself remains deterministic so this file is never required).
pub fn ensure_source_registered(source: &Source) -> Result<PathBuf, String> {
    let root = snapshot::data_root()?;
    let dir = source.storage_dir(&root);
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("Failed to create source dir {}: {e}", dir.display()))?;
    let path = dir.join("source.json");
    let bytes =
        serde_json::to_vec_pretty(source).map_err(|e| format!("Failed to encode source: {e}"))?;
    // Best effort: a partially written source.json must never block syncing.
    // Write atomically via temp + rename.
    let tmp = dir.join(format!(
        ".source.{}.tmp",
        &uuid::Uuid::new_v4().to_string()[..8]
    ));
    if std::fs::write(&tmp, &bytes).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
    Ok(dir)
}

/// Status snapshot for one source (no syncing).
#[must_use]
pub fn sync_status(source: &Source) -> SyncStatus {
    let snapshots = crate::storage::list_snapshots(&source.id);
    let latest = snapshots.first();
    SyncStatus {
        source_id: source.id.clone(),
        provider: source.provider.clone(),
        latest_snapshot_id: latest.map(|s| s.snapshot_id.clone()),
        snapshot_count: snapshots.len(),
        reachable: source_reachable(source),
        last_error: None,
    }
}

fn source_reachable(source: &Source) -> bool {
    match source.original_root.as_deref() {
        Some(root) => Path::new(root).is_dir(),
        None => false,
    }
}

/// CLI/log-friendly one-line status.
#[must_use]
pub fn snapshot_status_for_cli(status: &SyncStatus) -> String {
    format!(
        "source={} provider={} snapshots={} latest={} reachable={}",
        status.source_id,
        status.provider,
        status.snapshot_count,
        status.latest_snapshot_id.as_deref().unwrap_or("-"),
        status.reachable,
    )
}

/// Data root inside a completed snapshot that provider parsers should read.
/// Falls back to `None` when no completed snapshot exists.
#[must_use]
pub fn resolve_snapshot_data_root(source_id: &str) -> Option<PathBuf> {
    crate::storage::latest_completed_snapshot(source_id).map(|s| s.data_path)
}

/// Whether `path` lives inside the CCHV archive (snapshots or staging).
/// Parser-derived caches must never be written there: completed snapshots are
/// immutable.
#[must_use]
pub fn is_snapshot_data_path(path: &Path) -> bool {
    let Ok(root) = snapshot::data_root() else {
        return false;
    };
    let sources = root.join("sources");
    path == sources || path.starts_with(&sources)
}

/// Find the registered source whose `original_root` is the longest prefix of
/// `path`. Reads `source.json` files; the source count is tiny so no cache.
#[must_use]
pub fn find_source_for_original_path(path: &Path) -> Option<Source> {
    let root = snapshot::data_root().ok()?;
    let sources_dir = root.join("sources");
    let entries = std::fs::read_dir(&sources_dir).ok()?;
    let mut best: Option<(usize, Source)> = None;
    for entry in entries.flatten() {
        let source_file = entry.path().join("source.json");
        let Ok(bytes) = std::fs::read(&source_file) else {
            continue;
        };
        let Ok(source) = serde_json::from_slice::<Source>(&bytes) else {
            continue;
        };
        if source.provider.is_empty() {
            continue;
        }
        if is_snapshot_data_path(path) {
            continue;
        }
        if let Some(original_root) = source.original_root.as_deref() {
            let original_root = Path::new(original_root);
            if path.strip_prefix(original_root).is_ok() {
                // Prefer the most specific root on overlap.
                let specificity = original_root.as_os_str().len();
                if best.as_ref().map_or(true, |(len, _)| specificity > *len) {
                    best = Some((specificity, source.clone()));
                }
            }
        }
    }
    best.map(|(_, source)| source)
}

/// All registered remote sources matching an endpoint + provider, with the
/// remote root each one last synced from (from its latest manifest).
#[must_use]
pub fn find_remote_sources(endpoint: &str, provider: &str) -> Vec<(Source, Option<String>)> {
    let normalized = endpoint.trim_end_matches('/');
    let mut out = Vec::new();
    let Ok(root) = snapshot::data_root() else {
        return out;
    };
    let sources_dir = root.join("sources");
    let Ok(entries) = std::fs::read_dir(&sources_dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let source_file = entry.path().join("source.json");
        let Ok(bytes) = std::fs::read(&source_file) else {
            continue;
        };
        let Ok(source) = serde_json::from_slice::<Source>(&bytes) else {
            continue;
        };
        if source.kind != crate::storage::SourceKind::Remote || source.provider != provider {
            continue;
        }
        if source.endpoint.as_deref().map(|e| e.trim_end_matches('/')) != Some(normalized) {
            continue;
        }
        let remote_root = crate::storage::latest_completed_snapshot(&source.id)
            .and_then(|snap| snap.manifest.original_root.clone());
        out.push((source, remote_root));
    }
    out.sort_by(|a, b| a.0.id.cmp(&b.0.id));
    out
}

/// Map a remote inner path (absolute on the remote machine) to its preserved
/// snapshot copy. Returns the owning source, snapshot, and mapped path.
#[must_use]
pub fn map_remote_path_to_snapshot(
    endpoint: &str,
    provider: &str,
    inner_path: &str,
) -> Option<(Source, crate::storage::SnapshotInfo, PathBuf)> {
    for (source, remote_root) in find_remote_sources(endpoint, provider) {
        let snapshot = crate::storage::latest_completed_snapshot(&source.id)?;
        let Some(root) = remote_root
            .as_deref()
            .or(snapshot.manifest.original_root.as_deref())
        else {
            continue;
        };
        // Claude inner paths are absolute on the remote host; relativize them
        // against the synced root. A bare relative inner path is taken as
        // already relative to the root.
        let relative = if let Some(stripped) = inner_path.strip_prefix(root) {
            stripped.trim_start_matches('/').to_string()
        } else if Path::new(inner_path).is_absolute() {
            continue;
        } else {
            inner_path.trim_start_matches('/').to_string()
        };
        if !relative.is_empty()
            && relative
                .split('/')
                .any(|seg| seg.is_empty() || seg == "." || seg == "..")
        {
            continue;
        }
        let mapped = if relative.is_empty() {
            snapshot.data_path.clone()
        } else {
            snapshot.data_path.join(native_path(&relative))
        };
        if mapped.exists() {
            return Some((source, snapshot, mapped));
        }
    }
    None
}

/// Map any original absolute path to its snapshot-backed equivalent when a
/// completed snapshot covers it. Returns `None` when no snapshot exists or
/// the mapped file/dir is absent from the snapshot.
#[must_use]
pub fn map_original_path_to_snapshot(original_path: &Path) -> Option<PathBuf> {
    if is_snapshot_data_path(original_path) {
        return Some(original_path.to_path_buf());
    }
    let source = find_source_for_original_path(original_path)?;
    let mapped = map_original_to_snapshot_path(&source, original_path)?;
    if mapped.exists() {
        Some(mapped)
    } else {
        None
    }
}

/// Map an original absolute path to its snapshot-backed equivalent.
///
/// Returns `None` when there is no snapshot, or when `original_path` is not
/// under the source's recorded `original_root`.
#[must_use]
pub fn map_original_to_snapshot_path(source: &Source, original_path: &Path) -> Option<PathBuf> {
    let data_root = resolve_snapshot_data_root(&source.id)?;
    let original_root = source.original_root.as_deref()?;
    let relative = original_path.strip_prefix(original_root).ok()?;
    if relative.as_os_str().is_empty() {
        return Some(data_root);
    }
    Some(data_root.join(relative))
}

/// Sync a local directory tree into a new immutable snapshot.
///
/// The copy is full-tree by design (correctness first). Unchanged files are
/// hardlinked from the previous completed snapshot when possible; changed and
/// new files are copied. Hardlinks are only ever created from CCHV-owned
/// completed snapshots, never from live source files.
pub fn sync_local_directory(source: &Source, original_root: &Path) -> Result<SyncOutcome, String> {
    with_source_lock(&source.id, || {
        sync_local_directory_locked(source, original_root)
    })
}

fn sync_local_directory_locked(
    source: &Source,
    original_root: &Path,
) -> Result<SyncOutcome, String> {
    if !original_root.is_absolute() {
        return Err(format!(
            "Refusing to snapshot non-absolute root {}",
            original_root.display()
        ));
    }
    if !original_root.is_dir() {
        return Err(format!(
            "Source root {} is not a directory",
            original_root.display()
        ));
    }
    // Never snapshot our own archive (feedback loop) or a bare file.
    let root = snapshot::data_root()?;
    let canonical_original =
        std::fs::canonicalize(original_root).unwrap_or_else(|_| original_root.to_path_buf());
    if canonical_original == root || canonical_original.starts_with(root.join("sources")) {
        return Err("Refusing to snapshot the CCHV archive itself".to_string());
    }

    ensure_source_registered(source)?;

    let snapshot_id = snapshot::allocate_snapshot_id();
    let staging_snapshot_dir = snapshot::staging_root(&root, &source.id).join(&snapshot_id);
    let staging_data_dir = staging_snapshot_dir.join("data");
    std::fs::create_dir_all(&staging_data_dir).map_err(|e| {
        format!(
            "Failed to create staging dir {}: {e}",
            staging_data_dir.display()
        )
    })?;

    // Previous completed snapshot for hardlink reuse + change detection.
    let previous = crate::storage::latest_completed_snapshot(&source.id);
    let previous_files: HashMap<String, (u64, i64, PathBuf)> = previous
        .as_ref()
        .map(|snap| {
            snap.manifest
                .files
                .iter()
                .map(|f| {
                    (
                        f.path.clone(),
                        (
                            f.size,
                            f.mtime_secs,
                            snap.data_path.join(native_path(&f.path)),
                        ),
                    )
                })
                .collect()
        })
        .unwrap_or_default();

    let mut entries = Vec::new();
    copy_tree_for_snapshot(
        original_root,
        original_root,
        &staging_data_dir,
        &previous_files,
        &mut entries,
    )?;

    let created_at = chrono::Utc::now().to_rfc3339();
    let manifest = SnapshotManifest::completed(source, &snapshot_id, &created_at, entries);
    finish_staged_snapshot(&root, source, &snapshot_id, &staging_snapshot_dir, manifest)
}

/// One synced remote file: bytes already transferred, path relative to the
/// provider root with `/` separators (e.g. `projects/foo/session.jsonl`).
#[derive(Debug, Clone)]
pub struct RemoteSyncedFile {
    pub path: String,
    pub bytes: Vec<u8>,
    pub mtime_secs: i64,
}

/// Sync remotely-fetched provider files into a new immutable snapshot.
///
/// Same crash-safe ordering as [`sync_local_directory`]: staging → manifest →
/// verify → atomic publish → index. Unchanged files are hardlinked from the
/// previous completed snapshot; only new/changed bytes are written. Failed
/// staging is left behind, never promoted.
pub fn sync_remote_files_to_snapshot(
    source: &Source,
    remote_root: &str,
    files: Vec<RemoteSyncedFile>,
) -> Result<SyncOutcome, String> {
    with_source_lock(&source.id, || {
        sync_remote_files_locked(source, remote_root, files)
    })
}

fn sync_remote_files_locked(
    source: &Source,
    remote_root: &str,
    files: Vec<RemoteSyncedFile>,
) -> Result<SyncOutcome, String> {
    if source.kind != crate::storage::SourceKind::Remote {
        return Err("Remote file sync requires a remote source".to_string());
    }
    let root = snapshot::data_root()?;
    ensure_source_registered(source)?;

    let snapshot_id = snapshot::allocate_snapshot_id();
    let staging_snapshot_dir = snapshot::staging_root(&root, &source.id).join(&snapshot_id);
    let staging_data_dir = staging_snapshot_dir.join("data");
    std::fs::create_dir_all(&staging_data_dir).map_err(|e| {
        format!(
            "Failed to create staging dir {}: {e}",
            staging_data_dir.display()
        )
    })?;

    let previous = crate::storage::latest_completed_snapshot(&source.id);
    let mut entries = Vec::with_capacity(files.len());
    for file in &files {
        stage_remote_file(&staging_data_dir, previous.as_ref(), file, &mut entries)?;
    }

    let created_at = chrono::Utc::now().to_rfc3339();
    let mut manifest = SnapshotManifest::completed(
        &source_for_manifest(source, remote_root),
        &snapshot_id,
        &created_at,
        entries,
    );
    // Attribute the snapshot to the remote root it was taken from.
    manifest.original_root = Some(remote_root.to_string());
    manifest.endpoint.clone_from(&source.endpoint);
    finish_staged_snapshot(&root, source, &snapshot_id, &staging_snapshot_dir, manifest)
}

/// [`SnapshotManifest::completed`] reads identity off [`Source`]; remote
/// sources carry no local root, so build a manifest-view with the remote root
/// attached without mutating the registered source.
fn source_for_manifest(source: &Source, remote_root: &str) -> Source {
    let mut view = source.clone();
    view.original_root = Some(remote_root.to_string());
    view
}

fn stage_remote_file(
    staging_data_dir: &Path,
    previous: Option<&crate::storage::SnapshotInfo>,
    file: &RemoteSyncedFile,
    entries: &mut Vec<ManifestFileEntry>,
) -> Result<(), String> {
    if file.path.trim().is_empty()
        || file.path.starts_with('/')
        || file
            .path
            .split('/')
            .any(|seg| seg.is_empty() || seg == "." || seg == "..")
    {
        return Err(format!(
            "Refusing to stage unsafe remote path {:?}",
            file.path
        ));
    }
    if file.path == ".session_cache.json" || file.path.ends_with("/.session_cache.json") {
        return Ok(());
    }
    let dest = staging_data_dir.join(native_path(&file.path));
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create staging parent {}: {e}", parent.display()))?;
    }
    if let Some(prev) = previous {
        if let Some(entry) = prev.manifest.files.iter().find(|f| f.path == file.path) {
            if entry.size == file.bytes.len() as u64 && entry.mtime_secs == file.mtime_secs {
                let prev_path = prev.data_path.join(native_path(&file.path));
                if prev_path.is_file()
                    && std::fs::read(&prev_path).ok().as_deref() == Some(file.bytes.as_slice())
                    && std::fs::hard_link(&prev_path, &dest).is_ok()
                {
                    entries.push(ManifestFileEntry {
                        path: file.path.clone(),
                        size: file.bytes.len() as u64,
                        mtime_secs: file.mtime_secs,
                    });
                    return Ok(());
                }
            }
        }
    }
    std::fs::write(&dest, &file.bytes)
        .map_err(|e| format!("Failed to stage remote file {}: {e}", file.path))?;
    entries.push(ManifestFileEntry {
        path: file.path.clone(),
        size: file.bytes.len() as u64,
        mtime_secs: file.mtime_secs,
    });
    Ok(())
}

/// Shared finalize: write manifest, verify staging, skip publish when the file
/// set is identical to the previous snapshot, otherwise atomically publish and
/// move the `current.json` pointer.
fn finish_staged_snapshot(
    root: &Path,
    source: &Source,
    snapshot_id: &str,
    staging_snapshot_dir: &Path,
    manifest: SnapshotManifest,
) -> Result<SyncOutcome, String> {
    let previous = crate::storage::latest_completed_snapshot(&source.id);
    let manifest_bytes = serde_json::to_vec_pretty(&manifest)
        .map_err(|e| format!("Failed to encode snapshot manifest: {e}"))?;
    std::fs::write(staging_snapshot_dir.join("manifest.json"), &manifest_bytes)
        .map_err(|e| format!("Failed to write staging manifest: {e}"))?;

    verify_staging_usable(staging_snapshot_dir, &manifest)?;

    // Fast path: identical file set to the previous snapshot → no new history.
    if let Some(prev) = previous.as_ref() {
        if same_file_set(&prev.manifest, &manifest) {
            // Leave the identical staging behind as an incomplete witness per the
            // no-auto-delete rule, but do not publish it.
            return Ok(SyncOutcome::Unchanged(prev.clone()));
        }
    }

    publish_staging(root, &source.id, snapshot_id, staging_snapshot_dir)?;

    let published = crate::storage::latest_completed_snapshot(&source.id)
        .ok_or_else(|| "Snapshot publish succeeded but latest snapshot is missing".to_string())?;
    Ok(SyncOutcome::Created(published))
}

fn native_path(relative_posix: &str) -> PathBuf {
    relative_posix.split('/').collect()
}

fn relative_posix(root: &Path, path: &Path) -> Option<String> {
    let relative = path.strip_prefix(root).ok()?;
    let mut parts = Vec::new();
    for component in relative.components() {
        use std::path::Component as C;
        match component {
            C::Normal(part) => parts.push(part.to_string_lossy().to_string()),
            C::CurDir => {}
            // Refuse to preserve anything that escapes or is non-portable.
            C::ParentDir | C::Prefix(_) | C::RootDir => return None,
        }
    }
    if parts.is_empty() {
        return None;
    }
    Some(parts.join("/"))
}

#[allow(clippy::too_many_arguments)]
fn copy_tree_for_snapshot(
    original_root: &Path,
    current_dir: &Path,
    staging_data_dir: &Path,
    previous_files: &HashMap<String, (u64, i64, PathBuf)>,
    entries: &mut Vec<ManifestFileEntry>,
) -> Result<(), String> {
    let read_dir = std::fs::read_dir(current_dir)
        .map_err(|e| format!("Failed to list {}: {e}", current_dir.display()))?;
    // Sort for deterministic manifests.
    let mut children: Vec<_> = read_dir.flatten().collect();
    children.sort_by_key(std::fs::DirEntry::file_name);

    for child in children {
        let path = child.path();
        let file_name = child.file_name();
        let name = file_name.to_string_lossy().to_string();
        // CCHV-derived cache inside a live project dir: skip so snapshots stay
        // a copy of provider data, not of our own derived state.
        if name == ".session_cache.json" {
            continue;
        }
        let meta = std::fs::symlink_metadata(&path)
            .map_err(|e| format!("Failed to stat {}: {e}", path.display()))?;
        if meta.file_type().is_symlink() {
            // Dereference symlinks into real files/dirs so the snapshot has no
            // links that could escape the archive later.
            let Ok(target_meta) = std::fs::metadata(&path) else {
                continue;
            };
            if target_meta.is_dir() {
                copy_symlinked_dir_for_snapshot(
                    original_root,
                    &path,
                    staging_data_dir,
                    previous_files,
                    entries,
                )?;
            } else if target_meta.is_file() {
                copy_one_file_for_snapshot(
                    original_root,
                    &path,
                    staging_data_dir,
                    previous_files,
                    entries,
                    &target_meta,
                )?;
            }
            continue;
        }
        if meta.is_dir() {
            copy_tree_for_snapshot(
                original_root,
                &path,
                staging_data_dir,
                previous_files,
                entries,
            )?;
        } else if meta.is_file() {
            copy_one_file_for_snapshot(
                original_root,
                &path,
                staging_data_dir,
                previous_files,
                entries,
                &meta,
            )?;
        }
    }
    Ok(())
}

/// Copy the contents of a symlinked directory as real snapshot directories.
fn copy_symlinked_dir_for_snapshot(
    original_root: &Path,
    link_path: &Path,
    staging_data_dir: &Path,
    previous_files: &HashMap<String, (u64, i64, PathBuf)>,
    entries: &mut Vec<ManifestFileEntry>,
) -> Result<(), String> {
    let Ok(target) = std::fs::canonicalize(link_path) else {
        return Ok(());
    };
    // Refuse links that escape to the CCHV archive itself.
    if let Ok(root) = snapshot::data_root() {
        if target == root || target.starts_with(root.join("sources")) {
            return Ok(());
        }
    }
    // Walk the target but record paths as if they lived under the link.
    walk_external_dir_for_snapshot(
        original_root,
        link_path,
        &target,
        staging_data_dir,
        previous_files,
        entries,
    )
}

fn walk_external_dir_for_snapshot(
    original_root: &Path,
    link_path: &Path,
    target_dir: &Path,
    staging_data_dir: &Path,
    previous_files: &HashMap<String, (u64, i64, PathBuf)>,
    entries: &mut Vec<ManifestFileEntry>,
) -> Result<(), String> {
    let mut stack = vec![target_dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(read_dir) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut children: Vec<_> = read_dir.flatten().collect();
        children.sort_by_key(std::fs::DirEntry::file_name);
        for child in children {
            let path = child.path();
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if meta.file_type().is_symlink() {
                // One level of dereference is enough; deeper links are skipped
                // rather than risking cycles.
                if let Ok(target_meta) = std::fs::metadata(&path) {
                    if target_meta.is_file() {
                        // Map target file back under the link path.
                        let Ok(relative_target) = path.strip_prefix(target_dir) else {
                            continue;
                        };
                        let virtual_source = link_path.join(relative_target);
                        copy_one_file_for_snapshot(
                            original_root,
                            &virtual_source,
                            staging_data_dir,
                            previous_files,
                            entries,
                            &target_meta,
                        )?;
                    }
                }
                continue;
            }
            if meta.is_dir() {
                stack.push(path);
            } else if meta.is_file() {
                let Ok(relative_target) = path.strip_prefix(target_dir) else {
                    continue;
                };
                let virtual_source = link_path.join(relative_target);
                copy_one_file_for_snapshot(
                    original_root,
                    &virtual_source,
                    staging_data_dir,
                    previous_files,
                    entries,
                    &meta,
                )?;
            }
        }
    }
    Ok(())
}

fn copy_one_file_for_snapshot(
    original_root: &Path,
    source_file: &Path,
    staging_data_dir: &Path,
    previous_files: &HashMap<String, (u64, i64, PathBuf)>,
    entries: &mut Vec<ManifestFileEntry>,
    meta: &std::fs::Metadata,
) -> Result<(), String> {
    let Some(relative) = relative_posix(original_root, source_file) else {
        return Ok(());
    };
    let dest = staging_data_dir.join(native_path(&relative));
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create staging parent {}: {e}", parent.display()))?;
    }
    let size = meta.len();
    #[allow(clippy::cast_possible_wrap)]
    let mtime_secs = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    // Reuse an unchanged byte-identical file from the previous immutable
    // snapshot via hardlink. The link source is CCHV-owned and immutable, so
    // later external mutation of the live source cannot affect either copy.
    // The destination stays a private inode on filesystems without hardlink
    // support (fallback to copy below).
    //
    // Size+mtime alone is not sufficient: same-size edits within one mtime
    // second would alias two different contents onto one inode. On a match we
    // byte-compare before linking so a changed file is always copied.
    if let Some((prev_size, prev_mtime, prev_path)) = previous_files.get(&relative) {
        if *prev_size == size && *prev_mtime == mtime_secs && prev_path.is_file() {
            let unchanged = std::fs::read(source_file)
                .ok()
                .and_then(|current| std::fs::read(prev_path).ok().map(|prev| prev == current))
                .unwrap_or(false);
            if unchanged && std::fs::hard_link(prev_path, &dest).is_ok() {
                entries.push(ManifestFileEntry {
                    path: relative,
                    size,
                    mtime_secs,
                });
                return Ok(());
            }
            if unchanged {
                // Hardlink unsupported: fall through to the copy below, which
                // re-reads the source. The equality check above already proved
                // the bytes match, so this is only an efficiency loss.
            }
        }
    }

    // Changed/new file: full byte copy. Read+write (not rename) so a
    // half-written live source file cannot corrupt the previous snapshot, and
    // the previous snapshot is never the copy source for changed bytes.
    //
    // SQLite-backed providers (opencode.db, trae state.vscdb, ...) may be
    // mid-checkpoint while we read. We copy bytes as observed; the parser
    // layer opens copies read-only and must tolerate a torn copy by falling
    // back to the previous snapshot. Never copy-then-replace into a completed
    // snapshot: the destination here is staging-only.
    //
    // An unreadable file mid-sync (deleted/rotated by the provider) skips the
    // file rather than failing the whole snapshot; the manifest only lists
    // what was actually staged, so verification stays consistent.
    let Ok(bytes) = std::fs::read(source_file) else {
        log::warn!("Snapshot skipped unreadable file {}", source_file.display());
        return Ok(());
    };
    std::fs::write(&dest, &bytes)
        .map_err(|e| format!("Failed to stage {}: {e}", dest.display()))?;
    entries.push(ManifestFileEntry {
        path: relative,
        size: bytes.len() as u64,
        mtime_secs,
    });
    Ok(())
}

fn verify_staging_usable(
    staging_snapshot_dir: &Path,
    manifest: &SnapshotManifest,
) -> Result<(), String> {
    let data_dir = staging_snapshot_dir.join("data");
    if !data_dir.is_dir() {
        return Err("Staging data directory is missing".to_string());
    }
    // Every manifest entry must exist with the observed size. A failed or
    // incomplete copy must never be published.
    for file in &manifest.files {
        let path = data_dir.join(native_path(&file.path));
        let meta = std::fs::metadata(&path)
            .map_err(|e| format!("Staged file {} went missing: {e}", file.path))?;
        if !meta.is_file() {
            return Err(format!("Staged path {} is not a file", file.path));
        }
        if meta.len() != file.size {
            return Err(format!(
                "Staged file {} size changed during copy ({} != {})",
                file.path,
                meta.len(),
                file.size
            ));
        }
    }
    Ok(())
}

fn same_file_set(a: &SnapshotManifest, b: &SnapshotManifest) -> bool {
    if a.file_count != b.file_count || a.total_bytes != b.total_bytes {
        return false;
    }
    a.files == b.files
}

fn publish_staging(
    root: &Path,
    source_id: &str,
    snapshot_id: &str,
    staging_snapshot_dir: &Path,
) -> Result<(), String> {
    let final_dir = snapshot::snapshot_path(root, source_id, snapshot_id);
    if final_dir.exists() {
        return Err(format!(
            "Refusing to overwrite existing snapshot {}",
            final_dir.display()
        ));
    }
    let snapshots_dir = snapshot::snapshots_dir(root, source_id);
    std::fs::create_dir_all(&snapshots_dir).map_err(|e| {
        format!(
            "Failed to create snapshots dir {}: {e}",
            snapshots_dir.display()
        )
    })?;
    std::fs::rename(staging_snapshot_dir, &final_dir)
        .map_err(|e| format!("Failed to publish snapshot {}: {e}", final_dir.display()))?;
    snapshot::write_current_pointer(root, source_id, snapshot_id)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::SourceKind;
    use std::io::Write;

    fn unique_source(provider: &str) -> Source {
        Source {
            id: format!(
                "test-sync-{provider}-{}",
                &uuid::Uuid::new_v4().to_string()[..8]
            ),
            kind: SourceKind::Local,
            provider: provider.to_string(),
            original_root: None,
            endpoint: None,
            label: None,
        }
    }

    fn write_file(path: &Path, content: &[u8]) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let mut file = std::fs::File::create(path).unwrap();
        file.write_all(content).unwrap();
    }

    #[test]
    #[serial_test::serial]
    fn sync_preserves_history_across_modification() {
        let _sandbox = crate::test_utils::SandboxHome::new();
        let original = tempfile::tempdir().unwrap();
        let root = original.path().join(".claude");
        let project = root.join("projects").join("foo");
        write_file(&project.join("session.jsonl"), b"{\"a\":1}\n");

        let mut source = unique_source("claude");
        source.original_root = Some(root.to_string_lossy().to_string());

        let first = match sync_local_directory(&source, &root).unwrap() {
            SyncOutcome::Created(snap) => snap,
            SyncOutcome::Unchanged(_) => panic!("first sync must create"),
        };
        let first_bytes =
            std::fs::read(first.data_path.join("projects/foo/session.jsonl")).unwrap();
        assert_eq!(first_bytes, b"{\"a\":1}\n");

        // Modify source and sync again.
        write_file(&project.join("session.jsonl"), b"{\"a\":2}\n");
        write_file(&project.join("new.jsonl"), b"{}\n");
        let second = match sync_local_directory(&source, &root).unwrap() {
            SyncOutcome::Created(snap) => snap,
            SyncOutcome::Unchanged(_) => panic!("modified sync must create"),
        };
        assert_ne!(first.snapshot_id, second.snapshot_id);

        // Both snapshots preserved; first never modified.
        let first_again =
            std::fs::read(first.data_path.join("projects/foo/session.jsonl")).unwrap();
        assert_eq!(first_again, b"{\"a\":1}\n");
        let second_bytes =
            std::fs::read(second.data_path.join("projects/foo/session.jsonl")).unwrap();
        assert_eq!(second_bytes, b"{\"a\":2}\n");

        // Latest pointer moved forward.
        let latest = crate::storage::latest_completed_snapshot(&source.id).unwrap();
        assert_eq!(latest.snapshot_id, second.snapshot_id);
    }

    #[test]
    #[serial_test::serial]
    fn sync_survives_source_deletion() {
        let _sandbox = crate::test_utils::SandboxHome::new();
        let original = tempfile::tempdir().unwrap();
        let root = original.path().join(".claude");
        write_file(&root.join("projects/p/s.jsonl"), b"hello\n");

        let mut source = unique_source("claude");
        source.original_root = Some(root.to_string_lossy().to_string());
        let snap = match sync_local_directory(&source, &root).unwrap() {
            SyncOutcome::Created(s) => s,
            SyncOutcome::Unchanged(_) => panic!("must create"),
        };
        let data_path = snap.data_path.join("projects/p/s.jsonl");
        assert!(data_path.is_file());

        // Delete the original source entirely.
        std::fs::remove_dir_all(&root).unwrap();
        assert!(!root.exists());
        // Snapshot remains readable.
        assert!(data_path.is_file());
        assert_eq!(std::fs::read(&data_path).unwrap(), b"hello\n");
        // A later sync fails but never replaces the completed snapshot.
        assert!(sync_local_directory(&source, &root).is_err());
        let latest = crate::storage::latest_completed_snapshot(&source.id).unwrap();
        assert_eq!(latest.snapshot_id, snap.snapshot_id);
    }

    #[test]
    #[serial_test::serial]
    fn failed_sync_never_replaces_current() {
        let _sandbox = crate::test_utils::SandboxHome::new();
        let original = tempfile::tempdir().unwrap();
        let root = original.path().join(".claude");
        write_file(&root.join("projects/p/s.jsonl"), b"v1\n");
        let mut source = unique_source("claude");
        source.original_root = Some(root.to_string_lossy().to_string());
        let first = match sync_local_directory(&source, &root).unwrap() {
            SyncOutcome::Created(s) => s,
            SyncOutcome::Unchanged(_) => panic!("must create"),
        };
        // Sync a non-existent root: must error and keep current.
        let missing = original.path().join("does-not-exist");
        assert!(sync_local_directory(&source, &missing).is_err());
        let latest = crate::storage::latest_completed_snapshot(&source.id).unwrap();
        assert_eq!(latest.snapshot_id, first.snapshot_id);
    }

    #[test]
    #[serial_test::serial]
    fn new_sync_never_modifies_previous_snapshot() {
        let _sandbox = crate::test_utils::SandboxHome::new();
        let original = tempfile::tempdir().unwrap();
        let root = original.path().join(".claude");
        write_file(&root.join("projects/p/s.jsonl"), b"v1\n");
        let mut source = unique_source("claude");
        source.original_root = Some(root.to_string_lossy().to_string());
        let first = match sync_local_directory(&source, &root).unwrap() {
            SyncOutcome::Created(s) => s,
            SyncOutcome::Unchanged(_) => panic!("must create"),
        };
        let marker = first.data_path.join("projects/p/s.jsonl");
        let before = std::fs::metadata(&marker).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(15));
        write_file(
            &root.join("projects/p/s.jsonl"),
            b"v2 much longer content\n",
        );
        let _ = sync_local_directory(&source, &root).unwrap();
        let after = std::fs::metadata(&marker).unwrap().modified().unwrap();
        assert_eq!(before, after);
        assert_eq!(std::fs::read(&marker).unwrap(), b"v1\n");
    }

    #[test]
    #[serial_test::serial]
    fn unchanged_source_reports_unchanged() {
        let _sandbox = crate::test_utils::SandboxHome::new();
        let original = tempfile::tempdir().unwrap();
        let root = original.path().join(".claude");
        write_file(&root.join("projects/p/s.jsonl"), b"same\n");
        let mut source = unique_source("claude");
        source.original_root = Some(root.to_string_lossy().to_string());
        let first = match sync_local_directory(&source, &root).unwrap() {
            SyncOutcome::Created(s) => s,
            SyncOutcome::Unchanged(_) => panic!("must create"),
        };
        match sync_local_directory(&source, &root).unwrap() {
            SyncOutcome::Unchanged(s) => assert_eq!(s.snapshot_id, first.snapshot_id),
            SyncOutcome::Created(_) => panic!("identical tree must not create"),
        }
    }

    fn claude_session_jsonl() -> String {
        [
            serde_json::json!({
                "uuid": "11111111-1111-4111-8111-111111111111",
                "sessionId": "session-preserved",
                "timestamp": "2026-09-01T00:00:00Z",
                "type": "user",
                "cwd": "/tmp/preserved-work",
                "message": {"role": "user", "content": "Preserve this conversation"},
            })
            .to_string(),
            serde_json::json!({
                "uuid": "22222222-2222-4222-8222-222222222222",
                "parentUuid": "11111111-1111-4111-8111-111111111111",
                "sessionId": "session-preserved",
                "timestamp": "2026-09-01T00:01:00Z",
                "type": "assistant",
                "cwd": "/tmp/preserved-work",
                "message": {
                    "role": "assistant",
                    "content": [{"type": "text", "text": "Preserved reply"}],
                    "model": "claude-opus-4-1",
                },
            })
            .to_string(),
        ]
        .join("\n")
    }

    /// Vertical slice: original → snapshot → existing parser. Deleting the
    /// original afterwards must not break browsing.
    #[tokio::test]
    #[serial_test::serial]
    async fn claude_vertical_slice_survives_source_deletion() {
        let _sandbox = crate::test_utils::SandboxHome::new();
        let original = tempfile::tempdir().unwrap();
        let base = original.path().join("fake-claude");
        let project_dir = base.join("projects").join("proj");
        write_file(
            &project_dir.join("session.jsonl"),
            claude_session_jsonl().as_bytes(),
        );

        let projects = crate::commands::project::scan_claude_base_with_snapshot(
            base.to_string_lossy().to_string(),
            None,
        )
        .await;
        assert_eq!(projects.len(), 1, "snapshot scan must find the project");
        let project_path = projects[0].path.clone();
        assert!(
            project_path.starts_with(base.to_string_lossy().as_ref()),
            "external contract keeps original paths, got {project_path}"
        );

        let sessions =
            crate::commands::session::load_project_sessions(project_path.clone(), Some(false))
                .await
                .expect("sessions load from snapshot");
        assert_eq!(sessions.len(), 1);
        let session_file = sessions[0].file_path.clone();
        assert!(
            session_file.starts_with(base.to_string_lossy().as_ref()),
            "session keeps original path, got {session_file}"
        );

        let messages = crate::commands::session::load_session_messages(session_file.clone())
            .await
            .expect("messages load from snapshot");
        assert!(
            messages.len() >= 2,
            "expected parsed conversation, got {}",
            messages.len()
        );

        // Delete the entire original source.
        std::fs::remove_dir_all(&base).unwrap();
        assert!(!base.exists());

        // Browsing still works from the preserved copy.
        let projects_after = crate::commands::project::scan_claude_base_with_snapshot(
            base.to_string_lossy().to_string(),
            None,
        )
        .await;
        assert_eq!(projects_after.len(), 1);
        assert_eq!(projects_after[0].path, project_path);

        let sessions_after =
            crate::commands::session::load_project_sessions(project_path.clone(), Some(false))
                .await
                .expect("sessions survive source deletion");
        assert_eq!(sessions_after.len(), 1);

        let messages_after = crate::commands::session::load_session_messages(session_file.clone())
            .await
            .expect("messages survive source deletion");
        assert_eq!(messages_after.len(), messages.len());
    }

    #[test]
    #[serial_test::serial]
    fn remote_file_sync_rejects_unsafe_paths_and_skips_cache() {
        let _sandbox = crate::test_utils::SandboxHome::new();
        let mut source = unique_source("claude");
        source.kind = crate::storage::SourceKind::Remote;
        source.id = format!("test-remote-{}", &uuid::Uuid::new_v4().to_string()[..8]);
        source.original_root = None;
        source.endpoint = Some("http://example:3728".to_string());
        crate::storage::ensure_source_registered(&source).unwrap();

        // Traversal is refused without publishing anything.
        let bad = vec![crate::storage::RemoteSyncedFile {
            path: "../escape.jsonl".to_string(),
            bytes: b"{}".to_vec(),
            mtime_secs: 1,
        }];
        assert!(crate::storage::sync_remote_files_to_snapshot(&source, "/r/.claude", bad).is_err());
        assert!(crate::storage::latest_completed_snapshot(&source.id).is_none());

        // Cache files are skipped; real files are preserved.
        let files = vec![
            crate::storage::RemoteSyncedFile {
                path: "projects/p/.session_cache.json".to_string(),
                bytes: b"{}".to_vec(),
                mtime_secs: 1,
            },
            crate::storage::RemoteSyncedFile {
                path: "projects/p/s.jsonl".to_string(),
                bytes: b"{\"a\":1}\n".to_vec(),
                mtime_secs: 2,
            },
        ];
        match crate::storage::sync_remote_files_to_snapshot(&source, "/r/.claude", files).unwrap() {
            SyncOutcome::Created(snap) => {
                assert!(!snap
                    .data_path
                    .join("projects/p/.session_cache.json")
                    .exists());
                assert!(snap.data_path.join("projects/p/s.jsonl").is_file());
            }
            SyncOutcome::Unchanged(_) => panic!("must create"),
        }
    }

    /// Vertical slice: modifying a session preserves both old and new snapshots.
    #[tokio::test]
    #[serial_test::serial]
    async fn claude_vertical_slice_preserves_both_versions() {
        let _sandbox = crate::test_utils::SandboxHome::new();
        let original = tempfile::tempdir().unwrap();
        let base = original.path().join("fake-claude-v");
        let project_dir = base.join("projects").join("proj");
        write_file(
            &project_dir.join("session.jsonl"),
            claude_session_jsonl().as_bytes(),
        );

        let source = crate::commands::project::claude_source_for_base(&base, None);
        let first = match sync_local_directory(&source, &base).unwrap() {
            SyncOutcome::Created(s) => s,
            SyncOutcome::Unchanged(_) => panic!("first sync must create"),
        };

        // Append a new message to the live session.
        {
            use std::io::Write as _;
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(project_dir.join("session.jsonl"))
                .unwrap();
            writeln!(
                file,
                "{}",
                serde_json::json!({
                    "uuid": "33333333-3333-4333-8333-333333333333",
                    "parentUuid": "22222222-2222-4222-8222-222222222222",
                    "sessionId": "session-preserved",
                    "timestamp": "2026-09-01T00:02:00Z",
                    "type": "user",
                    "cwd": "/tmp/preserved-work",
                    "message": {"role": "user", "content": "Follow-up"},
                })
            )
            .unwrap();
        }
        let second = match sync_local_directory(&source, &base).unwrap() {
            SyncOutcome::Created(s) => s,
            SyncOutcome::Unchanged(_) => panic!("modified sync must create"),
        };
        assert_ne!(first.snapshot_id, second.snapshot_id);

        let old_bytes = std::fs::read(first.data_path.join("projects/proj/session.jsonl")).unwrap();
        let new_bytes =
            std::fs::read(second.data_path.join("projects/proj/session.jsonl")).unwrap();
        assert!(new_bytes.len() > old_bytes.len());
        assert_eq!(
            std::fs::read(first.data_path.join("projects/proj/session.jsonl")).unwrap(),
            old_bytes,
            "old snapshot must be untouched"
        );

        let snapshots = crate::storage::list_snapshots(&source.id);
        assert!(snapshots.len() >= 2);
    }
}
