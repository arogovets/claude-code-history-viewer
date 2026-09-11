//! Raw-file sync transport (server side).
//!
//! The remote file-sync protocol exposes provider *files* (not parsed
//! conversations) so the aggregator can preserve them into immutable local
//! snapshots and parse them with the existing local parsers:
//!
//! ```text
//! POST /api/sync/sources  -> [{provider, root, label}]
//! POST /api/sync/manifest -> {provider, root?} -> {files: [{path, size, mtimeSecs}]}
//! POST /api/sync/file     -> {provider, root?, path} -> {contentBase64, size, mtimeSecs}
//! ```
//!
//! First slice covers the `claude` provider (`<root>/projects/**`). Other
//! providers keep their existing parsed-response endpoints until migrated.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// One syncable provider root on this machine.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncSourceInfo {
    pub provider: String,
    pub root: String,
    pub label: Option<String>,
}

/// One file in a sync manifest; `path` is `/`-separated, relative to `root`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncFileEntry {
    pub path: String,
    pub size: u64,
    pub mtime_secs: i64,
}

/// Manifest response.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncManifest {
    pub provider: String,
    pub root: String,
    pub files: Vec<SyncFileEntry>,
}

/// File content response (base64 so it stays JSON-compatible with the
/// existing API surface and auth middleware).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncFileContent {
    pub path: String,
    pub content_base64: String,
    pub size: u64,
    pub mtime_secs: i64,
}

const MAX_MANIFEST_FILES: usize = 200_000;
const MAX_SYNC_FILE_BYTES: u64 = 128 * 1024 * 1024;

fn default_claude_root() -> Option<PathBuf> {
    if let Some(base) = crate::providers::claude::get_base_path() {
        return Some(PathBuf::from(base));
    }
    let home = crate::utils::home_dir()?;
    let candidate = home.join(".claude");
    if candidate.is_dir() {
        Some(candidate)
    } else {
        None
    }
}

fn custom_claude_roots() -> Vec<(PathBuf, Option<String>)> {
    let mut out = Vec::new();
    let Ok(user_data_path) = crate::commands::metadata::get_user_data_path() else {
        return out;
    };
    let Ok(content) = std::fs::read_to_string(&user_data_path) else {
        return out;
    };
    let Ok(metadata) = serde_json::from_str::<serde_json::Value>(&content) else {
        return out;
    };
    let Some(entries) = metadata
        .get("settings")
        .and_then(|s| s.get("customClaudePaths"))
        .and_then(|v| v.as_array())
    else {
        return out;
    };
    for entry in entries {
        let Some(path_str) = entry.get("path").and_then(|p| p.as_str()) else {
            continue;
        };
        let label = entry
            .get("label")
            .and_then(|l| l.as_str())
            .map(str::to_string);
        let base = PathBuf::from(path_str);
        if crate::utils::validate_custom_claude_path(&base).is_ok() {
            out.push((base, label));
        }
    }
    out
}

/// List syncable provider roots on this machine.
///
/// `async` for handler compatibility (`handler_json!` awaits); the body is
/// synchronous today and may stream manifests later.
#[allow(clippy::unused_async)]
pub async fn list_sync_sources(
    providers: Option<Vec<String>>,
) -> Result<Vec<SyncSourceInfo>, String> {
    let wanted = |id: &str| {
        providers
            .as_deref()
            .map_or(true, |list| list.iter().any(|p| p == id))
    };
    let mut sources = Vec::new();
    if wanted("claude") {
        if let Some(root) = default_claude_root() {
            sources.push(SyncSourceInfo {
                provider: "claude".to_string(),
                root: root.to_string_lossy().to_string(),
                label: None,
            });
        }
        for (root, label) in custom_claude_roots() {
            sources.push(SyncSourceInfo {
                provider: "claude".to_string(),
                root: root.to_string_lossy().to_string(),
                label,
            });
        }
    }
    Ok(sources)
}

/// Resolve and authorize a sync root for a provider. The root must be one of
/// the listed sources so callers cannot point the reader at arbitrary paths.
fn resolve_sync_root(provider: &str, root_param: Option<&str>) -> Result<PathBuf, String> {
    if provider != "claude" {
        return Err(format!(
            "File sync not yet supported for provider: {provider}"
        ));
    }
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(root) = default_claude_root() {
        candidates.push(root);
    }
    for (root, _) in custom_claude_roots() {
        candidates.push(root);
    }
    if candidates.is_empty() {
        return Err("No Claude data directory found on this host".to_string());
    }
    let Some(raw) = root_param.filter(|r| !r.trim().is_empty()) else {
        return candidates
            .into_iter()
            .next()
            .ok_or_else(|| "No sync root".to_string());
    };
    let requested = PathBuf::from(raw);
    if !requested.is_absolute() {
        return Err("Sync root must be an absolute path".to_string());
    }
    // Compare canonical forms so symlinked spellings of the same root match.
    let canonical_requested = std::fs::canonicalize(&requested)
        .map_err(|_| "Unknown sync root for this host".to_string())?;
    for candidate in &candidates {
        let canonical_candidate =
            std::fs::canonicalize(candidate).unwrap_or_else(|_| candidate.clone());
        if canonical_requested == canonical_candidate {
            return Ok(canonical_candidate);
        }
    }
    Err("Unknown sync root for this host".to_string())
}

fn relative_posix(root: &Path, path: &Path) -> Option<String> {
    let relative = path.strip_prefix(root).ok()?;
    let mut parts = Vec::new();
    for component in relative.components() {
        match component {
            std::path::Component::Normal(part) => parts.push(part.to_string_lossy().to_string()),
            std::path::Component::CurDir => {}
            _ => return None,
        }
    }
    if parts.is_empty() {
        return None;
    }
    Some(parts.join("/"))
}

#[allow(clippy::cast_possible_wrap)]
fn file_mtime_secs(meta: &std::fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// List preservable files under a provider root (Claude: `projects/**`).
///
/// `async` for handler compatibility; the walk is synchronous today.
#[allow(clippy::unused_async)]
pub async fn sync_manifest(provider: String, root: Option<String>) -> Result<SyncManifest, String> {
    let root_path = resolve_sync_root(&provider, root.as_deref())?;
    let projects_dir = root_path.join("projects");
    if !projects_dir.is_dir() {
        return Ok(SyncManifest {
            provider,
            root: root_path.to_string_lossy().to_string(),
            files: Vec::new(),
        });
    }
    let mut files = Vec::new();
    // Canonical directories already walked: symlink cycles (a link pointing at
    // an ancestor) must terminate instead of looping forever.
    let mut visited: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    if let Ok(canonical) = std::fs::canonicalize(&projects_dir) {
        visited.insert(canonical);
    }
    let mut stack = vec![projects_dir.clone()];
    while let Some(dir) = stack.pop() {
        let Ok(read_dir) = std::fs::read_dir(&dir) else {
            continue;
        };
        for child in read_dir.flatten() {
            let path = child.path();
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if meta.file_type().is_symlink() {
                // Dereferenced one level: record the target's bytes under the
                // link's logical path so the snapshot has no links.
                let Ok(target_meta) = std::fs::metadata(&path) else {
                    continue;
                };
                if target_meta.is_dir() {
                    // Walk the target under the link's logical path, once per
                    // canonical directory so cycles terminate.
                    let canonical = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
                    if visited.insert(canonical) {
                        stack.push(path.clone());
                    }
                    continue;
                }
                if target_meta.is_file() {
                    if let Some(relative) = relative_posix(&root_path, &path) {
                        if !is_skipped_sync_file(&relative) {
                            files.push(SyncFileEntry {
                                path: relative,
                                size: target_meta.len(),
                                mtime_secs: file_mtime_secs(&target_meta),
                            });
                        }
                    }
                }
                continue;
            }
            if meta.is_dir() {
                // Real directories always terminate (only symlinks can cycle,
                // and those are guarded above), so every logical path is walked
                // even if two of them resolve to one physical directory.
                stack.push(path);
            } else if meta.is_file() {
                if let Some(relative) = relative_posix(&root_path, &path) {
                    if !is_skipped_sync_file(&relative) {
                        files.push(SyncFileEntry {
                            path: relative,
                            size: meta.len(),
                            mtime_secs: file_mtime_secs(&meta),
                        });
                    }
                }
            }
            if files.len() > MAX_MANIFEST_FILES {
                return Err("Sync manifest exceeds file limit".to_string());
            }
        }
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(SyncManifest {
        provider,
        root: root_path.to_string_lossy().to_string(),
        files,
    })
}

fn is_skipped_sync_file(relative_posix: &str) -> bool {
    relative_posix == ".session_cache.json" || relative_posix.ends_with("/.session_cache.json")
}

/// Read one synced file's bytes.
///
/// `async` for handler compatibility; the read is synchronous today.
#[allow(clippy::unused_async)]
pub async fn read_sync_file(
    provider: String,
    root: Option<String>,
    path: String,
) -> Result<SyncFileContent, String> {
    let root_path = resolve_sync_root(&provider, root.as_deref())?;
    if path.trim().is_empty() || path.starts_with('/') || path.contains('\\') {
        return Err("Invalid sync path".to_string());
    }
    if path
        .split('/')
        .any(|seg| seg.is_empty() || seg == "." || seg == "..")
    {
        return Err("Invalid sync path".to_string());
    }
    if is_skipped_sync_file(&path) {
        return Err("File is not syncable".to_string());
    }
    let candidate: PathBuf = path.split('/').collect();
    let full = root_path.join(candidate);
    let canonical_root = std::fs::canonicalize(&root_path).unwrap_or_else(|_| root_path.clone());
    // Resolve the parent (the file itself may be a symlink to a real session).
    let canonical_parent = full
        .parent()
        .and_then(|p| std::fs::canonicalize(p).ok())
        .ok_or_else(|| "Sync path does not exist".to_string())?;
    if !canonical_parent.starts_with(&canonical_root) {
        return Err("Sync path escapes the provider root".to_string());
    }
    let meta = std::fs::metadata(&full).map_err(|_| "Sync path does not exist".to_string())?;
    if !meta.is_file() {
        return Err("Sync path is not a file".to_string());
    }
    if meta.len() > MAX_SYNC_FILE_BYTES {
        return Err("Sync file exceeds size limit".to_string());
    }
    let bytes = std::fs::read(&full).map_err(|e| format!("Failed to read sync file: {e}"))?;
    if bytes.len() as u64 > MAX_SYNC_FILE_BYTES {
        return Err("Sync file exceeds size limit".to_string());
    }
    use base64::Engine as _;
    Ok(SyncFileContent {
        path,
        content_base64: base64::engine::general_purpose::STANDARD.encode(&bytes),
        size: bytes.len() as u64,
        mtime_secs: file_mtime_secs(&meta),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derived_cache_files_are_not_syncable() {
        assert!(is_skipped_sync_file(".session_cache.json"));
        assert!(is_skipped_sync_file("projects/p/.session_cache.json"));
        assert!(!is_skipped_sync_file("projects/p/s.jsonl"));
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn manifest_lists_project_files_without_cache() {
        let sandbox = crate::test_utils::SandboxHome::new();
        // Build a fake remote home with a .claude tree.
        let home = sandbox.path();
        let root = home.join(".claude");
        std::fs::create_dir_all(root.join("projects/proj")).unwrap();
        std::fs::write(root.join("projects/proj/s.jsonl"), b"{}\n").unwrap();
        std::fs::write(root.join("projects/proj/.session_cache.json"), b"{}").unwrap();

        let manifest = sync_manifest("claude".to_string(), None).await.unwrap();
        assert!(manifest
            .files
            .iter()
            .any(|f| f.path == "projects/proj/s.jsonl"));
        assert!(!manifest
            .files
            .iter()
            .any(|f| f.path.contains(".session_cache.json")));
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn sync_file_rejects_traversal_and_unknown_roots() {
        let sandbox = crate::test_utils::SandboxHome::new();
        let home = sandbox.path();
        std::fs::create_dir_all(home.join(".claude").join("projects")).unwrap();

        assert!(
            read_sync_file("claude".to_string(), None, "../x".to_string())
                .await
                .is_err()
        );
        assert!(
            read_sync_file("claude".to_string(), None, "/abs".to_string())
                .await
                .is_err()
        );
        assert!(
            read_sync_file("codex".to_string(), None, "sessions/x".to_string())
                .await
                .is_err()
        );
        assert!(read_sync_file(
            "claude".to_string(),
            Some("/definitely/not/a/sync/root".to_string()),
            "projects/x".to_string()
        )
        .await
        .is_err());
    }
}
