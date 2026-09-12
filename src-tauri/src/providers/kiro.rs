use crate::models::{ClaudeMessage, ClaudeProject, ClaudeSession};
use crate::providers::q_conversation;
use crate::providers::ProviderInfo;
use crate::utils::{ms_to_iso, search_json_value_case_insensitive};
use rusqlite::{Connection, OpenFlags};
use serde_json::Value;
use std::path::PathBuf;

const PROVIDER: &str = "kiro";

/// Detect Kiro CLI installation
pub fn detect() -> Option<ProviderInfo> {
    let db = get_db_path()?;
    Some(ProviderInfo {
        id: PROVIDER.to_string(),
        display_name: "Kiro CLI".to_string(),
        base_path: db.parent()?.to_string_lossy().to_string(),
        is_available: db.is_file(),
    })
}

fn get_db_path() -> Option<PathBuf> {
    // data_local_dir(): macOS ~/Library/Application Support, Linux ~/.local/share,
    // Windows %LOCALAPPDATA% — matches upstream kiro-cli (dirs::data_local_dir).
    // (Previously hardcoded Windows to AppData/Roaming, which was wrong.)
    Some(
        dirs::data_local_dir()?
            .join("kiro-cli")
            .join("data.sqlite3"),
    )
}

/// Open a Kiro database at an explicit path (snapshot copy or live file),
/// read-only.
fn open_db_at(path: &std::path::Path) -> Result<Connection, String> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| format!("Failed to open Kiro DB: {e}"))?;
    conn.busy_timeout(std::time::Duration::from_secs(5))
        .map_err(|e| format!("Failed to set busy timeout: {e}"))?;
    Ok(conn)
}

/// Scan Kiro projects (grouped by cwd/key)
pub fn scan_projects() -> Result<Vec<ClaudeProject>, String> {
    let db = get_db_path().ok_or("Kiro CLI not found")?;
    scan_projects_in(&db)
}

/// [`scan_projects`] against an explicit database path (snapshot or live).
pub fn scan_projects_in(db_path: &std::path::Path) -> Result<Vec<ClaudeProject>, String> {
    let conn = open_db_at(db_path)?;
    let mut stmt = conn
        .prepare(
            "SELECT key, COUNT(*) as cnt, MAX(updated_at) as last_upd
             FROM conversations_v2 GROUP BY key ORDER BY last_upd DESC",
        )
        .map_err(|e| e.to_string())?;

    let projects = stmt
        .query_map([], |row| {
            let key: String = row.get(0)?;
            let count: usize = row.get(1)?;
            let updated: u64 = row.get(2)?;
            Ok((key, count, updated))
        })
        .map_err(|e| e.to_string())?
        .flatten()
        .map(|(key, count, updated)| {
            let name = PathBuf::from(&key)
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            ClaudeProject {
                name,
                path: format!("kiro://{key}"),
                actual_path: key,
                session_count: count,
                message_count: 0,
                last_modified: ms_to_iso(updated),
                git_info: None,
                provider: Some(PROVIDER.to_string()),
                storage_type: Some("sqlite".to_string()),
                custom_directory_label: None,
            }
        })
        .collect();

    Ok(projects)
}

/// Load sessions for a Kiro project
pub fn load_sessions(
    project_path: &str,
    exclude_sidechain: bool,
) -> Result<Vec<ClaudeSession>, String> {
    let db = get_db_path().ok_or("Kiro CLI not found")?;
    load_sessions_in(&db, project_path, exclude_sidechain)
}

/// [`load_sessions`] against an explicit database path (snapshot or live).
pub fn load_sessions_in(
    db_path: &std::path::Path,
    project_path: &str,
    _exclude_sidechain: bool,
) -> Result<Vec<ClaudeSession>, String> {
    let key = project_path.strip_prefix("kiro://").unwrap_or(project_path);
    let conn = open_db_at(db_path)?;

    let mut stmt = conn
        .prepare(
            "SELECT conversation_id, value, created_at, updated_at
             FROM conversations_v2 WHERE key = ?1 ORDER BY updated_at DESC",
        )
        .map_err(|e| e.to_string())?;

    let project_name = PathBuf::from(key)
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    let sessions = stmt
        .query_map([key], |row| {
            let conv_id: String = row.get(0)?;
            let value: String = row.get(1)?;
            let created: u64 = row.get(2)?;
            let updated: u64 = row.get(3)?;
            Ok((conv_id, value, created, updated))
        })
        .map_err(|e| e.to_string())?
        .flatten()
        .filter_map(|(conv_id, value, created, updated)| {
            let json: Value = serde_json::from_str(&value).ok()?;
            let history = json.get("history")?.as_array()?;
            let msg_count = history.len();

            // Extract summary from first user prompt
            let summary = history.first().and_then(|h| {
                h.get("user")?
                    .get("content")?
                    .get("Prompt")?
                    .get("prompt")?
                    .as_str()
                    .map(|s| s.chars().take(100).collect::<String>())
            });

            let has_tool_use = history
                .iter()
                .any(|h| h.get("assistant").and_then(|a| a.get("ToolUse")).is_some());

            Some(ClaudeSession {
                session_id: format!("kiro://{conv_id}"),
                actual_session_id: conv_id.clone(),
                // Must be the conversation id (not the project key) — load_messages
                // strips `kiro://` and queries `WHERE conversation_id = ?` (#324).
                file_path: format!("kiro://{conv_id}"),
                project_name: project_name.clone(),
                message_count: msg_count,
                first_message_time: ms_to_iso(created),
                last_message_time: ms_to_iso(updated),
                last_modified: ms_to_iso(updated),
                has_tool_use,
                has_errors: false,
                summary,
                is_renamed: false,
                provider: Some(PROVIDER.to_string()),
                storage_type: Some("sqlite".to_string()),
                entrypoint: None,
            })
        })
        .collect();

    Ok(sessions)
}

/// Load messages from a Kiro conversation
pub fn load_messages(session_path: &str) -> Result<Vec<ClaudeMessage>, String> {
    let db = get_db_path().ok_or("Kiro CLI not found")?;
    load_messages_in(&db, session_path)
}

/// [`load_messages`] against an explicit database path (snapshot or live).
pub fn load_messages_in(
    db_path: &std::path::Path,
    session_path: &str,
) -> Result<Vec<ClaudeMessage>, String> {
    let conv_id = session_path.strip_prefix("kiro://").unwrap_or(session_path);
    let conn = open_db_at(db_path)?;

    let value: String = conn
        .query_row(
            "SELECT value FROM conversations_v2 WHERE conversation_id = ?1",
            [conv_id],
            |row| row.get(0),
        )
        .map_err(|e| format!("Conversation not found: {e}"))?;

    Ok(q_conversation::parse_history(PROVIDER, &value, conv_id))
}

// ============================================================================
// Archive glue (snapshot-backed reads; the explicit-db seams above are reused
// against the snapshotted database copy, never the live one).
// ============================================================================

use crate::storage::registry::DiscoveredSource as ArchiveDiscoveredSource;
use crate::storage::{SnapshotInfo as ArchiveSnapshotInfo, Source as ArchiveSource};

/// Physical Kiro dir on this machine, if the database exists.
pub(crate) fn archive_discover() -> Vec<ArchiveDiscoveredSource> {
    let machine = crate::storage::registry::discovery_machine_id();
    match get_db_path() {
        Some(db) => match db.parent().and_then(|base| {
            db.file_name()
                .and_then(|n| n.to_str())
                .map(|file_name| (base.to_path_buf(), file_name.to_string()))
        }) {
            Some((base, file_name)) => {
                let mut found =
                    ArchiveDiscoveredSource::local(crate::storage::ROLE_PRIMARY, base, &machine);
                found.sqlite_dbs = vec![file_name];
                vec![found]
            }
            None => Vec::new(),
        },
        None => Vec::new(),
    }
}

/// Snapshot database path for a source (filename from the source record).
fn archive_db_path(
    source: &ArchiveSource,
    snapshot: &ArchiveSnapshotInfo,
) -> Result<std::path::PathBuf, String> {
    let file_name = source
        .sqlite_dbs
        .first()
        .ok_or("Kiro source has no captured database")?;
    Ok(snapshot.data_path.join(file_name))
}

/// Scan projects from the snapshot database.
pub(crate) fn archive_scan(
    source: &ArchiveSource,
    snapshot: &ArchiveSnapshotInfo,
) -> Result<Vec<ClaudeProject>, String> {
    scan_projects_in(&archive_db_path(source, snapshot)?)
}

/// Sessions for a stable project URI, read from the snapshot database.
pub(crate) fn archive_load_sessions(
    source: &ArchiveSource,
    snapshot: &ArchiveSnapshotInfo,
    stable_project: &str,
) -> Result<Vec<ClaudeSession>, String> {
    load_sessions_in(&archive_db_path(source, snapshot)?, stable_project, false)
}

/// Messages for a stable conversation URI, read from the snapshot database.
pub(crate) fn archive_load_messages(
    source: &ArchiveSource,
    snapshot: &ArchiveSnapshotInfo,
    stable_session: &str,
) -> Result<Vec<ClaudeMessage>, String> {
    load_messages_in(&archive_db_path(source, snapshot)?, stable_session)
}

/// Search confined to one snapshot database.
pub(crate) fn archive_search(
    source: &ArchiveSource,
    snapshot: &ArchiveSnapshotInfo,
    query: &str,
    limit: usize,
) -> Result<Vec<ClaudeMessage>, String> {
    search_in(&archive_db_path(source, snapshot)?, query, limit)
}

/// Search across all Kiro conversations
pub fn search(query: &str, limit: usize) -> Result<Vec<ClaudeMessage>, String> {
    if query.is_empty() || limit == 0 {
        return Ok(Vec::new());
    }

    let db = get_db_path().ok_or("Kiro CLI not found")?;
    search_in(&db, query, limit)
}

/// [`search`] against an explicit database path (snapshot or live).
pub fn search_in(
    db_path: &std::path::Path,
    query: &str,
    limit: usize,
) -> Result<Vec<ClaudeMessage>, String> {
    if query.is_empty() || limit == 0 {
        return Ok(Vec::new());
    }
    let conn = open_db_at(db_path)?;
    let pattern = format!("%{query}%");
    let query_lower = query.to_lowercase();

    let mut stmt = conn
        .prepare("SELECT key, conversation_id, value FROM conversations_v2 WHERE value LIKE ?1")
        .map_err(|e| e.to_string())?;

    let mut results = Vec::new();

    let rows = stmt
        .query_map([&pattern], |row| {
            let key: String = row.get(0)?;
            let conv_id: String = row.get(1)?;
            let value: String = row.get(2)?;
            Ok((key, conv_id, value))
        })
        .map_err(|e| e.to_string())?;

    for row in rows.flatten() {
        let (_key, conv_id, value) = row;
        let json: Value = match serde_json::from_str(&value) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let history = match json.get("history").and_then(Value::as_array) {
            Some(h) => h,
            None => continue,
        };

        for (i, entry) in history.iter().enumerate() {
            if results.len() >= limit {
                return Ok(results);
            }
            if let Some(user) = entry.get("user") {
                if let Some(mut msg) =
                    q_conversation::convert_user_message(PROVIDER, user, &conv_id, i)
                {
                    if let Some(ref c) = msg.content {
                        if search_json_value_case_insensitive(c, &query_lower) {
                            msg.project_name = Some("Kiro CLI".to_string());
                            results.push(msg);
                        }
                    }
                }
            }
            if results.len() >= limit {
                return Ok(results);
            }
            if let Some(assistant) = entry.get("assistant") {
                if let Some(mut msg) =
                    q_conversation::convert_assistant_message(PROVIDER, assistant, &conv_id, i)
                {
                    if let Some(ref c) = msg.content {
                        if search_json_value_case_insensitive(c, &query_lower) {
                            msg.project_name = Some("Kiro CLI".to_string());
                            results.push(msg);
                        }
                    }
                }
            }
        }
    }

    Ok(results)
}
