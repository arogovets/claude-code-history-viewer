use crate::commands::session::SessionPage;
use crate::models::{ClaudeMessage, ClaudeProject, ClaudeSession, MessagePage};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocatedSession {
    pub project: ClaudeProject,
    pub session: ClaudeSession,
}

#[allow(clippy::cast_possible_wrap)]
fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

pub fn get_cache_db_path() -> Result<PathBuf, String> {
    #[cfg(test)]
    if std::env::var_os("CCHV_TEST_HOME").is_none() {
        return Err("No CCHV_TEST_HOME set in test environment".to_string());
    }

    let home =
        crate::utils::home_dir().ok_or_else(|| "Could not determine home directory".to_string())?;
    let dir = home.join(".claude-history-viewer");
    if !dir.exists() {
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("Failed to create cache directory: {e}"))?;
    }
    Ok(dir.join("cache.db"))
}

pub fn open_connection() -> Result<Connection, String> {
    let db_path = get_cache_db_path()?;
    let conn = Connection::open(&db_path).map_err(|e| {
        format!(
            "Failed to open cache database at {}: {e}",
            db_path.display()
        )
    })?;

    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         PRAGMA busy_timeout = 5000;",
    )
    .map_err(|e| format!("Failed to set SQLite PRAGMAs: {e}"))?;

    init_tables(&conn)?;
    Ok(conn)
}

#[cfg(test)]
pub fn open_test_connection() -> Result<Connection, String> {
    let conn = Connection::open_in_memory().map_err(|e| e.to_string())?;
    init_tables(&conn)?;
    Ok(conn)
}

pub fn init_tables(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS projects (
            path TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            actual_path TEXT NOT NULL,
            session_count INTEGER NOT NULL DEFAULT 0,
            message_count INTEGER NOT NULL DEFAULT 0,
            last_modified TEXT NOT NULL,
            provider TEXT,
            storage_type TEXT,
            custom_directory_label TEXT,
            host_id TEXT,
            data_json TEXT NOT NULL,
            updated_at INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_projects_host_id ON projects(host_id);
        CREATE INDEX IF NOT EXISTS idx_projects_provider ON projects(provider);

        CREATE TABLE IF NOT EXISTS sessions (
            session_id TEXT PRIMARY KEY,
            actual_session_id TEXT NOT NULL,
            project_path TEXT NOT NULL,
            provider TEXT,
            file_path TEXT NOT NULL,
            summary TEXT,
            data_json TEXT NOT NULL,
            updated_at INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_sessions_actual_id ON sessions(actual_session_id);
        CREATE INDEX IF NOT EXISTS idx_sessions_project_path ON sessions(project_path);
        CREATE INDEX IF NOT EXISTS idx_sessions_file_path ON sessions(file_path);

        CREATE TABLE IF NOT EXISTS session_messages (
            session_path TEXT NOT NULL,
            provider TEXT NOT NULL,
            messages_json TEXT NOT NULL,
            updated_at INTEGER NOT NULL,
            PRIMARY KEY (session_path, provider)
        );",
    )
    .map_err(|e| format!("Failed to initialize database tables: {e}"))?;

    Ok(())
}

/// Upsert projects into the local SQLite cache
#[allow(clippy::cast_possible_wrap)]
pub fn save_projects(
    conn: &Connection,
    projects: &[ClaudeProject],
    host_id: Option<&str>,
) -> Result<(), String> {
    let now = now_secs();
    let tx = conn
        .unchecked_transaction()
        .map_err(|e| format!("Failed to start transaction: {e}"))?;

    {
        let mut stmt = tx
            .prepare_cached(
                "INSERT INTO projects (
                    path, name, actual_path, session_count, message_count,
                    last_modified, provider, storage_type, custom_directory_label,
                    host_id, data_json, updated_at
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
                ON CONFLICT(path) DO UPDATE SET
                    name = excluded.name,
                    actual_path = excluded.actual_path,
                    session_count = excluded.session_count,
                    message_count = excluded.message_count,
                    last_modified = excluded.last_modified,
                    provider = excluded.provider,
                    storage_type = excluded.storage_type,
                    custom_directory_label = excluded.custom_directory_label,
                    host_id = excluded.host_id,
                    data_json = excluded.data_json,
                    updated_at = excluded.updated_at;",
            )
            .map_err(|e| format!("Failed to prepare project insert: {e}"))?;

        for p in projects {
            let data_json = serde_json::to_string(p)
                .map_err(|e| format!("Failed to serialize project: {e}"))?;

            stmt.execute(params![
                p.path,
                p.name,
                p.actual_path,
                p.session_count as i64,
                p.message_count as i64,
                p.last_modified,
                p.provider.as_deref(),
                p.storage_type.as_deref(),
                p.custom_directory_label.as_deref(),
                host_id,
                data_json,
                now
            ])
            .map_err(|e| format!("Failed to execute project insert: {e}"))?;
        }
    }

    tx.commit()
        .map_err(|e| format!("Failed to commit project transaction: {e}"))?;

    Ok(())
}

/// Retrieve cached projects, optionally filtered by `host_id`
pub fn get_cached_projects(
    conn: &Connection,
    host_id: Option<&str>,
) -> Result<Vec<ClaudeProject>, String> {
    let mut projects = Vec::new();

    if let Some(h_id) = host_id {
        let mut stmt = conn
            .prepare_cached(
                "SELECT data_json FROM projects WHERE host_id = ?1 ORDER BY last_modified DESC",
            )
            .map_err(|e| format!("Failed to prepare query: {e}"))?;
        let rows = stmt
            .query_map([h_id], |row| row.get::<_, String>(0))
            .map_err(|e| format!("Query failed: {e}"))?;

        for json_str in rows.flatten() {
            if let Ok(p) = serde_json::from_str::<ClaudeProject>(&json_str) {
                projects.push(p);
            }
        }
    } else {
        let mut stmt = conn
            .prepare_cached("SELECT data_json FROM projects ORDER BY last_modified DESC")
            .map_err(|e| format!("Failed to prepare query: {e}"))?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|e| format!("Query failed: {e}"))?;

        for json_str in rows.flatten() {
            if let Ok(p) = serde_json::from_str::<ClaudeProject>(&json_str) {
                projects.push(p);
            }
        }
    }

    Ok(projects)
}

/// Upsert sessions into the local SQLite cache for a project
pub fn save_sessions(
    conn: &Connection,
    project_path: &str,
    provider: &str,
    sessions: &[ClaudeSession],
) -> Result<(), String> {
    let now = now_secs();
    let tx = conn
        .unchecked_transaction()
        .map_err(|e| format!("Failed to start transaction: {e}"))?;

    {
        let mut stmt = tx
            .prepare_cached(
                "INSERT INTO sessions (
                    session_id, actual_session_id, project_path, provider,
                    file_path, summary, data_json, updated_at
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                ON CONFLICT(session_id) DO UPDATE SET
                    actual_session_id = excluded.actual_session_id,
                    project_path = excluded.project_path,
                    provider = excluded.provider,
                    file_path = excluded.file_path,
                    summary = excluded.summary,
                    data_json = excluded.data_json,
                    updated_at = excluded.updated_at;",
            )
            .map_err(|e| format!("Failed to prepare session insert: {e}"))?;

        for s in sessions {
            let data_json = serde_json::to_string(s)
                .map_err(|e| format!("Failed to serialize session: {e}"))?;

            stmt.execute(params![
                s.session_id,
                s.actual_session_id,
                project_path,
                provider,
                s.file_path,
                s.summary.as_deref(),
                data_json,
                now
            ])
            .map_err(|e| format!("Failed to execute session insert: {e}"))?;
        }
    }

    tx.commit()
        .map_err(|e| format!("Failed to commit session transaction: {e}"))?;

    Ok(())
}

/// Retrieve cached sessions for a given project path and provider
pub fn get_cached_sessions(
    conn: &Connection,
    project_path: &str,
    provider: &str,
) -> Result<Vec<ClaudeSession>, String> {
    let mut stmt = conn
        .prepare_cached(
            "SELECT data_json FROM sessions 
             WHERE project_path = ?1 AND (provider = ?2 OR provider IS NULL)
             ORDER BY updated_at DESC",
        )
        .map_err(|e| format!("Failed to prepare query: {e}"))?;

    let rows = stmt
        .query_map(params![project_path, provider], |row| {
            row.get::<_, String>(0)
        })
        .map_err(|e| format!("Query failed: {e}"))?;

    let mut sessions = Vec::new();
    for json_str in rows.flatten() {
        if let Ok(s) = serde_json::from_str::<ClaudeSession>(&json_str) {
            sessions.push(s);
        }
    }

    // Sort by last_message_time or last_modified desc
    sessions.sort_by(|a, b| b.last_modified.cmp(&a.last_modified));

    Ok(sessions)
}

/// Retrieve a paginated list of cached sessions
pub fn get_cached_sessions_page(
    conn: &Connection,
    project_path: &str,
    provider: &str,
    offset: usize,
    limit: usize,
) -> Result<SessionPage, String> {
    let all = get_cached_sessions(conn, project_path, provider)?;
    let total = all.len();
    let end = (offset + limit).min(total);
    let page_sessions = if offset < total {
        all[offset..end].to_vec()
    } else {
        Vec::new()
    };
    let has_more = end < total;
    let next_offset = end;

    Ok(SessionPage {
        sessions: page_sessions,
        total,
        offset,
        limit,
        next_offset,
        has_more,
    })
}

/// Instantly locate a session across all projects and remote hosts
pub fn locate_session(
    conn: &Connection,
    session_id_or_prefix: &str,
) -> Result<Option<LocatedSession>, String> {
    let trimmed = session_id_or_prefix.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }

    let pattern = format!("%{trimmed}%");

    // 1. Try exact matches first
    let mut stmt = conn
        .prepare_cached(
            "SELECT s.data_json, p.data_json
             FROM sessions s
             LEFT JOIN projects p ON s.project_path = p.path
             WHERE s.session_id = ?1 OR s.actual_session_id = ?1
             LIMIT 1",
        )
        .map_err(|e| format!("Failed to prepare locate exact query: {e}"))?;

    let exact = stmt
        .query_row(params![trimmed], |row| {
            let session_json: String = row.get(0)?;
            let project_json: Option<String> = row.get(1)?;
            Ok((session_json, project_json))
        })
        .optional()
        .map_err(|e| format!("Query exact locate error: {e}"))?;

    if let Some((s_json, p_json_opt)) = exact {
        if let Ok(session) = serde_json::from_str::<ClaudeSession>(&s_json) {
            let project = if let Some(p_json) = p_json_opt {
                serde_json::from_str::<ClaudeProject>(&p_json).ok()
            } else {
                None
            };
            let project = project.unwrap_or_else(|| fallback_project_for_session(&session));
            return Ok(Some(LocatedSession { project, session }));
        }
    }

    // 2. Try prefix or substring matches
    let mut stmt2 = conn
        .prepare_cached(
            "SELECT s.data_json, p.data_json
             FROM sessions s
             LEFT JOIN projects p ON s.project_path = p.path
             WHERE s.session_id LIKE ?1 OR s.actual_session_id LIKE ?1 OR s.file_path LIKE ?1
             LIMIT 1",
        )
        .map_err(|e| format!("Failed to prepare locate like query: {e}"))?;

    let like = stmt2
        .query_row(params![pattern], |row| {
            let session_json: String = row.get(0)?;
            let project_json: Option<String> = row.get(1)?;
            Ok((session_json, project_json))
        })
        .optional()
        .map_err(|e| format!("Query like locate error: {e}"))?;

    if let Some((s_json, p_json_opt)) = like {
        if let Ok(session) = serde_json::from_str::<ClaudeSession>(&s_json) {
            let project = if let Some(p_json) = p_json_opt {
                serde_json::from_str::<ClaudeProject>(&p_json).ok()
            } else {
                None
            };
            let project = project.unwrap_or_else(|| fallback_project_for_session(&session));
            return Ok(Some(LocatedSession { project, session }));
        }
    }

    Ok(None)
}

fn fallback_project_for_session(session: &ClaudeSession) -> ClaudeProject {
    let name = if session.project_name.is_empty() {
        "Unknown Project".to_string()
    } else {
        session.project_name.clone()
    };
    ClaudeProject {
        name,
        path: session.file_path.clone(),
        actual_path: session.file_path.clone(),
        session_count: 1,
        message_count: session.message_count,
        last_modified: session.last_modified.clone(),
        git_info: None,
        provider: session.provider.clone(),
        storage_type: session.storage_type.clone(),
        custom_directory_label: None,
    }
}

/// Cache messages for a session
pub fn save_session_messages(
    conn: &Connection,
    session_path: &str,
    provider: &str,
    messages: &[ClaudeMessage],
) -> Result<(), String> {
    let now = now_secs();
    let messages_json = serde_json::to_string(messages)
        .map_err(|e| format!("Failed to serialize messages: {e}"))?;

    conn.execute(
        "INSERT INTO session_messages (session_path, provider, messages_json, updated_at)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(session_path, provider) DO UPDATE SET
            messages_json = excluded.messages_json,
            updated_at = excluded.updated_at;",
        params![session_path, provider, messages_json, now],
    )
    .map_err(|e| format!("Failed to save session messages: {e}"))?;

    Ok(())
}

/// Retrieve cached messages for a session
pub fn get_cached_session_messages(
    conn: &Connection,
    session_path: &str,
    provider: &str,
) -> Result<Option<Vec<ClaudeMessage>>, String> {
    let mut stmt = conn
        .prepare_cached(
            "SELECT messages_json FROM session_messages 
             WHERE session_path = ?1 AND provider = ?2",
        )
        .map_err(|e| format!("Failed to prepare query: {e}"))?;

    let result = stmt
        .query_row(params![session_path, provider], |row| {
            row.get::<_, String>(0)
        })
        .optional()
        .map_err(|e| format!("Failed to query cached messages: {e}"))?;

    if let Some(json_str) = result {
        let messages = serde_json::from_str::<Vec<ClaudeMessage>>(&json_str)
            .map_err(|e| format!("Failed to deserialize cached messages: {e}"))?;
        return Ok(Some(messages));
    }

    // Fallback for remote paths that may differ by host/endpoint prefix
    let inner_suffix = session_path
        .split_once('#')
        .map(|(_, inner)| inner)
        .unwrap_or(session_path);
    let pattern = format!("%#{inner_suffix}");

    let mut stmt2 = conn
        .prepare_cached(
            "SELECT messages_json FROM session_messages 
             WHERE (session_path LIKE ?1 OR session_path = ?2) AND provider = ?3
             LIMIT 1",
        )
        .map_err(|e| format!("Failed to prepare fallback query: {e}"))?;

    let result2 = stmt2
        .query_row(params![pattern, inner_suffix, provider], |row| {
            row.get::<_, String>(0)
        })
        .optional()
        .map_err(|e| format!("Failed to query cached messages fallback: {e}"))?;

    if let Some(json_str) = result2 {
        let messages = serde_json::from_str::<Vec<ClaudeMessage>>(&json_str)
            .map_err(|e| format!("Failed to deserialize cached messages: {e}"))?;
        return Ok(Some(messages));
    }

    Ok(None)
}

/// Retrieve cached messages as a paginated `MessagePage`
pub fn get_cached_session_messages_paginated(
    conn: &Connection,
    session_path: &str,
    provider: &str,
    offset: usize,
    limit: usize,
    exclude_sidechain: Option<bool>,
) -> Result<Option<MessagePage>, String> {
    let cached = get_cached_session_messages(conn, session_path, provider)?;
    let Some(mut msgs) = cached else {
        return Ok(None);
    };

    if exclude_sidechain.unwrap_or(false) {
        msgs.retain(|m| !m.is_sidechain.unwrap_or(false));
    }

    let total_count = msgs.len();
    let end = (offset + limit).min(total_count);
    let page_msgs = if offset < total_count {
        msgs[offset..end].to_vec()
    } else {
        Vec::new()
    };
    let has_more = end < total_count;
    let next_offset = end;

    Ok(Some(MessagePage {
        messages: page_msgs,
        total_count,
        has_more,
        next_offset,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sqlite_cache_roundtrip() {
        let conn = open_test_connection().expect("open test conn");

        let project = ClaudeProject {
            name: "test-proj".to_string(),
            path: "remote://http://100.93.94.80:3728#/projects/p1".to_string(),
            actual_path: "/home/user/p1".to_string(),
            session_count: 2,
            message_count: 50,
            last_modified: "2026-09-01T00:00:00Z".to_string(),
            git_info: None,
            provider: Some("codex".to_string()),
            storage_type: None,
            custom_directory_label: Some("arogovets@100.93.94.80".to_string()),
        };

        save_projects(
            &conn,
            std::slice::from_ref(&project),
            Some("arogovets@100.93.94.80"),
        )
        .expect("save projects");

        let cached_projects =
            get_cached_projects(&conn, Some("arogovets@100.93.94.80")).expect("get cached");
        assert_eq!(cached_projects.len(), 1);
        assert_eq!(cached_projects[0].name, "test-proj");

        let session = ClaudeSession {
            session_id: "remote://http://100.93.94.80:3728#/sessions/s1".to_string(),
            actual_session_id: "01a07b5a-3433-7d43-9b98-d1936258a02e".to_string(),
            file_path: "/home/user/p1/s1.jsonl".to_string(),
            project_name: "test-proj".to_string(),
            message_count: 10,
            first_message_time: "2026-09-01T00:00:00Z".to_string(),
            last_message_time: "2026-09-01T01:00:00Z".to_string(),
            last_modified: "2026-09-01T01:00:00Z".to_string(),
            has_tool_use: false,
            has_errors: false,
            summary: Some("Eligible session summary".to_string()),
            is_renamed: false,
            provider: Some("codex".to_string()),
            storage_type: None,
            entrypoint: None,
        };

        save_sessions(
            &conn,
            &project.path,
            "codex",
            std::slice::from_ref(&session),
        )
        .expect("save sessions");

        let cached_sessions =
            get_cached_sessions(&conn, &project.path, "codex").expect("get sessions");
        assert_eq!(cached_sessions.len(), 1);
        assert_eq!(
            cached_sessions[0].actual_session_id,
            "01a07b5a-3433-7d43-9b98-d1936258a02e"
        );

        // Test locate_session
        let located = locate_session(&conn, "01a07b5a-3433-7d43-9b98-d1936258a02e")
            .expect("locate session")
            .expect("should find session");
        assert_eq!(located.session.session_id, session.session_id);
        assert_eq!(located.project.path, project.path);

        // Test locate by prefix
        let located_prefix = locate_session(&conn, "01a07b5a")
            .expect("locate prefix")
            .expect("should find session by prefix");
        assert_eq!(located_prefix.session.session_id, session.session_id);
    }
}
