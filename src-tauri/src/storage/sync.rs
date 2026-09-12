//! Sync into cumulative immutable snapshots.
//!
//! Crash-safe ordering:
//! ```text
//! 1. claim canonical source identity (adopt older lineage dirs, never delete)
//! 2. allocate unique snapshot id
//! 3. create staging directory
//! 4. carry every previously preserved file into staging (hardlink)
//! 5. overlay currently observable upstream bytes (hash-verified)
//! 6. capture SQLite stores through the online backup API + quick_check
//! 7. generate manifest (v2: path + sha256 + presence)
//! 8. verify staging, compare fingerprints with previous snapshot
//! 9. identical → drop staging, report Unchanged
//!    changed → make files read-only, atomically publish, move current pointer
//! 10. only after publish, parse/index into SQLite (see storage::index)
//! ```
//!
//! Every new snapshot is cumulative: files deleted upstream stay preserved
//! with `present = false`, so the latest completed snapshot always holds the
//! entire observed history. Older snapshots stay immutable and preserved.
//! Staging lives at `sources/<id>/.staging/<snapshot-id>/` and is renamed to
//! `sources/<id>/snapshots/<snapshot-id>/`. Failed staging is left behind for
//! diagnosis, never promoted; provably byte-identical staging is discarded.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use once_cell::sync::Lazy;

use crate::storage::cas;
use crate::storage::hash::sha256_hex;
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
    pub role: String,
    pub latest_snapshot_id: Option<String>,
    pub snapshot_count: usize,
    pub reachable: bool,
    pub last_error: Option<String>,
}

/// Options for [`sync_source`]. Provider-registry discovery feeds dynamic
/// inputs (e.g. Cursor workspace databases found at sync time) here.
#[derive(Debug, Clone, Default)]
pub struct SyncOptions {
    /// Additional snapshot-relative SQLite paths needing backup capture.
    pub extra_sqlite_dbs: Vec<String>,
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
/// Older lineage directories are adopted (renamed + aliased), never deleted.
pub fn ensure_source_registered(source: &Source) -> Result<PathBuf, String> {
    let effective = crate::storage::source::claim_canonical_source(source)?;
    let root = snapshot::data_root()?;
    Ok(effective.storage_dir(&root))
}

/// Status snapshot for one source (no syncing).
#[must_use]
pub fn sync_status(source: &Source) -> SyncStatus {
    let snapshots = crate::storage::list_snapshots(&source.id);
    let latest = snapshots.first();
    SyncStatus {
        source_id: source.id.clone(),
        provider: source.provider.clone(),
        role: source.role.clone(),
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

/// All registered remote sources for a provider that belong to the machine
/// behind `endpoint`. Uses the learned endpoint→machine map when available
/// and falls back to legacy endpoint matching for unlearned hosts.
#[must_use]
pub fn find_remote_sources(endpoint: &str, provider: &str) -> Vec<(Source, Option<String>)> {
    let normalized = endpoint.trim_end_matches('/');
    let mut out = Vec::new();
    let Ok(root) = snapshot::data_root() else {
        return out;
    };
    let learned = crate::storage::source::remote_machine_for_endpoint(&root, normalized);
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
        let belongs = match (&learned, source.machine_id.as_str()) {
            (Some(mid), sid) if !sid.is_empty() => sid == mid,
            // Unlearned host, or legacy record awaiting adoption: match by
            // endpoint so reads keep working before the next sync adopts it.
            _ => source.endpoint.as_deref().map(|e| e.trim_end_matches('/')) == Some(normalized),
        };
        if !belongs {
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
        // Inner paths are absolute on the remote host; relativize them
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

// ---------------------------------------------------------------------------
// Sync entry points.
// ---------------------------------------------------------------------------

/// Sync one source's live tree into a new cumulative immutable snapshot.
///
/// `live_root` is the provider directory as observed on this machine (or a
/// UNC path for WSL sources). `opts` carries dynamic inputs such as
/// databases discovered at sync time.
pub fn sync_source(
    source: &Source,
    live_root: &Path,
    opts: &SyncOptions,
) -> Result<SyncOutcome, String> {
    with_source_lock(&source.id, || {
        let effective = crate::storage::source::claim_canonical_source(source)?;
        sync_source_locked(&effective, live_root, opts)
    })
}

/// Backwards-compatible file-tree sync with default options.
pub fn sync_local_directory(source: &Source, original_root: &Path) -> Result<SyncOutcome, String> {
    sync_source(source, original_root, &SyncOptions::default())
}

fn sync_source_locked(
    source: &Source,
    live_root: &Path,
    opts: &SyncOptions,
) -> Result<SyncOutcome, String> {
    if !live_root.is_absolute() {
        return Err(format!(
            "Refusing to snapshot non-absolute root {}",
            live_root.display()
        ));
    }
    if !live_root.is_dir() {
        return Err(format!(
            "Source root {} is not a directory",
            live_root.display()
        ));
    }
    // Never snapshot our own archive (feedback loop) or a bare file.
    let root = snapshot::data_root()?;
    let canonical_live =
        std::fs::canonicalize(live_root).unwrap_or_else(|_| live_root.to_path_buf());
    if canonical_live == root || canonical_live.starts_with(root.join("sources")) {
        return Err("Refusing to snapshot the CCHV archive itself".to_string());
    }

    let previous_latest = crate::storage::latest_completed_snapshot(&source.id);
    let carry_from = carry_base_snapshots(&root, &source.id, previous_latest.as_ref());

    let snapshot_id = snapshot::allocate_snapshot_id();
    let staging_snapshot_dir = snapshot::staging_root(&root, &source.id).join(&snapshot_id);
    let staging_data_dir = staging_snapshot_dir.join("data");
    std::fs::create_dir_all(&staging_data_dir).map_err(|e| {
        format!(
            "Failed to create staging dir {}: {e}",
            staging_data_dir.display()
        )
    })?;

    let now_secs = unix_now_secs();

    // 1. Consistent SQLite captures join the overlay as ordinary files.
    // Databases are captured ONLY through the backup API: the file walk
    // skips them so no torn plain copy ever competes with a backup.
    let mut dbs: Vec<String> = source.sqlite_dbs.clone();
    dbs.extend(opts.extra_sqlite_dbs.iter().cloned());
    let db_relpaths = dedup_relpaths(dbs);
    let db_skip: std::collections::HashSet<String> = db_relpaths.iter().cloned().collect();

    // 2. Walk the live tree (bytes stay in hand only for hashing/staging).
    let mut live_files = Vec::new();
    collect_live_tree(
        live_root,
        live_root,
        &source.includes,
        source.max_depth,
        0,
        &db_skip,
        &mut live_files,
    )?;

    let mut db_carry_hint: HashMap<String, bool> = HashMap::new();
    for db_rel in &db_relpaths {
        match capture_sqlite_live(live_root, db_rel, &staging_data_dir) {
            Ok(staged) => {
                let bytes = std::fs::read(&staged).map_err(|e| {
                    format!("Failed to read staged SQLite {}: {e}", staged.display())
                })?;
                live_files.push(LiveFile {
                    rel: db_rel.clone(),
                    bytes: Some(bytes),
                    mtime_secs: live_mtime(&live_root.join(native_path(db_rel))),
                });
            }
            Err(e) => {
                // Keep the previous good copy when one exists; abort only
                // when no preserved copy exists at all (strict P0 rule: never
                // publish a snapshot whose SQLite stores failed validation
                // without a preserved fallback).
                if carry_has(&carry_from, db_rel) {
                    log::warn!("SQLite capture failed for {db_rel}, keeping preserved copy: {e}");
                    db_carry_hint.insert(db_rel.clone(), true);
                } else {
                    cleanup_staging(&staging_snapshot_dir);
                    return Err(format!("SQLite capture failed for {db_rel}: {e}"));
                }
            }
        }
    }

    // 3. Assemble the cumulative candidate.
    let entries = assemble_candidate(
        &root,
        &staging_data_dir,
        &carry_from,
        live_files,
        &db_carry_hint,
        now_secs,
    )?;

    let created_at = chrono::Utc::now().to_rfc3339();
    let manifest = SnapshotManifest::completed(source, &snapshot_id, &created_at, entries);
    finish_staged_snapshot(
        &root,
        source,
        &snapshot_id,
        &staging_snapshot_dir,
        manifest,
        previous_latest.as_ref(),
    )
}

/// Snapshots to carry history forward from: the cumulative latest v2 alone,
/// or — once, when transitioning off point-in-time v1 history — the union of
/// all previous snapshots (newest wins per path).
fn carry_base_snapshots(
    data_root: &Path,
    source_id: &str,
    previous_latest: Option<&crate::storage::SnapshotInfo>,
) -> Vec<crate::storage::SnapshotInfo> {
    let mut all = crate::storage::snapshot::list_snapshots_in_root(data_root, source_id);
    // Oldest first so newer entries overwrite older ones in the carry map.
    all.sort_by(|a, b| a.snapshot_id.cmp(&b.snapshot_id));
    match previous_latest {
        Some(latest) if latest.manifest.version >= crate::storage::manifest::MANIFEST_VERSION => {
            vec![latest.clone()]
        }
        _ => all,
    }
}

/// One observable upstream file. `bytes: None` means listed-but-unreadable
/// (carry the preserved copy when one exists, skip otherwise).
struct LiveFile {
    rel: String,
    bytes: Option<Vec<u8>>,
    mtime_secs: i64,
}

/// Current Unix time in seconds (0 when the clock is unavailable).
#[allow(clippy::cast_possible_wrap)]
fn unix_now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[allow(clippy::cast_possible_wrap)]
fn live_mtime(path: &Path) -> i64 {
    std::fs::metadata(path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn dedup_relpaths(paths: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for path in paths {
        let normalized = path.trim().trim_start_matches('/').replace('\\', "/");
        if normalized.is_empty()
            || normalized.split('/').any(|seg| {
                seg.is_empty() || seg == "." || seg == ".." || seg == ".session_cache.json"
            })
        {
            continue;
        }
        if seen.insert(normalized.clone()) {
            out.push(normalized);
        }
    }
    out.sort();
    out
}

/// Capture one live SQLite database into the staging tree via the online
/// backup API, validated before use. Returns the staged path.
fn capture_sqlite_live(
    live_root: &Path,
    db_rel: &str,
    staging_data_dir: &Path,
) -> Result<PathBuf, String> {
    let live_db = live_root.join(native_path(db_rel));
    let staged = staging_data_dir.join(native_path(db_rel));
    if let Some(parent) = staged.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create staging parent {}: {e}", parent.display()))?;
    }
    crate::storage::sqlite_capture::backup_sqlite_into(&live_db, &staged)?;
    Ok(staged)
}

fn carry_has(carry_from: &[crate::storage::SnapshotInfo], rel: &str) -> bool {
    carry_from
        .iter()
        .any(|snap| snap.manifest.files.iter().any(|f| f.path == rel))
}

// ---------------------------------------------------------------------------
// Candidate assembly (shared by local, remote, and replication flows).
// ---------------------------------------------------------------------------

/// Assemble a cumulative candidate in `staging_data_dir`:
/// carry every previously preserved file, overlay live bytes, hash everything.
/// Returns the manifest entries. `db_keep_present` lists relpaths whose live
/// capture failed but whose preserved copy must stay marked present.
#[allow(clippy::too_many_arguments)]
fn assemble_candidate(
    data_root: &Path,
    staging_data_dir: &Path,
    carry_from: &[crate::storage::SnapshotInfo],
    live_files: Vec<LiveFile>,
    db_keep_present: &HashMap<String, bool>,
    now_secs: i64,
) -> Result<Vec<ManifestFileEntry>, String> {
    // Newest snapshot wins per path for the carry base.
    let mut carry: BTreeMap<String, (PathBuf, ManifestFileEntry)> = BTreeMap::new();
    for snap in carry_from {
        for entry in &snap.manifest.files {
            let abs = snap.data_path.join(native_path(&entry.path));
            if abs.is_file() {
                carry.insert(entry.path.clone(), (abs, entry.clone()));
            }
        }
    }
    // v1 entries have no hash: re-hash the preserved bytes once, now. The
    // stored v1 manifest is never mutated; only the new v2 entry carries it.
    let carry: BTreeMap<String, (PathBuf, ManifestFileEntry, String)> = carry
        .into_iter()
        .map(|(rel, (abs, mut entry))| {
            if entry.sha256.is_empty() || !crate::storage::hash::is_plausible_hash(&entry.sha256) {
                entry.sha256 = std::fs::read(&abs)
                    .map(|b| sha256_hex(&b))
                    .unwrap_or_default();
            }
            let hash = entry.sha256.clone();
            (rel, (abs, entry, hash))
        })
        .collect();

    let mut entries = Vec::new();
    let mut seen_live: HashMap<String, bool> = HashMap::new();
    for live in live_files {
        seen_live.insert(live.rel.clone(), true);
        match live.bytes {
            Some(bytes) => {
                let hash = sha256_hex(&bytes);
                let size = bytes.len() as u64;
                if let Some((prev_abs, prev_entry, prev_hash)) = carry.get(&live.rel) {
                    if *prev_hash == hash {
                        // Proven identical: keep the preserved inode.
                        link_or_copy(prev_abs, &staging_data_dir.join(native_path(&live.rel)))?;
                        entries.push(ManifestFileEntry {
                            path: live.rel.clone(),
                            size: prev_entry.size,
                            mtime_secs: live.mtime_secs,
                            sha256: prev_hash.clone(),
                            present: true,
                            last_seen_secs: now_secs,
                        });
                        continue;
                    }
                }
                // New or changed: stage bytes (CAS first), record hash.
                stage_bytes(data_root, staging_data_dir, &live.rel, &bytes)?;
                entries.push(ManifestFileEntry {
                    path: live.rel.clone(),
                    size,
                    mtime_secs: live.mtime_secs,
                    sha256: hash,
                    present: true,
                    last_seen_secs: now_secs,
                });
            }
            None => {
                // Listed but unreadable upstream: preserve, keep marked present.
                if let Some((prev_abs, prev_entry, prev_hash)) = carry.get(&live.rel) {
                    log::warn!(
                        "Upstream file {} unreadable, keeping preserved copy",
                        live.rel
                    );
                    link_or_copy(prev_abs, &staging_data_dir.join(native_path(&live.rel)))?;
                    entries.push(ManifestFileEntry {
                        path: live.rel.clone(),
                        size: prev_entry.size,
                        mtime_secs: prev_entry.mtime_secs,
                        sha256: prev_hash.clone(),
                        present: true,
                        last_seen_secs: prev_entry.last_seen_secs,
                    });
                }
            }
        }
    }
    // Deleted upstream (or failed SQLite captures flagged to keep): carry
    // forward, marked by presence.
    for (rel, (prev_abs, prev_entry, prev_hash)) in &carry {
        if seen_live.contains_key(rel) {
            continue;
        }
        link_or_copy(prev_abs, &staging_data_dir.join(native_path(rel)))?;
        entries.push(ManifestFileEntry {
            path: rel.clone(),
            size: prev_entry.size,
            mtime_secs: prev_entry.mtime_secs,
            sha256: prev_hash.clone(),
            present: db_keep_present.contains_key(rel),
            last_seen_secs: prev_entry.last_seen_secs,
        });
    }
    Ok(entries)
}

/// Hardlink `src` onto `dest` (creating parents), falling back to a copy, and
/// finally to reading+writing bytes when even the read needs help. The last
/// resort reads `src` fully; callers pass it only for CCHV-owned files.
fn link_or_copy(src: &Path, dest: &Path) -> Result<(), String> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create staging parent {}: {e}", parent.display()))?;
    }
    if std::fs::hard_link(src, dest).is_ok() {
        return Ok(());
    }
    // Cross-device or hardlink-less filesystems: plain copy of owned bytes.
    // (Copying over an existing dest must keep it writable for later stages:
    // some platforms preserve the source mode bits, which would freeze a
    // read-only mode onto staging.)
    std::fs::copy(src, dest).map_err(|e| format!("Failed to stage {}: {e}", dest.display()))?;
    make_staging_writable(dest);
    Ok(())
}

/// Stage new bytes, reusing identical CAS content when already archived.
fn stage_bytes(
    data_root: &Path,
    staging_data_dir: &Path,
    rel: &str,
    bytes: &[u8],
) -> Result<(), String> {
    let dest = staging_data_dir.join(native_path(rel));
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create staging parent {}: {e}", parent.display()))?;
    }
    let hash = sha256_hex(bytes);
    if cas::cas_link_into(data_root, &hash, &dest) {
        return Ok(());
    }
    // A previous stage step may have left a read-only file here (e.g. a
    // mode-preserving copy); staging stays writable until publish.
    let _ = std::fs::remove_file(&dest);
    std::fs::write(&dest, bytes).map_err(|e| format!("Failed to stage {rel}: {e}"))?;
    make_staging_writable(&dest);
    // Best effort: CAS population must never fail a sync.
    let _ = cas::cas_insert_bytes(data_root, bytes);
    Ok(())
}

/// Keep staging files owner-writable (publication freezes them later).
fn make_staging_writable(dest: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if let Ok(meta) = std::fs::metadata(dest) {
            let mut perms = meta.permissions();
            perms.set_mode(perms.mode() | 0o200);
            let _ = std::fs::set_permissions(dest, perms);
        }
    }
    #[cfg(windows)]
    {
        if let Ok(meta) = std::fs::metadata(dest) {
            let mut perms = meta.permissions();
            perms.set_readonly(false);
            let _ = std::fs::set_permissions(dest, perms);
        }
    }
}

// ---------------------------------------------------------------------------
// Local tree walk (produces LiveFiles; bytes stay in hand for hashing).
// ---------------------------------------------------------------------------

/// Collect currently observable files under `live_root`, honoring `includes`
/// subtree prefixes (empty = everything). Symlinks are dereferenced into
/// plain entries; CCHV-derived caches are skipped; listed-but-unreadable
/// files yield `bytes: None` so assembly can carry the preserved copy.
#[allow(clippy::too_many_arguments)]
fn collect_live_tree(
    live_root: &Path,
    current_dir: &Path,
    includes: &[String],
    max_depth: Option<usize>,
    depth: usize,
    skip_relpaths: &std::collections::HashSet<String>,
    out: &mut Vec<LiveFile>,
) -> Result<(), String> {
    let read_dir = std::fs::read_dir(current_dir)
        .map_err(|e| format!("Failed to list {}: {e}", current_dir.display()))?;
    let mut children: Vec<_> = read_dir.flatten().collect();
    children.sort_by_key(std::fs::DirEntry::file_name);

    for child in children {
        let path = child.path();
        let name = child.file_name().to_string_lossy().to_string();
        if name == ".session_cache.json" {
            continue;
        }
        let meta = std::fs::symlink_metadata(&path)
            .map_err(|e| format!("Failed to stat {}: {e}", path.display()))?;
        if meta.file_type().is_symlink() {
            // Dereference into real entries so snapshots hold no links.
            let Ok(target_meta) = std::fs::metadata(&path) else {
                continue;
            };
            if target_meta.is_dir() {
                collect_symlinked_dir(live_root, &path, includes, skip_relpaths, out);
            } else if target_meta.is_file() {
                push_live_file(live_root, &path, includes, skip_relpaths, out);
            }
            continue;
        }
        if meta.is_dir() {
            // Depth counts the root's children as 1; unbounded when unset.
            if max_depth.is_some_and(|max| depth + 1 > max) {
                continue;
            }
            if dir_pruned(live_root, &path, includes) {
                continue;
            }
            collect_live_tree(
                live_root,
                &path,
                includes,
                max_depth,
                depth + 1,
                skip_relpaths,
                out,
            )?;
        } else if meta.is_file() {
            push_live_file(live_root, &path, includes, skip_relpaths, out);
        }
    }
    Ok(())
}

fn include_prefixes(includes: &[String]) -> Vec<String> {
    includes
        .iter()
        .map(|s| s.trim().trim_start_matches('/').replace('\\', "/"))
        .filter(|s| !s.is_empty())
        .collect()
}

/// Whether a directory can hold no included file and may be pruned.
fn dir_pruned(live_root: &Path, dir: &Path, includes: &[String]) -> bool {
    let prefixes = include_prefixes(includes);
    if prefixes.is_empty() {
        return false;
    }
    let Some(rel) = relative_posix(live_root, dir) else {
        return true;
    };
    !prefixes.iter().any(|inc| {
        inc == &rel || inc.starts_with(&format!("{rel}/")) || rel.starts_with(&format!("{inc}/"))
    })
}

fn file_included(live_root: &Path, file: &Path, includes: &[String]) -> Option<String> {
    let prefixes = include_prefixes(includes);
    let rel = relative_posix(live_root, file)?;
    if prefixes.is_empty() {
        return Some(rel);
    }
    prefixes
        .iter()
        .any(|inc| inc == &rel || rel.starts_with(&format!("{inc}/")))
        .then_some(rel)
}

#[allow(clippy::too_many_arguments)]
fn push_live_file(
    live_root: &Path,
    source_file: &Path,
    includes: &[String],
    skip_relpaths: &std::collections::HashSet<String>,
    out: &mut Vec<LiveFile>,
) {
    let Some(rel) = file_included(live_root, source_file, includes) else {
        return;
    };
    // SQLite stores are captured exclusively through the backup API; a torn
    // plain copy must never compete with (or duplicate) a backup.
    if skip_relpaths.contains(&rel) {
        return;
    }
    let mtime_secs = live_mtime(source_file);
    match std::fs::read(source_file) {
        Ok(bytes) => out.push(LiveFile {
            rel,
            bytes: Some(bytes),
            mtime_secs,
        }),
        Err(e) => {
            // Listed but unreadable (rotated/deleted mid-walk): assembly
            // carries the preserved copy when one exists.
            log::warn!(
                "Snapshot listed but could not read {}: {e}",
                source_file.display()
            );
            out.push(LiveFile {
                rel,
                bytes: None,
                mtime_secs,
            });
        }
    }
}

/// Walk a symlinked directory's target, recording entries under the link's
/// logical path (one dereference level; deeper links resolve files only).
/// Dangling links and archive escapes yield no entries; every other outcome
/// is infallible by construction.
#[allow(clippy::too_many_arguments)]
fn collect_symlinked_dir(
    live_root: &Path,
    link_path: &Path,
    includes: &[String],
    skip_relpaths: &std::collections::HashSet<String>,
    out: &mut Vec<LiveFile>,
) {
    let Ok(target) = std::fs::canonicalize(link_path) else {
        return;
    };
    if let Ok(root) = snapshot::data_root() {
        if target == root || target.starts_with(root.join("sources")) {
            return;
        }
    }
    let mut stack = vec![target.clone()];
    // Cycle guard: never revisit a canonical directory through links.
    let mut visited = std::collections::HashSet::new();
    visited.insert(target.clone());
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
                if let Ok(target_meta) = std::fs::metadata(&path) {
                    if target_meta.is_file() {
                        // Map target file back under the link path, relative
                        // to the canonical target root (correct at any depth).
                        let Ok(relative_target) = path.strip_prefix(&target) else {
                            continue;
                        };
                        let virtual_source = link_path.join(relative_target);
                        push_live_file_via(
                            live_root,
                            &virtual_source,
                            includes,
                            skip_relpaths,
                            out,
                        );
                    } else if target_meta.is_dir() {
                        let canonical =
                            std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
                        if visited.insert(canonical) {
                            stack.push(path);
                        }
                    }
                }
                continue;
            }
            if meta.is_dir() {
                stack.push(path);
            } else if meta.is_file() {
                // Every stack entry descends from the canonical target root,
                // so relativizing against it stays correct at any depth.
                let Ok(relative_target) = path.strip_prefix(&target) else {
                    continue;
                };
                let virtual_source = link_path.join(relative_target);
                push_live_file_via(live_root, &virtual_source, includes, skip_relpaths, out);
            }
        }
    }
}

/// Push a file addressed through a symlink (reads follow the link).
fn push_live_file_via(
    live_root: &Path,
    virtual_source: &Path,
    includes: &[String],
    skip_relpaths: &std::collections::HashSet<String>,
    out: &mut Vec<LiveFile>,
) {
    // `virtual_source` may itself contain symlink components; resolve the
    // logical relpath lexically against the live root.
    let rel = match virtual_source.strip_prefix(live_root) {
        Ok(rel) => {
            let mut parts = Vec::new();
            let mut ok = true;
            for component in rel.components() {
                use std::path::Component as C;
                match component {
                    C::Normal(part) => parts.push(part.to_string_lossy().to_string()),
                    C::CurDir => {}
                    _ => {
                        ok = false;
                        break;
                    }
                }
            }
            if !ok || parts.is_empty() {
                return;
            }
            parts.join("/")
        }
        Err(_) => return,
    };
    let prefixes = include_prefixes(includes);
    if !prefixes.is_empty()
        && !prefixes
            .iter()
            .any(|inc| inc == &rel || rel.starts_with(&format!("{inc}/")))
    {
        return;
    }
    if skip_relpaths.contains(&rel) {
        return;
    }
    let mtime_secs = live_mtime(virtual_source);
    match std::fs::read(virtual_source) {
        Ok(bytes) => out.push(LiveFile {
            rel,
            bytes: Some(bytes),
            mtime_secs,
        }),
        Err(_) => out.push(LiveFile {
            rel,
            bytes: None,
            mtime_secs,
        }),
    }
}

// ---------------------------------------------------------------------------
// Remote snapshots (all-or-nothing over an accepted manifest).
// ---------------------------------------------------------------------------

/// One remotely-fetched provider file: bytes already transferred, path
/// relative to the provider root with `/` separators.
#[derive(Debug, Clone)]
pub struct RemoteSyncedFile {
    pub path: String,
    pub bytes: Vec<u8>,
    pub mtime_secs: i64,
}

/// Sync remotely-fetched provider files into a new cumulative snapshot.
///
/// `expected` is the accepted remote manifest as `(path, sha256)` pairs
/// (`sha256` may be empty for pre-v2 remotes, which skips hash verification).
/// Every expected path must be supplied exactly once with matching bytes, or
/// the whole sync fails and the previous completed snapshot stays current.
/// A genuinely empty remote root (`expected` empty) carries history forward
/// and reports `Unchanged` when nothing changed.
pub fn sync_remote_snapshot(
    source: &Source,
    remote_root: &str,
    expected: &[(String, String)],
    files: Vec<RemoteSyncedFile>,
) -> Result<SyncOutcome, String> {
    with_source_lock(&source.id, || {
        sync_remote_locked(source, remote_root, expected, files)
    })
}

fn sync_remote_locked(
    source: &Source,
    remote_root: &str,
    expected: &[(String, String)],
    files: Vec<RemoteSyncedFile>,
) -> Result<SyncOutcome, String> {
    if source.kind != crate::storage::SourceKind::Remote {
        return Err("Remote snapshot sync requires a remote source".to_string());
    }
    // CCHV-derived caches are never part of snapshots, even if a manifest
    // lists them: filter both sides identically so set equality still holds.
    let is_derived =
        |path: &str| path == ".session_cache.json" || path.ends_with("/.session_cache.json");
    let expected: Vec<(String, String)> = expected
        .iter()
        .filter(|(path, _)| {
            let keep = !is_derived(path);
            if !keep {
                log::debug!("Ignoring derived cache entry in remote manifest: {path}");
            }
            keep
        })
        .cloned()
        .collect();
    let files: Vec<RemoteSyncedFile> = files
        .into_iter()
        .filter(|file| !is_derived(&file.path))
        .collect();
    let expected_map: BTreeMap<&str, &str> = expected
        .iter()
        .map(|(p, h)| (p.as_str(), h.as_str()))
        .collect();
    if expected_map.len() != expected.len() {
        return Err("Remote manifest lists duplicate paths".to_string());
    }
    // All-or-nothing: every required file present exactly once, no extras.
    if files.len() != expected_map.len() {
        return Err(format!(
            "Incomplete remote transfer: {} of {} required files",
            files.len(),
            expected_map.len()
        ));
    }
    let mut seen = std::collections::HashSet::new();
    for file in &files {
        if !seen.insert(file.path.clone()) {
            return Err(format!("Duplicate remote file {}", file.path));
        }
        let Some(want_hash) = expected_map.get(file.path.as_str()) else {
            return Err(format!("Unexpected remote file {}", file.path));
        };
        validate_remote_relpath(&file.path)?;
        if !want_hash.is_empty() {
            let actual = sha256_hex(&file.bytes);
            if actual != *want_hash {
                return Err(format!(
                    "Hash mismatch for remote file {} (transfer corrupt)",
                    file.path
                ));
            }
        }
        if file.path == ".session_cache.json" || file.path.ends_with("/.session_cache.json") {
            return Err(format!(
                "Remote manifest lists derived cache file {}",
                file.path
            ));
        }
    }

    let root = snapshot::data_root()?;
    let effective = crate::storage::source::claim_canonical_source(source)?;
    let previous_latest = crate::storage::latest_completed_snapshot(&effective.id);
    let carry_from = carry_base_snapshots(&root, &effective.id, previous_latest.as_ref());

    let snapshot_id = snapshot::allocate_snapshot_id();
    let staging_snapshot_dir = snapshot::staging_root(&root, &effective.id).join(&snapshot_id);
    let staging_data_dir = staging_snapshot_dir.join("data");
    std::fs::create_dir_all(&staging_data_dir).map_err(|e| {
        format!(
            "Failed to create staging dir {}: {e}",
            staging_data_dir.display()
        )
    })?;

    let now_secs = unix_now_secs();
    let live_files: Vec<LiveFile> = files
        .into_iter()
        .map(|f| LiveFile {
            rel: f.path,
            bytes: Some(f.bytes),
            mtime_secs: f.mtime_secs,
        })
        .collect();
    let entries = assemble_candidate(
        &root,
        &staging_data_dir,
        &carry_from,
        live_files,
        &HashMap::new(),
        now_secs,
    )?;

    let created_at = chrono::Utc::now().to_rfc3339();
    let mut view = effective.clone();
    view.original_root = Some(remote_root.to_string());
    let mut manifest = SnapshotManifest::completed(&view, &snapshot_id, &created_at, entries);
    manifest.original_root = Some(remote_root.to_string());
    manifest.endpoint.clone_from(&effective.endpoint);
    finish_staged_snapshot(
        &root,
        &effective,
        &snapshot_id,
        &staging_snapshot_dir,
        manifest,
        previous_latest.as_ref(),
    )
}

/// Backwards-compatible wrapper used while callers migrate to hash-verified
/// manifests: completeness is enforced by set equality against `expected`.
pub fn sync_remote_files_to_snapshot(
    source: &Source,
    remote_root: &str,
    files: Vec<RemoteSyncedFile>,
) -> Result<SyncOutcome, String> {
    let expected: Vec<(String, String)> = files
        .iter()
        .map(|f| (f.path.clone(), String::new()))
        .collect();
    sync_remote_snapshot(source, remote_root, &expected, files)
}

fn validate_remote_relpath(rel: &str) -> Result<(), String> {
    if rel.trim().is_empty()
        || rel.starts_with('/')
        || rel
            .split('/')
            .any(|seg| seg.is_empty() || seg == "." || seg == "..")
    {
        return Err(format!("Refusing to stage unsafe remote path {rel:?}"));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Finalize: verify, compare, publish-or-discard.
// ---------------------------------------------------------------------------

/// Shared finalize: write manifest, verify staging, publish when the
/// fingerprint set differs from the previous snapshot, otherwise discard the
/// provably identical staging directory and report `Unchanged`.
fn finish_staged_snapshot(
    data_root: &Path,
    source: &Source,
    snapshot_id: &str,
    staging_snapshot_dir: &Path,
    manifest: SnapshotManifest,
    previous: Option<&crate::storage::SnapshotInfo>,
) -> Result<SyncOutcome, String> {
    let manifest_bytes = serde_json::to_vec_pretty(&manifest)
        .map_err(|e| format!("Failed to encode snapshot manifest: {e}"))?;
    std::fs::write(staging_snapshot_dir.join("manifest.json"), &manifest_bytes)
        .map_err(|e| format!("Failed to write staging manifest: {e}"))?;

    verify_staging_usable(staging_snapshot_dir, &manifest)?;

    if let Some(prev) = previous {
        if same_fingerprint_set(&prev.manifest, &manifest) {
            // Proven byte-identical candidate: no unique history lives here,
            // so discard staging instead of accumulating it.
            cleanup_staging(staging_snapshot_dir);
            return Ok(SyncOutcome::Unchanged(prev.clone()));
        }
    }

    // Operational immutability: files read-only before publication.
    snapshot::make_snapshot_files_read_only(staging_snapshot_dir);
    publish_staging(data_root, &source.id, snapshot_id, staging_snapshot_dir)?;

    let published = crate::storage::latest_completed_snapshot(&source.id)
        .ok_or_else(|| "Snapshot publish succeeded but latest snapshot is missing".to_string())?;
    Ok(SyncOutcome::Created(published))
}

/// Best-effort removal of a staging directory. Never touches completed data.
fn cleanup_staging(staging_dir: &Path) {
    if let Err(e) = std::fs::remove_dir_all(staging_dir) {
        log::warn!("Failed to discard staging {}: {e}", staging_dir.display());
    }
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
    // The stored manifest must round-trip (a corrupt manifest is unusable).
    let stored = std::fs::read(snapshot::manifest_path(staging_snapshot_dir))
        .map_err(|e| format!("Staged manifest went missing: {e}"))?;
    let parsed: SnapshotManifest =
        serde_json::from_slice(&stored).map_err(|e| format!("Staged manifest corrupt: {e}"))?;
    if parsed.files.len() != manifest.files.len() {
        return Err("Staged manifest does not match assembled entries".to_string());
    }
    Ok(())
}

/// Snapshot equality on content identity: same `(path, sha256, present)` set.
/// Size/mtime are optimization metadata and never decide equality. Entries
/// with unknown hashes (v1 carry-overs that somehow stayed unhashed) compare
/// by size+mtime as a conservative fallback that errs toward publishing.
fn same_fingerprint_set(a: &SnapshotManifest, b: &SnapshotManifest) -> bool {
    if a.files.len() != b.files.len() {
        return false;
    }
    let key = |f: &ManifestFileEntry| {
        if f.sha256.is_empty() {
            format!("{}|{}|{}|{}", f.path, f.size, f.mtime_secs, f.present)
        } else {
            format!("{}|{}|{}", f.path, f.sha256, f.present)
        }
    };
    let mut left: Vec<String> = a.files.iter().map(key).collect();
    let mut right: Vec<String> = b.files.iter().map(key).collect();
    left.sort();
    right.sort();
    left == right
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
    use std::io::Write as _;

    fn test_source(provider: &str, machine: &str, root: &Path) -> Source {
        Source::local(provider, machine, root)
    }

    fn unique_local_source(provider: &str, root: &Path) -> Source {
        // Unique temp roots give unique deterministic ids without randomness.
        Source::local(provider, "test-machine", root)
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

        let source = unique_local_source("claude", &root);
        let first = match sync_local_directory(&source, &root).unwrap() {
            SyncOutcome::Created(snap) => snap,
            SyncOutcome::Unchanged(_) => panic!("first sync must create"),
        };
        assert_eq!(first.manifest.version, 2);
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

        let latest = crate::storage::latest_completed_snapshot(&source.id).unwrap();
        assert_eq!(latest.snapshot_id, second.snapshot_id);
    }

    #[test]
    #[serial_test::serial]
    fn deletion_stays_preserved_in_latest_snapshot() {
        let _sandbox = crate::test_utils::SandboxHome::new();
        let original = tempfile::tempdir().unwrap();
        let root = original.path().join(".claude");
        let project = root.join("projects").join("p");
        write_file(&project.join("keep.jsonl"), b"keep\n");
        write_file(&project.join("gone.jsonl"), b"gone\n");

        let source = unique_local_source("claude", &root);
        match sync_local_directory(&source, &root).unwrap() {
            SyncOutcome::Created(_) => {}
            SyncOutcome::Unchanged(_) => panic!("must create"),
        }

        // Upstream deletes a file; the next snapshot must still carry it.
        std::fs::remove_file(project.join("gone.jsonl")).unwrap();
        let second = match sync_local_directory(&source, &root).unwrap() {
            SyncOutcome::Created(snap) => snap,
            SyncOutcome::Unchanged(_) => panic!("deletion must create a new snapshot"),
        };
        let carried = second.data_path.join("projects/p/gone.jsonl");
        assert!(
            carried.is_file(),
            "deleted file must stay in latest snapshot"
        );
        assert_eq!(std::fs::read(&carried).unwrap(), b"gone\n");
        let entry = second
            .manifest
            .files
            .iter()
            .find(|f| f.path == "projects/p/gone.jsonl")
            .expect("manifest lists carried file");
        assert!(!entry.present, "carried file marked not-present");
        assert!(!entry.sha256.is_empty(), "carried file keeps its hash");
    }

    #[test]
    #[serial_test::serial]
    fn sync_survives_source_deletion() {
        let _sandbox = crate::test_utils::SandboxHome::new();
        let original = tempfile::tempdir().unwrap();
        let root = original.path().join(".claude");
        write_file(&root.join("projects/p/s.jsonl"), b"hello\n");

        let source = unique_local_source("claude", &root);
        let snap = match sync_local_directory(&source, &root).unwrap() {
            SyncOutcome::Created(s) => s,
            SyncOutcome::Unchanged(_) => panic!("must create"),
        };
        let data_path = snap.data_path.join("projects/p/s.jsonl");
        assert!(data_path.is_file());

        std::fs::remove_dir_all(&root).unwrap();
        assert!(!root.exists());
        assert!(data_path.is_file());
        assert_eq!(std::fs::read(&data_path).unwrap(), b"hello\n");
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
        let source = unique_local_source("claude", &root);
        let first = match sync_local_directory(&source, &root).unwrap() {
            SyncOutcome::Created(s) => s,
            SyncOutcome::Unchanged(_) => panic!("must create"),
        };
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
        let source = unique_local_source("claude", &root);
        let first = match sync_local_directory(&source, &root).unwrap() {
            SyncOutcome::Created(s) => s,
            SyncOutcome::Unchanged(_) => panic!("must create"),
        };
        let marker = first.data_path.join("projects/p/s.jsonl");
        let before = std::fs::read(&marker).unwrap();
        write_file(
            &root.join("projects/p/s.jsonl"),
            b"v2 much longer content\n",
        );
        let _ = sync_local_directory(&source, &root).unwrap();
        assert_eq!(std::fs::read(&marker).unwrap(), before);
        assert_eq!(before, b"v1\n");
    }

    #[test]
    #[serial_test::serial]
    fn unchanged_source_discards_staging() {
        let _sandbox = crate::test_utils::SandboxHome::new();
        let original = tempfile::tempdir().unwrap();
        let root = original.path().join(".claude");
        write_file(&root.join("projects/p/s.jsonl"), b"same\n");
        let source = unique_local_source("claude", &root);
        let first = match sync_local_directory(&source, &root).unwrap() {
            SyncOutcome::Created(s) => s,
            SyncOutcome::Unchanged(_) => panic!("must create"),
        };
        match sync_local_directory(&source, &root).unwrap() {
            SyncOutcome::Unchanged(s) => assert_eq!(s.snapshot_id, first.snapshot_id),
            SyncOutcome::Created(_) => panic!("identical tree must not create"),
        }
        // Provably identical staging is discarded, not accumulated.
        let data_root = crate::storage::snapshot::data_root().unwrap();
        let staged: usize = std::fs::read_dir(crate::storage::snapshot::staging_root(
            &data_root, &source.id,
        ))
        .map(std::iter::Iterator::count)
        .unwrap_or(0);
        assert_eq!(staged, 0, "identical staging must be discarded");
    }

    #[test]
    #[serial_test::serial]
    fn completed_snapshot_files_are_read_only() {
        let _sandbox = crate::test_utils::SandboxHome::new();
        let original = tempfile::tempdir().unwrap();
        let root = original.path().join(".claude");
        write_file(&root.join("projects/p/s.jsonl"), b"data\n");
        let source = unique_local_source("claude", &root);
        let snap = match sync_local_directory(&source, &root).unwrap() {
            SyncOutcome::Created(s) => s,
            SyncOutcome::Unchanged(_) => panic!("must create"),
        };
        let preserved = snap.data_path.join("projects/p/s.jsonl");
        let opened = std::fs::OpenOptions::new().write(true).open(&preserved);
        assert!(
            opened.is_err(),
            "completed snapshot files must not be writable"
        );
    }

    #[test]
    #[serial_test::serial]
    fn same_fingerprint_set_ignores_size_mtime() {
        use crate::storage::ManifestFileEntry;
        let entry = |hash: &str| ManifestFileEntry {
            path: "a.jsonl".to_string(),
            size: 10,
            mtime_secs: 100,
            sha256: hash.to_string(),
            present: true,
            last_seen_secs: 0,
        };
        let mut renamed = entry("h1");
        renamed.size = 999;
        renamed.mtime_secs = 9999;
        let a = SnapshotManifest::completed(
            &test_source("claude", "m", Path::new("/r")),
            "s1",
            "t",
            vec![entry("h1")],
        );
        let b = SnapshotManifest::completed(
            &test_source("claude", "m", Path::new("/r")),
            "s2",
            "t",
            vec![renamed],
        );
        assert!(same_fingerprint_set(&a, &b));
        let mut changed = entry("h2");
        changed.size = 10;
        let c = SnapshotManifest::completed(
            &test_source("claude", "m", Path::new("/r")),
            "s3",
            "t",
            vec![changed],
        );
        assert!(!same_fingerprint_set(&a, &c));
        let mut gone = entry("h1");
        gone.present = false;
        let d = SnapshotManifest::completed(
            &test_source("claude", "m", Path::new("/r")),
            "s4",
            "t",
            vec![gone],
        );
        assert!(!same_fingerprint_set(&a, &d));
    }

    #[test]
    #[serial_test::serial]
    fn v1_manifests_stay_readable_and_upgrade() {
        let _sandbox = crate::test_utils::SandboxHome::new();
        let data_root = crate::storage::snapshot::data_root().unwrap();
        let source = unique_local_source("claude", Path::new("/v1root"));
        let dir = data_root
            .join("sources")
            .join(&source.id)
            .join("snapshots")
            .join("v1-snap");
        std::fs::create_dir_all(dir.join("data/projects")).unwrap();
        std::fs::write(dir.join("data/projects/old.jsonl"), b"old\n").unwrap();
        // Hand-written f94c008-era manifest (no hashes, no presence flags).
        std::fs::write(
            dir.join("manifest.json"),
            serde_json::json!({
                "version": 1,
                "source_id": source.id,
                "snapshot_id": "v1-snap",
                "created_at": "2026-01-01T00:00:00Z",
                "provider": "claude",
                "source_kind": "local",
                "original_root": "/v1root",
                "endpoint": null,
                "status": "completed",
                "files": [{"path": "projects/old.jsonl", "size": 4, "mtime_secs": 10}],
                "file_count": 1,
                "total_bytes": 4
            })
            .to_string(),
        )
        .unwrap();
        let found = crate::storage::list_snapshots(&source.id);
        assert_eq!(found.len(), 1, "v1 snapshot must stay discoverable");
        assert!(found[0].manifest.files[0].sha256.is_empty());
        assert!(found[0].manifest.files[0].present);
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

        let sessions =
            crate::commands::session::load_project_sessions(project_path.clone(), Some(false))
                .await
                .expect("sessions load from snapshot");
        assert_eq!(sessions.len(), 1);
        let session_file = sessions[0].file_path.clone();

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

        let source = crate::commands::project::claude_source_for_base("test-machine", &base, None);
        let first = match sync_local_directory(&source, &base).unwrap() {
            SyncOutcome::Created(s) => s,
            SyncOutcome::Unchanged(_) => panic!("first sync must create"),
        };

        // Append a new message to the live session.
        {
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
        // The latest snapshot carries both the new bytes and full history.
        let sessions = crate::commands::session::load_project_sessions(
            project_dir.to_string_lossy().to_string(),
            Some(false),
        )
        .await
        .unwrap();
        assert_eq!(sessions.len(), 1);

        let snapshots = crate::storage::list_snapshots(&source.id);
        assert!(snapshots.len() >= 2);
    }

    /// Data-disappearance conformance: delete A, modify B, add C upstream.
    /// The latest snapshot must serve A (preserved), B-new, and C, while the
    /// older snapshot keeps B-old. No history disappears.
    #[test]
    #[serial_test::serial]
    fn disappearance_conformance_delete_modify_add() {
        let _sandbox = crate::test_utils::SandboxHome::new();
        let original = tempfile::tempdir().unwrap();
        let root = original.path().join("src");
        write_file(&root.join("a.jsonl"), b"A-v1\n");
        write_file(&root.join("b.jsonl"), b"B-v1\n");

        let source = unique_local_source("p", &root);
        let first = match sync_local_directory(&source, &root).unwrap() {
            SyncOutcome::Created(s) => s,
            SyncOutcome::Unchanged(_) => panic!("must create"),
        };

        std::fs::remove_file(root.join("a.jsonl")).unwrap();
        write_file(&root.join("b.jsonl"), b"B-v2-longer\n");
        write_file(&root.join("c.jsonl"), b"C-v1\n");
        let second = match sync_local_directory(&source, &root).unwrap() {
            SyncOutcome::Created(s) => s,
            SyncOutcome::Unchanged(_) => panic!("changes must create"),
        };

        // Latest serves everything.
        assert_eq!(
            std::fs::read(second.data_path.join("a.jsonl")).unwrap(),
            b"A-v1\n"
        );
        assert_eq!(
            std::fs::read(second.data_path.join("b.jsonl")).unwrap(),
            b"B-v2-longer\n"
        );
        assert_eq!(
            std::fs::read(second.data_path.join("c.jsonl")).unwrap(),
            b"C-v1\n"
        );
        let presents: std::collections::HashMap<_, _> = second
            .manifest
            .files
            .iter()
            .map(|f| (f.path.as_str(), f.present))
            .collect();
        assert_eq!(presents.get("a.jsonl"), Some(&false));
        assert_eq!(presents.get("b.jsonl"), Some(&true));
        assert_eq!(presents.get("c.jsonl"), Some(&true));

        // Older snapshot keeps B-old untouched.
        assert_eq!(
            std::fs::read(first.data_path.join("b.jsonl")).unwrap(),
            b"B-v1\n"
        );
        assert!(first.data_path.join("a.jsonl").is_file());
    }

    /// Concurrent provider writes during capture must yield consistent
    /// snapshots or a clean rejection — never a torn published database.
    #[test]
    #[serial_test::serial]
    fn sqlite_capture_under_concurrent_writes() {
        let _sandbox = crate::test_utils::SandboxHome::new();
        let original = tempfile::tempdir().unwrap();
        let root = original.path().join("mixed");
        let db_path = root.join("store.db");
        std::fs::create_dir_all(&root).unwrap();
        {
            let conn = rusqlite::Connection::open(&db_path).unwrap();
            conn.execute_batch(
                "CREATE TABLE events(id INTEGER PRIMARY KEY, body TEXT);
                 INSERT INTO events(body) VALUES ('seed');",
            )
            .unwrap();
        }
        write_file(&root.join("note.jsonl"), b"{}\n");

        let mut source = unique_local_source("mixedprov", &root);
        source.sqlite_dbs = vec!["store.db".to_string()];

        // Hammer the live database from another thread while syncing.
        let hammer = std::thread::spawn({
            let db_path = db_path.clone();
            move || {
                for i in 0..60 {
                    if let Ok(conn) = rusqlite::Connection::open(&db_path) {
                        let _ = conn.execute(
                            "INSERT INTO events(body) VALUES (?1)",
                            rusqlite::params![format!("w{i}")],
                        );
                    }
                    std::thread::sleep(std::time::Duration::from_millis(2));
                }
            }
        });
        for _ in 0..4 {
            // Syncs may carry the previous good copy while the db is hot;
            // they must never publish a corrupt one.
            let _ = sync_source(&source, &root, &SyncOptions::default());
        }
        hammer.join().unwrap();
        // Final quiescent sync converges.
        match sync_source(&source, &root, &SyncOptions::default()).unwrap() {
            SyncOutcome::Created(_) | SyncOutcome::Unchanged(_) => {}
        }

        // Every published snapshot's database validates.
        let data_root = crate::storage::snapshot::data_root().unwrap();
        let snapshots = crate::storage::snapshot::list_snapshots_in_root(&data_root, &source.id);
        assert!(!snapshots.is_empty());
        for snap in &snapshots {
            crate::storage::sqlite_capture::quick_check_db(&snap.data_path.join("store.db"))
                .unwrap();
        }
        // The plain-file sidecar rode along too.
        let latest = crate::storage::latest_completed_snapshot(&source.id).unwrap();
        assert!(latest.data_path.join("note.jsonl").is_file());
    }

    fn remote_source() -> Source {
        let mut source = Source {
            id: format!("test-remote-{}", &uuid::Uuid::new_v4().to_string()[..8]),
            kind: SourceKind::Remote,
            provider: "claude".to_string(),
            role: crate::storage::ROLE_PRIMARY.to_string(),
            machine_id: "remote-m".to_string(),
            origin: "/r/.claude".to_string(),
            original_root: None,
            endpoint: Some("http://example:3728".to_string()),
            label: None,
            sqlite_dbs: Vec::new(),
            includes: Vec::new(),
            max_depth: None,
        };
        // Claim first so the id is stable for the rest of the test.
        source = crate::storage::source::claim_canonical_source(&source).unwrap();
        source
    }

    fn remote_file(path: &str, bytes: &[u8]) -> RemoteSyncedFile {
        RemoteSyncedFile {
            path: path.to_string(),
            bytes: bytes.to_vec(),
            mtime_secs: 2,
        }
    }

    #[test]
    #[serial_test::serial]
    fn remote_partial_transfer_aborts_without_publish() {
        let _sandbox = crate::test_utils::SandboxHome::new();
        let source = remote_source();
        let expected = vec![
            ("projects/p/a.jsonl".to_string(), String::new()),
            ("projects/p/b.jsonl".to_string(), String::new()),
        ];
        // Only one of two required files arrives.
        let err = sync_remote_snapshot(
            &source,
            "/r/.claude",
            &expected,
            vec![remote_file("projects/p/a.jsonl", b"a")],
        )
        .unwrap_err();
        assert!(err.contains("Incomplete remote transfer"), "got: {err}");
        assert!(crate::storage::latest_completed_snapshot(&source.id).is_none());
    }

    #[test]
    #[serial_test::serial]
    fn remote_hash_mismatch_aborts() {
        let _sandbox = crate::test_utils::SandboxHome::new();
        let source = remote_source();
        let good_hash = crate::storage::hash::sha256_hex(b"a");
        let expected = vec![("projects/p/a.jsonl".to_string(), good_hash)];
        let err = sync_remote_snapshot(
            &source,
            "/r/.claude",
            &expected,
            vec![remote_file("projects/p/a.jsonl", b"tampered")],
        )
        .unwrap_err();
        assert!(err.contains("Hash mismatch"), "got: {err}");
        assert!(crate::storage::latest_completed_snapshot(&source.id).is_none());
    }

    #[test]
    #[serial_test::serial]
    fn remote_same_bytes_new_snapshot_on_content_change() {
        // P0 acceptance: same path + same size + same mtime + changed bytes
        // must still publish, because equality is hash-based.
        let _sandbox = crate::test_utils::SandboxHome::new();
        let source = remote_source();
        let expected = vec![("projects/p/a.jsonl".to_string(), String::new())];
        let first = match sync_remote_snapshot(
            &source,
            "/r/.claude",
            &expected,
            vec![remote_file("projects/p/a.jsonl", b"12345678")],
        )
        .unwrap()
        {
            SyncOutcome::Created(s) => s,
            SyncOutcome::Unchanged(_) => panic!("must create"),
        };
        let second = match sync_remote_snapshot(
            &source,
            "/r/.claude",
            &expected,
            vec![remote_file("projects/p/a.jsonl", b"87654321")],
        )
        .unwrap()
        {
            SyncOutcome::Created(s) => s,
            SyncOutcome::Unchanged(_) => panic!("changed bytes must create"),
        };
        assert_ne!(first.snapshot_id, second.snapshot_id);
        // Old bytes preserved in the older snapshot.
        assert_eq!(
            std::fs::read(first.data_path.join("projects/p/a.jsonl")).unwrap(),
            b"12345678"
        );
        // Identical re-transfer is Unchanged.
        match sync_remote_snapshot(
            &source,
            "/r/.claude",
            &expected,
            vec![remote_file("projects/p/a.jsonl", b"87654321")],
        )
        .unwrap()
        {
            SyncOutcome::Unchanged(s) => assert_eq!(s.snapshot_id, second.snapshot_id),
            SyncOutcome::Created(_) => panic!("identical must not create"),
        }
    }

    #[test]
    #[serial_test::serial]
    fn remote_file_sync_rejects_unsafe_paths_and_skips_cache() {
        let _sandbox = crate::test_utils::SandboxHome::new();
        let mut source = unique_local_source("claude", Path::new("/x"));
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
}
