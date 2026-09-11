//! Derived-index maintenance: snapshot → parser → SQLite.
//!
//! Ordering is always filesystem-first. These functions run *after* a snapshot
//! is finalized, never before. Indexing is idempotent: every write goes
//! through the additive `merge_and_save_*` helpers and `mark_snapshot_indexed`
//! is an upsert, so a crash between finalize and commit is recovered by simply
//! re-running reconciliation.

use std::path::{Path, PathBuf};

/// Summary of a rebuild/reconcile pass.
#[derive(Debug, Clone, Default)]
pub struct IndexReport {
    pub sources_seen: usize,
    pub snapshots_seen: usize,
    pub snapshots_indexed: usize,
    pub snapshots_skipped: usize,
}

/// Read all registered sources from `sources/*/source.json`.
fn list_registered_sources() -> Vec<crate::storage::Source> {
    let mut out = Vec::new();
    let data_root = match crate::storage::snapshot::data_root() {
        Ok(root) => root,
        Err(_) => return out,
    };
    let sources_dir = data_root.join("sources");
    let Ok(entries) = std::fs::read_dir(&sources_dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let source_file = entry.path().join("source.json");
        let Ok(bytes) = std::fs::read(&source_file) else {
            continue;
        };
        if let Ok(source) = serde_json::from_slice::<crate::storage::Source>(&bytes) {
            out.push(source);
        }
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

/// Index one snapshot's projects + sessions into the derived SQLite index.
///
/// Paths stored in the index use the *original* locations (not the snapshot
/// interior) so the external contract is identical to a live scan. Messages
/// are read through from snapshots at open time and are not materialized here.
fn index_one_snapshot(
    conn: &rusqlite::Connection,
    source: &crate::storage::Source,
    snapshot: &crate::storage::SnapshotInfo,
) -> Result<(), String> {
    match source.provider.as_str() {
        "claude" => index_claude_snapshot(conn, source, snapshot),
        // Other providers follow the same shape as they migrate; unknown ones
        // only record index bookkeeping so reconciliation converges.
        _ => Ok(()),
    }
}

/// How indexed paths are keyed: local filesystem paths for local/custom
/// sources, `remote://` URLs for remote ones (matching the keys the remote
/// read path and the legacy cache already use).
enum IndexPathScheme {
    Local {
        original_base: PathBuf,
    },
    Remote {
        endpoint: String,
        remote_root: String,
    },
}

fn index_scheme_for(
    source: &crate::storage::Source,
    snapshot: &crate::storage::SnapshotInfo,
) -> IndexPathScheme {
    if source.kind == crate::storage::SourceKind::Remote {
        // Remote sources carry no local root; the synced root is recorded per
        // snapshot manifest instead (see `sync_remote_files_to_snapshot`).
        IndexPathScheme::Remote {
            endpoint: source.endpoint.clone().unwrap_or_default(),
            remote_root: snapshot.manifest.original_root.clone().unwrap_or_default(),
        }
    } else {
        IndexPathScheme::Local {
            original_base: source
                .original_root
                .as_deref()
                .map(PathBuf::from)
                .unwrap_or_default(),
        }
    }
}

fn index_claude_snapshot(
    conn: &rusqlite::Connection,
    source: &crate::storage::Source,
    snapshot: &crate::storage::SnapshotInfo,
) -> Result<(), String> {
    let scheme = index_scheme_for(source, snapshot);
    // Projects visible in this snapshot.
    let mut projects = crate::commands::project::scan_projects_from_root(&snapshot.data_path);
    for project in &mut projects {
        if let Some(key) = index_key(&scheme, &snapshot.data_path, Path::new(&project.path)) {
            project.path = key;
        }
        if project.provider.is_none() {
            project.provider = Some("claude".to_string());
        }
        if project.custom_directory_label.is_none() {
            project.custom_directory_label.clone_from(&source.label);
        }
    }
    if !projects.is_empty() {
        // Remote rows share the legacy host-scoped keys so the offline
        // fallback keeps working; local rows stay unscoped as before.
        let host_scope = match &scheme {
            IndexPathScheme::Remote { .. } => source.label.as_deref(),
            IndexPathScheme::Local { .. } => None,
        };
        crate::cache::db::merge_and_save_projects(conn, &projects, host_scope, Some("claude"))?;
    }

    // Sessions per snapshotted project directory.
    let projects_dir = snapshot.data_path.join("projects");
    let Ok(entries) = std::fs::read_dir(&projects_dir) else {
        return Ok(());
    };
    for entry in entries.flatten() {
        let snapshot_project_dir = entry.path();
        if !snapshot_project_dir.is_dir() {
            continue;
        }
        let mut sessions = crate::commands::session::load_project_sessions_blocking(
            snapshot_project_dir.to_string_lossy().to_string(),
            Some(false),
        );
        if sessions.is_empty() {
            continue;
        }
        let snapshot_prefix = snapshot_project_dir.to_string_lossy().to_string();
        let Some(index_project_path) =
            index_key(&scheme, &snapshot.data_path, &snapshot_project_dir)
        else {
            continue;
        };
        for session in &mut sessions {
            rewrite_session_prefix(session, &snapshot_prefix, &index_project_path);
            if session.provider.is_none() {
                session.provider = Some("claude".to_string());
            }
        }
        // Additive merge: sessions deleted from newer snapshots stay preserved.
        crate::cache::db::merge_and_save_sessions(conn, &index_project_path, "claude", &sessions)?;
    }
    Ok(())
}

/// Translate a snapshot-interior absolute path to its index key: the original
/// filesystem path for local/custom sources, the `remote://` URL for remote
/// ones. Returns `None` when the path is not under the snapshot root.
fn index_key(
    scheme: &IndexPathScheme,
    snapshot_data_root: &Path,
    snapshot_abs: &Path,
) -> Option<String> {
    let relative = snapshot_abs
        .strip_prefix(snapshot_data_root)
        .ok()?
        .to_string_lossy()
        .replace('\\', "/");
    match scheme {
        IndexPathScheme::Local { original_base } => Some(
            original_base
                .join(relative.split('/').collect::<PathBuf>())
                .to_string_lossy()
                .to_string(),
        ),
        IndexPathScheme::Remote {
            endpoint,
            remote_root,
        } => {
            let inner = if remote_root.is_empty() || relative.is_empty() {
                if relative.is_empty() {
                    remote_root.clone()
                } else {
                    relative
                }
            } else {
                format!("{}/{}", remote_root.trim_end_matches('/'), relative)
            };
            Some(crate::remote::format_remote_path(endpoint, &inner))
        }
    }
}

fn rewrite_session_prefix(
    session: &mut crate::models::ClaudeSession,
    snapshot_prefix: &str,
    original_prefix: &str,
) {
    if let Some(rest) = session.session_id.strip_prefix(snapshot_prefix) {
        session.session_id = format!("{original_prefix}{rest}");
    }
    if let Some(rest) = session.file_path.strip_prefix(snapshot_prefix) {
        session.file_path = format!("{original_prefix}{rest}");
    }
}

/// Index every completed snapshot not yet recorded in `snapshot_index`,
/// oldest first so additive merges converge to the union of all history.
/// This is the crash-recovery path: a snapshot finalized before a crash but
/// never indexed is discovered here on the next run.
pub fn reconcile_unindexed_snapshots() -> Result<IndexReport, String> {
    let conn = crate::cache::open_connection()?;
    let mut report = IndexReport::default();
    for source in list_registered_sources() {
        report.sources_seen += 1;
        let mut snapshots = crate::storage::list_snapshots(&source.id);
        // Oldest first for deterministic additive merging.
        snapshots.sort_by(|a, b| a.snapshot_id.cmp(&b.snapshot_id));
        for snapshot in snapshots {
            report.snapshots_seen += 1;
            if crate::cache::db::is_snapshot_indexed(&conn, &source.id, &snapshot.snapshot_id)? {
                report.snapshots_skipped += 1;
                continue;
            }
            // A torn snapshot must never poison the index: failures skip the
            // snapshot without marking it indexed, so a later run retries.
            index_one_snapshot(&conn, &source, &snapshot)?;
            crate::cache::db::mark_snapshot_indexed(&conn, &source.id, &snapshot.snapshot_id)?;
            report.snapshots_indexed += 1;
        }
    }
    Ok(report)
}

/// Rebuild the entire derived index from preserved snapshots.
///
/// Used after `rm cache.db` and covered by tests. Messages are not
/// materialized: they remain read-through from snapshots. Safe to run on a
/// live database; all writes are additive merges.
pub fn rebuild_index_from_snapshots() -> Result<IndexReport, String> {
    // Same implementation: unindexed snapshots are exactly what a fresh
    // database needs. Kept as a separate entry point for intent/CLI.
    reconcile_unindexed_snapshots()
}

/// Best-effort reconciliation for the live scan path. Never fails the scan.
pub(crate) fn best_effort_reconcile() {
    if let Err(e) = reconcile_unindexed_snapshots() {
        log::warn!("Snapshot index reconciliation skipped: {e}");
    }
}

/// Tauri command: per-source snapshot status (latest snapshot, counts).
#[tauri::command]
pub async fn snapshot_sync_status() -> Result<Vec<crate::storage::SyncStatus>, String> {
    let sources = list_registered_sources();
    Ok(sources.iter().map(crate::storage::sync_status).collect())
}

/// Tauri command: rebuild the derived SQLite index from preserved snapshots.
/// Safe to run anytime; all writes are additive and idempotent.
#[tauri::command]
pub async fn rebuild_snapshot_index() -> Result<IndexReportForUi, String> {
    let report = tauri::async_runtime::spawn_blocking(rebuild_index_from_snapshots)
        .await
        .map_err(|e| format!("Task join error: {e}"))??;
    Ok(IndexReportForUi {
        sources_seen: report.sources_seen,
        snapshots_seen: report.snapshots_seen,
        snapshots_indexed: report.snapshots_indexed,
        snapshots_skipped: report.snapshots_skipped,
    })
}

/// JSON-safe projection of [`IndexReport`] for the frontend.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexReportForUi {
    pub sources_seen: usize,
    pub snapshots_seen: usize,
    pub snapshots_indexed: usize,
    pub snapshots_skipped: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::SyncOutcome;

    fn session_jsonl() -> String {
        [
            serde_json::json!({
                "uuid": "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
                "sessionId": "rebuild-probe",
                "timestamp": "2026-09-01T00:00:00Z",
                "type": "user",
                "cwd": "/tmp/rebuild-probe",
                "message": {"role": "user", "content": "Rebuild me"},
            })
            .to_string(),
            serde_json::json!({
                "uuid": "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
                "parentUuid": "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
                "sessionId": "rebuild-probe",
                "timestamp": "2026-09-01T00:01:00Z",
                "type": "assistant",
                "cwd": "/tmp/rebuild-probe",
                "message": {
                    "role": "assistant",
                    "content": [{"type": "text", "text": "Rebuilt"}],
                    "model": "claude-opus-4-1",
                },
            })
            .to_string(),
        ]
        .join("\n")
    }

    fn write_file(path: &std::path::Path, content: &[u8]) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, content).unwrap();
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn crash_after_finalize_is_recovered_by_reconcile() {
        let _sandbox = crate::test_utils::SandboxHome::new();
        let original = tempfile::tempdir().unwrap();
        let base = original.path().join("crash-claude");
        write_file(
            &base.join("projects/proj/session.jsonl"),
            session_jsonl().as_bytes(),
        );

        // Simulate: snapshot finalized, process crashed before DB indexing.
        // `sync_local_directory` never touches SQLite, so this is exact.
        let source = crate::commands::project::claude_source_for_base(&base, None);
        let snapshot_id = match crate::storage::sync_local_directory(&source, &base).unwrap() {
            SyncOutcome::Created(s) => s.snapshot_id,
            SyncOutcome::Unchanged(_) => panic!("must create"),
        };

        let conn = crate::cache::open_connection().unwrap();
        assert!(!crate::cache::db::is_snapshot_indexed(&conn, &source.id, &snapshot_id).unwrap());

        let report = reconcile_unindexed_snapshots().unwrap();
        assert_eq!(report.snapshots_indexed, 1);

        // Now discoverable through the derived index.
        let sessions =
            crate::cache::load_sessions(&base.join("projects/proj").to_string_lossy(), "claude");
        assert_eq!(sessions.len(), 1);

        // Retry is a safe no-op.
        let retry = reconcile_unindexed_snapshots().unwrap();
        assert_eq!(retry.snapshots_indexed, 0);
        assert_eq!(retry.snapshots_skipped, 1);
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn deleting_cache_db_rebuilds_from_snapshots() {
        let _sandbox = crate::test_utils::SandboxHome::new();
        let original = tempfile::tempdir().unwrap();
        let base = original.path().join("rebuild-claude");
        write_file(
            &base.join("projects/proj/session.jsonl"),
            session_jsonl().as_bytes(),
        );

        // Normal scan path indexes the snapshot.
        let projects = crate::commands::project::scan_claude_base_with_snapshot(
            base.to_string_lossy().to_string(),
            None,
        )
        .await;
        assert_eq!(projects.len(), 1);
        reconcile_unindexed_snapshots().unwrap();
        let before =
            crate::cache::load_sessions(&base.join("projects/proj").to_string_lossy(), "claude");
        assert_eq!(before.len(), 1);

        // `rm cache.db`, restart equivalent: delete the derived database only.
        let db_path = crate::cache::get_cache_db_path().unwrap();
        assert!(db_path.is_file());
        std::fs::remove_file(&db_path).unwrap();

        // Snapshot reads keep working without any index at all.
        let sessions_direct = crate::commands::session::load_project_sessions(
            base.join("projects/proj").to_string_lossy().to_string(),
            Some(false),
        )
        .await
        .unwrap();
        assert_eq!(sessions_direct.len(), 1);

        // And the derived index rebuilds from preserved files.
        let report = rebuild_index_from_snapshots().unwrap();
        assert!(report.snapshots_indexed >= 1);
        let after =
            crate::cache::load_sessions(&base.join("projects/proj").to_string_lossy(), "claude");
        assert_eq!(after.len(), 1);

        // Idempotent retry.
        let retry = rebuild_index_from_snapshots().unwrap();
        assert_eq!(retry.snapshots_indexed, 0);
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn rebuild_preserves_sessions_deleted_from_latest_snapshot() {
        let _sandbox = crate::test_utils::SandboxHome::new();
        let original = tempfile::tempdir().unwrap();
        let base = original.path().join("rebuild-deleted");
        let project_dir = base.join("projects/proj");
        write_file(&project_dir.join("keep.jsonl"), session_jsonl().as_bytes());
        write_file(
            &project_dir.join("deleted.jsonl"),
            session_jsonl()
                .replace("rebuild-probe", "deleted-probe")
                .as_bytes(),
        );

        let source = crate::commands::project::claude_source_for_base(&base, None);
        match crate::storage::sync_local_directory(&source, &base).unwrap() {
            SyncOutcome::Created(_) => {}
            SyncOutcome::Unchanged(_) => panic!("must create"),
        }
        reconcile_unindexed_snapshots().unwrap();

        // Source deletes one session; a new snapshot only has the survivor.
        std::fs::remove_file(project_dir.join("deleted.jsonl")).unwrap();
        // Sleep to outlast mtime-second granularity so change detection fires.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        match crate::storage::sync_local_directory(&source, &base).unwrap() {
            SyncOutcome::Created(_) => {}
            SyncOutcome::Unchanged(_) => panic!("deletion must create a new snapshot"),
        }

        // Fresh database sees only the union after rebuild.
        let db_path = crate::cache::get_cache_db_path().unwrap();
        std::fs::remove_file(&db_path).unwrap();
        rebuild_index_from_snapshots().unwrap();
        let sessions =
            crate::cache::load_sessions(project_dir.to_string_lossy().as_ref(), "claude");
        assert_eq!(
            sessions.len(),
            2,
            "both preserved snapshots must union, got {}",
            sessions.len()
        );
    }

    /// Remote snapshots index under the same `remote://` keys the read path
    /// and legacy cache use — never as snapshot-interior or relative paths.
    #[tokio::test]
    #[serial_test::serial]
    async fn remote_snapshots_index_with_remote_keys() {
        let _sandbox = crate::test_utils::SandboxHome::new();
        let endpoint = "http://127.0.0.1:12";
        let remote_root = "/remote/.claude";
        let source =
            crate::storage::Source::remote_with_root("claude", endpoint, remote_root, Some("t"));
        let files = vec![crate::storage::RemoteSyncedFile {
            path: "projects/proj/session.jsonl".to_string(),
            bytes: session_jsonl().into_bytes(),
            mtime_secs: 1_700_000_000,
        }];
        match crate::storage::sync_remote_files_to_snapshot(&source, remote_root, files).unwrap() {
            SyncOutcome::Created(_) => {}
            SyncOutcome::Unchanged(_) => panic!("must create"),
        }

        rebuild_index_from_snapshots().unwrap();

        let remote_project = format!("remote://{endpoint}#{remote_root}/projects/proj");
        let sessions = crate::cache::load_sessions(&remote_project, "claude");
        assert_eq!(sessions.len(), 1);
        assert!(
            sessions[0].file_path.starts_with("remote://"),
            "session must keep its remote key, got {}",
            sessions[0].file_path
        );

        // No snapshot-interior or relative paths may leak into the index.
        let conn = crate::cache::open_connection().unwrap();
        let mut stmt = conn
            .prepare("SELECT session_id, file_path FROM sessions")
            .unwrap();
        let rows: Vec<(String, String)> = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .flatten()
            .collect();
        assert!(!rows.is_empty());
        for (session_id, file_path) in &rows {
            assert!(
                !file_path.contains(".claude-history-viewer/data"),
                "snapshot interior leaked into index: {file_path}"
            );
            assert!(
                !session_id.contains(".claude-history-viewer/data"),
                "snapshot interior leaked into index: {session_id}"
            );
        }
        // And the remote project row is discoverable through the project index.
        let projects = crate::cache::load_projects(Some("t"), None);
        assert!(
            projects.iter().any(|p| p.path == remote_project),
            "remote project must be indexed, got {:?}",
            projects.iter().map(|p| &p.path).collect::<Vec<_>>()
        );
    }
}
