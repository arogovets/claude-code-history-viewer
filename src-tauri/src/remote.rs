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
    #[cfg(test)]
    if std::env::var_os("CCHV_TEST_HOME").is_none() {
        return get_default_remote_hosts();
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
        .timeout(Duration::from_secs(30))
        .connect_timeout(Duration::from_secs(3))
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

// ─── Raw-file sync transport (client side) ──────────────────────────────
//
// Remote hosts are primarily sources of provider *files*. When reachable we
// sync those files into immutable local snapshots and parse them with the
// existing local parsers; when offline we read the latest completed snapshot.
// Parsed-response endpoints below remain as the legacy fallback until every
// provider migrates.

// Protocol surface for the file-sync transport. Not every field is consumed
// yet (`size`/`mtime_secs` will drive incremental fetching); they must still
// deserialize so the protocol stays forward-compatible.
#[allow(dead_code)]
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct SyncSourceInfo {
    provider: String,
    root: String,
    label: Option<String>,
}

#[allow(dead_code)]
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct SyncFileEntry {
    path: String,
    size: u64,
    mtime_secs: i64,
}

#[allow(dead_code)]
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct SyncManifestResponse {
    provider: String,
    root: String,
    files: Vec<SyncFileEntry>,
}

#[allow(dead_code)]
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct SyncFileResponse {
    path: String,
    content_base64: String,
    size: u64,
    mtime_secs: i64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SyncSourcesPayload<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    providers: Option<&'a [String]>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SyncManifestPayload<'a> {
    provider: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    root: Option<&'a str>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SyncFilePayload<'a> {
    provider: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    root: Option<&'a str>,
    path: &'a str,
}

fn sync_post_url(endpoint: &str, path: &str) -> String {
    format!("{}{path}", endpoint.trim_end_matches('/'))
}

async fn fetch_remote_sync_sources(
    host: &RemoteHostConfig,
    providers: Option<&[String]>,
) -> Result<Vec<SyncSourceInfo>, String> {
    let client = create_client();
    let mut req = client
        .post(sync_post_url(&host.endpoint, "/api/sync/sources"))
        .json(&SyncSourcesPayload { providers });
    if let Some(ref token) = host.auth_token {
        req = req.bearer_auth(token);
    }
    let resp = req
        .send()
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?;
    resp.json::<Vec<SyncSourceInfo>>()
        .await
        .map_err(|e| e.to_string())
}

async fn fetch_remote_sync_manifest(
    host: &RemoteHostConfig,
    provider: &str,
    root: &str,
) -> Result<SyncManifestResponse, String> {
    let client = create_client();
    let mut req = client
        .post(sync_post_url(&host.endpoint, "/api/sync/manifest"))
        .json(&SyncManifestPayload {
            provider,
            root: Some(root),
        });
    if let Some(ref token) = host.auth_token {
        req = req.bearer_auth(token);
    }
    let resp = req
        .send()
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?;
    resp.json::<SyncManifestResponse>()
        .await
        .map_err(|e| e.to_string())
}

async fn fetch_remote_sync_file(
    host: &RemoteHostConfig,
    provider: &str,
    root: &str,
    path: &str,
) -> Result<(Vec<u8>, i64), String> {
    use base64::Engine as _;
    let client = create_client();
    let mut req = client
        .post(sync_post_url(&host.endpoint, "/api/sync/file"))
        .json(&SyncFilePayload {
            provider,
            root: Some(root),
            path,
        });
    if let Some(ref token) = host.auth_token {
        req = req.bearer_auth(token);
    }
    let resp = req
        .send()
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?;
    let body = resp
        .json::<SyncFileResponse>()
        .await
        .map_err(|e| e.to_string())?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(body.content_base64.as_str())
        .map_err(|e| format!("Failed to decode sync file {path}: {e}"))?;
    Ok((bytes, body.mtime_secs))
}

// Throttle file-sync per endpoint so every UI refresh does not re-download a
// whole host history; scans stay responsive while sync converges.
lazy_static::lazy_static! {
    static ref REMOTE_FILE_SYNC_LAST: Mutex<HashMap<String, Instant>> = Mutex::new(HashMap::new());
}

fn remote_file_sync_throttled(endpoint: &str) -> bool {
    let mut map = REMOTE_FILE_SYNC_LAST
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let now = Instant::now();
    if let Some(last) = map.get(endpoint) {
        if last.elapsed() < Duration::from_secs(60) {
            return true;
        }
    }
    map.insert(endpoint.to_string(), now);
    false
}

/// Best-effort file sync for one host: download raw Claude files into new
/// immutable snapshots (one per remote root). Never throws away history:
/// failures keep the previous completed snapshot.
pub async fn sync_remote_host_files(host: &RemoteHostConfig) -> Result<usize, String> {
    let sources = fetch_remote_sync_sources(host, Some(&["claude".to_string()])).await?;
    let mut synced = 0usize;
    for source in sources.iter().filter(|s| s.provider == "claude") {
        let manifest = fetch_remote_sync_manifest(host, "claude", &source.root).await?;
        let mut files = Vec::with_capacity(manifest.files.len());
        let mut skipped = 0usize;
        for entry in &manifest.files {
            match fetch_remote_sync_file(host, "claude", &source.root, &entry.path).await {
                Ok((bytes, mtime)) => files.push(crate::storage::RemoteSyncedFile {
                    path: entry.path.clone(),
                    bytes,
                    mtime_secs: mtime,
                }),
                // One unreadable file (rotated mid-sync, exotic symlink, size
                // cap) skips just that file rather than failing the host: the
                // snapshot stays consistent by construction (its manifest only
                // lists staged files) and older snapshots keep the history.
                Err(e) => {
                    skipped += 1;
                    log::warn!("Remote file sync skipped {}: {e}", entry.path);
                }
            }
        }
        if files.is_empty() {
            // Never publish an empty snapshot over preserved history: an empty
            // file set means the fetch failed wholesale (or the root is
            // genuinely empty, in which case there is nothing to preserve yet
            // and skipping is equally correct).
            if skipped > 0 {
                return Err(format!(
                    "Remote file sync for {} fetched no files ({skipped} skipped)",
                    source.root
                ));
            }
            continue;
        }
        let storage_source = crate::storage::Source::remote_with_root(
            "claude",
            &host.endpoint,
            &manifest.root,
            Some(&host.name),
        );
        // Sync in a blocking task: staging does filesystem I/O.
        let outcome = tauri::async_runtime::spawn_blocking(move || {
            crate::storage::sync_remote_files_to_snapshot(&storage_source, &manifest.root, files)
        })
        .await
        .map_err(|e| format!("Task join error: {e}"))??;
        match outcome {
            crate::storage::SyncOutcome::Created(_) | crate::storage::SyncOutcome::Unchanged(_) => {
                synced += 1;
            }
        }
    }
    // Fold newly synced snapshots into the derived index (idempotent).
    if synced > 0 {
        let _ = tauri::async_runtime::spawn_blocking(crate::storage::reconcile_unindexed_snapshots)
            .await;
    }
    Ok(synced)
}

/// Parse the latest completed local snapshots for a remote endpoint into
/// projects with `remote://` paths. `None` when nothing is preserved yet.
fn load_snapshot_projects_for_remote(
    endpoint: &str,
    host_name: &str,
    provider: &str,
) -> Option<Vec<ClaudeProject>> {
    if provider != "claude" {
        return None;
    }
    let mut projects = Vec::new();
    for (source, _) in crate::storage::find_remote_sources(endpoint, provider) {
        let snapshot = crate::storage::latest_completed_snapshot(&source.id)?;
        let mut parsed = crate::commands::project::scan_projects_from_root(&snapshot.data_path);
        for project in &mut parsed {
            // Translate snapshot-interior paths back to remote paths.
            if let Some(relative) = project
                .path
                .strip_prefix(snapshot.data_path.to_string_lossy().as_ref())
            {
                let relative = relative.trim_start_matches('/');
                let remote_root = snapshot.manifest.original_root.as_deref().unwrap_or("");
                let inner = if remote_root.is_empty() {
                    relative.to_string()
                } else {
                    format!("{}/{}", remote_root.trim_end_matches('/'), relative)
                };
                project.path = format_remote_path(endpoint, &inner);
            }
            if project.provider.is_none() {
                project.provider = Some(provider.to_string());
            }
            if project.custom_directory_label.is_none() {
                project.custom_directory_label = Some(host_name.to_string());
            }
        }
        projects.extend(parsed);
    }
    if projects.is_empty() {
        None
    } else {
        Some(projects)
    }
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

    // 2. Filesystem first (Claude): refresh raw-file snapshots when reachable
    // (throttled), then parse the preserved copies locally. Other providers
    // keep the parsed-response path below until migrated.
    let mut snapshot_projects: Vec<ClaudeProject> = Vec::new();
    if active_providers.iter().any(|p| p == "claude") {
        if !remote_file_sync_throttled(&host.endpoint) {
            match sync_remote_host_files(host).await {
                Ok(n) => log::info!("Remote file sync for {}: {n} root(s)", host.name),
                Err(e) => log::warn!("Remote file sync for {} skipped: {e}", host.name),
            }
        }
        if let Some(parsed) =
            load_snapshot_projects_for_remote(&host.endpoint, &host.name, "claude")
        {
            snapshot_projects = parsed;
        }
    }
    // Providers not covered by snapshots still go over HTTP. When snapshots
    // cover Claude, it is excluded here so the same projects are not merged
    // twice from two transports.
    let http_providers: Vec<String> = if snapshot_projects.is_empty() {
        active_providers.to_vec()
    } else {
        active_providers
            .iter()
            .filter(|p| p.as_str() != "claude")
            .cloned()
            .collect()
    };

    let client = create_client();
    let url = format!(
        "{}/api/scan_all_projects",
        host.endpoint.trim_end_matches('/')
    );

    // When snapshots cover every requested provider, no HTTP is needed.
    let mut projects: Vec<ClaudeProject> = Vec::new();
    if !http_providers.is_empty() {
        let mut req = client.post(&url).json(&ScanProjectsPayload {
            active_providers: &http_providers,
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
                    "Remote host {} returned {status}: {err_text}, falling back to snapshots/SQLite",
                    host.name,
                );
                // Snapshots first, then the legacy SQLite cache.
                if !snapshot_projects.is_empty() {
                    let combined = crate::cache::sync_and_save_projects(
                        &snapshot_projects,
                        Some(&host.name),
                        None,
                    );
                    return Ok(combined);
                }
                let cached = crate::cache::load_projects(Some(&host.name), None);
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
                    "Remote host {} scan failed ({e}), falling back to snapshots/SQLite",
                    host.name
                );
                if !snapshot_projects.is_empty() {
                    let combined = crate::cache::sync_and_save_projects(
                        &snapshot_projects,
                        Some(&host.name),
                        None,
                    );
                    return Ok(combined);
                }
                let cached = crate::cache::load_projects(Some(&host.name), None);
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

        for raw in raw_projects {
            let is_unavailable =
                raw.path_status == Some(crate::models::ProjectPathStatus::Unavailable);
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
    }
    projects.extend(snapshot_projects);

    // Update in-memory TTL deduplication cache
    if let Ok(mut cache) = REMOTE_SCAN_CACHE.lock() {
        cache.insert(cache_key, (Instant::now(), projects.clone()));
    }

    schedule_offline_sync(host.clone(), projects.clone());

    // Persist to local SQLite cache for offline resiliency without losing deleted projects.
    // The SQLite row is legacy/derived: snapshots remain authoritative.
    let combined_projects = crate::cache::sync_and_save_projects(&projects, Some(&host.name), None);

    Ok(combined_projects)
}

/// Snapshot-first session read for remote Claude projects. Returns `None`
/// when no preserved snapshot covers the project yet (caller falls back to
/// HTTP, then to the legacy SQLite cache).
async fn load_snapshot_sessions_for_remote(
    endpoint: &str,
    provider: &str,
    inner_project_path: &str,
) -> Option<Vec<ClaudeSession>> {
    if provider != "claude" {
        return None;
    }
    let resolved = resolve_endpoint(endpoint);
    let (_source, snapshot, mapped) =
        crate::storage::map_remote_path_to_snapshot(&resolved, provider, inner_project_path)?;
    if !mapped.is_dir() {
        return None;
    }
    let snapshot_data = snapshot.data_path.clone();
    let remote_root = snapshot.manifest.original_root.clone().unwrap_or_default();
    let mut sessions = tauri::async_runtime::spawn_blocking(move || {
        crate::commands::session::load_project_sessions_blocking(
            mapped.to_string_lossy().to_string(),
            Some(false),
        )
    })
    .await
    .unwrap_or_default();
    if sessions.is_empty() {
        return None;
    }
    for session in &mut sessions {
        // Translate snapshot-interior absolute paths to the remote scheme.
        if let Some(rest) = session
            .file_path
            .strip_prefix(snapshot_data.to_string_lossy().as_ref())
        {
            let rest = rest.trim_start_matches('/');
            let inner = if remote_root.is_empty() {
                rest.to_string()
            } else {
                format!("{}/{}", remote_root.trim_end_matches('/'), rest)
            };
            let remote_path = format_remote_path(&resolved, &inner);
            session.file_path.clone_from(&remote_path);
            session.session_id = remote_path;
        }
        if session.provider.is_none() {
            session.provider = Some(provider.to_string());
        }
    }
    Some(sessions)
}

/// Snapshot-first message read for remote Claude sessions. Returns `None`
/// when no preserved snapshot covers the session yet.
async fn load_snapshot_messages_for_remote(
    endpoint: &str,
    provider: &str,
    inner_session_path: &str,
) -> Option<Vec<ClaudeMessage>> {
    if provider != "claude" {
        return None;
    }
    let resolved = resolve_endpoint(endpoint);
    let (_source, _snapshot, mapped) =
        crate::storage::map_remote_path_to_snapshot(&resolved, provider, inner_session_path)?;
    if !mapped.is_file() {
        return None;
    }
    // Boxed: the local loader routes `remote://` paths back here, which the
    // compiler sees as unbounded async recursion even though a snapshot-mapped
    // (local) path always terminates after one hop.
    let messages = Box::pin(crate::commands::session::load_session_messages(
        mapped.to_string_lossy().to_string(),
    ))
    .await
    .unwrap_or_default();
    if messages.is_empty() {
        None
    } else {
        Some(messages)
    }
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
    // Filesystem first: preserved snapshots answer without any network, so an
    // offline host keeps serving projects/sessions/messages.
    if let Some(snap_sessions) =
        load_snapshot_sessions_for_remote(endpoint, provider, inner_project_path).await
    {
        return Ok(crate::cache::sync_and_save_sessions(
            &remote_project_path,
            provider,
            &snap_sessions,
        ));
    }
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

    // Persist to local SQLite cache additively so remote deletions remain available offline
    let combined_sessions =
        crate::cache::sync_and_save_sessions(&remote_project_path, provider, &sessions);
    if endpoint != resolved_endpoint {
        let legacy_project_path = format_remote_path(endpoint, inner_project_path);
        crate::cache::sync_and_save_sessions(&legacy_project_path, provider, &combined_sessions);
    }

    Ok(combined_sessions)
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
    // Filesystem first for Claude: preserved snapshots answer offline.
    if provider == "claude" {
        if let Some(snap_sessions) =
            load_snapshot_sessions_for_remote(endpoint, provider, inner_project_path).await
        {
            let combined = crate::cache::sync_and_save_sessions(
                &remote_project_path,
                provider,
                &snap_sessions,
            );
            let total = combined.len();
            let end = (offset + limit).min(total);
            let page_sessions = if offset < total {
                combined[offset..end].to_vec()
            } else {
                Vec::new()
            };
            return Ok(SessionPage {
                offline: None,
                sessions: page_sessions,
                total,
                offset,
                limit,
                next_offset: end,
                has_more: end < total,
            });
        }
    }
    let result: Result<SessionPage, String> = async {
        let response = create_client()
            .post(format!(
                "{}/api/load_provider_sessions_page",
                resolved_endpoint.trim_end_matches('/')
            ))
            .json(&LoadSessionsPayload {
                provider,
                project_path: inner_project_path,
                exclude_sidechain,
                offset: Some(offset),
                limit: Some(limit),
            })
            .send()
            .await
            .map_err(|e| e.to_string())?
            .error_for_status()
            .map_err(|e| e.to_string())?;
        let mut page: SessionPage = response.json().await.map_err(|e| e.to_string())?;
        for session in &mut page.sessions {
            session.file_path = format_remote_path(&resolved_endpoint, &session.file_path);
            session.session_id = format_remote_path(&resolved_endpoint, &session.session_id);
        }
        let conn = crate::cache::open_connection()?;
        crate::cache::save_sessions(&conn, &remote_project_path, provider, &page.sessions)?;
        Ok(page)
    }
    .await;
    match result {
        Ok(page) => Ok(page),
        Err(error) => {
            log::warn!("Remote sessions unavailable at {endpoint}: {error}; reading saved history");
            let conn = crate::cache::open_connection()?;
            let mut page = crate::cache::get_cached_sessions_page(
                &conn,
                &remote_project_path,
                provider,
                offset,
                limit,
            )?;
            page.offline = Some(true);
            Ok(page)
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LoadMessagesPayload<'a> {
    provider: &'a str,
    session_path: &'a str,
}

/// When a remote host is offline or unreachable and no messages were previously cached
/// in `session_messages`, look up the session's preserved metadata in SQLite `sessions` table
/// and construct an informative fallback message so the session can still be inspected offline.
pub fn get_offline_session_fallback(
    conn: &rusqlite::Connection,
    remote_session_path: &str,
    endpoint: &str,
    provider: &str,
    inner_session_path: &str,
) -> Option<ClaudeMessage> {
    use rusqlite::OptionalExtension;

    let clean_id = crate::commands::session::clean_session_id(inner_session_path);
    let pattern = format!("%{clean_id}%");

    let mut stmt = conn
        .prepare_cached(
            "SELECT actual_session_id, summary, file_path, data_json
             FROM sessions
             WHERE file_path = ?1 OR session_id = ?1 OR actual_session_id = ?2
                OR file_path LIKE ?3 OR session_id LIKE ?3
             ORDER BY CASE WHEN actual_session_id = ?2 THEN 0 ELSE 1 END
             LIMIT 1",
        )
        .ok()?;

    let row = stmt
        .query_row(
            rusqlite::params![remote_session_path, clean_id, pattern],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            },
        )
        .optional()
        .ok()
        .flatten()?;

    let (actual_id, summary, file_path, data_json) = row;
    let session_obj: Option<ClaudeSession> = serde_json::from_str(&data_json).ok();
    let msg_count = session_obj.as_ref().map(|s| s.message_count).unwrap_or(0);
    let first_time = session_obj.as_ref().map(|s| s.first_message_time.clone());
    let last_time = session_obj.as_ref().map(|s| s.last_message_time.clone());

    let actual_id_display = if actual_id.is_empty() {
        clean_id
    } else {
        actual_id
    };
    let summary_display = summary.as_deref().unwrap_or("No summary available");
    let count_display = msg_count;
    let first_display = first_time.as_deref().unwrap_or("—");
    let last_display = last_time.as_deref().unwrap_or("—");

    let text = format!(
        "> [!WARNING]\n\
         > **Remote Host Offline (`{endpoint}`)**\n\
         >\n\
         > This session is preserved in your local SQLite database, but its full message history was not synchronized locally before remote host `{endpoint}` went offline.\n\
         >\n\
         > | Property | Value |\n\
         > | :--- | :--- |\n\
         > | **Session ID** | `{actual_id_display}` |\n\
         > | **Summary** | {summary_display} |\n\
         > | **Messages Count** | {count_display} messages |\n\
         > | **Created** | {first_display} |\n\
         > | **Last Active** | {last_display} |\n\
         > | **Provider** | {provider} |\n\
         > | **Remote Host** | `{endpoint}` |\n\
         > | **Original Path** | `{file_path}` |\n\
         >\n\
         > *When the remote host reconnects to the network, message history will automatically load and be retained in your local SQLite database for permanent offline viewing.*"
    );

    let ts = last_time
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());

    Some(ClaudeMessage {
        uuid: format!("offline-{actual_id_display}"),
        parent_uuid: None,
        session_id: remote_session_path.to_string(),
        timestamp: ts,
        message_type: "assistant".to_string(),
        content: Some(serde_json::json!([{"type": "text", "text": text}])),
        project_name: None,
        tool_use: None,
        tool_use_result: None,
        is_sidechain: Some(false),
        usage: None,
        role: Some("assistant".to_string()),
        model: None,
        stop_reason: None,
        cost_usd: None,
        duration_ms: None,
        message_id: None,
        snapshot: None,
        is_snapshot_update: None,
        data: None,
        tool_use_id: None,
        parent_tool_use_id: None,
        operation: None,
        subtype: None,
        level: None,
        hook_count: None,
        hook_infos: None,
        stop_reason_system: None,
        prevented_continuation: None,
        compact_metadata: None,
        microcompact_metadata: None,
        provider: Some(provider.to_string()),
    })
}

pub async fn load_remote_messages(
    endpoint: &str,
    provider: &str,
    inner_session_path: &str,
) -> Result<Vec<ClaudeMessage>, String> {
    let resolved_endpoint = resolve_endpoint(endpoint);
    let remote_session_path = format_remote_path(&resolved_endpoint, inner_session_path);
    // Filesystem first: preserved snapshots answer without any network.
    if let Some(snap_messages) =
        load_snapshot_messages_for_remote(endpoint, provider, inner_session_path).await
    {
        crate::cache::cache_messages(&remote_session_path, provider, &snap_messages);
        return Ok(snap_messages);
    }
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
                    if !cached.is_empty() {
                        return Ok(cached);
                    }
                }
                if let Some(fallback) = get_offline_session_fallback(
                    &conn,
                    &remote_session_path,
                    endpoint,
                    provider,
                    inner_session_path,
                ) {
                    return Ok(vec![fallback]);
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
                    if !cached.is_empty() {
                        return Ok(cached);
                    }
                }
                if let Some(fallback) = get_offline_session_fallback(
                    &conn,
                    &remote_session_path,
                    endpoint,
                    provider,
                    inner_session_path,
                ) {
                    return Ok(vec![fallback]);
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
                    if !cached.is_empty() {
                        return Ok(cached);
                    }
                }
                if let Some(fallback) = get_offline_session_fallback(
                    &conn,
                    &remote_session_path,
                    endpoint,
                    provider,
                    inner_session_path,
                ) {
                    return Ok(vec![fallback]);
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
    // Filesystem first for Claude: preserved snapshots answer offline with the
    // same chat-style pagination as the local loader.
    if provider == "claude" {
        if let Some((_source, _snapshot, mapped)) = crate::storage::map_remote_path_to_snapshot(
            &resolved_endpoint,
            provider,
            inner_session_path,
        ) {
            if mapped.is_file() {
                if let Ok(page) =
                    Box::pin(crate::commands::session::load_session_messages_paginated(
                        mapped.to_string_lossy().to_string(),
                        offset,
                        limit,
                        exclude_sidechain,
                    ))
                    .await
                {
                    if !page.messages.is_empty() {
                        return Ok(page);
                    }
                }
            }
        }
    }
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
                    if !page.messages.is_empty() {
                        return Ok(page);
                    }
                }
                if let Some(fallback) = get_offline_session_fallback(
                    &conn,
                    &remote_session_path,
                    endpoint,
                    provider,
                    inner_session_path,
                ) {
                    return Ok(MessagePage {
                        messages: vec![fallback],
                        total_count: 1,
                        has_more: false,
                        next_offset: 0,
                    });
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
                    if !page.messages.is_empty() {
                        return Ok(page);
                    }
                }
                if let Some(fallback) = get_offline_session_fallback(
                    &conn,
                    &remote_session_path,
                    endpoint,
                    provider,
                    inner_session_path,
                ) {
                    return Ok(MessagePage {
                        messages: vec![fallback],
                        total_count: 1,
                        has_more: false,
                        next_offset: 0,
                    });
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
                    if !page.messages.is_empty() {
                        return Ok(page);
                    }
                }
                if let Some(fallback) = get_offline_session_fallback(
                    &conn,
                    &remote_session_path,
                    endpoint,
                    provider,
                    inner_session_path,
                ) {
                    return Ok(MessagePage {
                        messages: vec![fallback],
                        total_count: 1,
                        has_more: false,
                        next_offset: 0,
                    });
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

        // Asynchronously fetch and cache full session messages into SQLite so it's permanently retained offline
        if page.has_more {
            let bg_endpoint = resolved_endpoint.clone();
            let bg_provider = provider.to_string();
            let bg_inner = inner_session_path.to_string();
            let bg_remote_path = remote_session_path.clone();
            tauri::async_runtime::spawn(async move {
                if let Ok(full_msgs) =
                    load_remote_messages(&bg_endpoint, &bg_provider, &bg_inner).await
                {
                    crate::cache::cache_messages(&bg_remote_path, &bg_provider, &full_msgs);
                }
            });
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

    let session_path = format_remote_path(&resolved_endpoint, inner_session_path);
    let fetched = async {
        client
            .post(&url)
            .json(&GetSessionSubagentsPayload {
                session_path: inner_session_path,
            })
            .send()
            .await
            .map_err(|e| e.to_string())?
            .error_for_status()
            .map_err(|e| e.to_string())?
            .json::<Vec<SubagentSession>>()
            .await
            .map_err(|e| e.to_string())
    }
    .await;
    let mut subagents = match fetched {
        Ok(subagents) => subagents,
        Err(error) => {
            if let Ok(conn) = crate::cache::open_connection() {
                if let Some(saved) =
                    crate::cache::db::get_cached_session_subagents(&conn, &session_path)?
                {
                    return Ok(saved);
                }
            }
            // Discovery is optional: an offline cache miss must not fail the
            // saved parent conversation. Do not persist this as an authoritative
            // empty list; retry discovery when the host becomes reachable.
            log::info!("Subagent discovery unavailable for {session_path}: {error}");
            return Ok(Vec::new());
        }
    };
    for sub in &mut subagents {
        sub.file_path = format_remote_path(&resolved_endpoint, &sub.file_path);
    }
    if let Ok(conn) = crate::cache::open_connection() {
        if let Err(error) =
            crate::cache::db::save_session_subagents(&conn, &session_path, &subagents)
        {
            log::warn!("Failed to cache subagents: {error}");
        }
    }
    // Preserve child transcripts even when their drill-down is never opened.
    let pending = subagents.clone();
    tauri::async_runtime::spawn(async move {
        for sub in pending {
            if let Some((host, path)) = parse_remote_path(&sub.file_path) {
                let provider = if path.starts_with("opencode://") {
                    "opencode"
                } else {
                    "claude"
                };
                if let Err(error) = load_remote_messages(host, provider, path).await {
                    log::warn!("Failed to preserve subagent transcript: {error}");
                }
            }
        }
    });

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

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LocateSessionPayload<'a> {
    session_id: &'a str,
}

pub async fn locate_remote_session(
    host: &RemoteHostConfig,
    session_id: &str,
) -> Option<crate::cache::LocatedSession> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .connect_timeout(Duration::from_secs(2))
        .build()
        .unwrap_or_else(|_| create_client());

    let url = format!("{}/api/locate_session", host.endpoint.trim_end_matches('/'));
    let mut req = client.post(&url).json(&LocateSessionPayload { session_id });
    if let Some(ref token) = host.auth_token {
        req = req.bearer_auth(token);
    }

    let resp = req.send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }

    let mut located: crate::cache::LocatedSession = resp.json().await.ok()?;

    // Wrap paths to remote scheme
    let ep = &host.endpoint;
    if !is_remote_path(&located.project.path) {
        located.project.path = format_remote_path(ep, &located.project.path);
    }
    if !is_remote_path(&located.session.session_id) {
        located.session.session_id = format_remote_path(ep, &located.session.session_id);
    }
    if !is_remote_path(&located.session.file_path) {
        located.session.file_path = format_remote_path(ep, &located.session.file_path);
    }
    if located.project.custom_directory_label.is_none() {
        located.project.custom_directory_label = Some(host.id.clone());
    }

    // Cache locally
    crate::cache::cache_projects(std::slice::from_ref(&located.project), Some(&host.id));
    crate::cache::cache_sessions(
        &located.project.path,
        located.project.provider.as_deref().unwrap_or("claude"),
        std::slice::from_ref(&located.session),
    );

    Some(located)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[serial_test::serial]
    async fn offline_subagents_use_saved_discovery() {
        let _sandbox = crate::test_utils::SandboxHome::new();
        let endpoint = "http://127.0.0.1:1";
        let conn = crate::cache::open_connection().unwrap();
        assert!(get_remote_session_subagents(endpoint, "/s.jsonl")
            .await
            .unwrap()
            .is_empty());
        assert!(crate::cache::db::get_cached_session_subagents(
            &conn,
            &format_remote_path(endpoint, "/s.jsonl")
        )
        .unwrap()
        .is_none());
        crate::cache::db::save_session_subagents(
            &conn,
            &format_remote_path(endpoint, "/s.jsonl"),
            &[],
        )
        .unwrap();
        assert!(get_remote_session_subagents(endpoint, "/s.jsonl")
            .await
            .unwrap()
            .is_empty());
    }

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

    #[tokio::test]
    #[serial_test::serial]
    async fn unavailable_page_uses_saved_sessions_and_reports_empty_cache() {
        let _sandbox = crate::test_utils::SandboxHome::new();
        let empty = load_remote_sessions_page(
            "http://127.0.0.1:1",
            "opencode",
            "opencode://project_hash",
            None,
            0,
            20,
        )
        .await
        .unwrap();
        assert_eq!(empty.offline, Some(true));
        assert!(empty.sessions.is_empty());
        let conn = crate::cache::open_connection().unwrap();
        let session = ClaudeSession {
            session_id: "remote://http://127.0.0.1:1#opencode://ses_test123".to_string(),
            actual_session_id: "ses_test123".to_string(),
            file_path: "remote://http://127.0.0.1:1#opencode://project_hash/ses_test123"
                .to_string(),
            project_name: "test-proj".to_string(),
            message_count: 42,
            first_message_time: "2026-09-05T08:00:00Z".to_string(),
            last_message_time: "2026-09-06T17:00:00Z".to_string(),
            last_modified: "2026-09-06T17:00:00Z".to_string(),
            has_tool_use: false,
            has_errors: false,
            summary: Some("Test Offline Session Summary".to_string()),
            is_renamed: false,
            provider: Some("opencode".to_string()),
            storage_type: None,
            entrypoint: None,
        };
        crate::cache::save_sessions(
            &conn,
            "remote://http://127.0.0.1:1#opencode://project_hash",
            "opencode",
            &[session],
        )
        .unwrap();
        let page = load_remote_sessions_page(
            "http://127.0.0.1:1",
            "opencode",
            "opencode://project_hash",
            None,
            0,
            20,
        )
        .await
        .unwrap();
        assert_eq!(page.offline, Some(true));
        assert_eq!(page.sessions.len(), 1);
        assert_eq!(page.sessions[0].actual_session_id, "ses_test123");
    }

    #[test]
    #[serial_test::serial]
    fn test_get_offline_session_fallback() {
        let _sandbox = crate::test_utils::SandboxHome::new();
        let conn = crate::cache::open_connection().expect("open conn");

        // Save a mock remote session to SQLite
        let session = ClaudeSession {
            session_id: "remote://http://100.93.94.80:3728#opencode://ses_test123".to_string(),
            actual_session_id: "ses_test123".to_string(),
            file_path: "remote://http://100.93.94.80:3728#opencode://project_hash/ses_test123"
                .to_string(),
            project_name: "test-proj".to_string(),
            message_count: 42,
            first_message_time: "2026-09-05T08:00:00Z".to_string(),
            last_message_time: "2026-09-06T17:00:00Z".to_string(),
            last_modified: "2026-09-06T17:00:00Z".to_string(),
            has_tool_use: false,
            has_errors: false,
            summary: Some("Test Offline Session Summary".to_string()),
            is_renamed: false,
            provider: Some("opencode".to_string()),
            storage_type: None,
            entrypoint: None,
        };

        crate::cache::save_sessions(
            &conn,
            "remote://http://100.93.94.80:3728#opencode://project_hash",
            "opencode",
            &[session],
        )
        .expect("save session");

        // Query offline session fallback
        let fallback = get_offline_session_fallback(
            &conn,
            "remote://http://100.93.94.80:3728#opencode://project_hash/ses_test123",
            "http://100.93.94.80:3728",
            "opencode",
            "opencode://project_hash/ses_test123",
        );

        assert!(fallback.is_some(), "Fallback message should be created");
        let msg = fallback.unwrap();
        assert_eq!(msg.message_type, "assistant");
        assert_eq!(msg.role.as_deref(), Some("assistant"));
        let text = msg.content.unwrap().to_string();
        assert!(text.contains("Remote Host Offline"));
        assert!(text.contains("ses_test123"));
        assert!(text.contains("Test Offline Session Summary"));
        assert!(text.contains("42 messages"));
    }

    fn remote_snapshot_jsonl() -> Vec<u8> {
        [
            serde_json::json!({
                "uuid": "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
                "sessionId": "remote-preserved",
                "timestamp": "2026-09-01T00:00:00Z",
                "type": "user",
                "cwd": "/remote/work",
                "message": {"role": "user", "content": "Remote hello"},
            })
            .to_string(),
            serde_json::json!({
                "uuid": "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
                "parentUuid": "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
                "sessionId": "remote-preserved",
                "timestamp": "2026-09-01T00:01:00Z",
                "type": "assistant",
                "cwd": "/remote/work",
                "message": {
                    "role": "assistant",
                    "content": [{"type": "text", "text": "Remote reply"}],
                    "model": "claude-opus-4-1",
                },
            })
            .to_string(),
        ]
        .join("\n")
        .into_bytes()
    }

    fn seed_remote_snapshot(endpoint: &str, remote_root: &str) -> crate::storage::Source {
        let source = crate::storage::Source::remote_with_root(
            "claude",
            endpoint,
            remote_root,
            Some("test-host"),
        );
        let files = vec![crate::storage::RemoteSyncedFile {
            path: "projects/proj/session.jsonl".to_string(),
            bytes: remote_snapshot_jsonl(),
            mtime_secs: 1_700_000_000,
        }];
        match crate::storage::sync_remote_files_to_snapshot(&source, remote_root, files).unwrap() {
            crate::storage::SyncOutcome::Created(s) => {
                assert_eq!(s.manifest.original_root.as_deref(), Some(remote_root));
            }
            crate::storage::SyncOutcome::Unchanged(_) => panic!("seed must create"),
        }
        source
    }

    /// Remote machine disappears after sync: projects/sessions/messages stay
    /// readable from preserved snapshots without any network.
    #[tokio::test]
    #[serial_test::serial]
    async fn remote_snapshot_survives_host_offline() {
        let _sandbox = crate::test_utils::SandboxHome::new();
        // Unroutable loopback port: connection refused immediately, no 3s hang.
        let endpoint = "http://127.0.0.1:9";
        let remote_root = "/remote/.claude";
        seed_remote_snapshot(endpoint, remote_root);

        let host = RemoteHostConfig {
            id: "test-host".to_string(),
            name: "test-host".to_string(),
            endpoint: endpoint.to_string(),
            auth_token: None,
            enabled: true,
        };
        // Scan: file sync fails (offline) but snapshots answer.
        let projects = scan_remote_projects(&host, &["claude".to_string()])
            .await
            .expect("offline scan from snapshots");
        assert_eq!(projects.len(), 1);
        assert!(projects[0].path.starts_with("remote://"));

        let inner_project = format!("{remote_root}/projects/proj");
        let sessions = load_remote_sessions(endpoint, "claude", &inner_project, None)
            .await
            .expect("offline sessions from snapshots");
        assert_eq!(sessions.len(), 1);
        assert!(sessions[0].file_path.starts_with("remote://"));

        let inner_session = format!("{remote_root}/projects/proj/session.jsonl");
        let messages = load_remote_messages(endpoint, "claude", &inner_session)
            .await
            .expect("offline messages from snapshots");
        assert!(messages.len() >= 2);

        let page = load_remote_messages_paginated(endpoint, "claude", &inner_session, 0, 200, None)
            .await
            .expect("offline paginated messages from snapshots");
        assert!(!page.messages.is_empty());
    }

    /// Removing a host config is "stop syncing", never "delete history".
    #[tokio::test]
    #[serial_test::serial]
    async fn removing_remote_host_keeps_snapshots() {
        let _sandbox = crate::test_utils::SandboxHome::new();
        let endpoint = "http://127.0.0.1:10";
        let remote_root = "/remote/.claude";
        let source = seed_remote_snapshot(endpoint, remote_root);
        let snapshot_id = crate::storage::latest_completed_snapshot(&source.id)
            .expect("snapshot")
            .snapshot_id;

        // "Remove" the host: drop the config entirely. Nothing deletes archive
        // data; the snapshot and its registry entry remain.
        drop(source);
        let sources = crate::storage::find_remote_sources(endpoint, "claude");
        assert_eq!(sources.len(), 1);
        let latest = crate::storage::latest_completed_snapshot(&sources[0].0.id)
            .expect("snapshot survives host removal");
        assert_eq!(latest.snapshot_id, snapshot_id);
        assert!(latest
            .data_path
            .join("projects/proj/session.jsonl")
            .is_file());

        // Still readable without the host.
        let sessions = load_remote_sessions(
            endpoint,
            "claude",
            &format!("{remote_root}/projects/proj"),
            None,
        )
        .await
        .expect("sessions after host removal");
        assert_eq!(sessions.len(), 1);
    }

    /// A failed file sync never replaces the latest completed snapshot.
    #[tokio::test]
    #[serial_test::serial]
    async fn failed_remote_file_sync_keeps_current() {
        let _sandbox = crate::test_utils::SandboxHome::new();
        let endpoint = "http://127.0.0.1:11";
        let remote_root = "/remote/.claude";
        let source = seed_remote_snapshot(endpoint, remote_root);
        let before = crate::storage::latest_completed_snapshot(&source.id)
            .expect("snapshot")
            .snapshot_id;

        let host = RemoteHostConfig {
            id: "dead-host".to_string(),
            name: "dead-host".to_string(),
            endpoint: endpoint.to_string(),
            auth_token: None,
            enabled: true,
        };
        assert!(sync_remote_host_files(&host).await.is_err());
        let after = crate::storage::latest_completed_snapshot(&source.id)
            .expect("snapshot")
            .snapshot_id;
        assert_eq!(before, after);
    }
}

// One bounded, sequential download per host; scans remain responsive. A failed
// transfer is retried on a later scan and never marks a session synchronized.
lazy_static::lazy_static! {
    static ref OFFLINE_SYNC: Mutex<HashMap<String, (bool, Instant)>> = Mutex::new(HashMap::new());
}
fn schedule_offline_sync(host: RemoteHostConfig, projects: Vec<ClaudeProject>) {
    {
        let Ok(mut jobs) = OFFLINE_SYNC.lock() else {
            return;
        };
        if jobs
            .get(&host.endpoint)
            .is_some_and(|(running, time)| *running || time.elapsed() < Duration::from_secs(60))
        {
            return;
        }
        jobs.insert(host.endpoint.clone(), (true, Instant::now()));
    }
    tauri::async_runtime::spawn(async move {
        if let Err(error) = sync_remote_history(&host, &projects).await {
            log::warn!("Offline sync for {} paused: {error}", host.name);
        }
        if let Ok(mut jobs) = OFFLINE_SYNC.lock() {
            jobs.insert(host.endpoint, (false, Instant::now()));
        }
    });
}

async fn sync_remote_history(
    host: &RemoteHostConfig,
    projects: &[ClaudeProject],
) -> Result<(), String> {
    let client = create_client();
    let conn = crate::cache::open_connection()?;
    conn.execute_batch("CREATE TABLE IF NOT EXISTS remote_session_sync (session_path TEXT NOT NULL, provider TEXT NOT NULL, last_modified TEXT NOT NULL, PRIMARY KEY(session_path, provider))").map_err(|e| e.to_string())?;
    for project in projects {
        if project.path.contains("?status=unavailable#") {
            continue;
        }
        let Some((_, inner)) = parse_remote_path(&project.path) else {
            continue;
        };
        let provider = project.provider.as_deref().unwrap_or("claude");
        let mut request = client
            .post(format!(
                "{}/api/load_provider_sessions",
                host.endpoint.trim_end_matches('/')
            ))
            .json(&LoadSessionsPayload {
                provider,
                project_path: inner,
                exclude_sidechain: Some(false),
                offset: None,
                limit: None,
            });
        if let Some(token) = &host.auth_token {
            request = request.bearer_auth(token);
        }
        let mut sessions: Vec<ClaudeSession> = request
            .send()
            .await
            .map_err(|e| e.to_string())?
            .error_for_status()
            .map_err(|e| e.to_string())?
            .json()
            .await
            .map_err(|e| e.to_string())?;
        for session in &mut sessions {
            session.file_path = format_remote_path(&host.endpoint, &session.file_path);
            session.session_id = format_remote_path(&host.endpoint, &session.session_id);
        }
        crate::cache::save_sessions(
            &conn,
            &format_remote_path(&host.endpoint, inner),
            provider,
            &sessions,
        )?;
        for session in sessions {
            let synced: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM remote_session_sync WHERE session_path=?1 AND provider=?2 AND last_modified=?3)", rusqlite::params![session.file_path, provider, session.last_modified], |row| row.get(0)).map_err(|e| e.to_string())?;
            if synced {
                continue;
            }
            let Some((_, session_inner)) = parse_remote_path(&session.file_path) else {
                continue;
            };
            let mut request = client
                .post(format!(
                    "{}/api/load_provider_messages",
                    host.endpoint.trim_end_matches('/')
                ))
                .json(&LoadMessagesPayload {
                    provider,
                    session_path: session_inner,
                });
            if let Some(token) = &host.auth_token {
                request = request.bearer_auth(token);
            }
            let messages: Vec<ClaudeMessage> = request
                .send()
                .await
                .map_err(|e| e.to_string())?
                .error_for_status()
                .map_err(|e| e.to_string())?
                .json()
                .await
                .map_err(|e| e.to_string())?;
            crate::cache::save_session_messages(&conn, &session.file_path, provider, &messages)?;
            conn.execute("INSERT OR REPLACE INTO remote_session_sync(session_path,provider,last_modified) VALUES (?1,?2,?3)", rusqlite::params![session.file_path, provider, session.last_modified]).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}
