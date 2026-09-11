//! `PearAI` provider.
//!
//! `PearAI` is a fork of Continue that rebrands the global directory from
//! `~/.continue` to `~/.pearai`. The session store format is identical
//! (`<sessionId>.json` + `sessions.json` index), so this module is a thin
//! wrapper over the shared [`super::continue_dev`] family core.
//!
//! Unlike Continue, `PearAI` here does NOT honor `CONTINUE_GLOBAL_DIR`: doing
//! so would let a Continue user's override dir be scanned twice (once per
//! provider) and mislabel Continue sessions as `PearAI`. `PearAI` uses
//! `~/.pearai` exclusively.

use super::continue_dev::{
    base_path_for, detect_for, load_messages_for, load_sessions_for, scan_projects_for, search_for,
    Family,
};
use super::ProviderInfo;
use crate::models::{ClaudeMessage, ClaudeProject, ClaudeSession};
use std::path::{Path, PathBuf};

pub(crate) const PEARAI: Family = Family {
    provider_id: "pearai",
    display_name: "PearAI",
    home_subdir: ".pearai",
    global_dir_env: None,
    scheme: "pearai://",
};

/// Detect a `PearAI` installation.
pub fn detect() -> Option<ProviderInfo> {
    detect_for(&PEARAI)
}

/// Base path for `PearAI` sessions: `~/.pearai/sessions`.
pub fn get_base_path() -> Option<String> {
    base_path_for(&PEARAI)
}

/// Scan `PearAI` projects under the default sessions root.
pub fn scan_projects() -> Result<Vec<ClaudeProject>, String> {
    scan_projects_for(&PEARAI)
}

/// Load the sessions belonging to one `PearAI` project.
pub fn load_sessions(
    project_path: &str,
    exclude_sidechain: bool,
) -> Result<Vec<ClaudeSession>, String> {
    load_sessions_for(&PEARAI, project_path, exclude_sidechain)
}

/// Load all messages from a single `PearAI` session file.
pub fn load_messages(session_path: &str) -> Result<Vec<ClaudeMessage>, String> {
    load_messages_for(&PEARAI, session_path)
}

/// Search across all `PearAI` sessions.
pub fn search(query: &str, limit: usize) -> Result<Vec<ClaudeMessage>, String> {
    search_for(&PEARAI, query, limit)
}

// ============================================================================
// Archive glue (explicit-root seams; see continue_dev).
// ============================================================================

/// Scan `PearAI` projects under an explicit sessions root.
pub(crate) fn scan_projects_in(base: &Path) -> Result<Vec<ClaudeProject>, String> {
    super::continue_dev::scan_in_for(&PEARAI, base)
}

/// Sessions for one project, listed from an explicit root.
pub(crate) fn load_sessions_in(
    base: &Path,
    project_path: &str,
    exclude_sidechain: bool,
) -> Result<Vec<ClaudeSession>, String> {
    super::continue_dev::load_sessions_in(&PEARAI, base, project_path, exclude_sidechain)
}

/// Messages confined to an explicit root.
pub(crate) fn load_messages_in(
    base: &Path,
    session_path: &str,
) -> Result<Vec<ClaudeMessage>, String> {
    super::continue_dev::load_messages_in(&PEARAI, base, session_path)
}

/// Search confined to an explicit root.
pub(crate) fn search_in(
    base: &Path,
    query: &str,
    limit: usize,
) -> Result<Vec<ClaudeMessage>, String> {
    super::continue_dev::search_in(&PEARAI, base, query, limit)
}

// ============================================================================
// Archive glue (snapshot-backed reads; the Continue family core is reused).
// ============================================================================

use crate::storage::registry::DiscoveredSource as ArchiveDiscoveredSource;
use crate::storage::{SnapshotInfo as ArchiveSnapshotInfo, Source as ArchiveSource};

/// Physical `PearAI` store root on this machine, if present.
pub(crate) fn archive_discover() -> Vec<ArchiveDiscoveredSource> {
    let machine = crate::storage::registry::discovery_machine_id();
    match base_path_for(&PEARAI) {
        Some(base) => vec![ArchiveDiscoveredSource::local(
            crate::storage::ROLE_PRIMARY,
            PathBuf::from(base),
            &machine,
        )],
        None => Vec::new(),
    }
}

/// Scan projects under an explicit root (snapshot data root at runtime).
pub(crate) fn archive_scan(
    _source: &ArchiveSource,
    snapshot: &ArchiveSnapshotInfo,
) -> Result<Vec<ClaudeProject>, String> {
    scan_projects_in(&snapshot.data_path)
}

/// Sessions for a stable project URI, filtered from snapshot content.
pub(crate) fn archive_load_sessions(
    _source: &ArchiveSource,
    snapshot: &ArchiveSnapshotInfo,
    stable_project: &str,
) -> Result<Vec<ClaudeSession>, String> {
    load_sessions_in(&snapshot.data_path, stable_project, false)
}

/// Messages for a stable session file, read from the snapshot.
pub(crate) fn archive_load_messages(
    source: &ArchiveSource,
    snapshot: &ArchiveSnapshotInfo,
    stable_session: &str,
) -> Result<Vec<ClaudeMessage>, String> {
    let mapped =
        crate::storage::registry::map_absolute_to_snapshot(source, snapshot, stable_session)
            .ok_or_else(|| format!("No preserved snapshot covers {stable_session}"))?;
    load_messages_in(&snapshot.data_path, &mapped)
}

/// Search confined to one snapshot.
pub(crate) fn archive_search(
    _source: &ArchiveSource,
    snapshot: &ArchiveSnapshotInfo,
    query: &str,
    limit: usize,
) -> Result<Vec<ClaudeMessage>, String> {
    search_in(&snapshot.data_path, query, limit)
}
