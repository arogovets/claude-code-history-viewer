use super::ProviderInfo;
use crate::models::{ClaudeMessage, ClaudeProject, ClaudeSession, TokenUsage};
use crate::utils::{build_provider_message, is_symlink};
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};

pub const PROVIDER_ID: &str = "deepseek";
const SESSIONS_DIR: &str = "sessions";
const SCHEME: &str = "dsh://";

pub fn detect() -> Option<ProviderInfo> {
    let base = get_base_path()?;
    let sessions_path = Path::new(&base).join(SESSIONS_DIR);

    Some(ProviderInfo {
        id: PROVIDER_ID.to_string(),
        display_name: "DeepSeek Harness".to_string(),
        base_path: base,
        is_available: sessions_path.exists() && sessions_path.is_dir(),
    })
}

pub fn get_base_path() -> Option<String> {
    if let Ok(env_val) = std::env::var("DSH_HOME") {
        let path = PathBuf::from(&env_val);
        let absolute_path = if path.is_absolute() {
            path
        } else {
            std::env::current_dir().ok()?.join(path)
        };
        if absolute_path.exists() {
            let normalized = absolute_path.canonicalize().unwrap_or(absolute_path);
            return Some(normalized.to_string_lossy().to_string());
        }
    }

    let default = crate::utils::home_dir()?.join(".dsh");
    if default.exists() {
        let normalized = default.canonicalize().unwrap_or(default);
        Some(normalized.to_string_lossy().to_string())
    } else {
        None
    }
}

/// Convert epoch milliseconds to RFC 3339 string
fn epoch_ms_to_rfc3339(ms: i64) -> String {
    let secs = ms / 1000;
    let nsecs = ((ms % 1000) * 1_000_000) as u32;
    match DateTime::from_timestamp(secs, nsecs) {
        Some(dt) => dt.to_rfc3339(),
        None => Utc::now().to_rfc3339(),
    }
}

/// Decode a directory slug like `--Users-emac-Dev-tutor--` to a path or check session cwd
fn slug_to_path(slug: &str) -> String {
    let trimmed = slug.trim_matches('-');
    if trimmed.is_empty() {
        return "/".to_string();
    }
    // Replace hyphens with slashes
    format!("/{}", trimmed.replace('-', "/"))
}

#[derive(Default)]
struct ProjectAccumulator {
    session_count: usize,
    message_count: usize,
    last_modified: String,
}

pub fn scan_projects() -> Result<Vec<ClaudeProject>, String> {
    let base = get_base_path().ok_or_else(|| "DeepSeek Harness directory not found".to_string())?;
    scan_projects_from_path(&base)
}

pub fn scan_projects_from_path(base_path: &str) -> Result<Vec<ClaudeProject>, String> {
    crate::utils::require_absolute_path(base_path, "DeepSeek Harness base path")?;
    let base = Path::new(base_path);
    let sessions_root = base.join(SESSIONS_DIR);

    if is_symlink(&sessions_root) || !sessions_root.is_dir() {
        return Ok(Vec::new());
    }

    let mut projects_map: HashMap<String, ProjectAccumulator> = HashMap::new();

    let entries = fs::read_dir(&sessions_root)
        .map_err(|e| format!("Failed to read DeepSeek Harness sessions: {e}"))?;

    for entry in entries {
        let entry = entry.map_err(|e| format!("Failed to read entry: {e}"))?;
        let entry_path = entry.path();
        if !entry_path.is_dir() || is_symlink(&entry_path) {
            continue;
        }

        let dir_name = entry.file_name().to_string_lossy().to_string();
        let fallback_cwd = slug_to_path(&dir_name);

        let sub_entries = match fs::read_dir(&entry_path) {
            Ok(dirs) => dirs,
            Err(_) => continue,
        };

        for session_dir in sub_entries.flatten() {
            let s_path = session_dir.path();
            if !s_path.is_dir() {
                continue;
            }

            let zstd_file = s_path.join("session.jsonl.zstd");
            let plain_file = s_path.join("session.jsonl");
            let target_file = if zstd_file.exists() {
                Some(zstd_file)
            } else if plain_file.exists() {
                Some(plain_file)
            } else {
                None
            };

            if let Some(file_path) = target_file {
                let (cwd, msg_count, modified) = inspect_session_file(&file_path)
                    .unwrap_or_else(|_| (fallback_cwd.clone(), 0, Utc::now().to_rfc3339()));

                let final_cwd = if cwd.is_empty() {
                    fallback_cwd.clone()
                } else {
                    cwd
                };
                let agg =
                    projects_map
                        .entry(final_cwd.clone())
                        .or_insert_with(|| ProjectAccumulator {
                            session_count: 0,
                            message_count: 0,
                            last_modified: modified.clone(),
                        });

                agg.session_count += 1;
                agg.message_count += msg_count;
                if modified > agg.last_modified {
                    agg.last_modified = modified;
                }
            }
        }
    }

    let mut projects: Vec<ClaudeProject> = projects_map
        .into_iter()
        .map(|(cwd, agg)| {
            let name = Path::new(&cwd)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| cwd.clone());

            ClaudeProject {
                name,
                path: format!("{SCHEME}{cwd}"),
                actual_path: cwd,
                session_count: agg.session_count,
                message_count: agg.message_count,
                last_modified: agg.last_modified,
                git_info: None,
                provider: Some(PROVIDER_ID.to_string()),
                custom_directory_label: None,
                storage_type: None,
            }
        })
        .collect();

    projects.sort_by(|a, b| b.last_modified.cmp(&a.last_modified));
    Ok(projects)
}

/// Decompresses (if zstd) and reads the content of a session file
fn read_session_bytes(path: &Path) -> Result<Vec<u8>, String> {
    let file = File::open(path).map_err(|e| format!("Failed to open session file: {e}"))?;
    let mut reader = BufReader::new(file);

    if path.extension().is_some_and(|ext| ext == "zstd") {
        zstd::decode_all(reader).map_err(|e| format!("Failed to decompress zstd session: {e}"))
    } else {
        let mut buffer = Vec::new();
        reader
            .read_to_end(&mut buffer)
            .map_err(|e| format!("Failed to read session file: {e}"))?;
        Ok(buffer)
    }
}

/// Quick inspection of session file for project scanning (cwd, message count, modified time)
fn inspect_session_file(path: &Path) -> Result<(String, usize, String), String> {
    let metadata = fs::metadata(path).map_err(|e| e.to_string())?;
    let modified = metadata
        .modified()
        .map(|t| {
            let dt: DateTime<Utc> = t.into();
            dt.to_rfc3339()
        })
        .unwrap_or_else(|_| Utc::now().to_rfc3339());

    let bytes = read_session_bytes(path)?;
    let mut cwd = String::new();
    let mut msg_count = 0;

    for line in bytes.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        if line.is_empty() {
            continue;
        }

        if let Ok(val) = serde_json::from_str::<Value>(&line) {
            let event_type = val.get("type").and_then(Value::as_str).unwrap_or("");
            if event_type == "session" {
                if let Some(c) = val.get("cwd").and_then(Value::as_str) {
                    cwd = c.to_string();
                }
            } else if event_type == "user/message" || event_type == "assistant/message" {
                msg_count += 1;
            }
        }
    }

    Ok((cwd, msg_count, modified))
}

pub fn load_sessions(
    project_path: &str,
    _exclude_sidechain: bool,
) -> Result<Vec<ClaudeSession>, String> {
    let base = get_base_path().ok_or_else(|| "DeepSeek Harness directory not found".to_string())?;
    let sessions_root = Path::new(&base).join(SESSIONS_DIR);

    let target_cwd = project_path.strip_prefix(SCHEME).unwrap_or(project_path);

    let mut sessions = Vec::new();

    if !sessions_root.exists() {
        return Ok(sessions);
    }

    let entries = fs::read_dir(&sessions_root)
        .map_err(|e| format!("Failed to read DeepSeek Harness sessions: {e}"))?;

    for entry in entries.flatten() {
        let entry_path = entry.path();
        if !entry_path.is_dir() {
            continue;
        }

        let sub_entries = match fs::read_dir(&entry_path) {
            Ok(dirs) => dirs,
            Err(_) => continue,
        };

        for session_dir in sub_entries.flatten() {
            let s_path = session_dir.path();
            if !s_path.is_dir() {
                continue;
            }

            let zstd_file = s_path.join("session.jsonl.zstd");
            let plain_file = s_path.join("session.jsonl");
            let target_file = if zstd_file.exists() {
                Some(zstd_file)
            } else if plain_file.exists() {
                Some(plain_file)
            } else {
                None
            };

            if let Some(file_path) = target_file {
                if let Some(session) = load_single_session_info(&file_path, target_cwd) {
                    sessions.push(session);
                }
            }
        }
    }

    sessions.sort_by(|a, b| b.last_message_time.cmp(&a.last_message_time));
    Ok(sessions)
}

fn load_single_session_info(file_path: &Path, target_cwd: &str) -> Option<ClaudeSession> {
    let bytes = read_session_bytes(file_path).ok()?;
    let mut session_id = String::new();
    let mut title = None;
    let mut cwd = String::new();
    let mut first_time = None;
    let mut last_time = None;
    let mut message_count = 0;
    let mut has_tool_use = false;
    let mut has_errors = false;

    for line in bytes.lines().map_while(Result::ok) {
        if line.is_empty() {
            continue;
        }
        let val: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let event_type = val.get("type").and_then(Value::as_str).unwrap_or("");
        let time_ms = val.get("time").and_then(Value::as_i64);

        if let Some(ms) = time_ms {
            let rfc = epoch_ms_to_rfc3339(ms);
            if first_time.is_none() {
                first_time = Some(rfc.clone());
            }
            last_time = Some(rfc);
        }

        match event_type {
            "session" => {
                if let Some(id) = val.get("id").and_then(Value::as_str) {
                    session_id = id.to_string();
                }
                if let Some(c) = val.get("cwd").and_then(Value::as_str) {
                    cwd = c.to_string();
                }
                if let Some(ms) = val.get("createdAt").and_then(Value::as_i64) {
                    if first_time.is_none() {
                        first_time = Some(epoch_ms_to_rfc3339(ms));
                    }
                }
            }
            "session/title" => {
                if let Some(t) = val.pointer("/data/title").and_then(Value::as_str) {
                    title = Some(t.to_string());
                }
            }
            "user/message" => {
                let is_user_source = val
                    .pointer("/data/source/kind")
                    .and_then(Value::as_str)
                    .map_or(true, |k| k == "user");
                if is_user_source {
                    message_count += 1;
                    if title.is_none() {
                        if let Some(arr) = val.pointer("/data/content").and_then(Value::as_array) {
                            for item in arr {
                                if item.get("type").and_then(Value::as_str) == Some("text") {
                                    if let Some(txt) = item.get("text").and_then(Value::as_str) {
                                        let snippet: String = txt
                                            .lines()
                                            .next()
                                            .unwrap_or("")
                                            .chars()
                                            .take(80)
                                            .collect();
                                        if !snippet.is_empty() {
                                            title = Some(snippet);
                                            break;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            "assistant/message" => {
                message_count += 1;
            }
            "tool/call" => {
                has_tool_use = true;
            }
            "tool/result"
                if val.pointer("/data/isError").and_then(Value::as_bool) == Some(true) =>
            {
                has_errors = true;
            }
            _ => {}
        }
    }

    if !cwd.is_empty() && cwd != target_cwd {
        return None;
    }

    let default_time = Utc::now().to_rfc3339();
    let first_msg_time = first_time.unwrap_or_else(|| default_time.clone());
    let last_msg_time = last_time.unwrap_or_else(|| first_msg_time.clone());

    let file_path_str = file_path.to_string_lossy().to_string();
    if session_id.is_empty() {
        session_id = file_path
            .parent()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown".to_string());
    }

    let project_name = Path::new(&cwd)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| cwd.clone());

    Some(ClaudeSession {
        session_id: file_path_str.clone(),
        actual_session_id: session_id,
        file_path: file_path_str,
        first_message_time: first_msg_time,
        last_message_time: last_msg_time.clone(),
        last_modified: last_msg_time,
        message_count,
        summary: title,
        has_tool_use,
        has_errors,
        is_renamed: false,
        project_name,
        provider: Some(PROVIDER_ID.to_string()),
        storage_type: None,
        entrypoint: Some("cli".to_string()),
    })
}

pub fn load_messages(session_path: &str) -> Result<Vec<ClaudeMessage>, String> {
    let path = Path::new(session_path);
    let target_file = if path.is_file() {
        path.to_path_buf()
    } else if path.join("session.jsonl.zstd").exists() {
        path.join("session.jsonl.zstd")
    } else if path.join("session.jsonl").exists() {
        path.join("session.jsonl")
    } else {
        return Err(format!("Session file not found at: {session_path}"));
    };

    let bytes = read_session_bytes(&target_file)?;
    let mut messages = Vec::new();
    let mut session_id = "unknown".to_string();

    for line in bytes.lines().map_while(Result::ok) {
        if line.is_empty() {
            continue;
        }

        let val: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let event_type = val.get("type").and_then(Value::as_str).unwrap_or("");
        let time_ms = val.get("time").and_then(Value::as_i64).unwrap_or(0);
        let timestamp = epoch_ms_to_rfc3339(time_ms);

        match event_type {
            "session" => {
                if let Some(id) = val.get("id").and_then(Value::as_str) {
                    session_id = id.to_string();
                }
            }
            "user/message" => {
                let uuid = val
                    .pointer("/data/id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let uuid = if uuid.is_empty() {
                    uuid::Uuid::new_v4().to_string()
                } else {
                    uuid
                };

                let text_content = val
                    .pointer("/data/content")
                    .and_then(Value::as_array)
                    .map(|arr| {
                        let mut text = String::new();
                        for item in arr {
                            if let Some(t) = item.get("text").and_then(Value::as_str) {
                                if !text.is_empty() {
                                    text.push('\n');
                                }
                                text.push_str(t);
                            }
                        }
                        text
                    })
                    .unwrap_or_default();

                let is_user_source = val
                    .pointer("/data/source/kind")
                    .and_then(Value::as_str)
                    .map_or(true, |k| k == "user");

                let content_value = json!([{"type": "text", "text": text_content}]);

                let mut msg = build_provider_message(
                    PROVIDER_ID,
                    uuid,
                    &session_id,
                    timestamp,
                    "user",
                    Some("user"),
                    Some(content_value),
                    None,
                );

                if !is_user_source {
                    msg.message_type = "system".to_string();
                }

                messages.push(msg);
            }
            "assistant/message" => {
                let uuid = val
                    .pointer("/data/message/id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let uuid = if uuid.is_empty() {
                    uuid::Uuid::new_v4().to_string()
                } else {
                    uuid
                };
                let model = val
                    .pointer("/data/message/source/model")
                    .and_then(Value::as_str)
                    .map(String::from);

                let mut content_blocks = Vec::new();
                if let Some(arr) = val
                    .pointer("/data/message/content")
                    .and_then(Value::as_array)
                {
                    for item in arr {
                        let block_type = item.get("type").and_then(Value::as_str).unwrap_or("");
                        let text = item.get("text").and_then(Value::as_str).unwrap_or("");
                        if block_type == "reasoning" {
                            content_blocks.push(json!({
                                "type": "thinking",
                                "thinking": text
                            }));
                        } else {
                            content_blocks.push(json!({
                                "type": "text",
                                "text": text
                            }));
                        }
                    }
                }

                let usage = val
                    .get("data")
                    .and_then(|d| d.get("usage"))
                    .map(|u| TokenUsage {
                        input_tokens: u
                            .get("inputTokens")
                            .and_then(Value::as_u64)
                            .map(|n| n as u32),
                        output_tokens: u
                            .get("outputTokens")
                            .and_then(Value::as_u64)
                            .map(|n| n as u32),
                        cache_creation_input_tokens: None,
                        cache_read_input_tokens: u
                            .get("cacheReadTokens")
                            .and_then(Value::as_u64)
                            .map(|n| n as u32),
                        ..Default::default()
                    });

                let mut msg = build_provider_message(
                    PROVIDER_ID,
                    uuid,
                    &session_id,
                    timestamp,
                    "assistant",
                    Some("assistant"),
                    Some(Value::Array(content_blocks)),
                    model,
                );
                msg.usage = usage;
                messages.push(msg);
            }
            "tool/call" => {
                let uuid = val
                    .pointer("/data/id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let uuid = if uuid.is_empty() {
                    uuid::Uuid::new_v4().to_string()
                } else {
                    uuid
                };
                let tool_name = val
                    .pointer("/data/name")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                let args = val.pointer("/data/args").cloned().unwrap_or(json!({}));

                let mut msg = build_provider_message(
                    PROVIDER_ID,
                    uuid.clone(),
                    &session_id,
                    timestamp,
                    "tool_use",
                    Some("assistant"),
                    None,
                    None,
                );
                msg.tool_use = Some(json!({
                    "id": uuid,
                    "name": tool_name,
                    "input": args
                }));
                messages.push(msg);
            }
            "tool/result" => {
                let call_id = val
                    .pointer("/data/callId")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let output = val.pointer("/data/result").cloned().unwrap_or(json!(""));

                let mut msg = build_provider_message(
                    PROVIDER_ID,
                    uuid::Uuid::new_v4().to_string(),
                    &session_id,
                    timestamp,
                    "tool_result",
                    Some("user"),
                    None,
                    None,
                );
                msg.tool_use_result = Some(json!({
                    "tool_use_id": call_id,
                    "content": output
                }));
                messages.push(msg);
            }
            _ => {}
        }
    }

    Ok(messages)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_slug_to_path() {
        assert_eq!(
            slug_to_path("--Users-emac-Dev-tutor--"),
            "/Users/emac/Dev/tutor"
        );
        assert_eq!(slug_to_path("----"), "/");
    }

    #[test]
    fn test_epoch_ms_to_rfc3339() {
        let ts = epoch_ms_to_rfc3339(1786951970519);
        assert!(ts.contains("2026") || ts.contains("2025") || ts.contains('T'));
    }
}
