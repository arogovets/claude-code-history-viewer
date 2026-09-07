use crate::cache::LocatedSession;
use crate::models::{ClaudeProject, ClaudeSession};
use std::fs;

#[tauri::command]
pub async fn locate_session(session_id: String) -> Result<Option<LocatedSession>, String> {
    let raw_id = session_id.trim();
    if raw_id.is_empty() {
        return Ok(None);
    }

    // Extract raw UUID if it is a remote:// path or file path
    let clean_id = if let Some((_, inner)) = raw_id.split_once('#') {
        inner
            .split('/')
            .next_back()
            .unwrap_or(inner)
            .trim_end_matches(".jsonl")
    } else {
        raw_id
            .split('/')
            .next_back()
            .unwrap_or(raw_id)
            .trim_end_matches(".jsonl")
    };

    // 1. Check local SQLite cache first (O(1) index lookup)
    if let Some(located) = crate::cache::locate(clean_id) {
        return Ok(Some(located));
    }
    if clean_id != raw_id {
        if let Some(located) = crate::cache::locate(raw_id) {
            return Ok(Some(located));
        }
    }

    // 2. Fast filesystem probe: search ~/.claude/projects/*/<session_id>.jsonl
    if let Some(home) = crate::utils::home_dir() {
        let projects_dir = home.join(".claude").join("projects");
        if projects_dir.is_dir() {
            let target_filename = format!("{clean_id}.jsonl");
            if let Ok(entries) = fs::read_dir(&projects_dir) {
                for entry in entries.flatten() {
                    let project_dir = entry.path();
                    if project_dir.is_dir() {
                        let candidate_file = project_dir.join(&target_filename);
                        if candidate_file.is_file() {
                            let project_path = project_dir.to_string_lossy().to_string();
                            let file_path = candidate_file.to_string_lossy().to_string();
                            let actual_path = crate::utils::decode_project_path(
                                project_dir
                                    .file_name()
                                    .unwrap_or_default()
                                    .to_string_lossy()
                                    .as_ref(),
                            );
                            let project_name = actual_path
                                .split('/')
                                .next_back()
                                .unwrap_or(&actual_path)
                                .to_string();

                            let session = ClaudeSession {
                                session_id: file_path.clone(),
                                actual_session_id: clean_id.to_string(),
                                file_path: file_path.clone(),
                                project_name: project_name.clone(),
                                message_count: 0,
                                first_message_time: String::new(),
                                last_message_time: String::new(),
                                last_modified: String::new(),
                                has_tool_use: false,
                                has_errors: false,
                                summary: None,
                                is_renamed: false,
                                provider: Some("claude".to_string()),
                                storage_type: None,
                                entrypoint: None,
                            };

                            let project = ClaudeProject {
                                name: project_name,
                                path: project_path.clone(),
                                actual_path: project_path,
                                session_count: 1,
                                message_count: 0,
                                last_modified: String::new(),
                                git_info: None,
                                provider: Some("claude".to_string()),
                                storage_type: None,
                                custom_directory_label: None,
                            };

                            // Cache for subsequent requests
                            crate::cache::cache_sessions(
                                &project.path,
                                "claude",
                                std::slice::from_ref(&session),
                            );
                            crate::cache::cache_projects(std::slice::from_ref(&project), None);

                            return Ok(Some(LocatedSession { project, session }));
                        }
                    }
                }
            }
        }

        // 3. Fast filesystem probe for Codex: search ~/.codex/sessions and ~/.codex/archived_sessions
        let codex_base = home.join(".codex");
        let codex_dirs = [
            codex_base.join("sessions"),
            codex_base.join("archived_sessions"),
        ];
        for sdir in codex_dirs {
            if !sdir.is_dir() {
                continue;
            }
            for entry in walkdir::WalkDir::new(&sdir)
                .min_depth(1)
                .into_iter()
                .filter_map(Result::ok)
                .filter(|e| e.file_type().is_file())
            {
                let fname = entry.file_name().to_string_lossy();
                if fname.contains(clean_id)
                    && (fname.ends_with(".jsonl") || fname.ends_with(".jsonl.zst"))
                {
                    let rollout_path = entry.path();
                    if let Ok(info) = crate::providers::codex::extract_session_info(rollout_path) {
                        let cwd = info.cwd.as_deref().unwrap_or("unknown");
                        let project_name = std::path::Path::new(cwd)
                            .file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_else(|| cwd.to_string());
                        let project_path = format!("codex://{cwd}");
                        let session = ClaudeSession {
                            session_id: info.file_path.clone(),
                            actual_session_id: info.session_id.clone(),
                            file_path: info.file_path.clone(),
                            project_name: project_name.clone(),
                            message_count: info.message_count,
                            first_message_time: info.first_message_time,
                            last_message_time: info.last_message_time,
                            last_modified: info.last_modified.clone(),
                            has_tool_use: info.has_tool_use,
                            has_errors: false,
                            summary: info.summary,
                            is_renamed: false,
                            provider: Some("codex".to_string()),
                            storage_type: None,
                            entrypoint: None,
                        };
                        let project = ClaudeProject {
                            name: project_name,
                            path: project_path.clone(),
                            actual_path: cwd.to_string(),
                            session_count: 1,
                            message_count: info.message_count,
                            last_modified: info.last_modified,
                            git_info: None,
                            provider: Some("codex".to_string()),
                            storage_type: None,
                            custom_directory_label: None,
                        };

                        crate::cache::cache_sessions(
                            &project.path,
                            "codex",
                            std::slice::from_ref(&session),
                        );
                        crate::cache::cache_projects(std::slice::from_ref(&project), None);

                        return Ok(Some(LocatedSession { project, session }));
                    }
                }
            }
        }
    }

    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[serial_test::serial]
    async fn test_locate_session_cached() {
        let _sandbox = crate::test_utils::SandboxHome::new();
        let project = ClaudeProject {
            name: "my-project".to_string(),
            path: "/path/to/project".to_string(),
            actual_path: "/path/to/project".to_string(),
            session_count: 1,
            message_count: 5,
            last_modified: "2026-09-01T00:00:00Z".to_string(),
            git_info: None,
            provider: Some("claude".to_string()),
            storage_type: None,
            custom_directory_label: None,
        };
        let session = ClaudeSession {
            session_id: "test-session-uuid-12345".to_string(),
            actual_session_id: "12345678-1234-1234-1234-123456789abc".to_string(),
            file_path: "/path/to/project/12345678-1234-1234-1234-123456789abc.jsonl".to_string(),
            project_name: "my-project".to_string(),
            message_count: 5,
            first_message_time: "2026-09-01T00:00:00Z".to_string(),
            last_message_time: "2026-09-01T01:00:00Z".to_string(),
            last_modified: "2026-09-01T01:00:00Z".to_string(),
            has_tool_use: false,
            has_errors: false,
            summary: Some("Locate test session".to_string()),
            is_renamed: false,
            provider: Some("claude".to_string()),
            storage_type: None,
            entrypoint: None,
        };

        crate::cache::cache_projects(std::slice::from_ref(&project), None);
        crate::cache::cache_sessions(&project.path, "claude", std::slice::from_ref(&session));

        let located = locate_session("12345678-1234-1234-1234-123456789abc".to_string())
            .await
            .unwrap()
            .expect("should locate session");

        assert_eq!(located.session.session_id, session.session_id);
        assert_eq!(located.project.name, "my-project");
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_locate_session_filesystem_probe() {
        let sandbox = crate::test_utils::SandboxHome::new();
        let claude_project_dir = sandbox
            .path()
            .join(".claude")
            .join("projects")
            .join("test-proj");
        std::fs::create_dir_all(&claude_project_dir).unwrap();
        let session_file = claude_project_dir.join("probe-session-uuid.jsonl");
        std::fs::write(&session_file, "{}\n").unwrap();

        let located = locate_session("probe-session-uuid".to_string())
            .await
            .unwrap()
            .expect("should locate session from filesystem probe");

        assert_eq!(located.session.actual_session_id, "probe-session-uuid");
        assert_eq!(located.session.provider.as_deref(), Some("claude"));
    }
}
