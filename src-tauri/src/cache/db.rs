use crate::commands::session::SessionPage;
use crate::models::{ClaudeMessage, ClaudeProject, ClaudeSession, MessagePage};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
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

        CREATE TABLE IF NOT EXISTS session_subagents (
            session_path TEXT PRIMARY KEY,
            data_json TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS session_messages (
            session_path TEXT NOT NULL,
            provider TEXT NOT NULL,
            messages_json TEXT NOT NULL,
            updated_at INTEGER NOT NULL,
            PRIMARY KEY (session_path, provider)
        );

        -- Derived-index bookkeeping for the filesystem snapshot archive.
        -- One row per indexed snapshot; the archive itself is authoritative.
        -- Deleting this table (or all of cache.db) only loses derived state:
        -- `rebuild_index_from_snapshots` restores it from preserved files.
        -- `provider` + `indexer_version` let indexing logic evolve: a version
        -- bump re-indexes snapshots instead of trusting stale derivations.
        CREATE TABLE IF NOT EXISTS snapshot_index (
            source_id TEXT NOT NULL,
            snapshot_id TEXT NOT NULL,
            indexed_at INTEGER NOT NULL,
            provider TEXT,
            indexer_version INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY (source_id, snapshot_id)
        );",
    )
    .map_err(|e| format!("Failed to initialize database tables: {e}"))?;

    migrate_snapshot_index_columns(conn)?;

    Ok(())
}

/// Additive migration for pre-existing `snapshot_index` tables (f94c008 era):
/// adds `provider` / `indexer_version` without touching existing rows.
fn migrate_snapshot_index_columns(conn: &Connection) -> Result<(), String> {
    for ddl in [
        "ALTER TABLE snapshot_index ADD COLUMN provider TEXT",
        "ALTER TABLE snapshot_index ADD COLUMN indexer_version INTEGER NOT NULL DEFAULT 0",
    ] {
        match conn.execute(ddl, []) {
            Ok(_) => {}
            // Duplicate-column means an earlier run already migrated.
            Err(e) => {
                let message = e.to_string();
                if !message.contains("duplicate column name") {
                    return Err(format!("Failed to migrate snapshot_index: {message}"));
                }
            }
        }
    }
    Ok(())
}

/// Record that a snapshot's parsed content has been folded into the index.
/// Idempotent: safe to retry after a crash between snapshot finalize and DB commit.
pub fn mark_snapshot_indexed(
    conn: &Connection,
    source_id: &str,
    snapshot_id: &str,
    provider: &str,
    indexer_version: u32,
) -> Result<(), String> {
    let version = i64::from(indexer_version);
    conn.execute(
        "INSERT INTO snapshot_index (source_id, snapshot_id, indexed_at, provider, indexer_version)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(source_id, snapshot_id) DO UPDATE SET
            indexed_at = excluded.indexed_at,
            provider = excluded.provider,
            indexer_version = excluded.indexer_version",
        params![source_id, snapshot_id, now_secs(), provider, version],
    )
    .map_err(|e| format!("Failed to mark snapshot indexed: {e}"))?;
    Ok(())
}

/// Whether a snapshot has already been indexed *by the current indexer*.
/// Rows written by older indexer versions (or unknown providers) report
/// `false` so reconciliation re-derives them instead of trusting stale state.
pub fn is_snapshot_indexed(
    conn: &Connection,
    source_id: &str,
    snapshot_id: &str,
    provider: &str,
    indexer_version: u32,
) -> Result<bool, String> {
    let version = i64::from(indexer_version);
    // Legacy rows (provider NULL, version 0) never match: they are re-derived
    // under the current indexer instead of being trusted.
    let current: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM snapshot_index
              WHERE source_id = ?1 AND snapshot_id = ?2
                AND provider = ?4 AND indexer_version = ?3)",
            params![source_id, snapshot_id, version, provider],
            |row| row.get(0),
        )
        .map_err(|e| format!("Failed to check snapshot index: {e}"))?;
    Ok(current)
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
                    host_id = COALESCE(excluded.host_id, projects.host_id),
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

/// Retrieve cached projects, optionally filtered by `host_id` and/or `provider`
pub fn get_cached_projects(
    conn: &Connection,
    host_id: Option<&str>,
    provider: Option<&str>,
) -> Result<Vec<ClaudeProject>, String> {
    let mut projects = Vec::new();
    let mut sql = "SELECT data_json FROM projects WHERE 1=1".to_string();
    let mut params_vec: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

    if let Some(h_id) = host_id {
        sql.push_str(" AND (host_id = ? OR custom_directory_label = ?)");
        params_vec.push(Box::new(h_id.to_string()));
        params_vec.push(Box::new(h_id.to_string()));
    }
    if let Some(prov) = provider {
        sql.push_str(" AND provider = ?");
        params_vec.push(Box::new(prov.to_string()));
    }
    sql.push_str(" ORDER BY last_modified DESC");

    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| format!("Failed to prepare query: {e}"))?;

    let param_refs: Vec<&dyn rusqlite::types::ToSql> =
        params_vec.iter().map(AsRef::as_ref).collect();

    let rows = stmt
        .query_map(param_refs.as_slice(), |row| row.get::<_, String>(0))
        .map_err(|e| format!("Query failed: {e}"))?;

    for json_str in rows.flatten() {
        if let Ok(p) = serde_json::from_str::<ClaudeProject>(&json_str) {
            projects.push(p);
        }
    }

    Ok(projects)
}

/// Additively merges fresh projects with existing SQLite cache so projects that disappear
/// from remote or local storage are never lost; their status is preserved as unavailable.
pub fn merge_and_save_projects(
    conn: &Connection,
    fresh_projects: &[ClaudeProject],
    host_id: Option<&str>,
    provider: Option<&str>,
) -> Result<Vec<ClaudeProject>, String> {
    let existing = get_cached_projects(conn, host_id, provider)?;
    let mut fresh_map: HashMap<String, ClaudeProject> = HashMap::new();

    for p in fresh_projects {
        let canon = p.path.replace("?status=unavailable#", "#");
        fresh_map.insert(canon, p.clone());
    }

    let mut combined_map: HashMap<String, ClaudeProject> = HashMap::new();

    // 1. Fresh projects take precedence (online/fresh)
    for (canon, p) in fresh_map {
        combined_map.insert(canon, p);
    }

    // 2. Cached projects not in fresh scan are preserved as unavailable
    for mut cached in existing {
        let canon = cached.path.replace("?status=unavailable#", "#");
        if let std::collections::hash_map::Entry::Vacant(e) = combined_map.entry(canon) {
            if cached.path.starts_with("remote://") && !cached.path.contains("?status=unavailable#")
            {
                if let Some((prefix, suffix)) = cached.path.split_once('#') {
                    cached.path = format!("{prefix}?status=unavailable#{suffix}");
                }
            }
            e.insert(cached);
        }
    }

    let mut combined: Vec<ClaudeProject> = combined_map.into_values().collect();
    combined.sort_by(|a, b| b.last_modified.cmp(&a.last_modified));

    // Clear previous projects in this scope so old duplicate paths are purged
    if let (Some(h_id), Some(prov)) = (host_id, provider) {
        let _ = conn.execute(
            "DELETE FROM projects WHERE host_id = ?1 AND provider = ?2",
            rusqlite::params![h_id, prov],
        );
    } else if let Some(h_id) = host_id {
        let _ = conn.execute("DELETE FROM projects WHERE host_id = ?1", [h_id]);
    } else if let Some(prov) = provider {
        let _ = conn.execute("DELETE FROM projects WHERE provider = ?1", [prov]);
    } else {
        let _ = conn.execute("DELETE FROM projects", []);
    }

    save_projects(conn, &combined, host_id)?;
    Ok(combined)
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
            "SELECT project_path, data_json FROM sessions WHERE provider = ?1 OR provider IS NULL",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![provider], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|e| e.to_string())?;
    let mut sessions = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for row in rows {
        let (stored_path, json) = row.map_err(|e| e.to_string())?;
        if !same_history_path(&stored_path, project_path) {
            continue;
        }
        if let Ok(session) = serde_json::from_str::<ClaudeSession>(&json) {
            if seen.insert(session.actual_session_id.clone()) {
                sessions.push(session);
            }
        }
    }

    // Sort by last_message_time or last_modified desc
    sessions.sort_by(|a, b| b.last_modified.cmp(&a.last_modified));

    Ok(sessions)
}

/// Additively merges fresh sessions with existing SQLite cache so sessions that disappear
/// from remote or local storage are never lost; their status is preserved with `is_available` = false.
pub fn merge_and_save_sessions(
    conn: &Connection,
    project_path: &str,
    provider: &str,
    fresh_sessions: &[ClaudeSession],
) -> Result<Vec<ClaudeSession>, String> {
    let existing = get_cached_sessions(conn, project_path, provider)?;
    let mut fresh_map: HashMap<String, ClaudeSession> = HashMap::new();

    for s in fresh_sessions {
        let canon = s.session_id.replace("?status=unavailable#", "#");
        fresh_map.insert(canon, s.clone());
    }

    let mut combined_map: HashMap<String, ClaudeSession> = HashMap::new();

    // 1. Fresh sessions take precedence
    for (canon, s) in fresh_map {
        combined_map.insert(canon, s);
    }

    // 2. Preserved missing cached sessions
    for mut cached in existing {
        let canon = cached.session_id.replace("?status=unavailable#", "#");
        if let std::collections::hash_map::Entry::Vacant(e) = combined_map.entry(canon) {
            if cached.session_id.starts_with("remote://")
                && !cached.session_id.contains("?status=unavailable#")
            {
                if let Some((prefix, suffix)) = cached.session_id.split_once('#') {
                    cached.session_id = format!("{prefix}?status=unavailable#{suffix}");
                }
            }
            if cached.file_path.starts_with("remote://")
                && !cached.file_path.contains("?status=unavailable#")
            {
                if let Some((prefix, suffix)) = cached.file_path.split_once('#') {
                    cached.file_path = format!("{prefix}?status=unavailable#{suffix}");
                }
            }
            e.insert(cached);
        }
    }

    let mut combined: Vec<ClaudeSession> = combined_map.into_values().collect();
    combined.sort_by(|a, b| b.last_modified.cmp(&a.last_modified));

    // Clear previous sessions for this project/provider before saving
    let _ = conn.execute(
        "DELETE FROM sessions WHERE project_path = ?1 AND provider = ?2",
        rusqlite::params![project_path, provider],
    );

    save_sessions(conn, project_path, provider, &combined)?;
    Ok(combined)
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
        offline: None,
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
             WHERE s.session_id = ?1 OR s.actual_session_id = ?1 OR s.file_path = ?1
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

    // 2. Try prefix or substring matches with ranked preference
    let prefix = format!("{trimmed}%");
    let mut stmt2 = conn
        .prepare_cached(
            "SELECT s.data_json, p.data_json
             FROM sessions s
             LEFT JOIN projects p ON s.project_path = p.path
             WHERE s.session_id LIKE ?1 OR s.actual_session_id LIKE ?1 OR s.file_path LIKE ?1
             ORDER BY
                CASE
                    WHEN s.actual_session_id LIKE ?2 OR s.session_id LIKE ?2 THEN 1
                    ELSE 2
                END ASC,
                s.updated_at DESC
             LIMIT 1",
        )
        .map_err(|e| format!("Failed to prepare locate like query: {e}"))?;

    let like = stmt2
        .query_row(params![pattern, prefix], |row| {
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

/// Search sessions by `session_id`, `actual_session_id`, `file_path`, or summary
pub fn search_sessions_by_id(
    conn: &Connection,
    query: &str,
    limit: usize,
) -> Result<Vec<LocatedSession>, String> {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }

    let exact_term = trimmed;
    let prefix_term = format!("{trimmed}%");
    let contains_term = format!("%{trimmed}%");
    let max_limit = limit.clamp(1, 100);

    let mut stmt = conn
        .prepare_cached(
            "SELECT s.data_json, p.data_json,
                    CASE
                        WHEN s.actual_session_id = ?1 OR s.session_id = ?1 THEN 1
                        WHEN s.actual_session_id LIKE ?2 OR s.session_id LIKE ?2 THEN 2
                        WHEN s.actual_session_id LIKE ?3 OR s.session_id LIKE ?3 OR s.file_path LIKE ?3 THEN 3
                        ELSE 4
                    END as match_score
             FROM sessions s
             LEFT JOIN projects p ON s.project_path = p.path
             WHERE s.actual_session_id = ?1
                OR s.session_id = ?1
                OR s.actual_session_id LIKE ?2
                OR s.session_id LIKE ?2
                OR s.actual_session_id LIKE ?3
                OR s.session_id LIKE ?3
                OR s.file_path LIKE ?3
                OR s.summary LIKE ?3
             ORDER BY match_score ASC, s.updated_at DESC
             LIMIT ?4",
        )
        .map_err(|e| format!("Failed to prepare search_sessions_by_id query: {e}"))?;

    let rows = stmt
        .query_map(
            params![exact_term, prefix_term, contains_term, max_limit],
            |row| {
                let session_json: String = row.get(0)?;
                let project_json: Option<String> = row.get(1)?;
                Ok((session_json, project_json))
            },
        )
        .map_err(|e| format!("Query search_sessions_by_id error: {e}"))?;

    let mut results = Vec::new();
    for row in rows.flatten() {
        let (s_json, p_json_opt) = row;
        if let Ok(session) = serde_json::from_str::<ClaudeSession>(&s_json) {
            let project = p_json_opt
                .and_then(|pj| serde_json::from_str::<ClaudeProject>(&pj).ok())
                .unwrap_or_else(|| fallback_project_for_session(&session));
            results.push(LocatedSession { project, session });
        }
    }

    Ok(results)
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
/// Cache messages for a session
pub fn save_session_messages(
    conn: &Connection,
    session_path: &str,
    provider: &str,
    messages: &[ClaudeMessage],
) -> Result<(), String> {
    let now = now_secs();

    // Check if existing messages are cached to prevent smaller paginated slices
    // from overwriting an already cached larger/complete session
    let existing_json: Option<String> = conn
        .query_row(
            "SELECT messages_json FROM session_messages WHERE session_path = ?1 AND provider = ?2",
            params![session_path, provider],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| format!("Failed to check existing session messages: {e}"))?;

    let final_messages: Vec<ClaudeMessage> = if let Some(json_str) = existing_json {
        if let Ok(mut existing_msgs) = serde_json::from_str::<Vec<ClaudeMessage>>(&json_str) {
            for message in messages {
                if let Some(existing) = existing_msgs.iter_mut().find(|m| m.uuid == message.uuid) {
                    *existing = message.clone();
                } else {
                    existing_msgs.push(message.clone());
                }
            }
            existing_msgs.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));
            existing_msgs
        } else {
            messages.to_vec()
        }
    } else {
        messages.to_vec()
    };

    let messages_json = serde_json::to_string(&final_messages)
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

    // Match aliases only within the same host; identical paths on another
    // machine must never supply a different conversation.
    let mut stmt = conn
        .prepare_cached(
            "SELECT session_path, messages_json FROM session_messages WHERE provider = ?1",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![provider], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|e| e.to_string())?;
    for row in rows {
        let (stored_path, json) = row.map_err(|e| e.to_string())?;
        if same_history_path(&stored_path, session_path) {
            return serde_json::from_str(&json)
                .map(Some)
                .map_err(|e| e.to_string());
        }
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
    // Match the live API: offset counts backwards from the newest message,
    // while messages within each page remain chronological.
    let end = total_count.saturating_sub(offset);
    let start = end.saturating_sub(limit);
    let page_msgs = msgs[start..end].to_vec();
    let has_more = start > 0;
    let next_offset = offset.saturating_add(page_msgs.len());

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
            get_cached_projects(&conn, Some("arogovets@100.93.94.80"), None).expect("get cached");
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

    #[test]
    fn test_merge_and_save_preserves_deleted_items() {
        let conn = open_test_connection().expect("open test conn");

        let p1 = ClaudeProject {
            name: "proj1".to_string(),
            path: "remote://http://host:3728#/p1".to_string(),
            actual_path: "/home/user/p1".to_string(),
            session_count: 1,
            message_count: 10,
            last_modified: "2026-09-01T00:00:00Z".to_string(),
            git_info: None,
            provider: Some("claude".to_string()),
            storage_type: None,
            custom_directory_label: Some("remote-host".to_string()),
        };
        let p2 = ClaudeProject {
            name: "proj2".to_string(),
            path: "remote://http://host:3728#/p2".to_string(),
            actual_path: "/home/user/p2".to_string(),
            session_count: 1,
            message_count: 5,
            last_modified: "2026-09-01T00:00:00Z".to_string(),
            git_info: None,
            provider: Some("claude".to_string()),
            storage_type: None,
            custom_directory_label: Some("remote-host".to_string()),
        };

        // First scan returns p1 and p2
        merge_and_save_projects(
            &conn,
            &[p1.clone(), p2.clone()],
            Some("remote-host"),
            Some("claude"),
        )
        .expect("initial save");

        // Second scan: remote host deleted p2, only returns p1
        let merged_projects = merge_and_save_projects(
            &conn,
            std::slice::from_ref(&p1),
            Some("remote-host"),
            Some("claude"),
        )
        .expect("merged save");

        // p2 must still be preserved in our DB and returned
        assert_eq!(merged_projects.len(), 2);
        let preserved_p2 = merged_projects.iter().find(|p| p.name == "proj2").unwrap();
        assert!(preserved_p2.path.contains("?status=unavailable#"));

        // Third scan: remote host restores p2, both are online again
        let restored_projects = merge_and_save_projects(
            &conn,
            &[p1.clone(), p2.clone()],
            Some("remote-host"),
            Some("claude"),
        )
        .expect("restored save");
        assert_eq!(restored_projects.len(), 2);
        assert!(!restored_projects
            .iter()
            .any(|p| p.path.contains("?status=unavailable#")));

        // Now test sessions: session s1 and s2
        let s1 = ClaudeSession {
            session_id: "remote://http://host:3728#/p1/s1.jsonl".to_string(),
            actual_session_id: "s1-uuid".to_string(),
            file_path: "/p1/s1.jsonl".to_string(),
            project_name: "proj1".to_string(),
            message_count: 5,
            first_message_time: "2026-09-01T00:00:00Z".to_string(),
            last_message_time: "2026-09-01T01:00:00Z".to_string(),
            last_modified: "2026-09-01T01:00:00Z".to_string(),
            has_tool_use: false,
            has_errors: false,
            summary: Some("Summary 1".to_string()),
            is_renamed: false,
            provider: Some("claude".to_string()),
            storage_type: None,
            entrypoint: None,
        };
        let s2 = ClaudeSession {
            session_id: "remote://http://host:3728#/p1/s2.jsonl".to_string(),
            actual_session_id: "s2-uuid".to_string(),
            file_path: "/p1/s2.jsonl".to_string(),
            project_name: "proj1".to_string(),
            message_count: 8,
            first_message_time: "2026-09-01T00:00:00Z".to_string(),
            last_message_time: "2026-09-01T02:00:00Z".to_string(),
            last_modified: "2026-09-01T02:00:00Z".to_string(),
            has_tool_use: false,
            has_errors: false,
            summary: Some("Summary 2".to_string()),
            is_renamed: false,
            provider: Some("claude".to_string()),
            storage_type: None,
            entrypoint: None,
        };

        // First scan saves s1 and s2
        merge_and_save_sessions(&conn, &p1.path, "claude", &[s1.clone(), s2.clone()])
            .expect("initial session save");

        // Remote host deletes s2, fresh scan only returns s1
        let merged_sessions =
            merge_and_save_sessions(&conn, &p1.path, "claude", std::slice::from_ref(&s1))
                .expect("merged session save");

        // s2 must STILL be preserved in our DB
        assert_eq!(merged_sessions.len(), 2);
        let preserved_s2 = merged_sessions
            .iter()
            .find(|s| s.actual_session_id == "s2-uuid")
            .unwrap();
        assert!(preserved_s2.session_id.contains("?status=unavailable#"));

        // Serialization check: preserved_s2 serializes is_available = false
        let s2_json = serde_json::to_string(&preserved_s2).expect("serialize s2");
        assert!(s2_json.contains("\"is_available\":false"));

        // Active s1 serializes without is_available: false
        let s1_json = serde_json::to_string(&s1).expect("serialize s1");
        assert!(!s1_json.contains("\"is_available\":false"));
    }
}

/// Offline markers and configured host aliases are presentation, not identity.
fn same_history_path(left: &str, right: &str) -> bool {
    match (
        crate::remote::parse_remote_path(left),
        crate::remote::parse_remote_path(right),
    ) {
        (Some((lh, lp)), Some((rh, rp))) => {
            lp == rp
                && crate::remote::resolve_endpoint(lh).trim_end_matches('/')
                    == crate::remote::resolve_endpoint(rh).trim_end_matches('/')
        }
        (None, None) => left == right,
        _ => false,
    }
}

#[cfg(test)]
mod offline_identity_tests {
    use super::*;
    #[test]
    fn offline_marker_and_host_alias_match_without_cross_host_leaks() {
        assert!(same_history_path(
            "remote://gortamazian?status=unavailable#/repo",
            "remote://http://s-macbook-pro.tail69ac27.ts.net:3728#/repo"
        ));
        assert!(!same_history_path(
            "remote://http://host-a:3728#/repo",
            "remote://http://host-b:3728#/repo"
        ));
        assert!(!same_history_path(
            "/repo",
            "remote://http://host-a:3728#/repo"
        ));
    }
}

/// Preserve discovery results, including deleted child sessions and known empty lists.
pub fn save_session_subagents(
    conn: &Connection,
    path: &str,
    fresh: &[crate::commands::session::SubagentSession],
) -> Result<(), String> {
    let mut combined = get_cached_session_subagents(conn, path)?.unwrap_or_default();
    for sub in fresh {
        if let Some(old) = combined
            .iter_mut()
            .find(|old| same_history_path(&old.file_path, &sub.file_path))
        {
            *old = sub.clone();
        } else {
            combined.push(sub.clone());
        }
    }
    let json = serde_json::to_string(&combined).map_err(|e| e.to_string())?;
    conn.execute("INSERT INTO session_subagents VALUES (?1, ?2) ON CONFLICT(session_path) DO UPDATE SET data_json = excluded.data_json", params![path, json]).map_err(|e| e.to_string())?;
    Ok(())
}

pub fn get_cached_session_subagents(
    conn: &Connection,
    path: &str,
) -> Result<Option<Vec<crate::commands::session::SubagentSession>>, String> {
    use crate::commands::session::SubagentSession;
    let mut found = false;
    let mut result: Vec<SubagentSession> = Vec::new();
    let mut stmt = conn
        .prepare("SELECT session_path, data_json FROM session_subagents")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .map_err(|e| e.to_string())?;
    for row in rows {
        let (stored, json) = row.map_err(|e| e.to_string())?;
        if same_history_path(&stored, path) {
            found = true;
            let saved: Vec<SubagentSession> =
                serde_json::from_str(&json).map_err(|e| e.to_string())?;
            for sub in saved {
                if !result
                    .iter()
                    .any(|old| same_history_path(&old.file_path, &sub.file_path))
                {
                    result.push(sub);
                }
            }
        }
    }
    // Recover discovery from child metadata saved by older versions.
    let prefix = format!("{}/subagents/", path.trim_end_matches(".jsonl"));
    let mut stmt = conn
        .prepare("SELECT data_json FROM sessions WHERE provider = 'claude'")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(|e| e.to_string())?;
    for row in rows {
        let session: ClaudeSession =
            serde_json::from_str(&row.map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        let Some((parent, relative)) = session.file_path.split_once("/subagents/") else {
            continue;
        };
        if !same_history_path(&format!("{parent}/subagents/"), &prefix) {
            continue;
        }
        found = true;
        if result
            .iter()
            .any(|old| same_history_path(&old.file_path, &session.file_path))
        {
            continue;
        }
        let stem = relative
            .rsplit('/')
            .next()
            .unwrap_or(relative)
            .trim_end_matches(".jsonl");
        result.push(SubagentSession {
            agent_id: stem.strip_prefix("agent-").unwrap_or(stem).to_string(),
            file_path: session.file_path.clone(),
            message_count: session.message_count,
            file_size: 0,
            first_message_time: Some(session.first_message_time),
            last_message_time: Some(session.last_message_time),
            summary: session.summary,
            tool_use_id: None,
            workflow_run_id: relative
                .strip_prefix("workflows/")
                .and_then(|s| s.split('/').next())
                .map(str::to_string),
        });
    }
    result.sort_by(|a, b| a.first_message_time.cmp(&b.first_message_time));
    Ok(found.then_some(result))
}

#[cfg(test)]
mod offline_history_tests {
    use super::*;
    use crate::commands::session::SubagentSession;

    #[test]
    fn offline_pages_walk_backwards_without_overlap() {
        let conn = open_test_connection().unwrap();
        let messages: Vec<ClaudeMessage> = (0..7).map(|i| serde_json::from_value(serde_json::json!({
            "uuid": format!("m{i}"), "sessionId": "s", "timestamp": format!("2026-09-01T00:00:0{i}Z"),
            "type": "user", "content": "hello", "isSidechain": i == 5
        })).unwrap()).collect();
        save_session_messages(&conn, "/s", "claude", &messages).unwrap();
        let page = |offset, limit| {
            get_cached_session_messages_paginated(&conn, "/s", "claude", offset, limit, Some(true))
                .unwrap()
                .unwrap()
        };
        let newest = page(0, 2);
        assert_eq!(
            newest
                .messages
                .iter()
                .map(|m| m.uuid.as_str())
                .collect::<Vec<_>>(),
            vec!["m4", "m6"]
        );
        assert_eq!(newest.total_count, 6);
        let older = page(newest.next_offset, 2);
        assert_eq!(
            older
                .messages
                .iter()
                .map(|m| m.uuid.as_str())
                .collect::<Vec<_>>(),
            vec!["m2", "m3"]
        );
        let oldest = page(older.next_offset, 2);
        assert_eq!(oldest.messages[0].uuid, "m0");
        assert!(!oldest.has_more);
        assert!(page(99, usize::MAX).messages.is_empty());
        assert!(page(0, 0).messages.is_empty());
    }

    #[test]
    fn subagents_recover_legacy_child_rows_on_same_host() {
        let conn = open_test_connection().unwrap();
        for host in ["host-a", "host-b"] {
            let path = format!("remote://http://{host}:3728?status=unavailable#/s/subagents/workflows/wf-1/agent-x.jsonl");
            let session: ClaudeSession = serde_json::from_value(serde_json::json!({
                "session_id": path, "actual_session_id": "s", "file_path": path,
                "project_name": "p", "message_count": 12,
                "first_message_time": "2026-09-01", "last_message_time": "2026-09-02",
                "last_modified": "2026-09-02", "has_tool_use": true, "has_errors": false,
                "provider": "claude"
            }))
            .unwrap();
            save_sessions(&conn, "/p", "claude", &[session]).unwrap();
        }
        let recovered = get_cached_session_subagents(&conn, "remote://http://host-a:3728#/s.jsonl")
            .unwrap()
            .unwrap();
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].agent_id, "x");
        assert_eq!(recovered[0].workflow_run_id.as_deref(), Some("wf-1"));
        assert!(recovered[0].file_path.contains("host-a"));
        assert!(
            get_cached_session_subagents(&conn, "remote://http://host-c:3728#/s.jsonl")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn subagents_preserve_metadata_and_distinguish_unknown_from_empty() {
        let conn = open_test_connection().unwrap();
        let parent = "remote://http://host-a:3728#/s.jsonl";
        assert!(get_cached_session_subagents(&conn, parent)
            .unwrap()
            .is_none());
        save_session_subagents(&conn, parent, &[]).unwrap();
        assert!(get_cached_session_subagents(&conn, parent)
            .unwrap()
            .unwrap()
            .is_empty());
        let sub = SubagentSession {
            agent_id: "x".into(),
            file_path: "remote://http://host-a:3728#/s/subagents/agent-x.jsonl".into(),
            message_count: 12,
            file_size: 100,
            first_message_time: None,
            last_message_time: None,
            summary: None,
            tool_use_id: Some("tool-1".into()),
            workflow_run_id: Some("wf-1".into()),
        };
        save_session_subagents(&conn, parent, &[sub]).unwrap();
        save_session_subagents(&conn, parent, &[]).unwrap();
        let saved = get_cached_session_subagents(
            &conn,
            "remote://http://host-a:3728?status=unavailable#/s.jsonl",
        )
        .unwrap()
        .unwrap();
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].tool_use_id.as_deref(), Some("tool-1"));
        assert_eq!(saved[0].workflow_run_id.as_deref(), Some("wf-1"));
        assert!(
            get_cached_session_subagents(&conn, "remote://http://host-b:3728#/s.jsonl")
                .unwrap()
                .is_none()
        );
    }
}
