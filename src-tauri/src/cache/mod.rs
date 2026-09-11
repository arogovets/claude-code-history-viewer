//! Derived index over the filesystem snapshot archive (plus legacy cache).
//!
//! SQLite here is **not** durable history storage: it is a rebuildable index
//! over CCHV-owned snapshots in `~/.claude-history-viewer/data/sources/` (see
//! `crate::storage`). `rm cache.db` must never lose conversations —
//! [`crate::storage::rebuild_index_from_snapshots`] restores the index from
//! preserved files.
//!
//! Migration is additive: pre-existing tables/rows are untouched, the new
//! `snapshot_index` table only records which snapshots have been folded in,
//! and every legacy parsed-response caching path (`sync_and_save_*`,
//! offline fallbacks) is retained until snapshot coverage provably subsumes
//! it. A remote host that is offline with history only in SQLite keeps
//! serving that history through the unchanged fallbacks.

pub mod db;

pub use db::{
    get_cache_db_path, get_cached_projects, get_cached_session_messages,
    get_cached_session_messages_paginated, get_cached_sessions, get_cached_sessions_page,
    locate_session, merge_and_save_projects, merge_and_save_sessions, open_connection,
    save_projects, save_session_messages, save_sessions, search_sessions_by_id, LocatedSession,
};

/// High-level helper to cache projects
pub fn cache_projects(projects: &[crate::models::ClaudeProject], host_id: Option<&str>) {
    if let Ok(conn) = open_connection() {
        if let Err(e) = save_projects(&conn, projects, host_id) {
            log::warn!("Failed to cache projects in SQLite: {e}");
        }
    }
}

/// High-level helper to additively sync projects with cache without dropping deleted ones
pub fn sync_and_save_projects(
    fresh_projects: &[crate::models::ClaudeProject],
    host_id: Option<&str>,
    provider: Option<&str>,
) -> Vec<crate::models::ClaudeProject> {
    if let Ok(conn) = open_connection() {
        match merge_and_save_projects(&conn, fresh_projects, host_id, provider) {
            Ok(combined) => return combined,
            Err(e) => log::warn!("Failed to merge projects in SQLite: {e}"),
        }
    }
    fresh_projects.to_vec()
}

/// High-level helper to load cached projects
pub fn load_projects(
    host_id: Option<&str>,
    provider: Option<&str>,
) -> Vec<crate::models::ClaudeProject> {
    if let Ok(conn) = open_connection() {
        get_cached_projects(&conn, host_id, provider).unwrap_or_default()
    } else {
        Vec::new()
    }
}

/// High-level helper to cache sessions
pub fn cache_sessions(
    project_path: &str,
    provider: &str,
    sessions: &[crate::models::ClaudeSession],
) {
    if let Ok(conn) = open_connection() {
        if let Err(e) = save_sessions(&conn, project_path, provider, sessions) {
            log::warn!("Failed to cache sessions in SQLite: {e}");
        }
    }
}

/// High-level helper to additively sync sessions with cache so deleted sessions stay preserved as unavailable
pub fn sync_and_save_sessions(
    project_path: &str,
    provider: &str,
    fresh_sessions: &[crate::models::ClaudeSession],
) -> Vec<crate::models::ClaudeSession> {
    if let Ok(conn) = open_connection() {
        match merge_and_save_sessions(&conn, project_path, provider, fresh_sessions) {
            Ok(combined) => return combined,
            Err(e) => log::warn!("Failed to merge sessions in SQLite: {e}"),
        }
    }
    fresh_sessions.to_vec()
}

/// High-level helper to load cached sessions
pub fn load_sessions(project_path: &str, provider: &str) -> Vec<crate::models::ClaudeSession> {
    if let Ok(conn) = open_connection() {
        get_cached_sessions(&conn, project_path, provider).unwrap_or_default()
    } else {
        Vec::new()
    }
}

/// High-level helper to cache session messages
pub fn cache_messages(
    session_path: &str,
    provider: &str,
    messages: &[crate::models::ClaudeMessage],
) {
    if let Ok(conn) = open_connection() {
        if let Err(e) = save_session_messages(&conn, session_path, provider, messages) {
            log::warn!("Failed to cache messages in SQLite: {e}");
        }
    }
}

/// High-level helper to locate a session
pub fn locate(session_id: &str) -> Option<LocatedSession> {
    if let Ok(conn) = open_connection() {
        locate_session(&conn, session_id).ok().flatten()
    } else {
        None
    }
}

/// High-level helper to search sessions by id, prefix, or path
pub fn search_sessions(query: &str, limit: usize) -> Vec<LocatedSession> {
    if let Ok(conn) = open_connection() {
        search_sessions_by_id(&conn, query, limit).unwrap_or_default()
    } else {
        Vec::new()
    }
}
