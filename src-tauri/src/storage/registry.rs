//! Universal provider archive registry.
//!
//! Every provider declares the physical storage it needs; one provider may
//! own several sources (roles), every source snapshots independently, and
//! scanners/loaders receive archived roots rather than hardcoded live roots.
//! Existing parsers are reused unchanged.
//!
//! Two shapes cover (almost) every provider:
//!
//! * **Absolute-path stable IDs** (plain paths or URI-wrapped, e.g.
//!   `gemini://{abs}`, `vscode://{abs}`): generic longest-prefix mapping
//!   between original roots and snapshot roots, in both directions.
//! * **Opaque stable IDs** (`opencode://id`, `trae://h#s`, `kiro://conv`, …):
//!   a small provider hook maps the ID into snapshot space; content-derived
//!   IDs need no output rewriting at all.
//!
//! [`ProviderArchiveSpec`] is one row per provider. Migrated providers are
//! served exclusively through [`scan_provider`], [`load_provider_sessions`],
//! [`load_provider_messages`], and [`search_provider`]; unmigrated providers
//! keep their legacy dispatch arms until their checkpoint lands.

use std::path::{Path, PathBuf};

use crate::models::{ClaudeMessage, ClaudeProject, ClaudeSession};
use crate::storage::{SnapshotInfo, Source, SourceKind};

// ---------------------------------------------------------------------------
// Spec types.
// ---------------------------------------------------------------------------

/// One physical root discovered for a provider.
#[derive(Debug, Clone)]
pub struct DiscoveredSource {
    /// Storage role (`primary`, `cli`, `vscode`, `global`, …).
    pub role: &'static str,
    /// Live root directory (or UNC root for WSL origins).
    pub root: PathBuf,
    /// Human label for UI/status.
    pub label: Option<String>,
    /// Local vs WSL origin. Remote origins are discovered separately.
    pub kind: SourceKind,
    /// Machine identity (`local_machine_id()` or `wsl:<distro>`).
    pub machine_id: String,
    /// Snapshot-relative database paths needing backup capture.
    pub sqlite_dbs: Vec<String>,
    /// Snapshot-relative subtree prefixes to capture (empty = whole root).
    /// Entries naming an exact file capture just that file.
    pub includes: Vec<String>,
    /// Maximum directory depth below the root to capture (`None` =
    /// unbounded). Counts the root's children as depth 1.
    pub max_depth: Option<usize>,
    /// Extra databases discovered at sync time (e.g. per-workspace stores).
    pub extra_sqlite_dbs: Vec<String>,
}

impl DiscoveredSource {
    pub fn local(role: &'static str, root: PathBuf, machine_id: &str) -> Self {
        Self {
            role,
            root,
            label: None,
            kind: SourceKind::Local,
            machine_id: machine_id.to_string(),
            sqlite_dbs: Vec::new(),
            includes: Vec::new(),
            max_depth: None,
            extra_sqlite_dbs: Vec::new(),
        }
    }
}

/// A registered source with its latest completed snapshot attached.
#[derive(Debug, Clone)]
pub struct ResolvedSource {
    pub source: Source,
    pub snapshot: SnapshotInfo,
}

/// Scan/parse seams operating on an explicit snapshot (or live root wrapped
/// the same way). Role-aware: multi-root providers (kimi legacy vs code)
/// branch on `source.role`.
pub type ScanFn = fn(&Source, &SnapshotInfo) -> Result<Vec<ClaudeProject>, String>;
/// `(source, snapshot, stable_project_arg)`.
pub type SessionsFn = fn(&Source, &SnapshotInfo, &str) -> Result<Vec<ClaudeSession>, String>;
/// `(source, snapshot, stable_session_arg)`.
pub type MessagesFn = fn(&Source, &SnapshotInfo, &str) -> Result<Vec<ClaudeMessage>, String>;
/// `(source, snapshot, query, limit)`.
pub type SearchFn = fn(&Source, &SnapshotInfo, &str, usize) -> Result<Vec<ClaudeMessage>, String>;
/// Pick the covering source index for a stable ID, if any.
pub type LocateFn = fn(&[ResolvedSource], &str) -> Option<usize>;

/// One row per migrated provider.
pub struct ProviderArchiveSpec {
    pub provider: &'static str,
    /// The provider's physical sources on this machine.
    pub discover: fn() -> Vec<DiscoveredSource>,
    /// Scan projects under an explicit root.
    pub scan: ScanFn,
    /// Load sessions for a stable project ID from a snapshot.
    pub load_sessions: SessionsFn,
    /// Load messages for a stable session ID from a snapshot.
    pub load_messages: MessagesFn,
    /// Search one snapshot. `None` when the provider has no search.
    pub search: Option<SearchFn>,
    /// Locate the covering source for a stable project/session ID.
    pub locate: LocateFn,
    /// Whether outputs embed absolute paths needing snapshot→stable rewrite.
    /// `false` for content-derived opaque URIs (opencode://, trae://, …).
    pub rewrite_outputs: bool,
}

// ---------------------------------------------------------------------------
// Spec table (grows per migration checkpoint).
// ---------------------------------------------------------------------------

fn spec_table() -> Vec<ProviderArchiveSpec> {
    use crate::providers;
    vec![
        ProviderArchiveSpec {
            provider: "pi",
            discover: providers::pi::archive_discover,
            scan: providers::pi::archive_scan,
            load_sessions: providers::pi::archive_load_sessions,
            load_messages: providers::pi::archive_load_messages,
            search: Some(providers::pi::archive_search),
            locate: locate_by_subpath_or_single,
            rewrite_outputs: true,
        },
        ProviderArchiveSpec {
            provider: "ompi",
            discover: providers::ompi::archive_discover,
            scan: providers::ompi::archive_scan,
            load_sessions: providers::ompi::archive_load_sessions,
            load_messages: providers::ompi::archive_load_messages,
            search: Some(providers::ompi::archive_search),
            locate: locate_by_subpath_or_single,
            rewrite_outputs: true,
        },
        ProviderArchiveSpec {
            provider: "continue",
            discover: providers::continue_dev::archive_discover,
            scan: providers::continue_dev::archive_scan,
            load_sessions: providers::continue_dev::archive_load_sessions,
            load_messages: providers::continue_dev::archive_load_messages,
            search: Some(providers::continue_dev::archive_search),
            locate: locate_by_subpath_or_single,
            rewrite_outputs: true,
        },
        ProviderArchiveSpec {
            provider: "pearai",
            discover: providers::pearai::archive_discover,
            scan: providers::pearai::archive_scan,
            load_sessions: providers::pearai::archive_load_sessions,
            load_messages: providers::pearai::archive_load_messages,
            search: Some(providers::pearai::archive_search),
            locate: locate_by_subpath_or_single,
            rewrite_outputs: true,
        },
        ProviderArchiveSpec {
            provider: "grok",
            discover: providers::grok::archive_discover,
            scan: providers::grok::archive_scan,
            load_sessions: providers::grok::archive_load_sessions,
            load_messages: providers::grok::archive_load_messages,
            search: Some(providers::grok::archive_search),
            locate: locate_by_subpath_or_single,
            rewrite_outputs: true,
        },
        ProviderArchiveSpec {
            provider: "kimi",
            discover: providers::kimi::archive_discover,
            scan: providers::kimi::archive_scan,
            load_sessions: providers::kimi::archive_load_sessions,
            load_messages: providers::kimi::archive_load_messages,
            search: Some(providers::kimi::archive_search),
            locate: locate_by_subpath_or_single,
            rewrite_outputs: true,
        },
        ProviderArchiveSpec {
            provider: "vibe",
            discover: providers::vibe::archive_discover,
            scan: providers::vibe::archive_scan,
            load_sessions: providers::vibe::archive_load_sessions,
            load_messages: providers::vibe::archive_load_messages,
            search: Some(providers::vibe::archive_search),
            locate: locate_by_subpath_or_single,
            rewrite_outputs: true,
        },
        ProviderArchiveSpec {
            provider: "gemini",
            discover: providers::gemini::archive_discover,
            scan: providers::gemini::archive_scan,
            load_sessions: providers::gemini::archive_load_sessions,
            load_messages: providers::gemini::archive_load_messages,
            search: Some(providers::gemini::archive_search),
            locate: locate_by_subpath_or_single,
            rewrite_outputs: true,
        },
        ProviderArchiveSpec {
            provider: "qwen",
            discover: providers::qwen::archive_discover,
            scan: providers::qwen::archive_scan,
            load_sessions: providers::qwen::archive_load_sessions,
            load_messages: providers::qwen::archive_load_messages,
            search: Some(providers::qwen::archive_search),
            locate: locate_by_subpath_or_single,
            rewrite_outputs: true,
        },
        ProviderArchiveSpec {
            provider: "deepseek",
            discover: providers::deepseek::archive_discover,
            scan: providers::deepseek::archive_scan,
            load_sessions: providers::deepseek::archive_load_sessions,
            load_messages: providers::deepseek::archive_load_messages,
            search: None,
            locate: locate_by_subpath_or_single,
            rewrite_outputs: true,
        },
        ProviderArchiveSpec {
            provider: "openhands",
            discover: providers::openhands::archive_discover,
            scan: providers::openhands::archive_scan,
            load_sessions: providers::openhands::archive_load_sessions,
            load_messages: providers::openhands::archive_load_messages,
            search: Some(providers::openhands::archive_search),
            locate: locate_by_subpath_or_single,
            rewrite_outputs: false,
        },
        ProviderArchiveSpec {
            provider: "aider",
            discover: providers::aider::archive_discover,
            scan: providers::aider::archive_scan,
            load_sessions: providers::aider::archive_load_sessions,
            load_messages: providers::aider::archive_load_messages,
            search: Some(providers::aider::archive_search),
            locate: locate_by_subpath_or_single,
            rewrite_outputs: true,
        },
        ProviderArchiveSpec {
            provider: "codebuddy",
            discover: providers::codebuddy::archive_discover,
            scan: providers::codebuddy::archive_scan,
            load_sessions: providers::codebuddy::archive_load_sessions,
            load_messages: providers::codebuddy::archive_load_messages,
            search: Some(providers::codebuddy::archive_search),
            locate: locate_by_subpath_or_single,
            rewrite_outputs: true,
        },
        ProviderArchiveSpec {
            provider: "cursor-agent",
            discover: providers::cursor_agent::archive_discover,
            scan: providers::cursor_agent::archive_scan,
            load_sessions: providers::cursor_agent::archive_load_sessions,
            load_messages: providers::cursor_agent::archive_load_messages,
            search: Some(providers::cursor_agent::archive_search),
            locate: locate_by_subpath_or_single,
            rewrite_outputs: true,
        },
    ]
}

/// Spec for a migrated provider, if one is registered.
#[must_use]
pub fn spec_for(provider: &str) -> Option<ProviderArchiveSpec> {
    // Rebuilt per call: the table is tiny and specs are plain fn pointers.
    spec_table().into_iter().find(|s| s.provider == provider)
}

/// Whether normal reads for `provider` are served from snapshots.
#[must_use]
pub fn is_migrated(provider: &str) -> bool {
    spec_for(provider).is_some()
}

/// All migrated provider ids (the registry table is the single source of
/// truth; dispatch sites iterate this instead of hardcoding lists).
#[must_use]
pub fn migrated_providers() -> Vec<&'static str> {
    spec_table().into_iter().map(|s| s.provider).collect()
}

/// Indexer for a migrated provider, if one is registered.
#[must_use]
pub fn indexer_for(provider: &str) -> Option<crate::storage::index::SnapshotIndexer> {
    spec_for(provider).map(|_| generic_filetree_indexer as crate::storage::index::SnapshotIndexer)
}

// ---------------------------------------------------------------------------
// Source construction + snapshot attachment.
// ---------------------------------------------------------------------------

/// Build the stable [`Source`] for a discovered root.
#[must_use]
pub fn source_for_discovered(provider: &str, discovered: &DiscoveredSource) -> Source {
    let origin = match discovered.kind {
        SourceKind::Wsl => {
            // Spelling-independent identity: distro + UNC shape. The exact UNC
            // spelling (wsl.localhost vs wsl$) is connection detail.
            format!(
                "{}|{}",
                discovered.machine_id,
                discovered.root.to_string_lossy()
            )
        }
        SourceKind::Remote => discovered.root.to_string_lossy().to_string(),
        SourceKind::Local | SourceKind::Custom => {
            crate::storage::source::canonical_path_origin(&discovered.root)
        }
    };
    let id = crate::storage::source::canonical_source_id(
        &discovered.machine_id,
        &discovered.kind,
        provider,
        discovered.role,
        &origin,
    );
    Source {
        id,
        kind: discovered.kind.clone(),
        provider: provider.to_string(),
        role: discovered.role.to_string(),
        machine_id: discovered.machine_id.clone(),
        origin,
        original_root: Some(discovered.root.to_string_lossy().to_string()),
        endpoint: None,
        label: discovered.label.clone(),
        sqlite_dbs: discovered.sqlite_dbs.clone(),
        includes: discovered.includes.clone(),
        max_depth: discovered.max_depth,
    }
}

/// Attach the latest completed snapshot to every registered source of a
/// provider that has one. Sources without snapshots are skipped (their live
/// roots simply have nothing preserved yet).
#[must_use]
pub fn resolved_sources(provider: &str, discovered: &[DiscoveredSource]) -> Vec<ResolvedSource> {
    let mut out = Vec::new();
    for found in discovered {
        let source = source_for_discovered(provider, found);
        // Claim first so older lineage dirs are adopted before reading.
        let source = match crate::storage::source::claim_canonical_source(&source) {
            Ok(effective) => effective,
            Err(e) => {
                log::warn!("Archive claim failed for {}: {e}", source.id);
                continue;
            }
        };
        if let Some(snapshot) = crate::storage::latest_completed_snapshot(&source.id) {
            out.push(ResolvedSource { source, snapshot });
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Mapping helpers shared by provider glue fns.
// ---------------------------------------------------------------------------

/// Normalize a path-ish string for cross-platform prefix matching.
fn normalize_for_match(raw: &str) -> String {
    raw.replace('\\', "/")
}

/// Map a stable absolute path (plain or URI-embedded) into snapshot space.
/// Returns the snapshot-space argument, or `None` when no registered root
/// covers it or the mapped file is absent (cumulative snapshots keep deleted
/// files, so absence means never observed).
#[must_use]
pub fn map_absolute_to_snapshot(
    source: &Source,
    snapshot: &SnapshotInfo,
    stable: &str,
) -> Option<String> {
    let original_root = source.original_root.as_deref()?;
    let normalized = normalize_for_match(stable);
    // Longest spelling wins (a root may appear in several spellings), whether
    // anchored (plain absolute paths) or embedded (URI-wrapped absolute
    // paths like `grok://{root}/…`).
    let mut best: Option<(String, String)> = None;
    for spelling in [
        original_root.to_string(),
        normalize_for_match(original_root),
    ] {
        if spelling.is_empty() {
            continue;
        }
        let trimmed = spelling.trim_end_matches('/');
        // Anchored match first.
        if normalized == spelling
            || normalized
                .strip_prefix(trimmed)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
        {
            let rest = normalized
                .strip_prefix(trimmed)
                .unwrap_or("")
                .trim_start_matches('/');
            if best
                .as_ref()
                .map_or(true, |(_, prev)| spelling.len() > prev.len())
            {
                best = Some((rest.to_string(), spelling));
            }
            continue;
        }
        // URI-embedded match: split on the first occurrence and keep the tail.
        if let Some((_, tail)) = normalized.split_once(trimmed) {
            if tail.is_empty() || tail.starts_with('/') {
                let rest = tail.trim_start_matches('/');
                if best
                    .as_ref()
                    .map_or(true, |(_, prev)| spelling.len() > prev.len())
                {
                    best = Some((rest.to_string(), spelling));
                }
            }
        }
    }
    let (rest, _spelling) = best?;
    let mapped = if rest.is_empty() {
        snapshot.data_path.clone()
    } else {
        snapshot.data_path.join(rest)
    };
    if mapped.exists() {
        Some(mapped.to_string_lossy().to_string())
    } else {
        None
    }
}

/// Default locator: longest original-root match (anchored for plain absolute
/// paths, substring for URI-embedded ones like `grok://{root}/…`), falling
/// back to the single source for content-filtered providers.
pub fn locate_by_subpath_or_single(sources: &[ResolvedSource], stable: &str) -> Option<usize> {
    let normalized = normalize_for_match(stable);
    let mut best: Option<(usize, usize)> = None;
    for (idx, candidate) in sources.iter().enumerate() {
        let Some(original_root) = candidate.source.original_root.as_deref() else {
            continue;
        };
        for spelling in [
            original_root.to_string(),
            normalize_for_match(original_root),
        ] {
            if spelling.is_empty() {
                continue;
            }
            let trimmed = spelling.trim_end_matches('/');
            let anchored = normalized == spelling
                || normalized
                    .strip_prefix(trimmed)
                    .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'));
            let embedded = !anchored
                && normalized
                    .split_once(trimmed)
                    .is_some_and(|(_, tail)| tail.is_empty() || tail.starts_with('/'));
            if (anchored || embedded) && best.map_or(true, |(_, len)| spelling.len() > len) {
                best = Some((idx, spelling.len()));
            }
        }
    }
    if let Some((idx, _)) = best {
        return Some(idx);
    }
    if sources.len() == 1 {
        return Some(0);
    }
    None
}

// ---------------------------------------------------------------------------
// Output rewriting (snapshot interior → stable external IDs).
// ---------------------------------------------------------------------------

/// Rewrite snapshot-interior absolute paths back to stable IDs in the
/// well-known identifier fields. Replaces the exact snapshot-root string
/// wherever it occurs (plain paths and URI-embedded ones like
/// `grok://{root}/…`). The snapshot root is a unique archive path, and only
/// identifier fields are touched, so real user paths and message content are
/// never altered.
pub fn rewrite_to_stable(
    snapshot_data_root: &Path,
    original_root: &str,
    projects: &mut [ClaudeProject],
    sessions: &mut [ClaudeSession],
    messages: &mut [ClaudeMessage],
) {
    if original_root.is_empty() {
        return;
    }
    let prefix = snapshot_data_root.to_string_lossy().to_string();
    if prefix.is_empty() {
        return;
    }
    let rewrite = |value: &mut String| {
        if value.contains(&prefix) {
            *value = value.replace(&prefix, original_root);
        }
    };
    for project in projects.iter_mut() {
        rewrite(&mut project.path);
    }
    for session in sessions.iter_mut() {
        rewrite(&mut session.session_id);
        rewrite(&mut session.file_path);
    }
    for message in messages.iter_mut() {
        rewrite(&mut message.session_id);
    }
}

/// Map a stable ID into snapshot space (alias for the Claude-era helper).
#[must_use]
pub fn map_stable_path(
    provider: &str,
    stable_path: &Path,
) -> Option<(
    crate::storage::Source,
    crate::storage::SnapshotInfo,
    PathBuf,
)> {
    let _ = provider;
    crate::storage::map_original_path_to_snapshot(stable_path).and_then(|mapped| {
        let source = crate::storage::find_source_for_original_path(stable_path)?;
        let snapshot = crate::storage::latest_completed_snapshot(&source.id)?;
        Some((source, snapshot, mapped))
    })
}

// ---------------------------------------------------------------------------
// Generic flows (sync → parse latest → rewrite → aggregate).
// ---------------------------------------------------------------------------

/// Best-effort machine id for discovery (empty when the id file is momentarily
/// unavailable; syncs must never fail on identity alone).
pub fn discovery_machine_id() -> String {
    crate::storage::machine::local_machine_id().unwrap_or_default()
}

/// Discover + sync + scan every source of a migrated provider.
pub async fn scan_provider(provider: &str) -> Result<Vec<ClaudeProject>, String> {
    let provider = provider.to_string();
    let Some(spec) = spec_for(&provider) else {
        return Err(format!("Provider {provider} is not archive-migrated"));
    };
    let discovered = (spec.discover)();
    // Sync + parse per source on the blocking pool (filesystem I/O throughout).
    // Live-discoverable roots sync first; registered-but-undiscoverable roots
    // (deleted dirs, offline WSL distros) parse their preserved snapshots
    // without syncing, so history never disappears from the listing.
    struct ScanJob {
        source: Source,
        live_root: Option<(PathBuf, crate::storage::SyncOptions)>,
    }
    let mut jobs = Vec::new();
    let mut seen_ids = std::collections::HashSet::new();
    for found in discovered {
        let source = source_for_discovered(&provider, &found);
        seen_ids.insert(source.id.clone());
        jobs.push(ScanJob {
            source,
            live_root: Some((
                found.root.clone(),
                crate::storage::SyncOptions {
                    extra_sqlite_dbs: found.extra_sqlite_dbs.clone(),
                },
            )),
        });
    }
    for stored in registered_sources_of(&provider) {
        if seen_ids.contains(&stored.id) {
            continue;
        }
        if crate::storage::latest_completed_snapshot(&stored.id).is_none() {
            continue;
        }
        jobs.push(ScanJob {
            source: stored,
            live_root: None,
        });
    }
    if jobs.is_empty() {
        return Ok(Vec::new());
    }
    let mut handles = Vec::new();
    for job in jobs {
        let provider_name = provider.clone();
        let source = job.source;
        let live_root = job.live_root;
        let scan = spec.scan;
        let rewrite_outputs = spec.rewrite_outputs;
        handles.push(tauri::async_runtime::spawn_blocking(move || {
            if let Some((root, opts)) = &live_root {
                match crate::storage::sync_source(&source, root, opts) {
                    Ok(_) => {}
                    Err(e) => {
                        log::warn!("Archive sync skipped for {}: {e}", source.id);
                    }
                }
            }
            let source = match crate::storage::source::claim_canonical_source(&source) {
                Ok(effective) => effective,
                Err(_) => source,
            };
            let Some(snapshot) = crate::storage::latest_completed_snapshot(&source.id) else {
                return Vec::new();
            };
            let mut projects = scan(&source, &snapshot).unwrap_or_default();
            if rewrite_outputs {
                if let Some(original_root) = source.original_root.as_deref() {
                    rewrite_to_stable(
                        &snapshot.data_path,
                        original_root,
                        &mut projects,
                        &mut [],
                        &mut [],
                    );
                }
            }
            for project in &mut projects {
                if project.provider.is_none() {
                    project.provider = Some(provider_name.clone());
                }
            }
            projects
        }));
    }
    let mut all = Vec::new();
    for handle in handles {
        match handle.await {
            Ok(mut projects) => all.append(&mut projects),
            Err(e) => log::warn!("Archive scan task failed for {provider}: {e}"),
        }
    }
    Ok(all)
}

/// Load sessions for a stable project ID from snapshots.
pub async fn load_provider_sessions(
    provider: &str,
    stable_project: &str,
    sources: &[ResolvedSource],
) -> Result<Vec<ClaudeSession>, String> {
    let Some(spec) = spec_for(provider) else {
        return Err(format!("Provider {provider} is not archive-migrated"));
    };
    let Some(idx) = (spec.locate)(sources, stable_project) else {
        return Err(format!("No preserved snapshot covers {stable_project}"));
    };
    let resolved = &sources[idx];
    let source = resolved.source.clone();
    let snapshot = resolved.snapshot.clone();
    let stable = stable_project.to_string();
    let load = spec.load_sessions;
    let rewrite_outputs = spec.rewrite_outputs;
    let provider_name = provider.to_string();
    tauri::async_runtime::spawn_blocking(move || {
        let mut sessions = load(&source, &snapshot, &stable)?;
        if rewrite_outputs {
            if let Some(original_root) = source.original_root.as_deref() {
                rewrite_to_stable(
                    &snapshot.data_path,
                    original_root,
                    &mut [],
                    &mut sessions,
                    &mut [],
                );
            }
        }
        for session in &mut sessions {
            if session.provider.is_none() {
                session.provider = Some(provider_name.clone());
            }
        }
        Ok(sessions)
    })
    .await
    .map_err(|e| format!("Task join error: {e}"))?
}

/// Load messages for a stable session ID from snapshots.
pub async fn load_provider_messages(
    provider: &str,
    stable_session: &str,
    sources: &[ResolvedSource],
) -> Result<Vec<ClaudeMessage>, String> {
    let Some(spec) = spec_for(provider) else {
        return Err(format!("Provider {provider} is not archive-migrated"));
    };
    let Some(idx) = (spec.locate)(sources, stable_session) else {
        return Err(format!("No preserved snapshot covers {stable_session}"));
    };
    let resolved = &sources[idx];
    let source = resolved.source.clone();
    let snapshot = resolved.snapshot.clone();
    let stable = stable_session.to_string();
    let load = spec.load_messages;
    let rewrite_outputs = spec.rewrite_outputs;
    let provider_name = provider.to_string();
    tauri::async_runtime::spawn_blocking(move || {
        let mut messages = load(&source, &snapshot, &stable)?;
        if rewrite_outputs {
            if let Some(original_root) = source.original_root.as_deref() {
                rewrite_to_stable(
                    &snapshot.data_path,
                    original_root,
                    &mut [],
                    &mut [],
                    &mut messages,
                );
            }
        }
        for message in &mut messages {
            if message.provider.is_none() {
                message.provider = Some(provider_name.clone());
            }
        }
        Ok(messages)
    })
    .await
    .map_err(|e| format!("Task join error: {e}"))?
}

/// Search preserved snapshots of a provider.
pub async fn search_provider(
    provider: &str,
    query: &str,
    limit: usize,
    sources: &[ResolvedSource],
) -> Result<Vec<ClaudeMessage>, String> {
    let Some(spec) = spec_for(provider) else {
        return Err(format!("Provider {provider} is not archive-migrated"));
    };
    let Some(search) = spec.search else {
        return Ok(Vec::new());
    };
    let mut handles = Vec::new();
    for resolved in sources {
        let source = resolved.source.clone();
        let snapshot = resolved.snapshot.clone();
        let query = query.to_string();
        let rewrite_outputs = spec.rewrite_outputs;
        let provider_name = provider.to_string();
        handles.push(tauri::async_runtime::spawn_blocking(move || {
            let mut messages = search(&source, &snapshot, &query, limit).unwrap_or_default();
            if rewrite_outputs {
                if let Some(original_root) = source.original_root.as_deref() {
                    rewrite_to_stable(
                        &snapshot.data_path,
                        original_root,
                        &mut [],
                        &mut [],
                        &mut messages,
                    );
                }
            }
            for message in &mut messages {
                if message.provider.is_none() {
                    message.provider = Some(provider_name.clone());
                }
            }
            messages
        }));
    }
    let mut all = Vec::new();
    for handle in handles {
        match handle.await {
            Ok(mut messages) => all.append(&mut messages),
            Err(e) => log::warn!("Archive search task failed for {provider}: {e}"),
        }
    }
    Ok(all)
}

/// Snapshot-attached sources for the read path (no syncing: syncs happen on
/// scan/coordinator passes; reads must never block on the network or disk
/// beyond parsing).
#[must_use]
pub fn read_sources(provider: &str) -> Vec<ResolvedSource> {
    let Some(spec) = spec_for(provider) else {
        return Vec::new();
    };
    let discovered = (spec.discover)();
    let mut out = Vec::new();
    for found in &discovered {
        let source = source_for_discovered(provider, found);
        // Resolve aliases without adopting: reads never rename directories.
        let id = {
            let Ok(data_root) = crate::storage::snapshot::data_root() else {
                continue;
            };
            crate::storage::source::resolve_source_id(&data_root, &source.id)
        };
        if let Some(snapshot) = crate::storage::latest_completed_snapshot(&id) {
            let mut effective = source.clone();
            effective.id = id;
            out.push(ResolvedSource {
                source: effective,
                snapshot,
            });
        }
    }
    // Also pick up adopted/legacy lineages whose live roots are currently
    // undiscoverable (deleted dirs, offline WSL): match registered records by
    // provider and attach their snapshots.
    for stored in registered_sources_of(provider) {
        if out.iter().any(|r| r.source.id == stored.id) {
            continue;
        }
        if let Some(snapshot) = crate::storage::latest_completed_snapshot(&stored.id) {
            out.push(ResolvedSource {
                source: stored,
                snapshot,
            });
        }
    }
    out
}

/// All registered sources of a provider (from `source.json` records).
#[must_use]
pub fn registered_sources_of(provider: &str) -> Vec<Source> {
    let Ok(data_root) = crate::storage::snapshot::data_root() else {
        return Vec::new();
    };
    let sources_dir = data_root.join("sources");
    let Ok(entries) = std::fs::read_dir(&sources_dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let Ok(bytes) = std::fs::read(entry.path().join("source.json")) else {
            continue;
        };
        if let Ok(source) = serde_json::from_slice::<Source>(&bytes) {
            if source.provider == provider && !source.id.is_empty() {
                out.push(source);
            }
        }
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

// ---------------------------------------------------------------------------
// Generic file-tree indexer.
// ---------------------------------------------------------------------------

/// Index one snapshot by scanning it and loading each project's sessions
/// through the provider's own seams. Works for every provider whose
/// `load_sessions(source, snapshot, stable_project)` resolves content the way
/// its scanner's project IDs describe.
pub fn generic_filetree_indexer(
    conn: &rusqlite::Connection,
    source: &Source,
    snapshot: &SnapshotInfo,
) -> Result<crate::storage::index::IndexSupport, String> {
    use crate::storage::index::IndexSupport;
    let Some(spec) = spec_for(&source.provider) else {
        return Ok(IndexSupport::Unsupported);
    };
    let mut projects = (spec.scan)(source, snapshot)?;
    if spec.rewrite_outputs {
        if let Some(original_root) = source.original_root.as_deref() {
            rewrite_to_stable(
                &snapshot.data_path,
                original_root,
                &mut projects,
                &mut [],
                &mut [],
            );
        }
    }
    for project in &mut projects {
        if project.provider.is_none() {
            project.provider = Some(source.provider.clone());
        }
    }
    if !projects.is_empty() {
        crate::cache::db::merge_and_save_projects(
            conn,
            &projects,
            None,
            Some(source.provider.as_str()),
        )?;
    }
    for project in &projects {
        let mut sessions = (spec.load_sessions)(source, snapshot, &project.path)?;
        if sessions.is_empty() {
            continue;
        }
        if spec.rewrite_outputs {
            if let Some(original_root) = source.original_root.as_deref() {
                rewrite_to_stable(
                    &snapshot.data_path,
                    original_root,
                    &mut [],
                    &mut sessions,
                    &mut [],
                );
            }
        }
        for session in &mut sessions {
            if session.provider.is_none() {
                session.provider = Some(source.provider.clone());
            }
        }
        crate::cache::db::merge_and_save_sessions(
            conn,
            &project.path,
            &source.provider,
            &sessions,
        )?;
    }
    Ok(IndexSupport::Indexed)
}

// ---------------------------------------------------------------------------
// Migration conformance suite.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod conformance_tests {
    use super::*;

    fn write_file(path: &Path, content: &[u8]) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, content).unwrap();
    }

    /// Clears provider env overrides for hermetic discovery (defaults under
    /// the sandbox home apply instead). Restores on drop.
    struct ClearEnvGuard {
        saved: Vec<(&'static str, Option<std::ffi::OsString>)>,
    }

    impl ClearEnvGuard {
        fn clear(keys: &[&'static str]) -> Self {
            let mut saved = Vec::new();
            for key in keys {
                saved.push((*key, std::env::var_os(key)));
                std::env::remove_var(key);
            }
            Self { saved }
        }
    }

    impl Drop for ClearEnvGuard {
        fn drop(&mut self) {
            for (key, value) in self.saved.drain(..) {
                match value {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
        }
    }

    const PROVIDER_ENVS: &[&str] = &[
        "KIMI_SHARE_DIR",
        "KIMI_HOME",
        "KIMI_CODE_HOME",
        "GROK_HOME",
        "VIBE_HOME",
        "CONTINUE_GLOBAL_DIR",
    ];

    async fn archived_sessions(provider: &str, project: &str) -> Vec<ClaudeSession> {
        let sources = read_sources(provider);
        assert!(
            !sources.is_empty(),
            "{provider}: expected preserved sources"
        );
        load_provider_sessions(provider, project, &sources)
            .await
            .unwrap()
    }

    async fn archived_messages(provider: &str, session: &str) -> Vec<ClaudeMessage> {
        let sources = read_sources(provider);
        load_provider_messages(provider, session, &sources)
            .await
            .unwrap()
    }

    fn message_text(messages: &[ClaudeMessage]) -> String {
        messages
            .iter()
            .filter_map(|m| m.content.as_ref())
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    }

    // -- pi / ompi (shared store format) -----------------------------------

    fn pi_session_file(
        root: &Path,
        project_dir: &str,
        file: &str,
        id: &str,
        cwd: &str,
        texts: &[&str],
    ) {
        let header = serde_json::json!({
            "type": "session", "version": 3, "id": id,
            "timestamp": "2026-09-01T00:00:00Z", "cwd": cwd,
        })
        .to_string();
        let mut lines = vec![header];
        for (i, text) in texts.iter().enumerate() {
            lines.push(
                serde_json::json!({
                    "type": "message", "id": format!("{id}-m{i}"),
                    "parentId": id, "timestamp": "2026-09-01T00:01:00Z",
                    "message": {
                        "role": "user", "timestamp": 1_757_000_000_000u64 + i as u64,
                        "content": [{"type": "text", "text": text}],
                    },
                })
                .to_string(),
            );
        }
        write_file(
            &root.join(project_dir).join(file),
            lines.join("\n").as_bytes(),
        );
    }

    async fn pi_like_conformance(provider: &str, dot_dir: &str) {
        let sandbox = crate::test_utils::SandboxHome::new();
        let _env = ClearEnvGuard::clear(PROVIDER_ENVS);
        let root = sandbox.path().join(dot_dir).join("agent/sessions");
        let marker = format!("conformance-marker-{provider}");
        pi_session_file(
            &root,
            "--w-projA--",
            "t1_s1.jsonl",
            "s1",
            "/w/projA",
            &[&format!("hello {marker} alpha")],
        );
        pi_session_file(
            &root,
            "--w-projB--",
            "t1_s2.jsonl",
            "s2",
            "/w/projB",
            &["hello beta"],
        );

        let projects = scan_provider(provider).await.unwrap();
        assert_eq!(
            projects.len(),
            2,
            "{provider}: two projects, got {projects:?}"
        );
        for project in &projects {
            assert!(
                !project.path.contains(".claude-history-viewer/data"),
                "snapshot interior leaked: {}",
                project.path
            );
        }
        let proj_a = projects
            .iter()
            .find(|p| p.actual_path == "/w/projA")
            .expect("projA")
            .path
            .clone();

        let sessions = archived_sessions(provider, &proj_a).await;
        assert_eq!(sessions.len(), 1);
        let s1_file = sessions[0].file_path.clone();
        let messages = archived_messages(provider, &s1_file).await;
        assert!(message_text(&messages).contains(&marker));

        // Search hits preserved content.
        let sources = read_sources(provider);
        let hits = search_provider(provider, &marker, 10, &sources)
            .await
            .unwrap();
        assert!(!hits.is_empty(), "{provider}: search must hit marker");

        // Disappearance: delete S1, extend S2, add S3 to projB.
        std::fs::remove_file(root.join("--w-projA--").join("t1_s1.jsonl")).unwrap();
        pi_session_file(
            &root,
            "--w-projB--",
            "t1_s2.jsonl",
            "s2",
            "/w/projB",
            &["hello beta", &format!("follow-up {marker}")],
        );
        pi_session_file(
            &root,
            "--w-projB--",
            "t2_s3.jsonl",
            "s3",
            "/w/projB",
            &["third"],
        );
        let projects_after = scan_provider(provider).await.unwrap();
        assert_eq!(projects_after.len(), 2);
        let sessions_b = archived_sessions(
            provider,
            &projects_after
                .iter()
                .find(|p| p.actual_path == "/w/projB")
                .expect("projB")
                .path,
        )
        .await;
        assert_eq!(sessions_b.len(), 2, "S2' + S3, got {sessions_b:?}");
        // S1 preserved despite upstream deletion.
        let sessions_a = archived_sessions(provider, &proj_a).await;
        assert_eq!(sessions_a.len(), 1, "deleted S1 must stay preserved");
        assert!(message_text(&archived_messages(provider, &s1_file).await).contains(&marker));

        // The live root can now vanish entirely; everything keeps working.
        let dot_root = sandbox.path().join(dot_dir);
        std::fs::remove_dir_all(&dot_root).unwrap();
        let projects_gone = scan_provider(provider).await.unwrap();
        assert_eq!(projects_gone.len(), 2);
        let sessions_gone = archived_sessions(provider, &proj_a).await;
        assert_eq!(sessions_gone.len(), 1);
        assert!(message_text(&archived_messages(provider, &s1_file).await).contains(&marker));
        let sources = read_sources(provider);
        assert!(!search_provider(provider, &marker, 10, &sources)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn pi_archive_conformance() {
        pi_like_conformance("pi", ".pi").await;
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn ompi_archive_conformance() {
        pi_like_conformance("ompi", ".omp").await;
    }

    // -- continue / pearai ---------------------------------------------------

    fn continue_session(workspace: &str, id: &str, texts: &[&str]) -> String {
        let history: Vec<_> = texts
            .iter()
            .map(|text| serde_json::json!({ "message": { "role": "user", "content": text } }))
            .collect();
        serde_json::json!({
            "sessionId": id,
            "title": format!("{id} title"),
            "workspaceDirectory": workspace,
            "history": history,
        })
        .to_string()
    }

    async fn continue_like_conformance(provider: &str, dot_dir: &str, scheme: &str) {
        let sandbox = crate::test_utils::SandboxHome::new();
        let _env = ClearEnvGuard::clear(PROVIDER_ENVS);
        let root = sandbox.path().join(dot_dir).join("sessions");
        let marker = format!("conformance-marker-{provider}");
        write_file(
            &root.join("sess-a.json"),
            continue_session("/w/pa", "sess-a", &[&format!("hello {marker} alpha")]).as_bytes(),
        );
        write_file(
            &root.join("sess-b.json"),
            continue_session("/w/pb", "sess-b", &["hello beta"]).as_bytes(),
        );

        let projects = scan_provider(provider).await.unwrap();
        assert_eq!(projects.len(), 2, "{provider}: {projects:?}");
        let proj_a = format!("{scheme}/w/pa");

        let sessions = archived_sessions(provider, &proj_a).await;
        assert_eq!(sessions.len(), 1);
        let s1_file = sessions[0].file_path.clone();
        assert!(message_text(&archived_messages(provider, &s1_file).await).contains(&marker));
        let sources = read_sources(provider);
        assert!(!search_provider(provider, &marker, 10, &sources)
            .await
            .unwrap()
            .is_empty());

        // Disappearance cycle.
        std::fs::remove_file(root.join("sess-a.json")).unwrap();
        write_file(
            &root.join("sess-b.json"),
            continue_session(
                "/w/pb",
                "sess-b",
                &["hello beta", &format!("more {marker}")],
            )
            .as_bytes(),
        );
        write_file(
            &root.join("sess-c.json"),
            continue_session("/w/pb", "sess-c", &["third"]).as_bytes(),
        );
        // Re-scan to snapshot the mutations.
        let projects_after = scan_provider(provider).await.unwrap();
        assert_eq!(projects_after.len(), 2);
        let sessions_b = archived_sessions(provider, &format!("{scheme}/w/pb")).await;
        assert_eq!(sessions_b.len(), 2, "sess-b' + sess-c, got {sessions_b:?}");
        assert_eq!(archived_sessions(provider, &proj_a).await.len(), 1);

        std::fs::remove_dir_all(sandbox.path().join(dot_dir)).unwrap();
        assert_eq!(scan_provider(provider).await.unwrap().len(), 2);
        assert_eq!(archived_sessions(provider, &proj_a).await.len(), 1);
        assert!(message_text(&archived_messages(provider, &s1_file).await).contains(&marker));
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn continue_archive_conformance() {
        continue_like_conformance("continue", ".continue", "continue://").await;
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn pearai_archive_conformance() {
        continue_like_conformance("pearai", ".pearai", "pearai://").await;
    }

    // -- gemini ------------------------------------------------------------------

    fn gemini_session(sid: &str, texts: &[&str]) -> String {
        let messages: Vec<_> = texts
            .iter()
            .enumerate()
            .map(|(i, text)| {
                serde_json::json!({
                    "type": "user", "id": format!("m{i}"),
                    "timestamp": "2026-09-01T00:00:00Z",
                    "role": "user", "content": text,
                })
            })
            .collect();
        serde_json::json!({
            "sessionId": sid,
            "startTime": "2026-09-01T00:00:00Z",
            "lastUpdated": "2026-09-01T00:01:00Z",
            "messages": messages,
        })
        .to_string()
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn gemini_archive_conformance() {
        let sandbox = crate::test_utils::SandboxHome::new();
        let _env = ClearEnvGuard::clear(PROVIDER_ENVS);
        let base = sandbox.path().join(".gemini");
        let marker = "conformance-marker-gemini";
        write_file(
            &base.join("tmp/projhash/chats/session-1.json"),
            gemini_session("gs1", &[&format!("hello {marker}")]).as_bytes(),
        );
        write_file(
            &base.join("tmp/projhash/chats/session-2.json"),
            gemini_session("gs2", &["second"]).as_bytes(),
        );

        let projects = scan_provider("gemini").await.unwrap();
        assert_eq!(projects.len(), 1, "gemini: {projects:?}");
        let project = projects[0].path.clone();
        assert!(project.starts_with("gemini://"));

        let sources = read_sources("gemini");
        let sessions = load_provider_sessions("gemini", &project, &sources)
            .await
            .unwrap();
        assert_eq!(sessions.len(), 2);
        let s1 = sessions
            .iter()
            .find(|s| s.actual_session_id == "gs1")
            .map(|s| s.file_path.clone())
            .unwrap_or_else(|| sessions[0].file_path.clone());
        assert!(message_text(
            &load_provider_messages("gemini", &s1, &sources)
                .await
                .unwrap()
        )
        .contains(marker));
        assert!(!search_provider("gemini", marker, 10, &sources)
            .await
            .unwrap()
            .is_empty());

        // Disappearance: delete session-1, extend session-2, add session-3.
        std::fs::remove_file(base.join("tmp/projhash/chats/session-1.json")).unwrap();
        write_file(
            &base.join("tmp/projhash/chats/session-2.json"),
            gemini_session("gs2", &["second", &format!("more {marker}")]).as_bytes(),
        );
        write_file(
            &base.join("tmp/projhash/chats/session-3.json"),
            gemini_session("gs3", &["third"]).as_bytes(),
        );
        let projects_after = scan_provider("gemini").await.unwrap();
        assert_eq!(projects_after.len(), 1);
        let sessions_after =
            load_provider_sessions("gemini", &projects_after[0].path, &read_sources("gemini"))
                .await
                .unwrap();
        assert_eq!(sessions_after.len(), 3, "s1 preserved + s2' + s3");

        std::fs::remove_dir_all(&base).unwrap();
        assert_eq!(scan_provider("gemini").await.unwrap().len(), 1);
        let sources = read_sources("gemini");
        assert_eq!(
            load_provider_sessions("gemini", &project, &sources)
                .await
                .unwrap()
                .len(),
            3
        );
    }

    // -- qwen --------------------------------------------------------------------

    fn qwen_session(cwd: &str, sid: &str, texts: &[&str]) -> String {
        texts
            .iter()
            .map(|text| {
                serde_json::json!({
                    "uuid": format!("{sid}-u"), "sessionId": sid,
                    "timestamp": "2026-09-01T00:00:00Z", "type": "user", "cwd": cwd,
                    "message": {"role": "user", "parts": [{"text": text}]},
                })
                .to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn qwen_archive_conformance() {
        let sandbox = crate::test_utils::SandboxHome::new();
        let _env = ClearEnvGuard::clear(PROVIDER_ENVS);
        let base = sandbox.path().join(".qwen");
        let marker = "conformance-marker-qwen";
        write_file(
            &base.join("projects/-w-qproj/chats/qs1.jsonl"),
            qwen_session("/w/qproj", "qs1", &[&format!("hello {marker}")]).as_bytes(),
        );
        write_file(
            &base.join("projects/-w-qproj/chats/qs2.jsonl"),
            qwen_session("/w/qproj", "qs2", &["second"]).as_bytes(),
        );

        let projects = scan_provider("qwen").await.unwrap();
        assert_eq!(projects.len(), 1, "qwen: {projects:?}");
        assert_eq!(projects[0].path, "qwen:///w/qproj");
        let project = projects[0].path.clone();

        let sources = read_sources("qwen");
        let sessions = load_provider_sessions("qwen", &project, &sources)
            .await
            .unwrap();
        assert_eq!(sessions.len(), 2);
        let s1 = sessions
            .iter()
            .find(|s| s.actual_session_id == "qs1")
            .map(|s| s.file_path.clone())
            .unwrap_or_else(|| sessions[0].file_path.clone());
        assert!(
            message_text(&load_provider_messages("qwen", &s1, &sources).await.unwrap())
                .contains(marker)
        );
        assert!(!search_provider("qwen", marker, 10, &sources)
            .await
            .unwrap()
            .is_empty());

        std::fs::remove_file(base.join("projects/-w-qproj/chats/qs1.jsonl")).unwrap();
        write_file(
            &base.join("projects/-w-qproj/chats/qs2.jsonl"),
            qwen_session("/w/qproj", "qs2", &["second", &format!("more {marker}")]).as_bytes(),
        );
        write_file(
            &base.join("projects/-w-qproj/chats/qs3.jsonl"),
            qwen_session("/w/qproj", "qs3", &["third"]).as_bytes(),
        );
        let projects_after = scan_provider("qwen").await.unwrap();
        assert_eq!(projects_after.len(), 1);
        let sessions_after =
            load_provider_sessions("qwen", &projects_after[0].path, &read_sources("qwen"))
                .await
                .unwrap();
        assert_eq!(sessions_after.len(), 3);

        std::fs::remove_dir_all(&base).unwrap();
        assert_eq!(scan_provider("qwen").await.unwrap().len(), 1);
        let sources = read_sources("qwen");
        assert_eq!(
            load_provider_sessions("qwen", &project, &sources)
                .await
                .unwrap()
                .len(),
            3
        );
    }

    // -- deepseek ------------------------------------------------------------------

    fn deepseek_session(id: &str, cwd: &str, texts: &[&str]) -> String {
        let mut lines = vec![
            serde_json::json!({"type": "session", "id": id, "cwd": cwd, "time": 1_757_000_000_000i64})
                .to_string(),
        ];
        for text in texts {
            lines.push(
                serde_json::json!({
                    "type": "user/message", "time": 1_757_000_001_000i64,
                    "data": {"content": [{"type": "text", "text": text}]},
                })
                .to_string(),
            );
        }
        lines.join("\n")
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn deepseek_archive_conformance() {
        let sandbox = crate::test_utils::SandboxHome::new();
        let _env = ClearEnvGuard::clear(PROVIDER_ENVS);
        let base = sandbox.path().join(".dsh");
        let marker = "conformance-marker-deepseek";
        write_file(
            &base.join("sessions/--w-dproj/ds1/session.jsonl"),
            deepseek_session("ds1", "/w/dproj", &[&format!("hello {marker}")]).as_bytes(),
        );
        write_file(
            &base.join("sessions/--w-dproj/ds2/session.jsonl"),
            deepseek_session("ds2", "/w/dproj", &["second"]).as_bytes(),
        );

        let projects = scan_provider("deepseek").await.unwrap();
        assert_eq!(projects.len(), 1, "deepseek: {projects:?}");
        let project = projects[0].path.clone();

        let sources = read_sources("deepseek");
        let sessions = load_provider_sessions("deepseek", &project, &sources)
            .await
            .unwrap();
        assert_eq!(sessions.len(), 2);
        let s1 = sessions
            .iter()
            .find(|s| s.actual_session_id == "ds1")
            .map(|s| s.file_path.clone())
            .unwrap_or_else(|| sessions[0].file_path.clone());
        assert!(!load_provider_messages("deepseek", &s1, &sources)
            .await
            .unwrap()
            .is_empty());

        std::fs::remove_dir_all(base.join("sessions/--w-dproj/ds1")).unwrap();
        write_file(
            &base.join("sessions/--w-dproj/ds2/session.jsonl"),
            deepseek_session("ds2", "/w/dproj", &["second", "more"]).as_bytes(),
        );
        write_file(
            &base.join("sessions/--w-dproj/ds3/session.jsonl"),
            deepseek_session("ds3", "/w/dproj", &["third"]).as_bytes(),
        );
        let projects_after = scan_provider("deepseek").await.unwrap();
        assert_eq!(projects_after.len(), 1);
        let sessions_after = load_provider_sessions(
            "deepseek",
            &projects_after[0].path,
            &read_sources("deepseek"),
        )
        .await
        .unwrap();
        assert_eq!(sessions_after.len(), 3, "ds1 preserved + ds2' + ds3");

        std::fs::remove_dir_all(&base).unwrap();
        assert_eq!(scan_provider("deepseek").await.unwrap().len(), 1);
        let sources = read_sources("deepseek");
        assert_eq!(
            load_provider_sessions("deepseek", &project, &sources)
                .await
                .unwrap()
                .len(),
            3
        );
        assert!(!load_provider_messages("deepseek", &s1, &sources)
            .await
            .unwrap()
            .is_empty());
    }

    // -- openhands ------------------------------------------------------------------

    fn openhands_session(texts: &[&str]) -> String {
        texts
            .iter()
            .enumerate()
            .map(|(i, text)| {
                serde_json::json!({
                    "source": "user", "action": "message", "id": i + 1,
                    "timestamp": "2026-09-01T00:00:00Z",
                    "args": {"content": text},
                })
                .to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn openhands_archive_conformance() {
        let sandbox = crate::test_utils::SandboxHome::new();
        let _env = ClearEnvGuard::clear(PROVIDER_ENVS);
        let sessions = sandbox.path().join(".openhands/sessions");
        let marker = "conformance-marker-openhands";
        write_file(
            &sessions.join("abc123/events/1.json"),
            openhands_session(&[&format!("hello {marker}")]).as_bytes(),
        );
        write_file(
            &sessions.join("def456/events/1.json"),
            openhands_session(&["second"]).as_bytes(),
        );

        let projects = scan_provider("openhands").await.unwrap();
        assert_eq!(projects.len(), 1, "openhands: {projects:?}");
        assert_eq!(projects[0].path, "openhands://__workspace__");
        let project = projects[0].path.clone();

        let sources = read_sources("openhands");
        let loaded = load_provider_sessions("openhands", &project, &sources)
            .await
            .unwrap();
        assert_eq!(loaded.len(), 2);
        assert!(message_text(
            &load_provider_messages("openhands", "openhands://abc123", &sources)
                .await
                .unwrap()
        )
        .contains(marker));
        assert!(!search_provider("openhands", marker, 10, &sources)
            .await
            .unwrap()
            .is_empty());

        std::fs::remove_dir_all(sessions.join("abc123")).unwrap();
        write_file(
            &sessions.join("def456/events/2.json"),
            openhands_session(&["more"]).as_bytes(),
        );
        write_file(
            &sessions.join("ghi789/events/1.json"),
            openhands_session(&["third"]).as_bytes(),
        );
        let projects_after = scan_provider("openhands").await.unwrap();
        assert_eq!(projects_after.len(), 1);
        let after = load_provider_sessions(
            "openhands",
            &projects_after[0].path,
            &read_sources("openhands"),
        )
        .await
        .unwrap();
        assert_eq!(after.len(), 3, "abc123 preserved + def456 + ghi789");

        std::fs::remove_dir_all(sandbox.path().join(".openhands")).unwrap();
        assert_eq!(scan_provider("openhands").await.unwrap().len(), 1);
        let sources = read_sources("openhands");
        assert_eq!(
            load_provider_sessions("openhands", &project, &sources)
                .await
                .unwrap()
                .len(),
            3
        );
        assert!(message_text(
            &load_provider_messages("openhands", "openhands://abc123", &sources)
                .await
                .unwrap()
        )
        .contains(marker));
    }

    // -- aider ------------------------------------------------------------------

    fn aider_history(texts: &[&str]) -> String {
        texts
            .iter()
            .enumerate()
            .map(|(i, text)| {
                format!(
                    "# aider chat started at 2026-09-0{} 00:00:0{}\n\n#### prompt {}\n\n{}\n",
                    i + 1,
                    i,
                    i,
                    text
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn aider_archive_conformance() {
        let sandbox = crate::test_utils::SandboxHome::new();
        let _env = ClearEnvGuard::clear(PROVIDER_ENVS);
        let home = sandbox.path().to_path_buf();
        let marker = "conformance-marker-aider";
        // Aider discovers ~/client (depth 2) and ~ itself (depth 0).
        write_file(
            &home.join("client/projA/.aider.chat.history.md"),
            aider_history(&[&format!("hello {marker}")]).as_bytes(),
        );
        write_file(
            &home.join("client/projB/.aider.chat.history.md"),
            aider_history(&["second"]).as_bytes(),
        );

        let projects = scan_provider("aider").await.unwrap();
        assert_eq!(projects.len(), 2, "aider: {projects:?}");
        let proj_a = projects
            .iter()
            .find(|p| p.name == "projA")
            .expect("projA")
            .path
            .clone();

        let sources = read_sources("aider");
        let sessions = load_provider_sessions("aider", &proj_a, &sources)
            .await
            .unwrap();
        assert_eq!(sessions.len(), 1);
        let s1 = sessions[0].session_id.clone();
        assert!(message_text(
            &load_provider_messages("aider", &s1, &sources)
                .await
                .unwrap()
        )
        .contains(marker));
        assert!(!search_provider("aider", marker, 10, &sources)
            .await
            .unwrap()
            .is_empty());

        std::fs::remove_file(home.join("client/projA/.aider.chat.history.md")).unwrap();
        write_file(
            &home.join("client/projB/.aider.chat.history.md"),
            aider_history(&["second", &format!("more {marker}")]).as_bytes(),
        );
        write_file(
            &home.join("client/projC/.aider.chat.history.md"),
            aider_history(&["third"]).as_bytes(),
        );
        let projects_after = scan_provider("aider").await.unwrap();
        assert_eq!(projects_after.len(), 3);
        let sessions_b = load_provider_sessions(
            "aider",
            &projects_after
                .iter()
                .find(|p| p.name == "projB")
                .expect("projB")
                .path,
            &read_sources("aider"),
        )
        .await
        .unwrap();
        assert_eq!(sessions_b.len(), 2, "extended history splits sessions");
        assert_eq!(
            load_provider_sessions("aider", &proj_a, &read_sources("aider"))
                .await
                .unwrap()
                .len(),
            1
        );

        // Removing ~/client entirely still leaves projA + projB + projC browsable.
        std::fs::remove_dir_all(home.join("client")).unwrap();
        assert_eq!(scan_provider("aider").await.unwrap().len(), 3);
        let sources = read_sources("aider");
        assert!(message_text(
            &load_provider_messages("aider", &s1, &sources)
                .await
                .unwrap()
        )
        .contains(marker));
    }

    // -- codebuddy ------------------------------------------------------------------

    fn codebuddy_session(sid: &str, texts: &[&str]) -> String {
        texts
            .iter()
            .map(|text| {
                serde_json::json!({
                    "type": "message", "sessionId": sid, "role": "user",
                    "timestamp": "2026-09-01T00:00:00Z",
                    "content": [{"type": "text", "text": text}],
                })
                .to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn codebuddy_archive_conformance() {
        let sandbox = crate::test_utils::SandboxHome::new();
        let _env = ClearEnvGuard::clear(PROVIDER_ENVS);
        let base = sandbox.path().join(".codebuddy/projects");
        let marker = "conformance-marker-codebuddy";
        write_file(
            &base.join("projA/cbs1.jsonl"),
            codebuddy_session("cbs1", &[&format!("hello {marker}")]).as_bytes(),
        );
        write_file(
            &base.join("projB/cbs2.jsonl"),
            codebuddy_session("cbs2", &["second"]).as_bytes(),
        );

        let projects = scan_provider("codebuddy").await.unwrap();
        assert_eq!(projects.len(), 2, "codebuddy: {projects:?}");
        let proj_a = projects
            .iter()
            .find(|p| p.actual_path.contains("projA") || p.name == "projA")
            .map(|p| p.path.clone())
            .unwrap_or_else(|| projects[0].path.clone());

        let sources = read_sources("codebuddy");
        let sessions = load_provider_sessions("codebuddy", &proj_a, &sources)
            .await
            .unwrap();
        assert!(!sessions.is_empty());
        let s1 = sessions[0].file_path.clone();
        assert!(message_text(
            &load_provider_messages("codebuddy", &s1, &sources)
                .await
                .unwrap()
        )
        .contains(marker));
        assert!(!search_provider("codebuddy", marker, 10, &sources)
            .await
            .unwrap()
            .is_empty());

        std::fs::remove_file(base.join("projA/cbs1.jsonl")).unwrap();
        write_file(
            &base.join("projB/cbs2.jsonl"),
            codebuddy_session("cbs2", &["second", &format!("more {marker}")]).as_bytes(),
        );
        write_file(
            &base.join("projB/cbs3.jsonl"),
            codebuddy_session("cbs3", &["third"]).as_bytes(),
        );
        let _ = scan_provider("codebuddy").await.unwrap();

        std::fs::remove_dir_all(sandbox.path().join(".codebuddy")).unwrap();
        let projects_gone = scan_provider("codebuddy").await.unwrap();
        assert_eq!(projects_gone.len(), 2);
        let sources = read_sources("codebuddy");
        assert!(message_text(
            &load_provider_messages("codebuddy", &s1, &sources)
                .await
                .unwrap()
        )
        .contains(marker));
    }

    // -- cursor-agent ------------------------------------------------------------------

    fn cursor_agent_transcript(texts: &[&str]) -> String {
        texts
            .iter()
            .map(|text| {
                serde_json::json!({"role": "user", "message": {"content": [{"type": "text", "text": text}]}})
                    .to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn cursor_agent_archive_conformance() {
        let sandbox = crate::test_utils::SandboxHome::new();
        let _env = ClearEnvGuard::clear(PROVIDER_ENVS);
        let base = sandbox.path().join(".cursor/projects");
        let marker = "conformance-marker-cursor-agent";
        let uuid_a = "11111111-1111-4111-8111-111111111111";
        let uuid_b = "22222222-2222-4222-8222-222222222222";
        write_file(
            &base.join(format!("projA/agent-transcripts/{uuid_a}/{uuid_a}.jsonl")),
            cursor_agent_transcript(&[&format!("hello {marker}")]).as_bytes(),
        );
        write_file(
            &base.join(format!("projB/agent-transcripts/{uuid_b}/{uuid_b}.jsonl")),
            cursor_agent_transcript(&["second"]).as_bytes(),
        );

        let projects = scan_provider("cursor-agent").await.unwrap();
        assert_eq!(projects.len(), 2, "cursor-agent: {projects:?}");
        let proj_a = projects
            .iter()
            .find(|p| p.name == "projA")
            .expect("projA")
            .path
            .clone();

        let sources = read_sources("cursor-agent");
        let sessions = load_provider_sessions("cursor-agent", &proj_a, &sources)
            .await
            .unwrap();
        assert_eq!(sessions.len(), 1);
        let s1 = sessions[0].file_path.clone();
        assert!(message_text(
            &load_provider_messages("cursor-agent", &s1, &sources)
                .await
                .unwrap()
        )
        .contains(marker));
        assert!(!search_provider("cursor-agent", marker, 10, &sources)
            .await
            .unwrap()
            .is_empty());

        std::fs::remove_dir_all(base.join(format!("projA/agent-transcripts/{uuid_a}"))).unwrap();
        write_file(
            &base.join(format!("projB/agent-transcripts/{uuid_b}/{uuid_b}.jsonl")),
            cursor_agent_transcript(&["second", &format!("more {marker}")]).as_bytes(),
        );
        let uuid_c = "33333333-3333-4333-8333-333333333333";
        write_file(
            &base.join(format!("projB/agent-transcripts/{uuid_c}/{uuid_c}.jsonl")),
            cursor_agent_transcript(&["third"]).as_bytes(),
        );
        let projects_after = scan_provider("cursor-agent").await.unwrap();
        assert_eq!(projects_after.len(), 2);
        let sessions_b = load_provider_sessions(
            "cursor-agent",
            &projects_after
                .iter()
                .find(|p| p.name == "projB")
                .expect("projB")
                .path,
            &read_sources("cursor-agent"),
        )
        .await
        .unwrap();
        assert_eq!(sessions_b.len(), 2);

        std::fs::remove_dir_all(sandbox.path().join(".cursor")).unwrap();
        assert_eq!(scan_provider("cursor-agent").await.unwrap().len(), 2);
        let sources = read_sources("cursor-agent");
        assert!(message_text(
            &load_provider_messages("cursor-agent", &s1, &sources)
                .await
                .unwrap()
        )
        .contains(marker));
    }

    // -- grok ---------------------------------------------------------------

    fn grok_session(root: &Path, encoded: &str, sid: &str, texts: &[&str]) {
        let dir = root.join("sessions").join(encoded).join(sid);
        let summary = serde_json::json!({
            "info": {"id": sid, "cwd": "/w/grokproj"},
            "generated_title": format!("{sid} title"),
            "updated_at": "2026-09-01T00:00:00Z",
        });
        write_file(
            &dir.join("summary.json"),
            serde_json::to_string_pretty(&summary).unwrap().as_bytes(),
        );
        let lines: Vec<String> = texts
            .iter()
            .map(|text| {
                serde_json::json!({"type": "user", "content": [{"type": "text", "text": text}]})
                    .to_string()
            })
            .collect();
        write_file(&dir.join("chat_history.jsonl"), lines.join("\n").as_bytes());
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn grok_archive_conformance() {
        let sandbox = crate::test_utils::SandboxHome::new();
        let _env = ClearEnvGuard::clear(PROVIDER_ENVS);
        let root = sandbox.path().join(".grok");
        let marker = "conformance-marker-grok";
        grok_session(
            &root,
            "%2Fw%2Fgrokproj",
            "sid-1",
            &[&format!("hello {marker}")],
        );
        grok_session(&root, "%2Fw%2Fgrokproj", "sid-2", &["second"]);

        let projects = scan_provider("grok").await.unwrap();
        assert_eq!(projects.len(), 1, "grok: {projects:?}");
        let project = projects[0].path.clone();
        assert!(!project.contains(".claude-history-viewer/data"));

        let sources = read_sources("grok");
        let sessions = load_provider_sessions("grok", &project, &sources)
            .await
            .unwrap();
        assert_eq!(sessions.len(), 2);
        let s1 = sessions
            .iter()
            .find(|s| s.actual_session_id == "sid-1")
            .expect("sid-1")
            .file_path
            .clone();
        assert!(
            message_text(&load_provider_messages("grok", &s1, &sources).await.unwrap())
                .contains(marker)
        );
        assert!(!search_provider("grok", marker, 10, &sources)
            .await
            .unwrap()
            .is_empty());

        // Disappearance: delete sid-1, extend sid-2, add sid-3.
        std::fs::remove_dir_all(root.join("sessions/%2Fw%2Fgrokproj/sid-1")).unwrap();
        grok_session(
            &root,
            "%2Fw%2Fgrokproj",
            "sid-2",
            &["second", &format!("more {marker}")],
        );
        grok_session(&root, "%2Fw%2Fgrokproj", "sid-3", &["third"]);
        let projects_after = scan_provider("grok").await.unwrap();
        assert_eq!(projects_after.len(), 1);
        let sessions_after =
            load_provider_sessions("grok", &projects_after[0].path, &read_sources("grok"))
                .await
                .unwrap();
        assert_eq!(sessions_after.len(), 3, "sid-1 preserved + sid-2' + sid-3");

        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!(scan_provider("grok").await.unwrap().len(), 1);
        let sources = read_sources("grok");
        let sessions_gone = load_provider_sessions("grok", &project, &sources)
            .await
            .unwrap();
        assert_eq!(sessions_gone.len(), 3);
        assert!(
            message_text(&load_provider_messages("grok", &s1, &sources).await.unwrap())
                .contains(marker)
        );
    }

    // -- kimi (legacy + code) -------------------------------------------------

    fn kimi_legacy_session(base: &Path, project: &str, session: &str, texts: &[&str]) {
        let dir = base.join("sessions").join(project).join(session);
        let context: String = texts
            .iter()
            .map(|text| format!(r#"{{"role":"user","content":"{text}"}}"#))
            .collect::<Vec<_>>()
            .join("\n");
        write_file(&dir.join("context.jsonl"), context.as_bytes());
        write_file(&dir.join("wire.jsonl"), br#"{"timestamp":1757000000.0}"#);
    }

    fn kimi_code_session(root: &Path, workspace: &str, session: &str, text: &str) {
        let wire_dir = root
            .join("sessions")
            .join(workspace)
            .join(session)
            .join("agents")
            .join("main");
        write_file(
            &wire_dir.join("wire.jsonl"),
            format!(
                r#"{{"type":"context.append_message","message":{{"role":"user","content":[{{"type":"text","text":"{text}"}}],"toolCalls":[],"origin":{{"kind":"user"}},"id":"{session}_u1"}},"time":1786959458000}}"#
            )
            .as_bytes(),
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn kimi_archive_conformance() {
        let sandbox = crate::test_utils::SandboxHome::new();
        let _env = ClearEnvGuard::clear(PROVIDER_ENVS);
        let legacy = sandbox.path().join(".kimi");
        let code = sandbox.path().join(".kimi-code");
        let marker = "conformance-marker-kimi";
        kimi_legacy_session(&legacy, "proj", "sess-1", &[&format!("hello {marker}")]);
        kimi_code_session(
            &code,
            "wd_abc",
            "session_1",
            &format!("code hello {marker}"),
        );

        let projects = scan_provider("kimi").await.unwrap();
        assert_eq!(projects.len(), 2, "kimi legacy + code: {projects:?}");
        let legacy_project = projects
            .iter()
            .find(|p| p.path.starts_with("kimi://"))
            .expect("legacy project")
            .path
            .clone();
        let code_project = projects
            .iter()
            .find(|p| p.path.starts_with("kimi-code://"))
            .expect("code project")
            .path
            .clone();
        for project in &projects {
            assert!(!project.path.contains(".claude-history-viewer/data"));
        }

        let sources = read_sources("kimi");
        let legacy_sessions = load_provider_sessions("kimi", &legacy_project, &sources)
            .await
            .unwrap();
        assert_eq!(legacy_sessions.len(), 1);
        let s1 = legacy_sessions[0].file_path.clone();
        assert!(
            message_text(&load_provider_messages("kimi", &s1, &sources).await.unwrap())
                .contains(marker)
        );
        let code_sessions = load_provider_sessions("kimi", &code_project, &sources)
            .await
            .unwrap();
        assert_eq!(code_sessions.len(), 1);
        assert!(!search_provider("kimi", marker, 10, &sources)
            .await
            .unwrap()
            .is_empty());

        // Disappearance on the legacy side; code side untouched.
        std::fs::remove_dir_all(legacy.join("sessions/proj/sess-1")).unwrap();
        kimi_legacy_session(&legacy, "proj", "sess-2", &["second"]);
        let projects_after = scan_provider("kimi").await.unwrap();
        assert_eq!(projects_after.len(), 2);
        let sources = read_sources("kimi");
        let legacy_after = load_provider_sessions("kimi", &legacy_project, &sources)
            .await
            .unwrap();
        assert_eq!(legacy_after.len(), 2, "sess-1 preserved + sess-2");
        let code_after = load_provider_sessions("kimi", &code_project, &sources)
            .await
            .unwrap();
        assert_eq!(code_after.len(), 1);

        // Both live roots can vanish; everything keeps working.
        std::fs::remove_dir_all(&legacy).unwrap();
        std::fs::remove_dir_all(&code).unwrap();
        assert_eq!(scan_provider("kimi").await.unwrap().len(), 2);
        let sources = read_sources("kimi");
        assert_eq!(
            load_provider_sessions("kimi", &legacy_project, &sources)
                .await
                .unwrap()
                .len(),
            2
        );
        assert!(
            message_text(&load_provider_messages("kimi", &s1, &sources).await.unwrap())
                .contains(marker)
        );
    }

    // -- vibe ------------------------------------------------------------------

    fn vibe_session(root: &Path, sess_dir: &str, cwd: &str, session_id: &str, texts: &[&str]) {
        let dir = root.join("logs/session").join(sess_dir);
        write_file(
            &dir.join("meta.json"),
            serde_json::to_string_pretty(&serde_json::json!({
                "session_id": session_id,
                "environment": {"working_directory": cwd},
                "title": format!("{session_id} title"),
            }))
            .unwrap()
            .as_bytes(),
        );
        let lines: Vec<String> = texts
            .iter()
            .enumerate()
            .map(|(i, text)| {
                serde_json::json!({
                    "role": "user", "content": text, "message_id": format!("{session_id}-m{i}"),
                })
                .to_string()
            })
            .collect();
        write_file(&dir.join("messages.jsonl"), lines.join("\n").as_bytes());
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn vibe_archive_conformance() {
        let sandbox = crate::test_utils::SandboxHome::new();
        let _env = ClearEnvGuard::clear(PROVIDER_ENVS);
        let root = sandbox.path().join(".vibe");
        let marker = "conformance-marker-vibe";
        vibe_session(
            &root,
            "sess_a",
            "/w/vibeproj",
            "vid-1",
            &[&format!("hello {marker}")],
        );
        vibe_session(&root, "sess_b", "/w/vibeproj", "vid-2", &["second"]);

        let projects = scan_provider("vibe").await.unwrap();
        assert_eq!(projects.len(), 1, "vibe: {projects:?}");
        let project = projects[0].path.clone();
        assert_eq!(project, "vibe:///w/vibeproj");

        let sources = read_sources("vibe");
        let sessions = load_provider_sessions("vibe", &project, &sources)
            .await
            .unwrap();
        assert_eq!(sessions.len(), 2);
        let s1 = sessions
            .iter()
            .find(|s| s.actual_session_id == "vid-1")
            .expect("vid-1")
            .file_path
            .clone();
        // Session file_path is the snapshot-space dir rewritten to stable.
        assert!(!s1.contains(".claude-history-viewer/data"), "rewrite: {s1}");
        assert!(
            message_text(&load_provider_messages("vibe", &s1, &sources).await.unwrap())
                .contains(marker)
        );
        assert!(!search_provider("vibe", marker, 10, &sources)
            .await
            .unwrap()
            .is_empty());

        // Disappearance: delete vid-1, extend vid-2, add vid-3.
        std::fs::remove_dir_all(root.join("logs/session/sess_a")).unwrap();
        vibe_session(
            &root,
            "sess_b",
            "/w/vibeproj",
            "vid-2",
            &["second", &format!("more {marker}")],
        );
        vibe_session(&root, "sess_c", "/w/vibeproj", "vid-3", &["third"]);
        let projects_after = scan_provider("vibe").await.unwrap();
        assert_eq!(projects_after.len(), 1);
        let sessions_after =
            load_provider_sessions("vibe", &projects_after[0].path, &read_sources("vibe"))
                .await
                .unwrap();
        assert_eq!(sessions_after.len(), 3, "vid-1 preserved + vid-2' + vid-3");

        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!(scan_provider("vibe").await.unwrap().len(), 1);
        let sources = read_sources("vibe");
        assert_eq!(
            load_provider_sessions("vibe", &project, &sources)
                .await
                .unwrap()
                .len(),
            3
        );
        assert!(
            message_text(&load_provider_messages("vibe", &s1, &sources).await.unwrap())
                .contains(marker)
        );
    }
}
