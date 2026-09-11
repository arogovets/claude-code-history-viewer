//! Centralized sync coordinator: local + remote sources, periodically.
//!
//! ```text
//! application starts → sync available sources
//! while running     → periodically re-sync local + remote sources
//! remote reachable  → new snapshot if needed
//! remote unavailable → keep serving the latest completed local snapshot
//! ```
//!
//! Every sync is per-source locked, idempotent, and append-only. Normal
//! browsing never blocks on an offline host: failures are logged and the last
//! completed snapshot keeps serving.
//!
//! Provider coverage is incremental: `claude` (local/custom/remote) is fully
//! wired. Remaining providers keep their legacy direct-read paths and gain
//! snapshot coverage by following the same three steps per provider:
//!
//! 1. expose `scan_projects_from_root(root)` / `load_*_from_root` seams that
//!    take a directory instead of reading a hardcoded home path (most already
//!    have `*_from_path`/`*_in` variants — reuse them, do not fork parsers);
//! 2. sync the provider root into a snapshot, then call the seam with the
//!    snapshot data root and rewrite snapshot-interior paths back to the
//!    stable external contract (see `commands::project` for the worked
//!    Claude example, including the immutable-snapshot cache guard in
//!    `commands::session::load`);
//! 3. for SQLite-backed providers (opencode, trae, …), copy the underlying
//!    store file into staging first and open the *copy* read-only; a torn
//!    copy must fall back to the previous snapshot, never to a live partial
//!    read.

use std::path::PathBuf;
use std::time::Duration;

/// How often the background loop re-syncs while the app runs.
pub const SYNC_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// One coordinator pass over every known source.
#[derive(Debug, Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoordinatorReport {
    pub local_synced: usize,
    pub local_failed: usize,
    pub remote_synced: usize,
    pub remote_failed: usize,
    pub snapshots_indexed: usize,
}

/// Default Claude base even when the live dir is gone (snapshots stay
/// browsable); plus every configured custom Claude dir.
fn local_claude_roots() -> Vec<(PathBuf, Option<String>)> {
    let mut roots = Vec::new();
    if let Some(home) = crate::utils::home_dir() {
        roots.push((home.join(".claude"), None));
    }
    let Ok(user_data_path) = crate::commands::metadata::get_user_data_path() else {
        return roots;
    };
    let Ok(content) = std::fs::read_to_string(&user_data_path) else {
        return roots;
    };
    let Ok(metadata) = serde_json::from_str::<serde_json::Value>(&content) else {
        return roots;
    };
    if let Some(entries) = metadata
        .get("settings")
        .and_then(|s| s.get("customClaudePaths"))
        .and_then(|v| v.as_array())
    {
        for entry in entries {
            if let Some(path_str) = entry.get("path").and_then(|p| p.as_str()) {
                let label = entry
                    .get("label")
                    .and_then(|l| l.as_str())
                    .map(str::to_string);
                roots.push((PathBuf::from(path_str), label));
            }
        }
    }
    roots
}

/// Sync every available source once: reconcile the index, snapshot local
/// Claude roots, file-sync reachable remote hosts. Best effort per source.
pub async fn sync_all_once() -> CoordinatorReport {
    let mut report = CoordinatorReport::default();

    // Crash recovery / `rm cache.db` repair first (idempotent, usually no-op).
    match tauri::async_runtime::spawn_blocking(crate::storage::reconcile_unindexed_snapshots).await
    {
        Ok(Ok(index_report)) => report.snapshots_indexed = index_report.snapshots_indexed,
        Ok(Err(e)) => log::warn!("Coordinator index reconciliation skipped: {e}"),
        Err(e) => log::warn!("Coordinator index task failed: {e}"),
    }

    // Local Claude roots (default + custom). Missing roots simply fail here;
    // their preserved snapshots keep serving through the normal read path.
    for (root, label) in local_claude_roots() {
        if !root.is_absolute() {
            continue;
        }
        let source = crate::commands::project::claude_source_for_base(&root, label.as_deref());
        let result = tauri::async_runtime::spawn_blocking(move || {
            if !root.is_dir() {
                return Err(format!("Source root {} is not available", root.display()));
            }
            crate::storage::sync_local_directory(&source, &root).map(|_| ())
        })
        .await;
        match result {
            Ok(Ok(())) => report.local_synced += 1,
            Ok(Err(e)) => {
                log::warn!("Coordinator local sync skipped: {e}");
                report.local_failed += 1;
            }
            Err(e) => {
                log::warn!("Coordinator local sync task failed: {e}");
                report.local_failed += 1;
            }
        }
    }

    // Remote hosts (reachable ones produce new snapshots; offline ones keep
    // serving their latest completed snapshot).
    for host in crate::remote::get_remote_hosts()
        .into_iter()
        .filter(|h| h.enabled)
    {
        match crate::remote::sync_remote_host_files(&host).await {
            Ok(_) => report.remote_synced += 1,
            Err(e) => {
                log::warn!("Coordinator remote sync for {} skipped: {e}", host.name);
                report.remote_failed += 1;
            }
        }
    }

    log::info!(
        "Coordinator pass: local synced={} failed={}, remote synced={} failed={}, indexed={}",
        report.local_synced,
        report.local_failed,
        report.remote_synced,
        report.remote_failed,
        report.snapshots_indexed,
    );
    report
}

/// Spawn the detached background loop (one per process). The loop sleeps with
/// the standard library so it works with or without the `webui-server`
/// feature; each tick's work is dispatched onto the Tauri async runtime.
pub fn spawn_background_sync() {
    std::thread::Builder::new()
        .name("cchv-sync-coordinator".to_string())
        .spawn(|| loop {
            std::thread::sleep(SYNC_INTERVAL);
            tauri::async_runtime::spawn(async {
                sync_all_once().await;
            });
        })
        .map(|_| ())
        .unwrap_or_else(|e| log::warn!("Failed to spawn sync coordinator: {e}"));
}

/// Tauri command: run one coordinator pass on demand (Settings/refresh UI).
#[tauri::command]
pub async fn run_sync_pass() -> Result<CoordinatorReport, String> {
    Ok(sync_all_once().await)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `CCHV_NO_REMOTE` is process-global; restore on drop so a panic cannot
    /// leak it into later tests in the same process.
    struct NoRemoteGuard {
        previous: Option<std::ffi::OsString>,
    }

    impl NoRemoteGuard {
        fn set() -> Self {
            let previous = std::env::var_os("CCHV_NO_REMOTE");
            std::env::set_var("CCHV_NO_REMOTE", "1");
            Self { previous }
        }
    }

    impl Drop for NoRemoteGuard {
        fn drop(&mut self) {
            match &self.previous {
                Some(value) => std::env::set_var("CCHV_NO_REMOTE", value),
                None => std::env::remove_var("CCHV_NO_REMOTE"),
            }
        }
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn coordinator_pass_with_unavailable_sources_still_reports() {
        let _sandbox = crate::test_utils::SandboxHome::new();
        // No .claude dir here; remotes disabled so the pass must complete
        // without hanging on network timeouts.
        let _no_remote = NoRemoteGuard::set();
        let report = sync_all_once().await;
        // Local default missing → failed, but the pass completed.
        assert_eq!(report.local_synced, 0);
        assert!(report.local_failed >= 1);
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn coordinator_syncs_a_local_source() {
        let sandbox = crate::test_utils::SandboxHome::new();
        let _no_remote = NoRemoteGuard::set();
        // Seed the sandbox home's own .claude so the coordinator finds it.
        let claude = sandbox.path().join(".claude");
        std::fs::create_dir_all(claude.join("projects/p")).unwrap();
        std::fs::write(claude.join("projects/p/s.jsonl"), b"{\"a\":1}\n").unwrap();
        let report = sync_all_once().await;
        assert_eq!(report.local_synced, 1);
        assert!(crate::storage::resolve_snapshot_data_root("local-claude").is_some());
    }
}
