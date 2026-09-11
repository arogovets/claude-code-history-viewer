//! CCHV-owned filesystem archive: append-only immutable snapshots.
//!
//! Architecture:
//! ```text
//! original local directory / remote host
//!             ↓
//! append-only local filesystem archive (this module)
//!             ↓
//! provider parsers
//!             ↓
//! SQLite/index (derived, rebuildable)
//! ```
//!
//! # Inventory that motivated this design (2026-09)
//!
//! - Local sources: every provider scans a hardcoded home path directly
//!   (`providers::claude` → `~/.claude/projects`, `providers::codex` →
//!   `~/.codex/sessions`, `providers::opencode` → SQLite `opencode.db`,
//!   `providers::*::get_base_path()`, plus custom Claude dirs via
//!   `customClaudePaths` and WSL UNC paths). No local copy existed.
//! - Custom directory handling: `scan_all_projects(custom_claude_paths)`
//!   validates with `utils::validate_custom_claude_path` then scans the
//!   original dir in place.
//! - Remote handling (`remote.rs`): fetches *parsed* projects/sessions/messages
//!   from remote CCHV HTTP endpoints (`/api/scan_all_projects`,
//!   `/api/load_provider_sessions*`, `/api/load_provider_messages*`) and stores
//!   parsed responses into SQLite as the durable offline fallback.
//! - Every `crate::cache::*` write (`save_projects`, `save_sessions`,
//!   `save_session_messages`, `merge_and_save_*`) was therefore the only copy of
//!   remote history — SQLite was durable storage, not a derived index.
//! - Provider path assumptions: `scan_projects()` reads default home paths;
//!   only some providers expose `*_from_path`/`*_in` seams. Claude project and
//!   session loaders take absolute original paths.
//! - Startup/refresh flow: `scan_all_projects` → live provider scans + remote
//!   scans → `sync_and_save_projects` (SQLite merge). No background sync.
//! - Server APIs: Axum handlers wrap the same Tauri commands; no raw-file sync
//!   endpoints existed before this module.
//!
//! # Guarantees
//!
//! - Completed snapshot directories are immutable: no function in this crate
//!   writes into one after publication.
//! - No sync path deletes historical archive files or overwrites completed
//!   snapshots. Staging failures are left in place, never promoted.
//! - Source removal means "stop syncing", never "delete history".
//! - `current.json` is only a mutable pointer to the latest completed snapshot.

pub mod cas;
pub mod coordinator;
pub mod hash;
pub mod index;
pub mod machine;
pub mod manifest;
pub mod registry;
pub mod snapshot;
pub mod source;
pub mod sqlite_capture;
pub mod sync;

pub use cas::{cas_contains, cas_insert_bytes, cas_link_into};
pub use hash::{is_plausible_hash, sha256_hex, sha256_hex_str};
pub use index::{rebuild_index_from_snapshots, reconcile_unindexed_snapshots, IndexReport};
pub use machine::{local_machine_id, wsl_machine_id};
pub use manifest::{content_hash_for, entry_fingerprint, ManifestFileEntry, SnapshotManifest};
pub use snapshot::{latest_completed_snapshot, list_snapshots, SnapshotInfo};
pub use source::{
    canonical_source_id, claim_canonical_source, resolve_source_id, Source, SourceKind,
    ROLE_PRIMARY,
};
pub use sync::{
    ensure_source_registered, find_remote_sources, find_source_for_original_path,
    is_snapshot_data_path, map_original_path_to_snapshot, map_original_to_snapshot_path,
    map_remote_path_to_snapshot, resolve_snapshot_data_root, snapshot_status_for_cli,
    sync_local_directory, sync_remote_files_to_snapshot, sync_remote_snapshot, sync_source,
    sync_status, with_source_lock, RemoteSyncedFile, SyncOptions, SyncOutcome, SyncStatus,
};
