//! Remote host aggregation support
//!
//! Connects to CCHV servers running on remote machines (e.g. over Tailscale SSH/VPN)
//! to aggregate projects, sessions, and messages into the local viewer.

use crate::commands::session::{SessionPage, SubagentSession};
use crate::models::{ClaudeMessage, ClaudeProject, ClaudeSession, MessagePage, RemoteHostConfig};
use serde::Serialize;
use std::time::Duration;

pub const DEFAULT_REMOTE_HOST_ID: &str = "arogovets";
pub const DEFAULT_REMOTE_HOST_NAME: &str = "arogovets@100.93.94.80";
pub const DEFAULT_REMOTE_HOST_ENDPOINT: &str = "http://100.93.94.80:3728";

pub fn get_default_remote_host() -> RemoteHostConfig {
    RemoteHostConfig {
        id: DEFAULT_REMOTE_HOST_ID.to_string(),
        name: DEFAULT_REMOTE_HOST_NAME.to_string(),
        endpoint: DEFAULT_REMOTE_HOST_ENDPOINT.to_string(),
        auth_token: None,
        enabled: true,
    }
}

/// Retrieve configured remote hosts, defaulting to arogovets@100.93.94.80 if none configured.
pub fn get_remote_hosts() -> Vec<RemoteHostConfig> {
    let current_user = std::env::var("USER").unwrap_or_default();
    if current_user == DEFAULT_REMOTE_HOST_ID || std::env::var("CCHV_NO_REMOTE").is_ok() {
        return Vec::new();
    }

    // Check if user has remote hosts configured in metadata
    if let Ok(path) = crate::commands::metadata::get_user_data_path() {
        if path.exists() {
            if let Ok(content) = std::fs::read_to_string(&path) {
                if let Ok(metadata) = serde_json::from_str::<crate::models::UserMetadata>(&content)
                {
                    let filtered: Vec<_> = metadata
                        .settings
                        .remote_hosts
                        .into_iter()
                        .filter(|h| h.id != current_user)
                        .collect();
                    if !filtered.is_empty() {
                        return filtered;
                    }
                }
            }
        }
    }

    vec![get_default_remote_host()]
}

pub fn is_remote_path(path: &str) -> bool {
    path.starts_with("remote://")
}

pub fn format_remote_path(endpoint: &str, path: &str) -> String {
    let clean_endpoint = endpoint.trim_end_matches('/');
    format!("remote://{clean_endpoint}#{path}")
}

pub fn parse_remote_path(path: &str) -> Option<(&str, &str)> {
    let rest = path.strip_prefix("remote://")?;
    let (endpoint_part, inner_path) = rest.split_once('#')?;
    let endpoint = endpoint_part
        .split_once('?')
        .map(|(ep, _)| ep)
        .unwrap_or(endpoint_part);
    Some((endpoint, inner_path))
}

fn create_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(8))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

#[derive(serde::Deserialize)]
struct RemoteProjectRaw {
    name: String,
    path: String,
    actual_path: String,
    session_count: usize,
    message_count: usize,
    last_modified: String,
    #[serde(default)]
    path_status: Option<crate::models::ProjectPathStatus>,
    git_info: Option<crate::models::GitInfo>,
    provider: Option<String>,
    storage_type: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ScanProjectsPayload<'a> {
    active_providers: &'a [String],
    include_remote: bool,
}

pub async fn scan_remote_projects(
    host: &RemoteHostConfig,
    active_providers: &[String],
) -> Result<Vec<ClaudeProject>, String> {
    let client = create_client();
    let url = format!(
        "{}/api/scan_all_projects",
        host.endpoint.trim_end_matches('/')
    );

    let mut req = client.post(&url).json(&ScanProjectsPayload {
        active_providers,
        include_remote: false,
    });
    if let Some(ref token) = host.auth_token {
        req = req.bearer_auth(token);
    }

    let resp = req
        .send()
        .await
        .map_err(|e| format!("Remote host {} scan failed: {e}", host.name))?;

    if !resp.status().is_success() {
        let err_text = resp.text().await.unwrap_or_default();
        return Err(format!(
            "Remote host {} returned status {}: {err_text}",
            host.name, err_text
        ));
    }

    let raw_projects: Vec<RemoteProjectRaw> = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse remote projects from {}: {e}", host.name))?;

    let mut projects = Vec::with_capacity(raw_projects.len());
    for raw in raw_projects {
        let is_unavailable = raw.path_status == Some(crate::models::ProjectPathStatus::Unavailable);
        let remote_path = if is_unavailable {
            format!(
                "remote://{}?status=unavailable#{}",
                host.endpoint.trim_end_matches('/'),
                raw.path
            )
        } else {
            format_remote_path(&host.endpoint, &raw.path)
        };

        projects.push(ClaudeProject {
            name: raw.name,
            path: remote_path,
            actual_path: raw.actual_path,
            session_count: raw.session_count,
            message_count: raw.message_count,
            last_modified: raw.last_modified,
            git_info: raw.git_info,
            provider: raw.provider,
            storage_type: raw.storage_type,
            custom_directory_label: Some(host.name.clone()),
        });
    }

    Ok(projects)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LoadSessionsPayload<'a> {
    provider: &'a str,
    project_path: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    exclude_sidechain: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    offset: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    limit: Option<usize>,
}

pub async fn load_remote_sessions(
    endpoint: &str,
    provider: &str,
    inner_project_path: &str,
    exclude_sidechain: Option<bool>,
) -> Result<Vec<ClaudeSession>, String> {
    let client = create_client();
    let url = format!(
        "{}/api/load_provider_sessions",
        endpoint.trim_end_matches('/')
    );

    let resp = client
        .post(&url)
        .json(&LoadSessionsPayload {
            provider,
            project_path: inner_project_path,
            exclude_sidechain,
            offset: None,
            limit: None,
        })
        .send()
        .await
        .map_err(|e| format!("Failed to load remote sessions from {endpoint}: {e}"))?;

    if !resp.status().is_success() {
        return Err(format!(
            "Remote sessions request failed with status {}",
            resp.status()
        ));
    }

    let mut sessions: Vec<ClaudeSession> = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse remote sessions: {e}"))?;

    for s in &mut sessions {
        s.file_path = format_remote_path(endpoint, &s.file_path);
        s.session_id = format_remote_path(endpoint, &s.session_id);
    }

    Ok(sessions)
}

pub async fn load_remote_sessions_page(
    endpoint: &str,
    provider: &str,
    inner_project_path: &str,
    exclude_sidechain: Option<bool>,
    offset: usize,
    limit: usize,
) -> Result<SessionPage, String> {
    let client = create_client();
    let url = format!(
        "{}/api/load_provider_sessions_page",
        endpoint.trim_end_matches('/')
    );

    let resp = client
        .post(&url)
        .json(&LoadSessionsPayload {
            provider,
            project_path: inner_project_path,
            exclude_sidechain,
            offset: Some(offset),
            limit: Some(limit),
        })
        .send()
        .await
        .map_err(|e| format!("Failed to load remote sessions page from {endpoint}: {e}"))?;

    if !resp.status().is_success() {
        return Err(format!(
            "Remote sessions page request failed with status {}",
            resp.status()
        ));
    }

    let mut page: SessionPage = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse remote sessions page: {e}"))?;

    for s in &mut page.sessions {
        s.file_path = format_remote_path(endpoint, &s.file_path);
        s.session_id = format_remote_path(endpoint, &s.session_id);
    }

    Ok(page)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LoadMessagesPayload<'a> {
    provider: &'a str,
    session_path: &'a str,
}

pub async fn load_remote_messages(
    endpoint: &str,
    provider: &str,
    inner_session_path: &str,
) -> Result<Vec<ClaudeMessage>, String> {
    let client = create_client();
    let url = format!(
        "{}/api/load_provider_messages",
        endpoint.trim_end_matches('/')
    );

    let resp = client
        .post(&url)
        .json(&LoadMessagesPayload {
            provider,
            session_path: inner_session_path,
        })
        .send()
        .await
        .map_err(|e| format!("Failed to load remote messages from {endpoint}: {e}"))?;

    if !resp.status().is_success() {
        return Err(format!(
            "Remote messages request failed with status {}",
            resp.status()
        ));
    }

    resp.json()
        .await
        .map_err(|e| format!("Failed to parse remote messages: {e}"))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LoadMessagesPaginatedPayload<'a> {
    provider: &'a str,
    session_path: &'a str,
    offset: usize,
    limit: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    exclude_sidechain: Option<bool>,
}

pub async fn load_remote_messages_paginated(
    endpoint: &str,
    provider: &str,
    inner_session_path: &str,
    offset: usize,
    limit: usize,
    exclude_sidechain: Option<bool>,
) -> Result<MessagePage, String> {
    let client = create_client();
    let url = format!(
        "{}/api/load_provider_messages_paginated",
        endpoint.trim_end_matches('/')
    );

    let resp = client
        .post(&url)
        .json(&LoadMessagesPaginatedPayload {
            provider,
            session_path: inner_session_path,
            offset,
            limit,
            exclude_sidechain,
        })
        .send()
        .await
        .map_err(|e| format!("Failed to load remote paginated messages from {endpoint}: {e}"))?;

    if !resp.status().is_success() {
        return Err(format!(
            "Remote paginated messages request failed with status {}",
            resp.status()
        ));
    }

    resp.json()
        .await
        .map_err(|e| format!("Failed to parse remote paginated messages: {e}"))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct MessageOffsetPayload<'a> {
    provider: &'a str,
    session_path: &'a str,
    message_uuid: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    exclude_sidechain: Option<bool>,
}

pub async fn get_remote_message_offset(
    endpoint: &str,
    provider: &str,
    inner_session_path: &str,
    message_uuid: &str,
    exclude_sidechain: Option<bool>,
) -> Result<Option<usize>, String> {
    let client = create_client();
    let url = format!(
        "{}/api/get_provider_message_offset",
        endpoint.trim_end_matches('/')
    );

    let resp = client
        .post(&url)
        .json(&MessageOffsetPayload {
            provider,
            session_path: inner_session_path,
            message_uuid,
            exclude_sidechain,
        })
        .send()
        .await
        .map_err(|e| format!("Failed to get remote message offset from {endpoint}: {e}"))?;

    if !resp.status().is_success() {
        return Err(format!(
            "Remote message offset request failed with status {}",
            resp.status()
        ));
    }

    resp.json()
        .await
        .map_err(|e| format!("Failed to parse remote message offset: {e}"))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GetSessionSubagentsPayload<'a> {
    session_path: &'a str,
}

pub async fn get_remote_session_subagents(
    endpoint: &str,
    inner_session_path: &str,
) -> Result<Vec<SubagentSession>, String> {
    let client = create_client();
    let url = format!(
        "{}/api/get_session_subagents",
        endpoint.trim_end_matches('/')
    );

    let resp = client
        .post(&url)
        .json(&GetSessionSubagentsPayload {
            session_path: inner_session_path,
        })
        .send()
        .await
        .map_err(|e| format!("Failed to get remote subagents from {endpoint}: {e}"))?;

    if !resp.status().is_success() {
        return Ok(Vec::new());
    }

    let mut subagents: Vec<SubagentSession> = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse remote subagents: {e}"))?;

    for sub in &mut subagents {
        sub.file_path = format_remote_path(endpoint, &sub.file_path);
    }

    Ok(subagents)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SearchAllProvidersPayload<'a> {
    query: &'a str,
    limit: usize,
    active_providers: &'a [String],
    include_remote: bool,
}

pub async fn search_remote_providers(
    host: &RemoteHostConfig,
    query: &str,
    limit: usize,
    active_providers: &[String],
) -> Result<Vec<ClaudeMessage>, String> {
    let client = create_client();
    let url = format!(
        "{}/api/search_all_providers",
        host.endpoint.trim_end_matches('/')
    );

    let resp = client
        .post(&url)
        .json(&SearchAllProvidersPayload {
            query,
            limit,
            active_providers,
            include_remote: false,
        })
        .send()
        .await
        .map_err(|e| format!("Failed to search remote host {}: {e}", host.name))?;

    if !resp.status().is_success() {
        return Err(format!(
            "Remote search failed with status {}",
            resp.status()
        ));
    }

    let messages: Vec<ClaudeMessage> = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse remote search results: {e}"))?;

    Ok(messages)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deserialize_remote_projects() {
        let json_str = r#"[{"actual_path":"/Users/arogovets","last_modified":"2026-09-05T17:46:00.269682157+00:00","message_count":15290,"name":"arogovets","path":"codex:///Users/arogovets","provider":"codex","session_count":3},{"actual_path":"/Volumes/Extreme SSD/tt/костя/ Милый-Ветта-Венская","git_info":{"worktree_type":"not_git"},"last_modified":"2026-08-07T04:36:26.017476805+00:00","message_count":31505,"name":" Милый-Ветта-Венская","path":"/Users/arogovets/.claude/projects/-Volumes-Extreme-SSD-tt---------------------------","path_status":"unavailable","provider":"claude","session_count":1}]"#;
        let projects: Result<Vec<ClaudeProject>, _> = serde_json::from_str(json_str);
        assert!(
            projects.is_ok(),
            "Failed to deserialize: {:?}",
            projects.err()
        );
    }

    #[tokio::test]
    async fn test_remote_scan_live() {
        let host = get_default_remote_host();
        let providers = vec![
            "claude".to_string(),
            "codex".to_string(),
            "opencode".to_string(),
        ];
        let res = scan_remote_projects(&host, &providers).await;
        println!("Live scan result: {:?}", res.as_ref().map(Vec::len));
        if let Err(ref e) = res {
            println!("Error: {e}");
        }
        assert!(res.is_ok(), "Scan failed: {:?}", res.err());
    }
}
