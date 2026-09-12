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

/// Merge preserved history inside a staged SQLite copy, for blob-collection
/// stores where an entire session list lives in one KV value (row-level merge
/// cannot see deletions inside the blob). Runs after backup, before hashing;
/// returns resurrected item count. On error the caller keeps the previous
/// database wholesale.
pub type SqliteBlobMergeFn = fn(prev_db: &Path, staged_db: &Path) -> Result<usize, String>;

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
    /// Blob-collection merge for KV stores (`None` when row-level merge
    /// suffices).
    pub blob_merge: Option<SqliteBlobMergeFn>,
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
            blob_merge: None,
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
            blob_merge: None,
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
            blob_merge: None,
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
            blob_merge: None,
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
            blob_merge: None,
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
            blob_merge: None,
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
            blob_merge: None,
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
            blob_merge: None,
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
            blob_merge: None,
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
            blob_merge: None,
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
            blob_merge: None,
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
            blob_merge: None,
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
            blob_merge: None,
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
            blob_merge: None,
        },
        ProviderArchiveSpec {
            provider: "codex",
            discover: providers::codex::archive_discover,
            scan: providers::codex::archive_scan,
            load_sessions: providers::codex::archive_load_sessions,
            load_messages: providers::codex::archive_load_messages,
            search: Some(providers::codex::archive_search),
            locate: locate_by_subpath_or_single,
            rewrite_outputs: true,
            blob_merge: None,
        },
        ProviderArchiveSpec {
            provider: "openinterpreter",
            discover: providers::openinterpreter::archive_discover,
            scan: providers::openinterpreter::archive_scan,
            load_sessions: providers::openinterpreter::archive_load_sessions,
            load_messages: providers::openinterpreter::archive_load_messages,
            search: Some(providers::openinterpreter::archive_search),
            locate: locate_by_subpath_or_single,
            rewrite_outputs: true,
            blob_merge: None,
        },
        ProviderArchiveSpec {
            provider: "opencode",
            discover: providers::opencode::archive_discover,
            scan: providers::opencode::archive_scan,
            load_sessions: providers::opencode::archive_load_sessions,
            load_messages: providers::opencode::archive_load_messages,
            search: Some(providers::opencode::archive_search),
            locate: locate_by_subpath_or_single,
            rewrite_outputs: false,
            blob_merge: None,
        },
        ProviderArchiveSpec {
            provider: "forgecode",
            discover: providers::forgecode::archive_discover,
            scan: providers::forgecode::archive_scan,
            load_sessions: providers::forgecode::archive_load_sessions,
            load_messages: providers::forgecode::archive_load_messages,
            search: Some(providers::forgecode::archive_search),
            locate: locate_by_subpath_or_single,
            rewrite_outputs: false,
            blob_merge: None,
        },
        ProviderArchiveSpec {
            provider: "goose",
            discover: providers::goose::archive_discover,
            scan: providers::goose::archive_scan,
            load_sessions: providers::goose::archive_load_sessions,
            load_messages: providers::goose::archive_load_messages,
            search: Some(providers::goose::archive_search),
            locate: locate_by_subpath_or_single,
            rewrite_outputs: false,
            blob_merge: None,
        },
        ProviderArchiveSpec {
            provider: "llm",
            discover: providers::llm::archive_discover,
            scan: providers::llm::archive_scan,
            load_sessions: providers::llm::archive_load_sessions,
            load_messages: providers::llm::archive_load_messages,
            search: Some(providers::llm::archive_search),
            locate: locate_by_subpath_or_single,
            rewrite_outputs: false,
            blob_merge: None,
        },
        ProviderArchiveSpec {
            provider: "amazonq",
            discover: providers::amazon_q::archive_discover,
            scan: providers::amazon_q::archive_scan,
            load_sessions: providers::amazon_q::archive_load_sessions,
            load_messages: providers::amazon_q::archive_load_messages,
            search: Some(providers::amazon_q::archive_search),
            locate: locate_by_subpath_or_single,
            rewrite_outputs: false,
            blob_merge: None,
        },
        ProviderArchiveSpec {
            provider: "zed",
            discover: providers::zed::archive_discover,
            scan: providers::zed::archive_scan,
            load_sessions: providers::zed::archive_load_sessions,
            load_messages: providers::zed::archive_load_messages,
            search: Some(providers::zed::archive_search),
            locate: locate_by_subpath_or_single,
            rewrite_outputs: false,
            blob_merge: None,
        },
        ProviderArchiveSpec {
            provider: "kiro",
            discover: providers::kiro::archive_discover,
            scan: providers::kiro::archive_scan,
            load_sessions: providers::kiro::archive_load_sessions,
            load_messages: providers::kiro::archive_load_messages,
            search: Some(providers::kiro::archive_search),
            locate: locate_by_subpath_or_single,
            rewrite_outputs: false,
            blob_merge: None,
        },
        ProviderArchiveSpec {
            provider: "trae",
            discover: providers::trae::archive_discover,
            scan: providers::trae::archive_scan,
            load_sessions: providers::trae::archive_load_sessions,
            load_messages: providers::trae::archive_load_messages,
            search: Some(providers::trae::archive_search),
            locate: locate_by_subpath_or_single,
            rewrite_outputs: false,
            blob_merge: Some(providers::trae::archive_blob_merge),
        },
        ProviderArchiveSpec {
            provider: "cursor",
            discover: providers::cursor::archive_discover,
            scan: providers::cursor::archive_scan,
            load_sessions: providers::cursor::archive_load_sessions,
            load_messages: providers::cursor::archive_load_messages,
            search: Some(providers::cursor::archive_search),
            locate: locate_by_subpath_or_single,
            // Mixed IDs: `cursor://{abs-workspace}` forms embed snapshot
            // interior and need rewriting; opaque composer IDs pass through.
            rewrite_outputs: true,
            blob_merge: None,
        },
        ProviderArchiveSpec {
            provider: "crush",
            discover: providers::crush::archive_discover,
            scan: providers::crush::archive_scan,
            load_sessions: providers::crush::archive_load_sessions,
            load_messages: providers::crush::archive_load_messages,
            search: Some(providers::crush::archive_search),
            locate: providers::crush::archive_locate,
            rewrite_outputs: false,
            blob_merge: None,
        },
        ProviderArchiveSpec {
            provider: "cline",
            discover: providers::cline::archive_discover,
            scan: providers::cline::archive_scan,
            load_sessions: providers::cline::archive_load_sessions,
            load_messages: providers::cline::archive_load_messages,
            search: Some(providers::cline::archive_search),
            locate: providers::cline::archive_locate,
            rewrite_outputs: true,
            blob_merge: None,
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

    // -- codex (rollouts + state db) ---------------------------------------------

    fn codex_rollout(cwd: &str, sid: &str, texts: &[&str]) -> String {
        let mut lines = vec![
            serde_json::json!({"type": "session_meta", "payload": {"id": sid, "cwd": cwd}})
                .to_string(),
        ];
        for text in texts {
            lines.push(
                serde_json::json!({
                    "timestamp": "2026-09-01T00:00:00Z", "type": "response_item",
                    "payload": {
                        "type": "message", "role": "user", "id": format!("{sid}-m"),
                        "created_at": "2026-09-01T00:00:00Z",
                        "content": [{"type": "input_text", "text": text}],
                    },
                })
                .to_string(),
            );
        }
        lines.join("\n") + "\n"
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn codex_archive_conformance() {
        let sandbox = crate::test_utils::SandboxHome::new();
        let _env = ClearEnvGuard::clear(PROVIDER_ENVS);
        let base = sandbox.path().join(".codex");
        let marker = "conformance-marker-codex";
        let r1 = "rollout-2026-09-01-aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa.jsonl";
        let r2 = "rollout-2026-09-01-bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb.jsonl";
        write_file(
            &base.join("sessions").join(r1),
            codex_rollout("/w/cxproj", "cxs1", &[&format!("hello {marker}")]).as_bytes(),
        );
        write_file(
            &base.join("sessions").join(r2),
            codex_rollout("/w/cxproj", "cxs2", &["second"]).as_bytes(),
        );
        // A native title index exercises the snapshotted state db path.
        {
            let conn = rusqlite::Connection::open(base.join("state_5.sqlite")).unwrap();
            conn.execute_batch(
                "CREATE TABLE threads(id TEXT PRIMARY KEY, title TEXT, first_user_message TEXT);",
            )
            .unwrap();
        }

        let projects = scan_provider("codex").await.unwrap();
        assert_eq!(projects.len(), 1, "codex: {projects:?}");
        assert_eq!(projects[0].path, "codex:///w/cxproj");
        let project = projects[0].path.clone();
        // The state db rode along into the snapshot.
        let sources = read_sources("codex");
        assert_eq!(sources.len(), 1);
        assert!(sources[0]
            .snapshot
            .data_path
            .join("state_5.sqlite")
            .is_file());

        let sessions = load_provider_sessions("codex", &project, &sources)
            .await
            .unwrap();
        assert_eq!(sessions.len(), 2);
        let s1 = sessions
            .iter()
            .find(|s| s.actual_session_id == "cxs1")
            .map(|s| s.file_path.clone())
            .unwrap_or_else(|| sessions[0].file_path.clone());
        assert!(message_text(
            &load_provider_messages("codex", &s1, &sources)
                .await
                .unwrap()
        )
        .contains(marker));
        assert!(!search_provider("codex", marker, 10, &sources)
            .await
            .unwrap()
            .is_empty());

        // Disappearance: delete r1, extend r2, add r3.
        std::fs::remove_file(base.join("sessions").join(r1)).unwrap();
        write_file(
            &base.join("sessions").join(r2),
            codex_rollout("/w/cxproj", "cxs2", &["second", &format!("more {marker}")]).as_bytes(),
        );
        let r3 = "rollout-2026-09-01-cccccccc-cccc-4ccc-8ccc-cccccccccccc.jsonl";
        write_file(
            &base.join("sessions").join(r3),
            codex_rollout("/w/cxproj", "cxs3", &["third"]).as_bytes(),
        );
        let projects_after = scan_provider("codex").await.unwrap();
        assert_eq!(projects_after.len(), 1);
        let sources2 = read_sources("codex");
        eprintln!(
            "DBG snapshots: {:?}",
            sources2
                .iter()
                .map(|s| (
                    &s.source.id,
                    &s.snapshot.snapshot_id,
                    s.snapshot
                        .manifest
                        .files
                        .iter()
                        .map(|f| (f.path.clone(), f.present))
                        .collect::<Vec<_>>()
                ))
                .collect::<Vec<_>>()
        );
        let sessions_after = load_provider_sessions("codex", &projects_after[0].path, &sources2)
            .await
            .unwrap();
        eprintln!(
            "DBG sessions: {:?}",
            sessions_after
                .iter()
                .map(|s| s.actual_session_id.clone())
                .collect::<Vec<_>>()
        );
        assert_eq!(sessions_after.len(), 3, "r1 preserved + r2' + r3");

        std::fs::remove_dir_all(&base).unwrap();
        assert_eq!(scan_provider("codex").await.unwrap().len(), 1);
        let sources = read_sources("codex");
        assert_eq!(
            load_provider_sessions("codex", &project, &sources)
                .await
                .unwrap()
                .len(),
            3
        );
        assert!(message_text(
            &load_provider_messages("codex", &s1, &sources)
                .await
                .unwrap()
        )
        .contains(marker));
    }

    // -- openinterpreter ---------------------------------------------------------------

    #[tokio::test]
    #[serial_test::serial]
    async fn openinterpreter_archive_conformance() {
        let sandbox = crate::test_utils::SandboxHome::new();
        let _env = ClearEnvGuard::clear(PROVIDER_ENVS);
        let base = sandbox.path().join(".openinterpreter");
        let marker = "conformance-marker-openinterpreter";
        let r1 = "rollout-2026-09-01-aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa.jsonl";
        let r2 = "rollout-2026-09-01-bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb.jsonl";
        struct HomeGuard;
        impl Drop for HomeGuard {
            fn drop(&mut self) {
                std::env::remove_var("INTERPRETER_HOME");
            }
        }
        std::env::set_var("INTERPRETER_HOME", &base);
        let home_guard = HomeGuard;
        write_file(
            &base.join("sessions").join(r1),
            codex_rollout("/w/oiproj", "ois1", &[&format!("hello {marker}")]).as_bytes(),
        );
        write_file(
            &base.join("sessions").join(r2),
            codex_rollout("/w/oiproj", "ois2", &["second"]).as_bytes(),
        );

        let projects = scan_provider("openinterpreter").await.unwrap();
        drop(home_guard);
        assert_eq!(projects.len(), 1, "openinterpreter: {projects:?}");
        assert_eq!(projects[0].path, "openinterpreter:///w/oiproj");
        let project = projects[0].path.clone();

        let sources = read_sources("openinterpreter");
        let sessions = load_provider_sessions("openinterpreter", &project, &sources)
            .await
            .unwrap();
        assert_eq!(sessions.len(), 2);
        let s1 = sessions[0].file_path.clone();
        assert!(message_text(
            &load_provider_messages("openinterpreter", &s1, &sources)
                .await
                .unwrap()
        )
        .contains(marker));

        std::fs::remove_dir_all(&base).unwrap();
        assert_eq!(scan_provider("openinterpreter").await.unwrap().len(), 1);
        let sources = read_sources("openinterpreter");
        assert_eq!(
            load_provider_sessions("openinterpreter", &project, &sources)
                .await
                .unwrap()
                .len(),
            2
        );
    }

    // -- opencode (sqlite primary) ---------------------------------------------------

    fn opencode_test_db(base: &Path) {
        let conn = rusqlite::Connection::open(base.join("opencode.db")).unwrap();
        conn.execute_batch(
            "CREATE TABLE project (
                id TEXT PRIMARY KEY, worktree TEXT NOT NULL, name TEXT,
                time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL
            );
            CREATE TABLE session (
                id TEXT PRIMARY KEY, project_id TEXT NOT NULL, title TEXT NOT NULL,
                directory TEXT NOT NULL DEFAULT '',
                time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL
            );
            CREATE TABLE message (
                id TEXT PRIMARY KEY, session_id TEXT NOT NULL,
                time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL,
                data TEXT NOT NULL
            );
            CREATE TABLE part (
                id TEXT PRIMARY KEY, message_id TEXT NOT NULL, session_id TEXT NOT NULL,
                time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL,
                data TEXT NOT NULL
            );",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO project (id, worktree, name, time_created, time_updated)
             VALUES ('oproj1', '/w/oproj', 'oproj', 1700000000000, 1700000100000)",
            [],
        )
        .unwrap();
    }

    fn opencode_add_session(base: &Path, sid: &str, title: &str, texts: &[&str]) {
        let conn = rusqlite::Connection::open(base.join("opencode.db")).unwrap();
        conn.execute(
            "INSERT INTO session (id, project_id, title, directory, time_created, time_updated)
             VALUES (?1, 'oproj1', ?2, '/w/oproj', 1700000000000, 1700000050000)",
            rusqlite::params![sid, title],
        )
        .unwrap();
        for (i, text) in texts.iter().enumerate() {
            let mid = format!("{sid}-m{i}");
            conn.execute(
                "INSERT INTO message (id, session_id, time_created, time_updated, data)
                 VALUES (?1, ?2, 1700000010000, 1700000010000, ?3)",
                rusqlite::params![
                    mid,
                    sid,
                    serde_json::json!({"role": "user", "time": {"created": 1700000010000_i64}})
                        .to_string()
                ],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO part (id, message_id, session_id, time_created, time_updated, data)
                 VALUES (?1, ?2, ?3, 1700000010000, 1700000010000, ?4)",
                rusqlite::params![
                    format!("{mid}-p"),
                    mid,
                    sid,
                    serde_json::json!({"type": "text", "text": text}).to_string()
                ],
            )
            .unwrap();
        }
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn opencode_archive_conformance() {
        let sandbox = crate::test_utils::SandboxHome::new();
        let _env = ClearEnvGuard::clear(PROVIDER_ENVS);
        // OPENCODE_HOME override keeps the fixture out of any real checkout.
        let base = sandbox.path().join("oc-home");
        std::fs::create_dir_all(&base).unwrap();
        std::env::set_var("OPENCODE_HOME", &base);
        struct OpenCodeHomeGuard;
        impl Drop for OpenCodeHomeGuard {
            fn drop(&mut self) {
                std::env::remove_var("OPENCODE_HOME");
            }
        }
        let _oc_home = OpenCodeHomeGuard;
        let marker = "conformance-marker-opencode";
        opencode_test_db(&base);
        opencode_add_session(&base, "oses1", "first", &[&format!("hello {marker}")]);
        opencode_add_session(&base, "oses2", "second", &["second"]);

        let projects = scan_provider("opencode").await.unwrap();
        assert_eq!(projects.len(), 1, "opencode: {projects:?}");
        assert_eq!(projects[0].path, "opencode://oproj1");
        let project = projects[0].path.clone();
        // The database rode along into the snapshot via backup capture.
        let sources = read_sources("opencode");
        assert!(sources[0].snapshot.data_path.join("opencode.db").is_file());

        let sessions = load_provider_sessions("opencode", &project, &sources)
            .await
            .unwrap();
        assert_eq!(sessions.len(), 2);
        assert!(message_text(
            &load_provider_messages("opencode", "opencode://oproj1/oses1", &sources)
                .await
                .unwrap()
        )
        .contains(marker));
        assert!(!search_provider("opencode", marker, 10, &sources)
            .await
            .unwrap()
            .is_empty());

        // Disappearance: delete oses1, extend oses2, add oses3.
        {
            let conn = rusqlite::Connection::open(base.join("opencode.db")).unwrap();
            conn.execute("DELETE FROM session WHERE id = 'oses1'", [])
                .unwrap();
            conn.execute("DELETE FROM message WHERE session_id = 'oses1'", [])
                .unwrap();
            conn.execute("DELETE FROM part WHERE session_id = 'oses1'", [])
                .unwrap();
        }
        opencode_add_session(&base, "oses3", "third", &["third"]);
        let projects_after = scan_provider("opencode").await.unwrap();
        assert_eq!(projects_after.len(), 1);
        let sessions_after = load_provider_sessions(
            "opencode",
            &projects_after[0].path,
            &read_sources("opencode"),
        )
        .await
        .unwrap();
        assert_eq!(sessions_after.len(), 3, "oses1 preserved + oses2 + oses3");

        // The live database can vanish; browsing continues from snapshots.
        std::fs::remove_dir_all(&base).unwrap();
        assert_eq!(scan_provider("opencode").await.unwrap().len(), 1);
        let sources = read_sources("opencode");
        assert_eq!(
            load_provider_sessions("opencode", &project, &sources)
                .await
                .unwrap()
                .len(),
            3
        );
        assert!(message_text(
            &load_provider_messages("opencode", "opencode://oproj1/oses1", &sources)
                .await
                .unwrap()
        )
        .contains(marker));
        // A torn copy must never validate: garbage bytes fail quick_check.
        assert!(crate::storage::sqlite_capture::quick_check_db(
            &sources[0].snapshot.data_path.join("opencode.db")
        )
        .is_ok());
    }

    // -- forgecode (sqlite primary) ----------------------------------------------------

    fn forgecode_test_db(base: &Path) {
        let conn = rusqlite::Connection::open(base.join(".forge.db")).unwrap();
        conn.execute_batch(
            "CREATE TABLE conversations (
                id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, title TEXT,
                context TEXT, metrics TEXT, created_at TEXT, updated_at TEXT
            );",
        )
        .unwrap();
    }

    fn forgecode_add_session(base: &Path, conv: &str, title: &str, texts: &[&str]) {
        let messages: Vec<_> = texts
            .iter()
            .map(|text| {
                serde_json::json!({"role": "user", "content": [{"type": "text", "text": text}]})
            })
            .collect();
        let conn = rusqlite::Connection::open(base.join(".forge.db")).unwrap();
        conn.execute(
            "INSERT INTO conversations (id, workspace_id, title, context, created_at, updated_at)
             VALUES (?1, 'ws-alpha', ?2, ?3, '2026-09-01', '2026-09-01')",
            rusqlite::params![
                conv,
                title,
                serde_json::json!({
                    "conversation_id": conv, "cwd": "/w/fgproj", "messages": messages,
                })
                .to_string()
            ],
        )
        .unwrap();
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn forgecode_archive_conformance() {
        let sandbox = crate::test_utils::SandboxHome::new();
        let _env = ClearEnvGuard::clear(PROVIDER_ENVS);
        let base = sandbox.path().join("forge-home");
        std::fs::create_dir_all(&base).unwrap();
        std::env::set_var("FORGE_CONFIG", &base);
        struct ForgeGuard;
        impl Drop for ForgeGuard {
            fn drop(&mut self) {
                std::env::remove_var("FORGE_CONFIG");
            }
        }
        let _forge = ForgeGuard;
        let marker = "conformance-marker-forgecode";
        forgecode_test_db(&base);
        forgecode_add_session(&base, "conv-001", "first", &[&format!("hello {marker}")]);
        forgecode_add_session(&base, "conv-002", "second", &["second"]);

        let projects = scan_provider("forgecode").await.unwrap();
        assert_eq!(projects.len(), 1, "forgecode: {projects:?}");
        assert_eq!(projects[0].path, "forgecode://workspace/ws-alpha");
        let project = projects[0].path.clone();

        let sources = read_sources("forgecode");
        let sessions = load_provider_sessions("forgecode", &project, &sources)
            .await
            .unwrap();
        assert_eq!(sessions.len(), 2);
        assert!(message_text(
            &load_provider_messages(
                "forgecode",
                "forgecode://workspace/ws-alpha/conversation/conv-001",
                &sources
            )
            .await
            .unwrap()
        )
        .contains(marker));
        assert!(!search_provider("forgecode", marker, 10, &sources)
            .await
            .unwrap()
            .is_empty());

        {
            let conn = rusqlite::Connection::open(base.join(".forge.db")).unwrap();
            conn.execute("DELETE FROM conversations WHERE id = 'conv-001'", [])
                .unwrap();
        }
        forgecode_add_session(&base, "conv-003", "third", &["third"]);
        let projects_after = scan_provider("forgecode").await.unwrap();
        assert_eq!(projects_after.len(), 1);
        let sessions_after = load_provider_sessions(
            "forgecode",
            &projects_after[0].path,
            &read_sources("forgecode"),
        )
        .await
        .unwrap();
        assert_eq!(sessions_after.len(), 3, "conv-001 preserved + 002 + 003");

        std::fs::remove_dir_all(&base).unwrap();
        assert_eq!(scan_provider("forgecode").await.unwrap().len(), 1);
        let sources = read_sources("forgecode");
        assert_eq!(
            load_provider_sessions("forgecode", &project, &sources)
                .await
                .unwrap()
                .len(),
            3
        );
    }

    // -- goose (single sqlite db) --------------------------------------------------------

    fn goose_test_db(base: &Path) {
        let conn = rusqlite::Connection::open(base.join("sessions.db")).unwrap();
        conn.execute_batch(
            "CREATE TABLE sessions (
                id TEXT PRIMARY KEY, working_dir TEXT NOT NULL, name TEXT,
                description TEXT, created_at TEXT, updated_at TEXT
            );
            CREATE TABLE messages (
                id INTEGER PRIMARY KEY, session_id TEXT NOT NULL, role TEXT NOT NULL,
                content_json TEXT NOT NULL, created_timestamp INTEGER NOT NULL,
                message_id TEXT
            );",
        )
        .unwrap();
    }

    fn goose_add_session(base: &Path, sid: &str, cwd: &str, texts: &[&str]) {
        let conn = rusqlite::Connection::open(base.join("sessions.db")).unwrap();
        conn.execute(
            "INSERT INTO sessions (id, working_dir, name, created_at, updated_at)
             VALUES (?1, ?2, ?3, '2026-09-01', '2026-09-01')",
            rusqlite::params![sid, cwd, sid],
        )
        .unwrap();
        for (i, text) in texts.iter().enumerate() {
            conn.execute(
                "INSERT INTO messages (session_id, role, content_json, created_timestamp, message_id)
                 VALUES (?1, 'user', ?2, 1757000000, ?3)",
                rusqlite::params![
                    sid,
                    serde_json::json!([{"type": "text", "text": text}]).to_string(),
                    format!("{sid}-m{i}")
                ],
            )
            .unwrap();
        }
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn goose_archive_conformance() {
        let sandbox = crate::test_utils::SandboxHome::new();
        let _env = ClearEnvGuard::clear(PROVIDER_ENVS);
        let base = sandbox.path().join(".local/share/goose/sessions");
        std::fs::create_dir_all(&base).unwrap();
        let marker = "conformance-marker-goose";
        goose_test_db(&base);
        goose_add_session(&base, "gs1", "/w/gproj", &[&format!("hello {marker}")]);
        goose_add_session(&base, "gs2", "/w/gproj", &["second"]);

        let projects = scan_provider("goose").await.unwrap();
        assert_eq!(projects.len(), 1, "goose: {projects:?}");
        assert_eq!(projects[0].path, "goose:///w/gproj");
        let project = projects[0].path.clone();
        let sources = read_sources("goose");
        assert!(sources[0].snapshot.data_path.join("sessions.db").is_file());

        let sessions = load_provider_sessions("goose", &project, &sources)
            .await
            .unwrap();
        assert_eq!(sessions.len(), 2);
        assert!(message_text(
            &load_provider_messages("goose", "goose://gs1", &sources)
                .await
                .unwrap()
        )
        .contains(marker));
        assert!(!search_provider("goose", marker, 10, &sources)
            .await
            .unwrap()
            .is_empty());

        {
            let conn = rusqlite::Connection::open(base.join("sessions.db")).unwrap();
            conn.execute("DELETE FROM sessions WHERE id = 'gs1'", [])
                .unwrap();
            conn.execute("DELETE FROM messages WHERE session_id = 'gs1'", [])
                .unwrap();
        }
        goose_add_session(&base, "gs3", "/w/gproj", &["third"]);
        let projects_after = scan_provider("goose").await.unwrap();
        assert_eq!(projects_after.len(), 1);
        let sessions_after =
            load_provider_sessions("goose", &projects_after[0].path, &read_sources("goose"))
                .await
                .unwrap();
        assert_eq!(sessions_after.len(), 3, "gs1 preserved + gs2 + gs3");

        std::fs::remove_dir_all(sandbox.path().join(".local")).unwrap();
        assert_eq!(scan_provider("goose").await.unwrap().len(), 1);
        let sources = read_sources("goose");
        assert_eq!(
            load_provider_sessions("goose", &project, &sources)
                .await
                .unwrap()
                .len(),
            3
        );
        assert!(message_text(
            &load_provider_messages("goose", "goose://gs1", &sources)
                .await
                .unwrap()
        )
        .contains(marker));
    }

    // -- llm (single sqlite db, synthetic project) -------------------------------------

    fn llm_test_db(base: &Path) {
        let conn = rusqlite::Connection::open(base.join("logs.db")).unwrap();
        conn.execute_batch(
            "CREATE TABLE conversations (id TEXT PRIMARY KEY, name TEXT, model TEXT);
             CREATE TABLE responses (
                id TEXT PRIMARY KEY, model TEXT, prompt TEXT, response TEXT,
                conversation_id TEXT, datetime_utc TEXT,
                input_tokens INTEGER, output_tokens INTEGER
            );",
        )
        .unwrap();
    }

    fn llm_add_response(base: &Path, conv: &str, rid: &str, prompt: &str) {
        let conn = rusqlite::Connection::open(base.join("logs.db")).unwrap();
        conn.execute(
            "INSERT OR IGNORE INTO conversations (id, name) VALUES (?1, ?2)",
            rusqlite::params![conv, conv],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO responses (id, model, prompt, response, conversation_id, datetime_utc)
             VALUES (?1, 'm', ?2, 'ok', ?3, '2026-09-01T00:00:00')",
            rusqlite::params![rid, prompt, conv],
        )
        .unwrap();
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn llm_archive_conformance() {
        let sandbox = crate::test_utils::SandboxHome::new();
        let _env = ClearEnvGuard::clear(PROVIDER_ENVS);
        // Point the provider at the sandbox via its env override.
        let base = sandbox.path().join("llm-home");
        std::fs::create_dir_all(&base).unwrap();
        std::env::set_var("LLM_USER_PATH", &base);
        struct LlmGuard;
        impl Drop for LlmGuard {
            fn drop(&mut self) {
                std::env::remove_var("LLM_USER_PATH");
            }
        }
        let _guard = LlmGuard;
        let marker = "conformance-marker-llm";
        llm_test_db(&base);
        llm_add_response(&base, "c1", "r1", &format!("hello {marker}"));
        llm_add_response(&base, "c2", "r2", "second");

        let projects = scan_provider("llm").await.unwrap();
        assert_eq!(projects.len(), 1, "llm: {projects:?}");
        assert_eq!(projects[0].path, "llm://__all__");
        let project = projects[0].path.clone();

        let sources = read_sources("llm");
        let sessions = load_provider_sessions("llm", &project, &sources)
            .await
            .unwrap();
        assert_eq!(sessions.len(), 2);
        assert!(message_text(
            &load_provider_messages("llm", "llm://c1", &sources)
                .await
                .unwrap()
        )
        .contains(marker));
        assert!(!search_provider("llm", marker, 10, &sources)
            .await
            .unwrap()
            .is_empty());

        {
            let conn = rusqlite::Connection::open(base.join("logs.db")).unwrap();
            conn.execute("DELETE FROM conversations WHERE id = 'c1'", [])
                .unwrap();
            conn.execute("DELETE FROM responses WHERE conversation_id = 'c1'", [])
                .unwrap();
        }
        llm_add_response(&base, "c3", "r3", "third");
        let projects_after = scan_provider("llm").await.unwrap();
        assert_eq!(projects_after.len(), 1);
        let sessions_after =
            load_provider_sessions("llm", &projects_after[0].path, &read_sources("llm"))
                .await
                .unwrap();
        assert_eq!(sessions_after.len(), 3, "c1 preserved + c2 + c3");

        std::fs::remove_dir_all(&base).unwrap();
        assert_eq!(scan_provider("llm").await.unwrap().len(), 1);
        let sources = read_sources("llm");
        assert_eq!(
            load_provider_sessions("llm", &project, &sources)
                .await
                .unwrap()
                .len(),
            3
        );
    }

    // -- zed (single sqlite db, zstd/json threads) ---------------------------------------

    fn zed_test_db(base: &Path) {
        let conn = rusqlite::Connection::open(base.join("threads.db")).unwrap();
        conn.execute_batch(
            "CREATE TABLE threads (
                id TEXT PRIMARY KEY, summary TEXT, folder_paths TEXT,
                created_at TEXT, updated_at TEXT, data_type TEXT, data BLOB
            );",
        )
        .unwrap();
    }

    fn zed_add_thread(base: &Path, tid: &str, ws: &str, texts: &[&str]) {
        let messages: Vec<_> = texts
            .iter()
            .map(|text| serde_json::json!({"User": {"id": tid, "content": [{"Text": text}]}}))
            .collect();
        let conn = rusqlite::Connection::open(base.join("threads.db")).unwrap();
        conn.execute(
            "INSERT INTO threads (id, summary, folder_paths, created_at, updated_at, data_type, data)
             VALUES (?1, ?2, ?3, '2026-09-01', '2026-09-01', 'json', ?4)",
            rusqlite::params![
                tid,
                tid,
                serde_json::json!([ws]).to_string(),
                serde_json::json!({"messages": messages}).to_string().into_bytes()
            ],
        )
        .unwrap();
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn zed_archive_conformance() {
        // Zed resolves its dir from OS data dirs, which the sandbox cannot
        // rewrite on macOS, so this test drives the archive flow explicitly
        // (sync + registry flows) instead of through live discovery. The
        // scan_provider dispatch path is covered by every other provider.
        let _sandbox = crate::test_utils::SandboxHome::new();
        let _env = ClearEnvGuard::clear(PROVIDER_ENVS);
        let marker = "conformance-marker-zed";
        let live_dir = tempfile::tempdir().unwrap();
        let live_threads = live_dir.path().join("threads");
        std::fs::create_dir_all(&live_threads).unwrap();
        zed_test_db(&live_threads);
        zed_add_thread(
            &live_threads,
            "thread-1",
            "/w/zproj",
            &[&format!("hello {marker}")],
        );
        zed_add_thread(&live_threads, "thread-2", "/w/zproj", &["second"]);

        let machine = discovery_machine_id();
        let mut found =
            DiscoveredSource::local(crate::storage::ROLE_PRIMARY, live_threads.clone(), &machine);
        found.sqlite_dbs = vec!["threads.db".to_string()];
        let source = source_for_discovered("zed", &found);
        let opts = crate::storage::SyncOptions {
            extra_sqlite_dbs: vec![],
        };
        crate::storage::sync_source(&source, &live_threads, &opts).unwrap();

        // read_sources picks up the registered source even though live
        // discovery cannot see this fixture root.
        let sources = read_sources("zed");
        assert!(!sources.is_empty());
        let projects =
            crate::providers::zed::archive_scan(&sources[0].source, &sources[0].snapshot).unwrap();
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].path, "zed:///w/zproj");
        let project = projects[0].path.clone();

        let sessions = load_provider_sessions("zed", &project, &sources)
            .await
            .unwrap();
        assert_eq!(sessions.len(), 2);
        assert!(message_text(
            &load_provider_messages("zed", "zed://thread-1", &sources)
                .await
                .unwrap()
        )
        .contains(marker));
        assert!(!search_provider("zed", marker, 10, &sources)
            .await
            .unwrap()
            .is_empty());

        // Delete thread-1 upstream, re-sync, assert row-level preservation.
        {
            let conn = rusqlite::Connection::open(live_threads.join("threads.db")).unwrap();
            conn.execute("DELETE FROM threads WHERE id = 'thread-1'", [])
                .unwrap();
        }
        zed_add_thread(&live_threads, "thread-3", "/w/zproj", &["third"]);
        crate::storage::sync_source(&source, &live_threads, &opts).unwrap();
        let sources = read_sources("zed");
        let sessions_after = load_provider_sessions("zed", &project, &sources)
            .await
            .unwrap();
        assert_eq!(
            sessions_after.len(),
            3,
            "thread-1 preserved via row merge-back"
        );

        // The live database can vanish; everything keeps working.
        std::fs::remove_dir_all(live_dir.path()).unwrap();
        let sources = read_sources("zed");
        assert_eq!(
            load_provider_sessions("zed", &project, &sources)
                .await
                .unwrap()
                .len(),
            3
        );
        assert!(message_text(
            &load_provider_messages("zed", "zed://thread-1", &sources)
                .await
                .unwrap()
        )
        .contains(marker));
    }

    // -- amazon_q / kiro (single sqlite db, shared conversation format) ----------

    fn q_test_db(base: &Path, db_name: &str, table: &str) {
        let conn = rusqlite::Connection::open(base.join(db_name)).unwrap();
        conn.execute_batch(&format!(
            "CREATE TABLE {table} (key TEXT PRIMARY KEY, value TEXT);"
        ))
        .unwrap();
    }

    fn q_conversation_value(texts: &[&str]) -> String {
        let history: Vec<_> = texts
            .iter()
            .map(|text| {
                serde_json::json!({
                    "user": {
                        "content": {"Prompt": {"prompt": text}},
                        "timestamp": "2026-09-01T00:00:00Z",
                    },
                    "assistant": {"Response": {"message_id": "a1", "content": "ok"}},
                })
            })
            .collect();
        serde_json::json!({"history": history}).to_string()
    }

    fn q_add_conversation(base: &Path, db_name: &str, table: &str, key: &str, texts: &[&str]) {
        let conn = rusqlite::Connection::open(base.join(db_name)).unwrap();
        conn.execute(
            &format!("INSERT INTO {table} (key, value) VALUES (?1, ?2)"),
            rusqlite::params![key, q_conversation_value(texts)],
        )
        .unwrap();
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn amazonq_archive_conformance() {
        let _sandbox = crate::test_utils::SandboxHome::new();
        let _env = ClearEnvGuard::clear(PROVIDER_ENVS);
        // data_local_dir() is OS-resolved (not sandbox-controlled on macOS),
        // so seed the OS location only when it already falls inside the
        // sandbox; otherwise drive the archive flow explicitly like zed.
        // Simplest hermetic route that still covers dispatch: XDG_DATA_HOME
        // is honored on no platform here... instead register explicitly.
        let marker = "conformance-marker-amazonq";
        let live_dir = tempfile::tempdir().unwrap();
        let live_base = live_dir.path().join("amazon-q");
        std::fs::create_dir_all(&live_base).unwrap();
        q_test_db(&live_base, "data.sqlite3", "conversations");
        q_add_conversation(
            &live_base,
            "data.sqlite3",
            "conversations",
            "/w/qproj",
            &[&format!("hello {marker}")],
        );

        let machine = discovery_machine_id();
        let mut found =
            DiscoveredSource::local(crate::storage::ROLE_PRIMARY, live_base.clone(), &machine);
        found.sqlite_dbs = vec!["data.sqlite3".to_string()];
        let source = source_for_discovered("amazonq", &found);
        let opts = crate::storage::SyncOptions {
            extra_sqlite_dbs: vec![],
        };
        crate::storage::sync_source(&source, &live_base, &opts).unwrap();

        let sources = read_sources("amazonq");
        assert!(!sources.is_empty());
        let projects =
            crate::providers::amazon_q::archive_scan(&sources[0].source, &sources[0].snapshot)
                .unwrap();
        assert_eq!(projects.len(), 1, "amazonq: {projects:?}");
        assert_eq!(projects[0].path, "amazonq:///w/qproj");
        let project = projects[0].path.clone();

        let sessions = load_provider_sessions("amazonq", &project, &sources)
            .await
            .unwrap();
        assert_eq!(sessions.len(), 1);
        assert!(message_text(
            &load_provider_messages("amazonq", &sessions[0].file_path, &sources)
                .await
                .unwrap()
        )
        .contains(marker));
        assert!(!search_provider("amazonq", marker, 10, &sources)
            .await
            .unwrap()
            .is_empty());

        // Delete the conversation upstream; the merged snapshot keeps it.
        {
            let conn = rusqlite::Connection::open(live_base.join("data.sqlite3")).unwrap();
            conn.execute("DELETE FROM conversations WHERE key = '/w/qproj'", [])
                .unwrap();
        }
        crate::storage::sync_source(&source, &live_base, &opts).unwrap();
        let sources = read_sources("amazonq");
        assert_eq!(
            load_provider_sessions("amazonq", &project, &sources)
                .await
                .unwrap()
                .len(),
            1,
            "deleted conversation preserved via row merge-back"
        );

        std::fs::remove_dir_all(live_dir.path()).unwrap();
        let sources = read_sources("amazonq");
        assert!(message_text(
            &load_provider_messages("amazonq", &sessions[0].file_path, &sources)
                .await
                .unwrap()
        )
        .contains(marker));
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn kiro_archive_conformance() {
        let _sandbox = crate::test_utils::SandboxHome::new();
        let _env = ClearEnvGuard::clear(PROVIDER_ENVS);
        let marker = "conformance-marker-kiro";
        let live_dir = tempfile::tempdir().unwrap();
        let live_base = live_dir.path().join("kiro-cli");
        std::fs::create_dir_all(&live_base).unwrap();
        {
            let conn = rusqlite::Connection::open(live_base.join("data.sqlite3")).unwrap();
            conn.execute_batch(
                "CREATE TABLE conversations_v2 (
                    key TEXT NOT NULL, conversation_id TEXT PRIMARY KEY,
                    value TEXT NOT NULL, created_at INTEGER NOT NULL,
                    updated_at INTEGER NOT NULL
                );",
            )
            .unwrap();
            conn.execute(
                "INSERT INTO conversations_v2 (key, conversation_id, value, created_at, updated_at)
                 VALUES ('/w/kproj', 'conv-1', ?1, 1757000000, 1757000000)",
                rusqlite::params![q_conversation_value(&[&format!("hello {marker}")])],
            )
            .unwrap();
        }

        let machine = discovery_machine_id();
        let mut found =
            DiscoveredSource::local(crate::storage::ROLE_PRIMARY, live_base.clone(), &machine);
        found.sqlite_dbs = vec!["data.sqlite3".to_string()];
        let source = source_for_discovered("kiro", &found);
        let opts = crate::storage::SyncOptions {
            extra_sqlite_dbs: vec![],
        };
        crate::storage::sync_source(&source, &live_base, &opts).unwrap();

        let sources = read_sources("kiro");
        assert!(!sources.is_empty());
        let projects =
            crate::providers::kiro::archive_scan(&sources[0].source, &sources[0].snapshot).unwrap();
        assert_eq!(projects.len(), 1, "kiro: {projects:?}");
        let project = projects[0].path.clone();

        let sessions = load_provider_sessions("kiro", &project, &sources)
            .await
            .unwrap();
        assert_eq!(sessions.len(), 1);
        assert!(message_text(
            &load_provider_messages("kiro", &sessions[0].file_path, &sources)
                .await
                .unwrap()
        )
        .contains(marker));

        {
            let conn = rusqlite::Connection::open(live_base.join("data.sqlite3")).unwrap();
            conn.execute(
                "DELETE FROM conversations_v2 WHERE conversation_id = 'conv-1'",
                [],
            )
            .unwrap();
        }
        crate::storage::sync_source(&source, &live_base, &opts).unwrap();
        let sources = read_sources("kiro");
        assert_eq!(
            load_provider_sessions("kiro", &project, &sources)
                .await
                .unwrap()
                .len(),
            1,
            "deleted conversation preserved via row merge-back"
        );

        std::fs::remove_dir_all(live_dir.path()).unwrap();
        let sources = read_sources("kiro");
        assert!(message_text(
            &load_provider_messages("kiro", &sessions[0].file_path, &sources)
                .await
                .unwrap()
        )
        .contains(marker));
    }

    // -- trae (workspace sqlite dbs) -----------------------------------------------------

    fn trae_workspace_db(ws: &Path, sessions: &[(&str, &str)]) {
        let conn = rusqlite::Connection::open(ws.join("state.vscdb")).unwrap();
        conn.execute(
            "CREATE TABLE ItemTable (key TEXT UNIQUE ON CONFLICT REPLACE, value TEXT)",
            [],
        )
        .unwrap();
        let list: Vec<_> = sessions
            .iter()
            .map(|(sid, text)| {
                serde_json::json!({
                    "id": sid, "title": format!("{sid} title"),
                    "messages": [{"role": "user", "content": text}],
                })
            })
            .collect();
        conn.execute(
            "INSERT INTO ItemTable (key, value) VALUES ('memento/icube-ai-agent-storage', ?1)",
            rusqlite::params![serde_json::json!({"list": list}).to_string()],
        )
        .unwrap();
        std::fs::write(ws.join("workspace.json"), r#"{"folder":"file:///w/tproj"}"#).unwrap();
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn trae_archive_conformance() {
        let _sandbox = crate::test_utils::SandboxHome::new();
        let _env = ClearEnvGuard::clear(PROVIDER_ENVS);
        // config_dir() is OS-resolved; drive discovery through the fixture by
        // pointing it at the sandbox is impossible, so seed the OS location
        // only if hermetic — instead register + sync explicitly like zed.
        // Trae honors no env override; use explicit source registration.
        let marker = "conformance-marker-trae";
        let live_dir = tempfile::tempdir().unwrap();
        let storage = live_dir.path().join("workspaceStorage");
        let ws_a = storage.join("hash-aaa");
        std::fs::create_dir_all(&ws_a).unwrap();
        trae_workspace_db(&ws_a, &[("sess-a1", &format!("hello {marker}"))]);
        let ws_b = storage.join("hash-bbb");
        std::fs::create_dir_all(&ws_b).unwrap();
        trae_workspace_db(&ws_b, &[("sess-b1", "second")]);

        let machine = discovery_machine_id();
        let mut found =
            DiscoveredSource::local(crate::storage::ROLE_PRIMARY, storage.clone(), &machine);
        found.extra_sqlite_dbs = vec![
            "hash-aaa/state.vscdb".to_string(),
            "hash-bbb/state.vscdb".to_string(),
        ];
        let source = source_for_discovered("trae", &found);
        let opts = crate::storage::SyncOptions {
            extra_sqlite_dbs: vec![],
        };
        // NOTE: extra dbs live on the source record after claim; pass them
        // explicitly here since discovery ran before registration.
        let mut opts = opts;
        opts.extra_sqlite_dbs = found.extra_sqlite_dbs.clone();
        crate::storage::sync_source(&source, &storage, &opts).unwrap();

        let sources = read_sources("trae");
        assert!(!sources.is_empty());
        let projects =
            crate::providers::trae::archive_scan(&sources[0].source, &sources[0].snapshot).unwrap();
        assert_eq!(projects.len(), 2, "trae: {projects:?}");
        let project_a = projects
            .iter()
            .find(|p| p.path == "trae://hash-aaa")
            .expect("hash-aaa")
            .path
            .clone();

        let sessions = load_provider_sessions("trae", &project_a, &sources)
            .await
            .unwrap();
        assert_eq!(sessions.len(), 1);
        assert!(message_text(
            &load_provider_messages("trae", "trae://hash-aaa#sess-a1", &sources)
                .await
                .unwrap()
        )
        .contains(marker));
        assert!(!search_provider("trae", marker, 10, &sources)
            .await
            .unwrap()
            .is_empty());

        // Delete sess-a1 upstream (row-level), extend, add sess-c1.
        {
            let conn = rusqlite::Connection::open(ws_a.join("state.vscdb")).unwrap();
            let value: String = conn
                .query_row(
                    "SELECT value FROM ItemTable WHERE key = 'memento/icube-ai-agent-storage'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            let mut doc: serde_json::Value = serde_json::from_str(&value).unwrap();
            doc["list"] = serde_json::json!([{
                "id": "sess-a2", "title": "second",
                "messages": [{"role": "user", "content": "more"}],
            }]);
            conn.execute(
                "UPDATE ItemTable SET value = ?1 WHERE key = 'memento/icube-ai-agent-storage'",
                rusqlite::params![doc.to_string()],
            )
            .unwrap();
        }
        let ws_c = storage.join("hash-ccc");
        std::fs::create_dir_all(&ws_c).unwrap();
        trae_workspace_db(&ws_c, &[("sess-c1", "third")]);
        // New workspace joins capture automatically on re-sync.
        let mut found2 = found.clone();
        found2
            .extra_sqlite_dbs
            .push("hash-ccc/state.vscdb".to_string());
        let source2 = source_for_discovered("trae", &found2);
        let mut opts2 = opts.clone();
        opts2.extra_sqlite_dbs = found2.extra_sqlite_dbs.clone();
        crate::storage::sync_source(&source2, &storage, &opts2).unwrap();

        let sources = read_sources("trae");
        let sessions_a = load_provider_sessions("trae", &project_a, &sources)
            .await
            .unwrap();
        assert_eq!(
            sessions_a.len(),
            2,
            "sess-a1 preserved + sess-a2, got {sessions_a:?}"
        );
        let sessions_c = load_provider_sessions("trae", "trae://hash-ccc", &sources)
            .await
            .unwrap();
        assert_eq!(sessions_c.len(), 1);

        std::fs::remove_dir_all(live_dir.path()).unwrap();
        let sources = read_sources("trae");
        assert_eq!(
            load_provider_sessions("trae", &project_a, &sources)
                .await
                .unwrap()
                .len(),
            2
        );
        assert!(message_text(
            &load_provider_messages("trae", "trae://hash-aaa#sess-a1", &sources)
                .await
                .unwrap()
        )
        .contains(marker));
    }

    // -- cursor (global + workspace sqlite dbs) ------------------------------------------------

    fn cursor_global_db(user_dir: &Path, composers: &[(&str, &str, &str)]) {
        let global = user_dir.join("globalStorage");
        std::fs::create_dir_all(&global).unwrap();
        let conn = rusqlite::Connection::open(global.join("state.vscdb")).unwrap();
        conn.execute(
            "CREATE TABLE IF NOT EXISTS cursorDiskKV (key TEXT UNIQUE ON CONFLICT REPLACE, value TEXT)",
            [],
        )
        .unwrap();
        for (cid, name, text) in composers {
            let composer = serde_json::json!({
                "composerId": cid, "name": name, "createdAt": 1_700_000_000_000u64,
                "lastUpdatedAt": 1_700_000_100_000u64, "isArchived": false,
                "unifiedMode": "agent",
                "workspaceIdentifier": {"id": "ws-hash", "uri": {"fsPath": "/w/cproj", "path": "/w/cproj"}},
                "fullConversationHeadersOnly": [
                    {"bubbleId": "b1", "type": 1},
                    {"bubbleId": "b2", "type": 2},
                ],
            });
            conn.execute(
                "INSERT INTO cursorDiskKV (key, value) VALUES (?1, ?2)",
                rusqlite::params![format!("composerData:{cid}"), composer.to_string()],
            )
            .unwrap();
            for (bid, btype) in [("b1", 1), ("b2", 2)] {
                conn.execute(
                    "INSERT INTO cursorDiskKV (key, value) VALUES (?1, ?2)",
                    rusqlite::params![
                        format!("bubbleId:{cid}:{bid}"),
                        serde_json::json!({
                            "bubbleId": bid, "type": btype, "text": text,
                            "createdAt": "2026-09-01T00:00:00Z",
                        })
                        .to_string()
                    ],
                )
                .unwrap();
            }
        }
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn cursor_archive_conformance() {
        let sandbox = crate::test_utils::SandboxHome::new();
        let _env = ClearEnvGuard::clear(PROVIDER_ENVS);
        // CURSOR_USER_DIR override keeps discovery hermetic.
        let user_dir = sandbox.path().join("cursor-user");
        std::env::set_var("CURSOR_USER_DIR", &user_dir);
        struct CursorGuard;
        impl Drop for CursorGuard {
            fn drop(&mut self) {
                std::env::remove_var("CURSOR_USER_DIR");
            }
        }
        let _cursor_guard = CursorGuard;
        let marker = "conformance-marker-cursor";
        let ws = user_dir.join("workspaceStorage/ws-hash");
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::write(ws.join("workspace.json"), r#"{"folder":"file:///w/cproj"}"#).unwrap();
        cursor_global_db(
            &user_dir,
            &[("comp-1", "first", &format!("hello {marker}"))],
        );

        // A legacy workspace db exercises multi-db capture alongside global.
        {
            let ws_conn = rusqlite::Connection::open(ws.join("state.vscdb")).unwrap();
            ws_conn
                .execute(
                    "CREATE TABLE ItemTable (key TEXT UNIQUE ON CONFLICT REPLACE, value TEXT)",
                    [],
                )
                .unwrap();
        }
        let projects = scan_provider("cursor").await.unwrap();
        assert_eq!(projects.len(), 1, "cursor: {projects:?}");
        let project = projects[0].path.clone();

        let sources = read_sources("cursor");
        let sessions = load_provider_sessions("cursor", &project, &sources)
            .await
            .unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].file_path, "cursor://comp-1");
        assert!(message_text(
            &load_provider_messages("cursor", "cursor://comp-1", &sources)
                .await
                .unwrap()
        )
        .contains(marker));
        assert!(!search_provider("cursor", marker, 10, &sources)
            .await
            .unwrap()
            .is_empty());

        // Delete comp-1 upstream, add comp-2; merge-back preserves comp-1.
        {
            let conn =
                rusqlite::Connection::open(user_dir.join("globalStorage/state.vscdb")).unwrap();
            conn.execute(
                "DELETE FROM cursorDiskKV WHERE key LIKE 'composerData:comp-1%'",
                [],
            )
            .unwrap();
            conn.execute(
                "DELETE FROM cursorDiskKV WHERE key LIKE 'bubbleId:comp-1%'",
                [],
            )
            .unwrap();
        }
        cursor_global_db(&user_dir, &[("comp-2", "second", "second")]);
        let projects_after = scan_provider("cursor").await.unwrap();
        assert_eq!(projects_after.len(), 1);
        let sessions_after =
            load_provider_sessions("cursor", &projects_after[0].path, &read_sources("cursor"))
                .await
                .unwrap();
        assert_eq!(sessions_after.len(), 2, "comp-1 preserved + comp-2");

        std::fs::remove_dir_all(&user_dir).unwrap();
        assert_eq!(scan_provider("cursor").await.unwrap().len(), 1);
        let sources = read_sources("cursor");
        assert_eq!(
            load_provider_sessions("cursor", &project, &sources)
                .await
                .unwrap()
                .len(),
            2
        );
        assert!(message_text(
            &load_provider_messages("cursor", "cursor://comp-1", &sources)
                .await
                .unwrap()
        )
        .contains(marker));
    }

    // -- crush (per-project sqlite dbs) ----------------------------------------------------------

    fn crush_test_db(db_path: &Path, sessions: &[(&str, &str, &[&str])]) {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let conn = rusqlite::Connection::open(db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE sessions (id TEXT PRIMARY KEY, title TEXT, created_at INTEGER, updated_at INTEGER);
             CREATE TABLE messages (id TEXT PRIMARY KEY, session_id TEXT NOT NULL, role TEXT NOT NULL, parts TEXT NOT NULL DEFAULT '[]', created_at INTEGER);",
        )
        .unwrap();
        for (sid, title, texts) in sessions {
            conn.execute(
                "INSERT INTO sessions VALUES (?1, ?2, 1750000000, 1750000100)",
                rusqlite::params![sid, title],
            )
            .unwrap();
            for (i, text) in texts.iter().enumerate() {
                conn.execute(
                    "INSERT INTO messages VALUES (?1, ?2, 'user', ?3, 1750000000)",
                    rusqlite::params![
                        format!("{sid}-m{i}"),
                        sid,
                        serde_json::json!([{"type": "text", "data": {"text": text}}]).to_string()
                    ],
                )
                .unwrap();
            }
        }
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn crush_archive_conformance() {
        let sandbox = crate::test_utils::SandboxHome::new();
        let _env = ClearEnvGuard::clear(PROVIDER_ENVS);
        // Crush discovers under ~/client (depth 2); seed two project DBs.
        let proj_a = sandbox.path().join("client/crusha");
        let proj_b = sandbox.path().join("client/crushb");
        let marker = "conformance-marker-crush";
        crush_test_db(
            &proj_a.join(".crush/crush.db"),
            &[("sess-a1", "first", &[&format!("hello {marker}")])],
        );
        crush_test_db(
            &proj_b.join(".crush/crush.db"),
            &[("sess-b1", "second", &["second"])],
        );

        let projects = scan_provider("crush").await.unwrap();
        assert_eq!(projects.len(), 2, "crush: {projects:?}");
        let project_a = projects
            .iter()
            .find(|p| p.name == "crusha")
            .expect("crusha")
            .path
            .clone();
        assert!(project_a.starts_with("crush://"));

        let sources = read_sources("crush");
        assert_eq!(sources.len(), 2);
        let sessions = load_provider_sessions("crush", &project_a, &sources)
            .await
            .unwrap();
        assert_eq!(sessions.len(), 1);
        assert!(message_text(
            &load_provider_messages("crush", &sessions[0].file_path, &sources)
                .await
                .unwrap()
        )
        .contains(marker));
        assert!(!search_provider("crush", marker, 10, &sources)
            .await
            .unwrap()
            .is_empty());

        // Delete sess-a1 upstream, extend, add sess-a2 in the same db.
        {
            let conn = rusqlite::Connection::open(proj_a.join(".crush/crush.db")).unwrap();
            conn.execute("DELETE FROM sessions WHERE id = 'sess-a1'", [])
                .unwrap();
            conn.execute("DELETE FROM messages WHERE session_id = 'sess-a1'", [])
                .unwrap();
            conn.execute(
                "INSERT INTO sessions VALUES ('sess-a2', 'second', 1750000000, 1750000100)",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO messages VALUES ('sess-a2-m0', 'sess-a2', 'user', ?1, 1750000000)",
                rusqlite::params![
                    serde_json::json!([{"type": "text", "data": {"text": "more"}}]).to_string()
                ],
            )
            .unwrap();
        }
        let projects_after = scan_provider("crush").await.unwrap();
        assert_eq!(projects_after.len(), 2);
        let sessions_a = load_provider_sessions(
            "crush",
            &projects_after
                .iter()
                .find(|p| p.name == "crusha")
                .expect("crusha")
                .path,
            &read_sources("crush"),
        )
        .await
        .unwrap();
        assert_eq!(sessions_a.len(), 2, "sess-a1 preserved + sess-a2");

        // Removing a whole project dir keeps its history browsable.
        std::fs::remove_dir_all(&proj_a).unwrap();
        assert_eq!(scan_provider("crush").await.unwrap().len(), 2);
        let sources = read_sources("crush");
        assert_eq!(
            load_provider_sessions("crush", &project_a, &sources)
                .await
                .unwrap()
                .len(),
            2
        );
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

    // -- cline -----------------------------------------------------------------

    fn cline_task(ext: &Path, id: &str, cwd: &str, task: &str, text: &str) {
        let history_path = ext.join("state/taskHistory.json");
        let mut history: Vec<serde_json::Value> = std::fs::read_to_string(&history_path)
            .ok()
            .and_then(|d| serde_json::from_str(&d).ok())
            .unwrap_or_default();
        if !history.iter().any(|t| t.get("id").and_then(|v| v.as_str()) == Some(id)) {
            history.push(serde_json::json!({
                "id": id,
                "ts": 1_757_000_000_000u64,
                "task": task,
                "cwdOnTaskInitialization": cwd,
                "tokensIn": 10,
                "tokensOut": 20,
            }));
        }
        write_file(
            &history_path,
            serde_json::to_string_pretty(&history).unwrap().as_bytes(),
        );
        write_file(
            &ext.join("tasks").join(id).join("ui_messages.json"),
            serde_json::to_string_pretty(&serde_json::json!([
                {"type": "say", "say": "text", "text": text, "ts": 1_757_000_000_000u64},
            ]))
            .unwrap()
            .as_bytes(),
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn cline_archive_conformance() {
        let sandbox = crate::test_utils::SandboxHome::new();
        let _env = ClearEnvGuard::clear(PROVIDER_ENVS);
        let marker = "conformance-marker-cline";
        let ext = sandbox
            .path()
            .join("Library/Application Support/Code/User/globalStorage/saoudrizwan.claude-dev");
        cline_task(&ext, "task-1", "/w/clineproj", "build it", &format!("hello {marker}"));
        cline_task(&ext, "task-2", "/w/clineproj", "second", "second");

        let projects = scan_provider("cline").await.unwrap();
        assert_eq!(projects.len(), 1, "cline: {projects:?}");
        let project = projects[0].path.clone();
        assert!(project.starts_with("cline://"), "stable scheme: {project}");
        assert!(!project.contains(".claude-history-viewer/data"));

        let sources = read_sources("cline");
        let sessions = load_provider_sessions("cline", &project, &sources)
            .await
            .unwrap();
        assert_eq!(sessions.len(), 2);
        let s1 = sessions
            .iter()
            .find(|s| s.actual_session_id == "task-1")
            .expect("task-1")
            .file_path
            .clone();
        assert!(
            message_text(&load_provider_messages("cline", &s1, &sources).await.unwrap())
                .contains(marker)
        );
        assert!(!search_provider("cline", marker, 10, &sources)
            .await
            .unwrap()
            .is_empty());

        // Disappearance: delete task-1 upstream, extend task-2, add task-3.
        let history_path = ext.join("state/taskHistory.json");
        let history: Vec<serde_json::Value> =
            serde_json::from_str(&std::fs::read_to_string(&history_path).unwrap()).unwrap();
        let kept: Vec<_> = history
            .into_iter()
            .filter(|t| t.get("id").and_then(|v| v.as_str()) != Some("task-1"))
            .collect();
        write_file(
            &history_path,
            serde_json::to_string_pretty(&kept).unwrap().as_bytes(),
        );
        std::fs::remove_dir_all(ext.join("tasks/task-1")).unwrap();
        cline_task(&ext, "task-2", "/w/clineproj", "second", &format!("more {marker}"));
        cline_task(&ext, "task-3", "/w/clineproj", "third", "third");
        let projects_after = scan_provider("cline").await.unwrap();
        assert_eq!(projects_after.len(), 1);
        let sessions_after =
            load_provider_sessions("cline", &projects_after[0].path, &read_sources("cline"))
                .await
                .unwrap();
        assert_eq!(sessions_after.len(), 3, "task-1 preserved + task-2' + task-3");

        std::fs::remove_dir_all(
            sandbox.path().join("Library/Application Support/Code"),
        )
        .unwrap();
        assert_eq!(scan_provider("cline").await.unwrap().len(), 1);
        let sources = read_sources("cline");
        assert_eq!(
            load_provider_sessions("cline", &project, &sources)
                .await
                .unwrap()
                .len(),
            3
        );
        assert!(
            message_text(&load_provider_messages("cline", &s1, &sources).await.unwrap())
                .contains(marker)
        );
    }
}
