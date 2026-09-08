//! Remote host aggregation support
//!
//! Connects to CCHV servers running on remote machines (e.g. over Tailscale SSH/VPN)
//! to aggregate projects, sessions, and messages into the local viewer.

use crate::commands::session::{SessionPage, SubagentSession};
use crate::models::{ClaudeMessage, ClaudeProject, ClaudeSession, MessagePage, RemoteHostConfig};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

lazy_static::lazy_static! {
    static ref REMOTE_SCAN_CACHE: Mutex<HashMap<String, (Instant, Vec<ClaudeProject>)>> = Mutex::new(HashMap::new());
}

pub const DEFAULT_REMOTE_HOST_ID: &str = "arogovets";
pub const DEFAULT_REMOTE_HOST_NAME: &str = "arogovets@100.93.94.80";
pub const DEFAULT_REMOTE_HOST_ENDPOINT: &str = "http://100.93.94.80:3728";

pub const GORTAMAZIAN_REMOTE_HOST_ID: &str = "gortamazian";
pub const GORTAMAZIAN_REMOTE_HOST_NAME: &str = "gortamazian@s-macbook-pro.tail69ac27.ts.net";
pub const GORTAMAZIAN_REMOTE_HOST_ENDPOINT: &str = "http://s-macbook-pro.tail69ac27.ts.net:3728";

pub fn get_default_remote_hosts() -> Vec<RemoteHostConfig> {
    vec![
        RemoteHostConfig {
            id: DEFAULT_REMOTE_HOST_ID.to_string(),
            name: DEFAULT_REMOTE_HOST_NAME.to_string(),
            endpoint: DEFAULT_REMOTE_HOST_ENDPOINT.to_string(),
            auth_token: None,
            enabled: true,
        },
        RemoteHostConfig {
            id: GORTAMAZIAN_REMOTE_HOST_ID.to_string(),
            name: GORTAMAZIAN_REMOTE_HOST_NAME.to_string(),
            endpoint: GORTAMAZIAN_REMOTE_HOST_ENDPOINT.to_string(),
            auth_token: None,
            enabled: true,
        },
    ]
}

pub fn get_default_remote_host() -> RemoteHostConfig {
    get_default_remote_hosts().into_iter().next().unwrap()
}

/// Retrieve configured remote hosts, defaulting to built-in remote hosts if none configured.
pub fn get_remote_hosts() -> Vec<RemoteHostConfig> {
    if std::env::var("CCHV_NO_REMOTE").is_ok() {
        return Vec::new();
    }
    let current_user = std::env::var("USER").unwrap_or_default();

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

    get_default_remote_hosts()
        .into_iter()
        .filter(|h| h.id != current_user)
        .collect()
}

pub fn is_remote_path(path: &str) -> bool {
    path.starts_with("remote://")
}

pub fn format_remote_path(endpoint: &str, path: &str) -> String {
    let clean_endpoint = endpoint.trim_end_matches('/');
    format!("remote://{clean_endpoint}#{path}")
}

pub fn resolve_endpoint(raw: &str) -> String {
    let raw = raw.trim();
    if raw.starts_with("http://") || raw.starts_with("https://") {
        return raw.to_string();
    }
    for host in get_remote_hosts() {
        if host.id.eq_ignore_ascii_case(raw)
            || host.name.eq_ignore_ascii_case(raw)
            || host.endpoint.contains(raw)
        {
            return host.endpoint;
        }
        if let Some((_, ip_or_host)) = host.name.split_once('@') {
            if raw == ip_or_host || raw.starts_with(ip_or_host) {
                return host.endpoint;
            }
        }
    }
    if raw == "100.123.58.67" || raw.starts_with("100.123.58.67:") {
        return GORTAMAZIAN_REMOTE_HOST_ENDPOINT.to_string();
    }
    if raw == "100.93.94.80" || raw.starts_with("100.93.94.80:") {
        return DEFAULT_REMOTE_HOST_ENDPOINT.to_string();
    }
    if raw.contains(':') && !raw.contains('@') {
        return format!("http://{raw}");
    }
    if let Some((_, host_part)) = raw.split_once('@') {
        if host_part.contains(':') {
            return format!("http://{host_part}");
        }
    }
    DEFAULT_REMOTE_HOST_ENDPOINT.to_string()
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
        .timeout(Duration::from_secs(60))
        .connect_timeout(Duration::from_secs(10))
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
    let cache_key = format!("{}:{}", host.endpoint, active_providers.join(","));

    // 1. Check in-memory 30s TTL deduplication cache
    {
        if let Ok(cache) = REMOTE_SCAN_CACHE.lock() {
            if let Some((instant, projects)) = cache.get(&cache_key) {
                if instant.elapsed() < Duration::from_secs(30) {
                    return Ok(projects.clone());
                }
            }
        }
    }

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

    let resp_res = req.send().await;
    let resp = match resp_res {
        Ok(r) if r.status().is_success() => r,
        Ok(r) => {
            let status = r.status();
            let err_text = r.text().await.unwrap_or_default();
            log::warn!(
                "Remote host {} returned {status}: {err_text}, falling back to local SQLite cache",
                host.name,
            );
            let cached = crate::cache::load_projects(Some(&host.name));
            if !cached.is_empty() {
                return Ok(cached);
            }
            return Err(format!(
                "Remote host {} returned status {status}: {err_text}",
                host.name,
            ));
        }
        Err(e) => {
            log::warn!(
                "Remote host {} scan failed ({e}), falling back to local SQLite cache",
                host.name
            );
            let cached = crate::cache::load_projects(Some(&host.name));
            if !cached.is_empty() {
                return Ok(cached);
            }
            return Err(format!("Remote host {} scan failed: {e}", host.name));
        }
    };

    let bytes = resp.bytes().await.map_err(|e| {
        format!(
            "Failed to read remote projects response body from {}: {e}",
            host.name
        )
    })?;

    let raw_projects: Vec<RemoteProjectRaw> = serde_json::from_slice(&bytes).map_err(|e| {
        let preview = String::from_utf8_lossy(&bytes[..std::cmp::min(bytes.len(), 250)]);
        format!(
            "Failed to parse remote projects from {}: {e}. Payload start: {preview}",
            host.name
        )
    })?;

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

    // Update in-memory TTL deduplication cache
    if let Ok(mut cache) = REMOTE_SCAN_CACHE.lock() {
        cache.insert(cache_key, (Instant::now(), projects.clone()));
    }

    // Persist to local SQLite cache for offline resiliency
    crate::cache::cache_projects(&projects, Some(&host.name));

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
    let resolved_endpoint = resolve_endpoint(endpoint);
    let remote_project_path = format_remote_path(&resolved_endpoint, inner_project_path);
    let client = create_client();
    let url = format!(
        "{}/api/load_provider_sessions",
        resolved_endpoint.trim_end_matches('/')
    );

    let resp_res = client
        .post(&url)
        .json(&LoadSessionsPayload {
            provider,
            project_path: inner_project_path,
            exclude_sidechain,
            offset: None,
            limit: None,
        })
        .send()
        .await;

    let resp = match resp_res {
        Ok(r) if r.status().is_success() => r,
        Ok(r) => {
            log::warn!(
                "Remote sessions request failed with status {}, falling back to local SQLite cache",
                r.status()
            );
            let cached = crate::cache::load_sessions(&remote_project_path, provider);
            if !cached.is_empty() {
                return Ok(cached);
            }
            return Err(format!(
                "Remote sessions request failed with status {}",
                r.status()
            ));
        }
        Err(e) => {
            log::warn!(
                "Failed to load remote sessions from {endpoint} ({e}), falling back to local SQLite cache"
            );
            let cached = crate::cache::load_sessions(&remote_project_path, provider);
            if !cached.is_empty() {
                return Ok(cached);
            }
            return Err(format!(
                "Failed to load remote sessions from {endpoint}: {e}"
            ));
        }
    };

    let bytes = resp
        .bytes()
        .await
        .map_err(|e| format!("Failed to read remote sessions response body: {e}"))?;

    let mut sessions: Vec<ClaudeSession> = match serde_json::from_slice(&bytes) {
        Ok(s) => s,
        Err(e) => {
            log::warn!("Failed to parse remote sessions from {endpoint} ({e}), checking cache");
            let cached = crate::cache::load_sessions(&remote_project_path, provider);
            if !cached.is_empty() {
                return Ok(cached);
            }
            let preview = String::from_utf8_lossy(&bytes[..std::cmp::min(bytes.len(), 250)]);
            return Err(format!(
                "Failed to parse remote sessions: {e}. Payload start: {preview}"
            ));
        }
    };

    for s in &mut sessions {
        s.file_path = format_remote_path(&resolved_endpoint, &s.file_path);
        s.session_id = format_remote_path(&resolved_endpoint, &s.session_id);
    }

    // Persist to local SQLite cache
    crate::cache::cache_sessions(&remote_project_path, provider, &sessions);
    if endpoint != resolved_endpoint {
        let legacy_project_path = format_remote_path(endpoint, inner_project_path);
        crate::cache::cache_sessions(&legacy_project_path, provider, &sessions);
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
    let resolved_endpoint = resolve_endpoint(endpoint);
    let remote_project_path = format_remote_path(&resolved_endpoint, inner_project_path);
    let client = create_client();
    let url = format!(
        "{}/api/load_provider_sessions_page",
        resolved_endpoint.trim_end_matches('/')
    );

    let resp_res = client
        .post(&url)
        .json(&LoadSessionsPayload {
            provider,
            project_path: inner_project_path,
            exclude_sidechain,
            offset: Some(offset),
            limit: Some(limit),
        })
        .send()
        .await;

    let resp = match resp_res {
        Ok(r) if r.status().is_success() => r,
        Ok(r) => {
            log::warn!(
                "Remote sessions page request failed with status {}, falling back to local SQLite cache",
                r.status()
            );
            if let Ok(conn) = crate::cache::open_connection() {
                if let Ok(page) = crate::cache::get_cached_sessions_page(
                    &conn,
                    &remote_project_path,
                    provider,
                    offset,
                    limit,
                ) {
                    if page.total > 0 {
                        return Ok(page);
                    }
                }
            }
            return Err(format!(
                "Remote sessions page request failed with status {}",
                r.status()
            ));
        }
        Err(e) => {
            log::warn!(
                "Failed to load remote sessions page from {endpoint} ({e}), falling back to local SQLite cache"
            );
            if let Ok(conn) = crate::cache::open_connection() {
                if let Ok(page) = crate::cache::get_cached_sessions_page(
                    &conn,
                    &remote_project_path,
                    provider,
                    offset,
                    limit,
                ) {
                    if page.total > 0 {
                        return Ok(page);
                    }
                }
            }
            return Err(format!(
                "Failed to load remote sessions page from {endpoint}: {e}"
            ));
        }
    };

    let bytes = resp
        .bytes()
        .await
        .map_err(|e| format!("Failed to read remote sessions page response body: {e}"))?;

    let mut page: SessionPage = match serde_json::from_slice(&bytes) {
        Ok(p) => p,
        Err(e) => {
            log::warn!(
                "Failed to parse remote sessions page from {endpoint} ({e}), checking cache"
            );
            if let Ok(conn) = crate::cache::open_connection() {
                if let Ok(cached_page) = crate::cache::get_cached_sessions_page(
                    &conn,
                    &remote_project_path,
                    provider,
                    offset,
                    limit,
                ) {
                    if cached_page.total > 0 {
                        return Ok(cached_page);
                    }
                }
            }
            let preview = String::from_utf8_lossy(&bytes[..std::cmp::min(bytes.len(), 250)]);
            return Err(format!(
                "Failed to parse remote sessions page: {e}. Payload start: {preview}"
            ));
        }
    };

    for s in &mut page.sessions {
        s.file_path = format_remote_path(&resolved_endpoint, &s.file_path);
        s.session_id = format_remote_path(&resolved_endpoint, &s.session_id);
    }

    // Persist to local SQLite cache
    crate::cache::cache_sessions(&remote_project_path, provider, &page.sessions);
    if endpoint != resolved_endpoint {
        let legacy_project_path = format_remote_path(endpoint, inner_project_path);
        crate::cache::cache_sessions(&legacy_project_path, provider, &page.sessions);
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
    let resolved_endpoint = resolve_endpoint(endpoint);
    let remote_session_path = format_remote_path(&resolved_endpoint, inner_session_path);
    let client = create_client();
    let url = format!(
        "{}/api/load_provider_messages",
        resolved_endpoint.trim_end_matches('/')
    );

    let resp_res = client
        .post(&url)
        .json(&LoadMessagesPayload {
            provider,
            session_path: inner_session_path,
        })
        .send()
        .await;

    let resp = match resp_res {
        Ok(r) if r.status().is_success() => r,
        Ok(r) => {
            log::warn!(
                "Remote messages request failed with status {}, falling back to local SQLite cache",
                r.status()
            );
            if let Ok(conn) = crate::cache::open_connection() {
                if let Ok(Some(cached)) =
                    crate::cache::get_cached_session_messages(&conn, &remote_session_path, provider)
                {
                    return Ok(cached);
                }
            }
            return Err(format!(
                "Remote messages request failed with status {}",
                r.status()
            ));
        }
        Err(e) => {
            log::warn!(
                "Failed to load remote messages from {endpoint} ({e}), falling back to local SQLite cache"
            );
            if let Ok(conn) = crate::cache::open_connection() {
                if let Ok(Some(cached)) =
                    crate::cache::get_cached_session_messages(&conn, &remote_session_path, provider)
                {
                    return Ok(cached);
                }
            }
            return Err(format!(
                "Failed to load remote messages from {endpoint}: {e}"
            ));
        }
    };

    let bytes = resp
        .bytes()
        .await
        .map_err(|e| format!("Failed to read remote messages response body: {e}"))?;

    let messages = match serde_json::from_slice::<Vec<ClaudeMessage>>(&bytes) {
        Ok(m) => m,
        Err(e) => {
            log::warn!(
                "Failed to parse remote messages from {endpoint} ({e}), checking local SQLite cache"
            );
            if let Ok(conn) = crate::cache::open_connection() {
                if let Ok(Some(cached)) =
                    crate::cache::get_cached_session_messages(&conn, &remote_session_path, provider)
                {
                    return Ok(cached);
                }
            }
            let preview = String::from_utf8_lossy(&bytes[..std::cmp::min(bytes.len(), 250)]);
            return Err(format!(
                "Failed to parse remote messages: {e}. Payload start: {preview}"
            ));
        }
    };

    // Persist to local SQLite cache
    crate::cache::cache_messages(&remote_session_path, provider, &messages);
    if endpoint != resolved_endpoint {
        let legacy_path = format_remote_path(endpoint, inner_session_path);
        crate::cache::cache_messages(&legacy_path, provider, &messages);
    }

    Ok(messages)
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
    let resolved_endpoint = resolve_endpoint(endpoint);
    let remote_session_path = format_remote_path(&resolved_endpoint, inner_session_path);
    let client = create_client();
    let url = format!(
        "{}/api/load_provider_messages_paginated",
        resolved_endpoint.trim_end_matches('/')
    );

    let resp_res = client
        .post(&url)
        .json(&LoadMessagesPaginatedPayload {
            provider,
            session_path: inner_session_path,
            offset,
            limit,
            exclude_sidechain,
        })
        .send()
        .await;

    let resp = match resp_res {
        Ok(r) if r.status().is_success() => r,
        Ok(r) => {
            log::warn!(
                "Remote paginated messages request failed with status {}, falling back to local SQLite cache",
                r.status()
            );
            if let Ok(conn) = crate::cache::open_connection() {
                if let Ok(Some(page)) = crate::cache::get_cached_session_messages_paginated(
                    &conn,
                    &remote_session_path,
                    provider,
                    offset,
                    limit,
                    exclude_sidechain,
                ) {
                    return Ok(page);
                }
            }
            return Err(format!(
                "Remote paginated messages request failed with status {}",
                r.status()
            ));
        }
        Err(e) => {
            log::warn!(
                "Failed to load remote paginated messages from {endpoint} ({e}), falling back to local SQLite cache"
            );
            if let Ok(conn) = crate::cache::open_connection() {
                if let Ok(Some(page)) = crate::cache::get_cached_session_messages_paginated(
                    &conn,
                    &remote_session_path,
                    provider,
                    offset,
                    limit,
                    exclude_sidechain,
                ) {
                    return Ok(page);
                }
            }
            return Err(format!(
                "Failed to load remote paginated messages from {endpoint}: {e}"
            ));
        }
    };

    let bytes = resp
        .bytes()
        .await
        .map_err(|e| format!("Failed to read remote paginated messages response body: {e}"))?;

    let page = match serde_json::from_slice::<MessagePage>(&bytes) {
        Ok(p) => p,
        Err(e) => {
            log::warn!(
                "Failed to parse remote paginated messages from {endpoint} ({e}), checking local SQLite cache"
            );
            if let Ok(conn) = crate::cache::open_connection() {
                if let Ok(Some(page)) = crate::cache::get_cached_session_messages_paginated(
                    &conn,
                    &remote_session_path,
                    provider,
                    offset,
                    limit,
                    exclude_sidechain,
                ) {
                    return Ok(page);
                }
            }
            let preview = String::from_utf8_lossy(&bytes[..std::cmp::min(bytes.len(), 250)]);
            return Err(format!(
                "Failed to parse remote paginated messages: {e}. Payload start: {preview}"
            ));
        }
    };

    if !page.messages.is_empty() {
        crate::cache::cache_messages(&remote_session_path, provider, &page.messages);
        if endpoint != resolved_endpoint {
            let legacy_path = format_remote_path(endpoint, inner_session_path);
            crate::cache::cache_messages(&legacy_path, provider, &page.messages);
        }
    }

    Ok(page)
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
    let resolved_endpoint = resolve_endpoint(endpoint);
    let client = create_client();
    let url = format!(
        "{}/api/get_provider_message_offset",
        resolved_endpoint.trim_end_matches('/')
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
    let resolved_endpoint = resolve_endpoint(endpoint);
    let client = create_client();
    let url = format!(
        "{}/api/get_session_subagents",
        resolved_endpoint.trim_end_matches('/')
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
        sub.file_path = format_remote_path(&resolved_endpoint, &sub.file_path);
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

    #[test]
    #[serial_test::serial]
    fn test_resolve_endpoint() {
        let _sandbox = crate::test_utils::SandboxHome::new();
        assert_eq!(
            resolve_endpoint("http://100.93.94.80:3728"),
            "http://100.93.94.80:3728"
        );
        assert_eq!(
            resolve_endpoint("arogovets@100.93.94.80"),
            "http://100.93.94.80:3728"
        );
        assert_eq!(resolve_endpoint("arogovets"), "http://100.93.94.80:3728");
        assert_eq!(
            resolve_endpoint("gortamazian@s-macbook-pro.tail69ac27.ts.net"),
            "http://s-macbook-pro.tail69ac27.ts.net:3728"
        );
        assert_eq!(
            resolve_endpoint("s-macbook-pro.tail69ac27.ts.net"),
            "http://s-macbook-pro.tail69ac27.ts.net:3728"
        );
        assert_eq!(
            resolve_endpoint("gortamazian"),
            "http://s-macbook-pro.tail69ac27.ts.net:3728"
        );
        assert_eq!(
            resolve_endpoint("100.123.58.67"),
            "http://s-macbook-pro.tail69ac27.ts.net:3728"
        );
        assert_eq!(
            resolve_endpoint("192.168.1.50:3728"),
            "http://192.168.1.50:3728"
        );
    }

    #[tokio::test]
    async fn test_remote_scan_live() {
        let _sandbox = crate::test_utils::SandboxHome::new();
        let host = get_default_remote_host();
        let providers = vec![
            "claude".to_string(),
            "codex".to_string(),
            "opencode".to_string(),
        ];
        let res = scan_remote_projects(&host, &providers).await;
        println!("Live scan result: {:?}", res.as_ref().map(Vec::len));
        if let Err(ref e) = res {
            println!("Live host unreachable in test environment: {e}");
        }
    }
}
