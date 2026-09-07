pub mod db;

pub use db::{
    get_cache_db_path, get_cached_projects, get_cached_session_messages,
    get_cached_session_messages_paginated, get_cached_sessions, get_cached_sessions_page,
    locate_session, open_connection, save_projects, save_session_messages, save_sessions,
    LocatedSession,
};

/// High-level helper to cache projects
pub fn cache_projects(projects: &[crate::models::ClaudeProject], host_id: Option<&str>) {
    if let Ok(conn) = open_connection() {
        if let Err(e) = save_projects(&conn, projects, host_id) {
            log::warn!("Failed to cache projects in SQLite: {e}");
        }
    }
}

/// High-level helper to load cached projects
pub fn load_projects(host_id: Option<&str>) -> Vec<crate::models::ClaudeProject> {
    if let Ok(conn) = open_connection() {
        get_cached_projects(&conn, host_id).unwrap_or_default()
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
