//! Aggregator for the unified GitHub Copilot provider.
//!
//! All three Copilot client surfaces — terminal CLI, Desktop app, and the
//! VS Code Copilot Chat extension — surface to the frontend as a single
//! provider with id `"copilot"`. Per-session disambiguation lives in the
//! `entrypoint` field (`copilot-cli` / `copilot-desktop` / `copilot-vscode`),
//! which the existing source-filter UI already understands.
//!
//! The aggregator calls into the three concrete scanners
//! (`copilot_cli`, `copilot_desktop`, `vscode`) and groups their results by
//! `actual_path` so a folder that has, say, both Copilot CLI sessions AND a
//! VS Code Copilot Chat history collapses into one project entry.
//!
//! Routing back to the right sub-scanner is done lazily: project paths
//! produced by the aggregator are minted with the synthetic
//! `copilot://<actual_path>` scheme, and `load_sessions` re-scans the three
//! sub-scanners and filters their projects by matching `actual_path`. This
//! costs us one extra scan on session-load, but avoids encoding multiple
//! storage hashes into the project URL.

use crate::models::{ClaudeMessage, ClaudeProject, ClaudeSession};
use crate::providers::{copilot_cli, vscode, ProviderInfo};
use crate::utils::parse_rfc3339_utc;
use base64::Engine;
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::path::Path;

/// Public provider id stamped on every record.
pub const PROVIDER_ID: &str = "copilot";

/// Synthetic URL scheme for merged Copilot projects.
const PROJECT_SCHEME: &str = "copilot://";

/// Which sub-provider a source path belongs to. Stored inside the merged
/// project URL so `load_sessions` can dispatch directly without rescanning
/// every sub-provider.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum SourceKind {
    /// `~/.copilot/session-state` walked by [`copilot_cli`] in CLI mode.
    Cli,
    /// Same storage walked by [`copilot_cli`] in Desktop mode.
    Desktop,
    /// VS Code Copilot Chat workspace storage walked by [`vscode`].
    VsCode,
}

/// One sub-source contributing to a merged project. `path` is whatever the
/// underlying scanner uses to identify the project (CLI: bare filesystem path
/// of the workspace folder; VS Code: encoded `vscode://...` path the vscode
/// scanner expects on load).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SourceRef {
    kind: SourceKind,
    path: String,
}

/// Payload encoded into the merged project URL so we can recover the original
/// sub-source paths on `load_sessions` without re-scanning everything.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ProjectRef {
    actual: String,
    sources: Vec<SourceRef>,
}

fn encode_project_ref(r: &ProjectRef) -> String {
    let json = serde_json::to_string(r).unwrap_or_default();
    let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json);
    format!("{PROJECT_SCHEME}{b64}")
}

fn decode_project_ref(project_path: &str) -> Option<ProjectRef> {
    let payload = project_path.strip_prefix(PROJECT_SCHEME)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Detect a Copilot installation. Reports available if any of the three
/// sub-providers has data on disk.
pub fn detect() -> Option<ProviderInfo> {
    let cli = copilot_cli::detect();
    let desktop = copilot_cli::detect_desktop();
    let vsc = vscode::detect();

    // Prefer the Copilot CLI/Desktop base path (`~/.copilot`) when available,
    // since that's where the bulk of session data lives. Fall back to the
    // VS Code user-data root.
    let base_path = cli
        .as_ref()
        .map(|i| i.base_path.clone())
        .or_else(|| desktop.as_ref().map(|i| i.base_path.clone()))
        .or_else(|| vsc.as_ref().map(|i| i.base_path.clone()))?;

    let is_available = cli.as_ref().is_some_and(|i| i.is_available)
        || desktop.as_ref().is_some_and(|i| i.is_available)
        || vsc.as_ref().is_some_and(|i| i.is_available);

    Some(ProviderInfo {
        id: PROVIDER_ID.to_string(),
        display_name: "Copilot".to_string(),
        base_path,
        is_available,
    })
}

/// Normalise an `actual_path` so equivalent CLI and VS Code references
/// collapse to the same key. VS Code records workspace folders as
/// `file:///path` URIs while the CLI uses bare filesystem paths; we drop
/// the `file://` prefix so they group together.
fn group_key(actual_path: &str) -> String {
    actual_path
        .strip_prefix("file://")
        .unwrap_or(actual_path)
        .trim_end_matches('/')
        .to_string()
}

/// Tag each project with its sub-source kind, then group by canonical folder.
fn merge_projects(parts: Vec<(SourceKind, ClaudeProject)>) -> Vec<ClaudeProject> {
    let mut grouped: HashMap<String, Vec<(SourceKind, ClaudeProject)>> = HashMap::new();
    for (kind, project) in parts {
        let key = group_key(&project.actual_path);
        grouped.entry(key).or_default().push((kind, project));
    }

    let mut merged: Vec<ClaudeProject> = grouped
        .into_values()
        .map(|mut group| {
            // Use the most-recently-modified project as the display template.
            group.sort_by(|a, b| b.1.last_modified.cmp(&a.1.last_modified));
            let template = group
                .first()
                .map(|(_, p)| p.clone())
                .expect("group is non-empty");
            let session_count = group.iter().map(|(_, p)| p.session_count).sum();
            let message_count = group.iter().map(|(_, p)| p.message_count).sum();
            let last_modified = group
                .iter()
                .map(|(_, p)| p.last_modified.as_str())
                .max()
                .unwrap_or("")
                .to_string();
            // Prefer a non-`file://` actual_path for display so the UI shows
            // a plain filesystem path.
            let actual_path = group
                .iter()
                .map(|(_, p)| p.actual_path.as_str())
                .find(|p| !p.starts_with("file://"))
                .unwrap_or(&template.actual_path)
                .to_string();
            let name = Path::new(&actual_path)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| template.name.clone());

            let sources: Vec<SourceRef> = group
                .iter()
                .map(|(kind, p)| SourceRef {
                    kind: *kind,
                    path: p.path.clone(),
                })
                .collect();
            let path = encode_project_ref(&ProjectRef {
                actual: actual_path.clone(),
                sources,
            });

            ClaudeProject {
                name,
                path,
                actual_path,
                session_count,
                message_count,
                last_modified,
                git_info: None,
                provider: Some(PROVIDER_ID.to_string()),
                storage_type: None,
                custom_directory_label: template.custom_directory_label,
            }
        })
        .collect();

    merged.sort_by(|a, b| b.last_modified.cmp(&a.last_modified));
    merged
}

fn tag<I: IntoIterator<Item = ClaudeProject>>(
    kind: SourceKind,
    iter: I,
) -> impl Iterator<Item = (SourceKind, ClaudeProject)> {
    iter.into_iter().map(move |p| (kind, p))
}

/// Scan all three Copilot sub-providers and return one merged project list.
pub fn scan_projects() -> Result<Vec<ClaudeProject>, String> {
    let mut all = Vec::new();
    if let Ok(p) = copilot_cli::scan_projects() {
        all.extend(tag(SourceKind::Cli, p));
    }
    if let Ok(p) = copilot_cli::scan_desktop_projects() {
        all.extend(tag(SourceKind::Desktop, p));
    }
    if let Ok(p) = vscode::scan_projects() {
        all.extend(tag(SourceKind::VsCode, p));
    }
    Ok(merge_projects(all))
}

/// WSL/custom-path variant. `copilot_base_path` is the `~/.copilot` directory
/// for the CLI+Desktop scan; `vscode_user_data_path` is the VS Code user-data
/// dir. Either may be `None` to skip that sub-scan.
pub fn scan_projects_from_paths(
    copilot_base_path: Option<&str>,
    vscode_user_data_path: Option<&Path>,
    custom_directory_label: Option<&str>,
) -> Result<Vec<ClaudeProject>, String> {
    let mut all = Vec::new();
    if let Some(base) = copilot_base_path {
        if let Ok(p) = copilot_cli::scan_projects_from_path(base, custom_directory_label) {
            all.extend(tag(SourceKind::Cli, p));
        }
        if let Ok(p) = copilot_cli::scan_desktop_projects_from_path(base, custom_directory_label) {
            all.extend(tag(SourceKind::Desktop, p));
        }
    }
    if let Some(base) = vscode_user_data_path {
        if let Ok(p) = vscode::scan_projects_from_user_data_path(base, custom_directory_label) {
            all.extend(tag(SourceKind::VsCode, p));
        }
    }
    Ok(merge_projects(all))
}

/// Load sessions for a merged project. Decodes the source list embedded in
/// the project URL and dispatches each sub-source directly. No rescan.
pub fn load_sessions(project_path: &str, exclude: bool) -> Result<Vec<ClaudeSession>, String> {
    let Some(project_ref) = decode_project_ref(project_path) else {
        // Older/malformed URL — degrade to a rescan-and-filter fallback so we
        // don't break in case stale URLs survive in any cache.
        return Ok(load_sessions_fallback(project_path, exclude));
    };

    let mut sessions = Vec::new();
    for src in project_ref.sources {
        let result = match src.kind {
            SourceKind::Cli | SourceKind::Desktop => copilot_cli::load_sessions(&src.path, exclude),
            SourceKind::VsCode => vscode::load_sessions(&src.path, exclude),
        };
        if let Ok(s) = result {
            sessions.extend(s);
        }
    }

    sessions.sort_by(|a, b| b.last_modified.cmp(&a.last_modified));
    Ok(sessions)
}

/// Legacy fallback: if a caller hands us a project URL without an embedded
/// source list (unlikely after this refactor, but defensive), fall back to
/// the old scan-and-filter behaviour.
fn load_sessions_fallback(project_path: &str, exclude: bool) -> Vec<ClaudeSession> {
    type Loader = dyn Fn(&str, bool) -> Result<Vec<ClaudeSession>, String>;
    let raw = project_path
        .strip_prefix(PROJECT_SCHEME)
        .unwrap_or(project_path);
    let target_key = group_key(raw);
    let mut sessions = Vec::new();

    let collect = |scanned: Vec<ClaudeProject>, loader: &Loader, sink: &mut Vec<ClaudeSession>| {
        for p in scanned {
            if group_key(&p.actual_path) == target_key {
                if let Ok(s) = loader(&p.path, exclude) {
                    sink.extend(s);
                }
            }
        }
    };

    if let Ok(scanned) = copilot_cli::scan_projects() {
        collect(scanned, &copilot_cli::load_sessions, &mut sessions);
    }
    if let Ok(scanned) = copilot_cli::scan_desktop_projects() {
        collect(scanned, &copilot_cli::load_sessions, &mut sessions);
    }
    if let Ok(scanned) = vscode::scan_projects() {
        collect(scanned, &vscode::load_sessions, &mut sessions);
    }

    sessions.sort_by(|a, b| b.last_modified.cmp(&a.last_modified));
    sessions
}

/// Heuristic: does this look like a VS Code chat session file path?
fn is_vscode_session_path(session_path: &str) -> bool {
    (session_path.contains("/workspaceStorage/") || session_path.contains("\\workspaceStorage\\"))
        && (session_path.contains("/chatSessions/") || session_path.contains("\\chatSessions\\"))
}

fn sort_and_truncate_results(results: &mut Vec<ClaudeMessage>, limit: usize) {
    results.sort_by(|a, b| {
        match (
            parse_rfc3339_utc(&a.timestamp),
            parse_rfc3339_utc(&b.timestamp),
        ) {
            (Some(a_ts), Some(b_ts)) => b_ts.cmp(&a_ts),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => b.timestamp.cmp(&a.timestamp),
        }
    });
    results.truncate(limit);
}

/// Load messages by sniffing the session file path and dispatching to the
/// correct sub-scanner. Both `copilot_cli` and `vscode` loaders already stamp
/// `provider: "copilot"` on each message because we updated their constants.
pub fn load_messages(session_path: &str) -> Result<Vec<ClaudeMessage>, String> {
    if is_vscode_session_path(session_path) {
        vscode::load_messages(session_path)
    } else {
        copilot_cli::load_messages(session_path)
    }
}

/// Search across all three sub-providers and merge results, capping at `limit`.
pub fn search(query: &str, limit: usize) -> Result<Vec<ClaudeMessage>, String> {
    let mut out = Vec::new();
    if let Ok(r) = copilot_cli::search(query, limit) {
        out.extend(r);
    }
    if let Ok(r) = copilot_cli::search_desktop(query, limit) {
        out.extend(r);
    }
    if let Ok(r) = vscode::search(query, limit) {
        out.extend(r);
    }
    sort_and_truncate_results(&mut out, limit);
    Ok(out)
}

/// WSL/custom-path search variant.
pub fn search_from_paths(
    copilot_base_path: Option<&str>,
    vscode_user_data_path: Option<&Path>,
    query: &str,
    limit: usize,
) -> Result<Vec<ClaudeMessage>, String> {
    let mut out = Vec::new();
    if let Some(base) = copilot_base_path {
        if let Ok(r) = copilot_cli::search_from_path(base, query, limit) {
            out.extend(r);
        }
        if let Ok(r) = copilot_cli::search_desktop_from_path(base, query, limit) {
            out.extend(r);
        }
    }
    if let Some(base) = vscode_user_data_path {
        if let Ok(r) = vscode::search_from_user_data_path(base, query, limit) {
            out.extend(r);
        }
    }
    sort_and_truncate_results(&mut out, limit);
    Ok(out)
}

// ============================================================================
// Archive glue (snapshot-backed reads; one source per physical root).
//
// The CLI/Desktop sub-scanners share `~/.copilot` (role `cli`); each VS Code
// `User` dir is its own source (role `vscode*`). Scans merge across all
// preserved snapshots with deterministic primary ownership so concurrent
// per-source scan jobs never duplicate projects.
// ============================================================================

use crate::storage::registry::DiscoveredSource as ArchiveDiscoveredSource;
use crate::storage::{SnapshotInfo as ArchiveSnapshotInfo, Source as ArchiveSource};

/// Physical Copilot roots: `~/.copilot` plus one VS Code `User` dir per
/// installed editor build.
pub(crate) fn archive_discover() -> Vec<ArchiveDiscoveredSource> {
    let machine = crate::storage::registry::discovery_machine_id();
    let mut out = Vec::new();
    if let Some(base) = copilot_cli::get_base_path() {
        out.push(ArchiveDiscoveredSource::local(
            "cli",
            std::path::PathBuf::from(base),
            &machine,
        ));
    }
    for user_dir in vscode::get_base_paths() {
        let role: &'static str = if user_dir.to_string_lossy().contains("Insiders") {
            "vscode-insiders"
        } else if user_dir.to_string_lossy().contains("VSCodium") {
            "vscodium"
        } else {
            "vscode"
        };
        let mut src = ArchiveDiscoveredSource::local(role, user_dir, &machine);
        src.label = Some("VS Code".to_string());
        out.push(src);
    }
    out
}

/// Snapshot of every Copilot source that has one, keyed by role.
fn snapshots_by_role() -> Vec<(String, ArchiveSource, ArchiveSnapshotInfo)> {
    let mut out = Vec::new();
    for found in archive_discover() {
        let source = crate::storage::registry::source_for_discovered("copilot", &found);
        let id = {
            let Ok(data_root) = crate::storage::snapshot::data_root() else {
                continue;
            };
            crate::storage::source::resolve_source_id(&data_root, &source.id)
        };
        if let Some(snapshot) = crate::storage::latest_completed_snapshot(&id) {
            let mut effective = source.clone();
            effective.id = id;
            out.push((found.role.to_string(), effective, snapshot));
        }
    }
    // Adopted lineages whose live roots are gone (deleted dirs) still serve.
    for stored in crate::storage::registry::registered_sources_of("copilot") {
        if out.iter().any(|(_, s, _)| s.id == stored.id) {
            continue;
        }
        if let Some(snapshot) = crate::storage::latest_completed_snapshot(&stored.id) {
            out.push((stored.role.clone(), stored, snapshot));
        }
    }
    out
}

/// This job owns the merged project list: `cli` when present, else the first
/// vscode source (deterministic so parallel scan jobs never duplicate).
fn owns_merge(role: &str, all_roles: &[String]) -> bool {
    if all_roles.iter().any(|r| r == "cli") {
        role == "cli"
    } else {
        all_roles.first().is_some_and(|first| first == role)
    }
}

/// Scan every preserved Copilot snapshot and merge by folder. Non-owning jobs
/// return empty so the registry concatenation holds exactly one copy.
#[allow(clippy::unnecessary_wraps)]
pub(crate) fn archive_scan(
    source: &ArchiveSource,
    _snapshot: &ArchiveSnapshotInfo,
) -> Result<Vec<ClaudeProject>, String> {
    let all = snapshots_by_role();
    let roles: Vec<String> = all.iter().map(|(r, _, _)| r.clone()).collect();
    if !owns_merge(&source.role, &roles) {
        return Ok(Vec::new());
    }
    let mut parts: Vec<(SourceKind, ClaudeProject)> = Vec::new();
    for (role, src, snap) in &all {
        let live_root = src.original_root.clone().unwrap_or_default();
        let snap_root = snap.data_path.to_string_lossy().to_string();
        if role == "cli" {
            let base = snap_root.clone();
            if let Ok(p) = copilot_cli::scan_projects_from_path(&base, None) {
                parts.extend(tag(SourceKind::Cli, p));
            }
            if let Ok(p) = copilot_cli::scan_desktop_projects_from_path(&base, None) {
                parts.extend(tag(SourceKind::Desktop, p));
            }
            // WSL-form project paths embed the snapshot base; rewrite to live.
            for (_, project) in &mut parts {
                if !live_root.is_empty() && project.path.contains(&snap_root) {
                    project.path = project.path.replace(&snap_root, &live_root);
                }
            }
        } else if let Ok(p) = vscode::scan_projects_from_user_data_path(&snap.data_path, None) {
            // `vscode://{snapshot}/…` → `vscode://{live}/…` before merging so
            // the encoded ref holds stable IDs (the generic rewriter cannot
            // see inside the base64 payload).
            let rewritten: Vec<ClaudeProject> = p
                .into_iter()
                .map(|mut project| {
                    if !live_root.is_empty() && project.path.contains(&snap_root) {
                        project.path = project.path.replace(&snap_root, &live_root);
                    }
                    project
                })
                .collect();
            parts.extend(tag(SourceKind::VsCode, rewritten));
        }
    }
    Ok(merge_projects(parts))
}

/// Rewrite one batch of sessions/messages from snapshot space to stable IDs
/// using that snapshot's own live root.
fn rewrite_batch(
    snap_root: &std::path::Path,
    live_root: &str,
    sessions: &mut [ClaudeSession],
    messages: &mut [ClaudeMessage],
) {
    if live_root.is_empty() {
        return;
    }
    let prefix = snap_root.to_string_lossy().to_string();
    if prefix.is_empty() {
        return;
    }
    for session in sessions.iter_mut() {
        if session.session_id.contains(&prefix) {
            session.session_id = session.session_id.replace(&prefix, live_root);
        }
        if session.file_path.contains(&prefix) {
            session.file_path = session.file_path.replace(&prefix, live_root);
        }
    }
    for message in messages.iter_mut() {
        if message.session_id.contains(&prefix) {
            message.session_id = message.session_id.replace(&prefix, live_root);
        }
    }
}

/// Locate the covering source: merged `copilot://` refs fan out per embedded
/// sub-source at load time, so route to the merge owner; native sub-paths
/// (`copilot-cli://…`, `vscode://…`, plain files) locate directly.
pub(crate) fn archive_locate(
    sources: &[crate::storage::registry::ResolvedSource],
    stable: &str,
) -> Option<usize> {
    if stable.starts_with(PROJECT_SCHEME) {
        let roles: Vec<String> = sources.iter().map(|r| r.source.role.clone()).collect();
        return sources
            .iter()
            .position(|r| owns_merge(&r.source.role, &roles));
    }
    if stable.starts_with("vscode://") || is_vscode_session_path(stable) {
        return crate::storage::registry::locate_by_subpath_or_single(sources, stable).or_else(
            || {
                sources.iter().position(|r| {
                    r.source.role.starts_with("vscode") || r.source.role == "vscodium"
                })
            },
        );
    }
    // CLI/Desktop paths and bare cwds live under the `cli` source.
    crate::storage::registry::locate_by_subpath_or_single(sources, stable)
        .or_else(|| sources.iter().position(|r| r.source.role == "cli"))
        .or_else(|| sources.first().map(|_| 0))
}

/// Sessions for a merged `copilot://` project (fan-out across preserved
/// snapshots) or a native sub-path (single-source read).
pub(crate) fn archive_load_sessions(
    source: &ArchiveSource,
    snapshot: &ArchiveSnapshotInfo,
    stable_project: &str,
) -> Result<Vec<ClaudeSession>, String> {
    if let Some(project_ref) = decode_project_ref(stable_project) {
        let all = snapshots_by_role();
        let mut sessions = Vec::new();
        for src in project_ref.sources {
            match src.kind {
                SourceKind::Cli | SourceKind::Desktop => {
                    let client = match src.kind {
                        SourceKind::Cli => copilot_cli::ClientKind::Cli,
                        _ => copilot_cli::ClientKind::Desktop,
                    };
                    // Sub-path is `copilot-{cli,desktop}://<cwd>` or the JSON
                    // form embedding the live base; either way the cwd is
                    // what selects sessions inside a snapshot.
                    let cwd = match copilot_cli::parse_project_path(&src.path) {
                        Ok((_, cwd, _)) => cwd,
                        Err(_) => continue,
                    };
                    for (_, s, snap) in all.iter().filter(|(r, _, _)| r == "cli") {
                        let root = snap.data_path.join("session-state");
                        if let Ok(mut batch) =
                            copilot_cli::load_sessions_from_root(&root, &cwd, client)
                        {
                            rewrite_batch(
                                &snap.data_path,
                                s.original_root.as_deref().unwrap_or(""),
                                &mut batch,
                                &mut [],
                            );
                            sessions.append(&mut batch);
                        }
                    }
                }
                SourceKind::VsCode => {
                    // `vscode://{ws}`: map the workspace dir into its snapshot.
                    let found = all.iter().find_map(|(r, s, snap)| {
                        if !(r.starts_with("vscode") || r == "vscodium") {
                            return None;
                        }
                        crate::storage::registry::map_absolute_to_snapshot(s, snap, &src.path)
                            .map(|mapped| (mapped, s, snap))
                    });
                    if let Some((mapped, owner_s, owner_snap)) = found {
                        // Validation roots are the snapshot workspaceStorage dirs.
                        let roots: Vec<std::path::PathBuf> = all
                            .iter()
                            .filter(|(r, _, _)| r.starts_with("vscode") || r == "vscodium")
                            .map(|(_, _, snap)| snap.data_path.join("workspaceStorage"))
                            .collect();
                        if let Ok(mut batch) = vscode::load_sessions_in(&mapped, &roots) {
                            rewrite_batch(
                                &owner_snap.data_path,
                                owner_s.original_root.as_deref().unwrap_or(""),
                                &mut batch,
                                &mut [],
                            );
                            sessions.append(&mut batch);
                        }
                    }
                }
            }
        }
        sessions.sort_by(|a, b| b.last_modified.cmp(&a.last_modified));
        return Ok(sessions);
    }
    // Native sub-path: single-source read.
    if stable_project.starts_with("vscode://") {
        let mapped =
            crate::storage::registry::map_absolute_to_snapshot(source, snapshot, stable_project)
                .ok_or_else(|| format!("No preserved snapshot covers {stable_project}"))?;
        let ws_root = snapshot.data_path.join("workspaceStorage");
        let mut sessions = vscode::load_sessions_in(&mapped, &[ws_root])?;
        rewrite_batch(
            &snapshot.data_path,
            source.original_root.as_deref().unwrap_or(""),
            &mut sessions,
            &mut [],
        );
        return Ok(sessions);
    }
    // CLI/Desktop native path.
    let (base_opt, cwd, client) = copilot_cli::parse_project_path(stable_project)
        .map_err(|e| format!("Invalid Copilot project path: {e}"))?;
    let root = match base_opt {
        Some(base) => {
            let mapped =
                crate::storage::registry::map_absolute_to_snapshot(source, snapshot, &base)
                    .ok_or_else(|| format!("No preserved snapshot covers {stable_project}"))?;
            std::path::PathBuf::from(mapped).join("session-state")
        }
        None => snapshot.data_path.join("session-state"),
    };
    let mut sessions = copilot_cli::load_sessions_from_root(&root, &cwd, client)?;
    rewrite_batch(
        &snapshot.data_path,
        source.original_root.as_deref().unwrap_or(""),
        &mut sessions,
        &mut [],
    );
    Ok(sessions)
}

/// Rewrite loaded messages against the owning snapshot (found by mapped-path
/// prefix among all Copilot snapshots, falling back to the passed one).
fn rewrite_owned_messages(
    mapped: &str,
    fallback: (&ArchiveSource, &ArchiveSnapshotInfo),
    messages: &mut [ClaudeMessage],
) {
    let mapped_path = std::path::Path::new(mapped);
    for (_, s, snap) in snapshots_by_role() {
        if mapped_path.starts_with(&snap.data_path) {
            rewrite_batch(
                &snap.data_path,
                s.original_root.as_deref().unwrap_or(""),
                &mut [],
                messages,
            );
            return;
        }
    }
    rewrite_batch(
        fallback.1.data_path.clone().as_path(),
        fallback.0.original_root.as_deref().unwrap_or(""),
        &mut [],
        messages,
    );
}

/// Messages for a stable session: VS Code chat files and Copilot
/// `events.jsonl` files are both plain absolute paths mappable into their
/// covering snapshot.
pub(crate) fn archive_load_messages(
    source: &ArchiveSource,
    snapshot: &ArchiveSnapshotInfo,
    stable_session: &str,
) -> Result<Vec<ClaudeMessage>, String> {
    if is_vscode_session_path(stable_session) || stable_session.starts_with("vscode://") {
        // Find the covering vscode snapshot (usually the passed one).
        let mapped =
            crate::storage::registry::map_absolute_to_snapshot(source, snapshot, stable_session)
                .or_else(|| {
                    snapshots_by_role().iter().find_map(|(r, s, snap)| {
                        if !(r.starts_with("vscode") || r == "vscodium") {
                            return None;
                        }
                        crate::storage::registry::map_absolute_to_snapshot(s, snap, stable_session)
                    })
                })
                .ok_or_else(|| format!("No preserved snapshot covers {stable_session}"))?;
        // Validate against snapshot workspaceStorage roots.
        let roots: Vec<std::path::PathBuf> = snapshots_by_role()
            .iter()
            .filter(|(r, _, _)| r.starts_with("vscode") || r == "vscodium")
            .map(|(_, _, snap)| snap.data_path.join("workspaceStorage"))
            .collect();
        let roots = if roots.is_empty() {
            vec![snapshot.data_path.join("workspaceStorage")]
        } else {
            roots
        };
        let mut messages = vscode::load_messages_in(&mapped, &roots)?;
        rewrite_owned_messages(&mapped, (source, snapshot), &mut messages);
        return Ok(messages);
    }
    let mapped =
        crate::storage::registry::map_absolute_to_snapshot(source, snapshot, stable_session)
            .or_else(|| {
                snapshots_by_role().iter().find_map(|(r, s, snap)| {
                    if r != "cli" {
                        return None;
                    }
                    crate::storage::registry::map_absolute_to_snapshot(s, snap, stable_session)
                })
            })
            .ok_or_else(|| format!("No preserved snapshot covers {stable_session}"))?;
    // The mapped file lives under some snapshot session-state dir; validate
    // against its parent snapshot root.
    let allowed = snapshots_by_role()
        .iter()
        .filter(|(r, _, _)| r == "cli")
        .map(|(_, _, snap)| snap.data_path.join("session-state"))
        .find(|root| {
            std::path::Path::new(&mapped)
                .canonicalize()
                .unwrap_or_else(|_| std::path::PathBuf::from(&mapped))
                .starts_with(root.canonicalize().unwrap_or_else(|_| root.clone()))
        })
        .unwrap_or_else(|| snapshot.data_path.join("session-state"));
    let mut messages = copilot_cli::load_messages_in(&mapped, &allowed)?;
    rewrite_owned_messages(&mapped, (source, snapshot), &mut messages);
    Ok(messages)
}

/// Search across all preserved Copilot snapshots, newest first.
// Returns Result for registry table uniformity; the scan is infallible.
#[allow(clippy::unnecessary_wraps)]
pub(crate) fn archive_search(
    _source: &ArchiveSource,
    _snapshot: &ArchiveSnapshotInfo,
    query: &str,
    limit: usize,
) -> Result<Vec<ClaudeMessage>, String> {
    let mut out = Vec::new();
    for (role, src, snap) in snapshots_by_role() {
        let live_root = src.original_root.clone().unwrap_or_default();
        if role == "cli" {
            // Snapshot-confined: read events files via the _in loader.
            let root = snap.data_path.join("session-state");
            if root.is_dir() {
                let query_lower = query.to_lowercase();
                for entry in walkdir::WalkDir::new(&root)
                    .follow_links(false)
                    .min_depth(2)
                    .max_depth(2)
                    .into_iter()
                    .filter_map(Result::ok)
                    .filter(|e| e.file_type().is_file())
                {
                    if entry.path().file_name().and_then(|n| n.to_str()) != Some("events.jsonl") {
                        continue;
                    }
                    if let Ok(mut messages) =
                        copilot_cli::load_messages_in(&entry.path().to_string_lossy(), &root)
                    {
                        rewrite_batch(&snap.data_path, &live_root, &mut [], &mut messages);
                        for msg in messages {
                            if out.len() >= limit * 2 {
                                break;
                            }
                            if let Some(content) = &msg.content {
                                if crate::utils::search_json_value_case_insensitive(
                                    content,
                                    &query_lower,
                                ) {
                                    out.push(msg);
                                }
                            }
                        }
                    }
                }
            }
        } else {
            // Snapshot-confined walker (the public search_from_user_data_path
            // internally calls the live-validating loader).
            let ws_root = snap.data_path.join("workspaceStorage");
            let query_lower = query.to_lowercase();
            let mut results = Vec::new();
            let _ = vscode::search_workspace_storage_in(
                &ws_root,
                &ws_root,
                &query_lower,
                limit,
                &mut results,
            );
            rewrite_batch(&snap.data_path, &live_root, &mut [], &mut results);
            out.extend(results);
        }
    }
    sort_and_truncate_results(&mut out, limit);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(actual_path: &str, path: &str, sessions: usize, messages: usize) -> ClaudeProject {
        ClaudeProject {
            name: Path::new(actual_path)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| actual_path.to_string()),
            path: path.to_string(),
            actual_path: actual_path.to_string(),
            session_count: sessions,
            message_count: messages,
            last_modified: "2026-01-01T00:00:00Z".to_string(),
            git_info: None,
            provider: Some("copilot".to_string()),
            storage_type: None,
            custom_directory_label: None,
        }
    }

    fn message(uuid: &str, timestamp: &str) -> ClaudeMessage {
        ClaudeMessage {
            uuid: uuid.to_string(),
            parent_uuid: None,
            session_id: "session".to_string(),
            timestamp: timestamp.to_string(),
            message_type: "user".to_string(),
            content: None,
            project_name: None,
            tool_use: None,
            tool_use_result: None,
            is_sidechain: None,
            usage: None,
            role: None,
            model: None,
            stop_reason: None,
            cost_usd: None,
            duration_ms: None,
            message_id: None,
            snapshot: None,
            is_snapshot_update: None,
            data: None,
            tool_use_id: None,
            parent_tool_use_id: None,
            operation: None,
            subtype: None,
            level: None,
            hook_count: None,
            hook_infos: None,
            stop_reason_system: None,
            prevented_continuation: None,
            compact_metadata: None,
            microcompact_metadata: None,
            provider: Some(PROVIDER_ID.to_string()),
        }
    }

    #[test]
    fn group_key_strips_file_prefix_and_trailing_slash() {
        assert_eq!(group_key("/Users/me/repo"), "/Users/me/repo");
        assert_eq!(group_key("file:///Users/me/repo"), "/Users/me/repo");
        assert_eq!(group_key("file:///Users/me/repo/"), "/Users/me/repo");
    }

    #[test]
    fn merge_collapses_cli_and_vscode_for_same_folder() {
        let cli = project("/Users/me/repo", "copilot-cli:///Users/me/repo", 2, 50);
        let vsc = project(
            "file:///Users/me/repo",
            "vscode:///Users/me/.vscode/workspaceStorage/abc",
            3,
            70,
        );
        let merged = merge_projects(vec![(SourceKind::Cli, cli), (SourceKind::VsCode, vsc)]);
        assert_eq!(merged.len(), 1);
        let p = &merged[0];
        assert_eq!(p.session_count, 5);
        assert_eq!(p.message_count, 120);
        assert_eq!(p.actual_path, "/Users/me/repo");
        assert!(p.path.starts_with("copilot://"));
        assert_eq!(p.provider.as_deref(), Some("copilot"));

        // Round-trip: decoded ref should preserve both source paths.
        let decoded = decode_project_ref(&p.path).expect("decodes");
        assert_eq!(decoded.actual, "/Users/me/repo");
        assert_eq!(decoded.sources.len(), 2);
        let cli_src = decoded
            .sources
            .iter()
            .find(|s| s.kind == SourceKind::Cli)
            .unwrap();
        let vsc_src = decoded
            .sources
            .iter()
            .find(|s| s.kind == SourceKind::VsCode)
            .unwrap();
        assert_eq!(cli_src.path, "copilot-cli:///Users/me/repo");
        assert_eq!(
            vsc_src.path,
            "vscode:///Users/me/.vscode/workspaceStorage/abc"
        );
    }

    #[test]
    fn merge_keeps_distinct_folders_separate() {
        let a = project("/repo/a", "copilot-cli:///repo/a", 1, 5);
        let b = project("/repo/b", "copilot-cli:///repo/b", 2, 10);
        let merged = merge_projects(vec![(SourceKind::Cli, a), (SourceKind::Cli, b)]);
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn project_ref_round_trips() {
        let r = ProjectRef {
            actual: "/Users/me/repo".to_string(),
            sources: vec![
                SourceRef {
                    kind: SourceKind::Cli,
                    path: "copilot-cli:///x".to_string(),
                },
                SourceRef {
                    kind: SourceKind::VsCode,
                    path: "vscode:///y".to_string(),
                },
            ],
        };
        let encoded = encode_project_ref(&r);
        assert!(encoded.starts_with("copilot://"));
        let decoded = decode_project_ref(&encoded).unwrap();
        assert_eq!(decoded.actual, r.actual);
        assert_eq!(decoded.sources.len(), 2);
    }

    #[test]
    fn decode_project_ref_returns_none_for_legacy_url() {
        // Old format without base64 payload should not falsely decode.
        assert!(decode_project_ref("copilot:///repo/a").is_none());
    }

    #[test]
    fn search_results_sort_before_truncate() {
        let mut results = vec![
            message("old", "2026-01-01T00:00:00Z"),
            message("invalid", "not-a-timestamp"),
            message("new", "2026-01-02T00:00:00Z"),
        ];

        sort_and_truncate_results(&mut results, 2);

        assert_eq!(
            results.iter().map(|m| m.uuid.as_str()).collect::<Vec<_>>(),
            vec!["new", "old"]
        );
    }

    #[test]
    fn is_vscode_session_path_detects_chatsessions_files() {
        assert!(is_vscode_session_path(
            "/Users/me/Library/Application Support/Code/User/workspaceStorage/abc/chatSessions/x.jsonl"
        ));
        assert!(is_vscode_session_path(
            r"C:\Users\me\AppData\Roaming\Code\User\workspaceStorage\abc\chatSessions\x.jsonl"
        ));
        assert!(!is_vscode_session_path(
            "/Users/me/.copilot/session-state/abc/events.jsonl"
        ));
    }
}
