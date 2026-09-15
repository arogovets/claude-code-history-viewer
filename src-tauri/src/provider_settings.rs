//! Source-aware control plane. History mirrors are never read or written here.
use crate::providers::collection::{specs, CollectionSpec, SettingsScope};
use crate::settings_transport::{SettingsTransport, SourceTransport};
use crate::sources::Source;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

pub const REDACTED: &str = "[REDACTED — preserve existing value]";
const LIMIT: usize = 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SettingsRequest {
    pub source_id: String,
    pub provider: String,
    pub scope: String,
    pub project_path: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ApplyRequest {
    pub selection: SettingsRequest,
    pub revision: String,
    pub content: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
// Independent wire-format capabilities, retained as booleans for the frontend.
#[allow(clippy::struct_excessive_bools)]
pub struct SettingsState {
    pub online: bool,
    #[serde(default)]
    pub configuration_required: bool,
    pub live: bool,
    pub write_enabled: bool,
    pub snapshot_at: Option<String>,
    pub revision: Option<String>,
    pub content: Option<String>,
    pub format: String,
    pub origin_path: Option<String>,
    pub reason: Option<String>,
}

pub fn settings_providers() -> Vec<&'static CollectionSpec> {
    specs().iter().filter(|p| p.settings.is_some()).collect()
}

fn scope(request: &SettingsRequest) -> Result<&'static SettingsScope, String> {
    settings_providers()
        .into_iter()
        .find(|p| p.provider.as_str() == request.provider)
        .and_then(|p| p.settings.as_ref())
        .and_then(|s| s.scopes.iter().find(|s| s.id == request.scope))
        .ok_or("Unknown provider settings scope".into())
}

fn absolute(path: &str) -> Result<PathBuf, String> {
    if !path.starts_with('/')
        || path.contains('\\')
        || path.chars().any(char::is_control)
        || path[1..]
            .split('/')
            .any(|p| p.is_empty() || p == "." || p == "..")
    {
        return Err("Invalid authoritative path".into());
    }
    Ok(path.into())
}

/// Translate only recorded project mounts; overlapping mappings must agree.
pub fn project_origin(source: &Source, project: &str) -> Result<PathBuf, String> {
    let project = absolute(project)?;
    let mut candidates = Vec::new();
    for mount in source
        .mounts
        .iter()
        .chain(&source.origin.project_roots)
        .chain(&source.origin.paths)
    {
        if mount.kind != "directory" {
            continue;
        }
        let mirror = source
            .current
            .join(crate::sources::relative_mount(&mount.mirror_path)?);
        if let Ok(suffix) = project.strip_prefix(&mirror) {
            let origin = absolute(&mount.path)?.join(suffix);
            if !candidates.contains(&origin) {
                candidates.push(origin);
            }
        }
    }
    if candidates.len() != 1 {
        return Err("Project origin is unmapped or ambiguous; this scope is read-only".into());
    }
    Ok(candidates.remove(0))
}

fn resolve_path(
    source: &Source,
    request: &SettingsRequest,
    scope: &SettingsScope,
) -> Result<PathBuf, String> {
    let path = match scope.base.as_str() {
        "home" => absolute(
            source
                .origin
                .home
                .as_deref()
                .ok_or("Source has no authoritative HOME")?,
        )?
        .join(crate::sources::relative_mount(&scope.path)?),
        "project" => project_origin(
            source,
            request
                .project_path
                .as_deref()
                .ok_or("Select a mapped project to read this scope")?,
        )?
        .join(crate::sources::relative_mount(&scope.path)?),
        "absolute" if !scope.editable => absolute(&scope.path)?,
        _ => return Err("Invalid provider settings specification".into()),
    };
    // Reject any registered mirror root, including nonexistent paths within it.
    if let Some(root) = crate::sources::root() {
        if path.starts_with(&root) {
            return Err("Provider settings cannot target history mirrors".into());
        }
    }
    if path.starts_with(&source.current) {
        return Err("Provider settings cannot target history mirrors".into());
    }
    Ok(path)
}

/// JSONC permits comments and trailing commas, but not JSON5 literals/keys.
fn strip_jsonc(content: &str) -> Result<String, String> {
    let mut bytes = content.as_bytes().to_vec();
    let mut i = 0;
    let mut string = false;
    while i < bytes.len() {
        if string {
            if bytes[i] == b'\\' {
                i += 2;
                continue;
            }
            if bytes[i] == b'"' {
                string = false;
            }
        } else if bytes[i] == b'"' {
            string = true;
        } else if bytes[i] == b'/' && bytes.get(i + 1) == Some(&b'/') {
            while i < bytes.len() && bytes[i] != b'\n' {
                bytes[i] = b' ';
                i += 1;
            }
            continue;
        } else if bytes[i] == b'/' && bytes.get(i + 1) == Some(&b'*') {
            bytes[i] = b' ';
            bytes[i + 1] = b' ';
            i += 2;
            loop {
                if i + 1 >= bytes.len() {
                    return Err("Unterminated JSONC comment".into());
                }
                if bytes[i] == b'*' && bytes[i + 1] == b'/' {
                    bytes[i] = b' ';
                    bytes[i + 1] = b' ';
                    i += 2;
                    break;
                }
                if bytes[i] != b'\n' {
                    bytes[i] = b' ';
                }
                i += 1;
            }
            continue;
        }
        i += 1;
    }
    i = 0;
    string = false;
    while i < bytes.len() {
        if string {
            if bytes[i] == b'\\' {
                i += 2;
                continue;
            }
            if bytes[i] == b'"' {
                string = false;
            }
        } else if bytes[i] == b'"' {
            string = true;
        } else if bytes[i] == b',' {
            let mut next = i + 1;
            while next < bytes.len() && bytes[next].is_ascii_whitespace() {
                next += 1;
            }
            let previous = bytes[..i].iter().rev().find(|b| !b.is_ascii_whitespace());
            if bytes.get(next).is_some_and(|b| *b == b'}' || *b == b']')
                && previous.is_some_and(|b| !b"{[:,".contains(b))
            {
                bytes[i] = b' ';
            }
        }
        i += 1;
    }
    String::from_utf8(bytes).map_err(|_| "Invalid JSONC encoding".into())
}

fn parse(content: &str, format: &str) -> Result<Value, String> {
    if content.len() > LIMIT {
        return Err("Configuration exceeds 1 MiB".into());
    }
    let value: Value = match format {
        "json" => serde_json::from_str(content).map_err(|_| "Invalid JSON configuration")?,
        "jsonc" => serde_json::from_str(&strip_jsonc(content)?)
            .map_err(|_| "Invalid JSONC configuration")?,
        "toml" => serde_json::to_value(
            toml::from_str::<toml::Value>(content).map_err(|_| "Invalid TOML configuration")?,
        )
        .map_err(|_| "Invalid TOML configuration")?,
        _ => return Err("Unsupported configuration format".into()),
    };
    if !value.is_object() {
        return Err("Configuration must be an object/table".into());
    }
    Ok(value)
}

fn encode(value: &Value, format: &str) -> Result<String, String> {
    if format == "toml" {
        toml::to_string_pretty(value).map_err(|_| "Invalid TOML value".into())
    } else {
        serde_json::to_string_pretty(value).map_err(|_| "Invalid JSON value".into())
    }
}

fn sensitive(key: &str) -> bool {
    let key = key.to_ascii_lowercase().replace(['_', '-'], "");
    [
        "secret",
        "token",
        "password",
        "passwd",
        "apikey",
        "authorization",
        "credential",
        "privatekey",
    ]
    .iter()
    .any(|part| key.contains(part))
        || [
            "env",
            "environment",
            "headers",
            "httpheaders",
            "args",
            "command",
            "apikeyhelper",
        ]
        .contains(&key.as_str())
}

fn secret_string(value: &str) -> bool {
    value.contains("://") && (value.contains('@') || value.contains('?'))
        || value.contains("Bearer ")
        || value.starts_with("sk-")
        || value.contains("-----BEGIN")
}

fn redact_leaves(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), redact_leaves(v)))
                .collect(),
        ),
        Value::Array(values) => Value::Array(values.iter().map(redact_leaves).collect()),
        _ => json!(REDACTED),
    }
}

pub fn sanitize(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, value)| {
                    (
                        key.clone(),
                        if sensitive(key) {
                            redact_leaves(value)
                        } else {
                            sanitize(value)
                        },
                    )
                })
                .collect(),
        ),
        Value::Array(values) => Value::Array(values.iter().map(sanitize).collect()),
        Value::String(s) if secret_string(s) => json!(REDACTED),
        _ => value.clone(),
    }
}

fn contains_redacted(value: &Value) -> bool {
    match value {
        Value::String(s) => s == REDACTED,
        Value::Object(map) => map.values().any(contains_redacted),
        Value::Array(values) => values.iter().any(contains_redacted),
        _ => false,
    }
}

fn masked_equal(origin: &Value, proposed: &Value) -> bool {
    if proposed.as_str() == Some(REDACTED) {
        return true;
    }
    match (origin, proposed) {
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| masked_equal(a, b))
        }
        (Value::Object(a), Value::Object(b)) => {
            a.len() == b.len()
                && a.iter()
                    .all(|(k, v)| b.get(k).is_some_and(|next| masked_equal(v, next)))
        }
        _ => origin == proposed,
    }
}

/// Add/update merge: omitted fields and redacted values always preserve origin.
/// Removing fields is intentionally not inferred from a stale whole-file editor.
fn merge(origin: &mut Value, proposed: &Value) -> Result<(), String> {
    if proposed.as_str() == Some(REDACTED) {
        return Ok(());
    }
    if let (Some(old), Some(new)) = (origin.as_object_mut(), proposed.as_object()) {
        for (key, value) in new {
            if let Some(existing) = old.get_mut(key) {
                merge(existing, value)?;
            } else if !contains_redacted(value) {
                old.insert(key.clone(), value.clone());
            }
        }
        return Ok(());
    }
    if contains_redacted(proposed) {
        if masked_equal(origin, proposed) {
            return Ok(());
        }
        return Err("An edited array contains redacted values; preserve it unchanged".into());
    }
    // Do not implicitly delete nested credentials by replacing their container.
    if (origin.is_object() || origin.is_array())
        && contains_redacted(&sanitize(origin))
        && *origin != *proposed
    {
        return Err("Cannot replace a container holding protected values".into());
    }
    *origin = proposed.clone();
    Ok(())
}

fn validate(value: &Value, provider: &str) -> Result<(), String> {
    // Validate only this provider's known containers, preserving other extensions.
    let fields: &[&str] = match provider {
        "claude" => &["mcpServers", "env", "permissions", "hooks"],
        "codex" => &["mcp_servers", "model_providers", "profiles"],
        "opencode" => &["mcp", "provider", "agent"],
        _ => &[],
    };
    for field in fields {
        if value.get(*field).is_some_and(|v| !v.is_object()) {
            return Err(format!("{field} must be an object/table"));
        }
    }
    if value.get("model").is_some_and(|v| !v.is_string()) {
        return Err("model must be a string".into());
    }
    Ok(())
}

fn key(source: &Source, request: &SettingsRequest) -> String {
    let identity = json!([source.id, source.origin, request]);
    format!("{:x}", Sha256::digest(identity.to_string().as_bytes()))
}

fn snapshot_root() -> Result<PathBuf, String> {
    let home = crate::utils::home_dir().ok_or("Could not resolve settings snapshot directory")?;
    Ok(home.join(".claude-history-viewer/settings-snapshots"))
}

fn persist(root: &Path, key: &str, state: &SettingsState) -> Result<(), String> {
    // Snapshot content is generated exclusively from parsed, sanitized values.
    let mut cursor = root;
    while let Some(parent) = cursor.parent() {
        if cursor.is_symlink() {
            return Err("Snapshot directory cannot contain symlinks".into());
        }
        cursor = parent;
    }
    if crate::sources::root().is_some_and(|mirror| root.starts_with(mirror)) {
        return Err("Settings snapshots cannot be stored inside history mirrors".into());
    }
    std::fs::create_dir_all(root).map_err(|_| "Cannot create settings snapshot directory")?;
    let mut temp =
        tempfile::NamedTempFile::new_in(root).map_err(|_| "Cannot create sanitized snapshot")?;
    use std::io::Write;
    temp.write_all(&serde_json::to_vec(state).map_err(|_| "Cannot encode snapshot")?)
        .map_err(|_| "Cannot write sanitized snapshot")?;
    temp.as_file()
        .sync_all()
        .map_err(|_| "Cannot sync sanitized snapshot")?;
    temp.persist(root.join(format!("{key}.json")))
        .map_err(|_| "Cannot replace sanitized snapshot")?;
    Ok(())
}

fn blank(scope: &SettingsScope, online: bool, reason: String) -> SettingsState {
    SettingsState {
        online,
        configuration_required: false,
        live: false,
        write_enabled: false,
        snapshot_at: None,
        revision: None,
        content: None,
        format: scope.format.clone(),
        origin_path: None,
        reason: Some(reason),
    }
}

pub fn read_with(
    source: &Source,
    request: &SettingsRequest,
    transport: &dyn SettingsTransport,
    snapshots: &Path,
) -> Result<SettingsState, String> {
    if source.id != request.source_id {
        return Err("Source selection mismatch".into());
    }
    let scope = scope(request)?;
    let online = transport.probe().is_ok();
    let path = match resolve_path(source, request, scope) {
        Ok(path) => path,
        Err(reason) => return Ok(blank(scope, online, reason)),
    };
    let result = if online {
        transport.read(&path)
    } else {
        Err("Source offline; no offline edits are queued".into())
    };
    let file = match result {
        Ok(file) => file,
        Err(reason) => {
            let snapshot_path = snapshots.join(format!("{}.json", key(source, request)));
            let mut state = if snapshot_path.is_symlink() {
                None
            } else {
                std::fs::read(snapshot_path)
                    .ok()
                    .and_then(|bytes| serde_json::from_slice::<SettingsState>(&bytes).ok())
            }
            .unwrap_or_else(|| blank(scope, online, reason.clone()));
            // Sanitize again on cache reads, including snapshots from older versions.
            state.content = state
                .content
                .and_then(|s| parse(&s, &scope.format).ok())
                .and_then(|v| encode(&sanitize(&v), &scope.format).ok());
            state.online = online;
            state.live = false;
            state.write_enabled = false;
            state.revision = None;
            state.reason = Some(reason);
            return Ok(state);
        }
    };
    let original =
        match file.bytes {
            Some(bytes) => match String::from_utf8(bytes)
                .ok()
                .and_then(|s| parse(&s, &scope.format).ok())
            {
                Some(value) => value,
                None => return Ok(blank(
                    scope,
                    true,
                    "Origin contains invalid configuration; repair it on the source before editing"
                        .into(),
                )),
            },
            None => json!({}),
        };
    let mut state = SettingsState {
        online: true,
        configuration_required: false,
        live: true,
        write_enabled: source.allow_settings_write && scope.editable,
        snapshot_at: Some(chrono::Utc::now().to_rfc3339()),
        revision: Some(file.revision),
        content: Some(encode(&sanitize(&original), &scope.format)?),
        format: scope.format.clone(),
        origin_path: Some(path.to_string_lossy().into()),
        reason: if !scope.editable {
            Some("Managed scope is read-only".into())
        } else if !source.allow_settings_write {
            Some("Settings writes are disabled for this source".into())
        } else {
            None
        },
    };
    if persist(snapshots, &key(source, request), &state).is_err() {
        state.write_enabled = false;
        state.reason = Some("Sanitized snapshot could not be saved; writes disabled".into());
    }
    Ok(state)
}

pub fn apply_with(
    source: &Source,
    request: &ApplyRequest,
    transport: &dyn SettingsTransport,
    snapshots: &Path,
) -> Result<SettingsState, String> {
    if source.id != request.selection.source_id {
        return Err("Source selection mismatch".into());
    }
    if !source.allow_settings_write {
        return Err("Settings writes are disabled for this source".into());
    }
    let scope = scope(&request.selection)?;
    if !scope.editable {
        return Err("Managed scope is read-only".into());
    }
    let path = resolve_path(source, &request.selection, scope)?;
    transport
        .probe()
        .map_err(|_| "Source unavailable; edits are never queued")?;
    let original = transport.read(&path)?;
    if original.revision != request.revision {
        return Err("Revision conflict: origin changed; refresh and review before applying".into());
    }
    let proposed = parse(&request.content, &scope.format)?;
    let mut value = match original.bytes {
        Some(bytes) => parse(
            std::str::from_utf8(&bytes).map_err(|_| "Invalid configuration encoding")?,
            &scope.format,
        )?,
        None => json!({}),
    };
    merge(&mut value, &proposed)?;
    validate(&value, &request.selection.provider)?;
    let encoded = encode(&value, &scope.format)?;
    parse(&encoded, &scope.format)?;
    // CAS is repeated on the origin inside the atomic replacement operation.
    transport.compare_replace(&path, &request.revision, encoded.as_bytes())?;
    // A separate origin read, never the submitted frontend content, refreshes UI/cache.
    let state = read_with(source, &request.selection, transport, snapshots)?;
    if !state.live || !state.online {
        return Err(
            "Write completed but origin reread failed; refresh before any further edit".into(),
        );
    }
    Ok(state)
}

fn source(request: &SettingsRequest) -> Result<Source, String> {
    crate::sources::inventory()?
        .into_iter()
        .find(|s| s.id == request.source_id)
        .ok_or("Unknown source".into())
}
fn transport(source: &Source) -> Result<SourceTransport, String> {
    Ok(SourceTransport {
        ssh: source.origin.ssh.clone(),
        home: source
            .origin
            .home
            .clone()
            .ok_or("Source has no authoritative HOME")?,
    })
}

#[tauri::command]
pub async fn list_provider_settings() -> Result<Value, String> {
    serde_json::to_value(settings_providers()).map_err(|_| "Invalid settings specifications".into())
}
#[tauri::command]
pub async fn read_provider_settings(selection: SettingsRequest) -> Result<SettingsState, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let source = source(&selection)?;
        let transport = if let Ok(transport) = transport(&source) {
            transport
        } else {
                let mut state = blank(scope(&selection)?, false,
                    "Add an authoritative home to this source’s collector configuration before reading provider settings. Settings writes remain disabled.".into());
                state.configuration_required = true;
                return Ok(state);
        };
        read_with(&source, &selection, &transport, &snapshot_root()?)
    })
    .await
    .map_err(|_| "Settings task failed")?
}
#[tauri::command]
pub async fn apply_provider_settings(request: ApplyRequest) -> Result<SettingsState, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let source = source(&request.selection)?;
        apply_with(&source, &request, &transport(&source)?, &snapshot_root()?)
    })
    .await
    .map_err(|_| "Settings task failed")?
}

/// Native file dialogs and `WebUI` exports cannot be used as a settings backdoor.
pub fn reject_generic_settings_path(path: &Path) -> Result<(), String> {
    if let Ok(canonical) = path.canonicalize() {
        if canonical != path {
            reject_generic_settings_path(&canonical)?;
        }
    }
    for provider in settings_providers() {
        for scope in &provider.settings.as_ref().expect("settings spec").scopes {
            if path.ends_with(&scope.path) {
                return Err(
                    "Use the source-aware provider settings API for configuration files".into(),
                );
            }
        }
    }
    if let Some(root) = crate::sources::root() {
        if path.starts_with(root) {
            return Err("Collected mirrors are read-only".into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings_transport::OriginFile;
    use crate::sources::{Mount, Origin};
    use std::cell::{Cell, RefCell};

    struct Memory {
        online: Cell<bool>,
        bytes: RefCell<Vec<u8>>,
        writes: Cell<usize>,
        reads: Cell<usize>,
        fail: Cell<bool>,
        race: Cell<bool>,
        after: RefCell<Option<Vec<u8>>>,
        paths: RefCell<Vec<PathBuf>>,
    }
    impl Memory {
        fn new(content: &str) -> Self {
            Self {
                online: Cell::new(true),
                bytes: RefCell::new(content.as_bytes().to_vec()),
                writes: Cell::new(0),
                reads: Cell::new(0),
                fail: Cell::new(false),
                race: Cell::new(false),
                after: RefCell::new(None),
                paths: RefCell::new(vec![]),
            }
        }
        fn file(&self) -> OriginFile {
            OriginFile {
                revision: format!("{:x}", Sha256::digest(&*self.bytes.borrow())),
                bytes: Some(self.bytes.borrow().clone()),
            }
        }
    }
    impl SettingsTransport for Memory {
        fn probe(&self) -> Result<(), String> {
            if self.online.get() {
                Ok(())
            } else {
                Err("offline".into())
            }
        }
        fn read(&self, path: &Path) -> Result<OriginFile, String> {
            self.probe()?;
            self.reads.set(self.reads.get() + 1);
            self.paths.borrow_mut().push(path.into());
            Ok(self.file())
        }
        fn compare_replace(
            &self,
            path: &Path,
            revision: &str,
            bytes: &[u8],
        ) -> Result<OriginFile, String> {
            self.probe()?;
            if self.race.get() {
                *self.bytes.borrow_mut() = br#"{"external":true}"#.to_vec();
            }
            if self.file().revision != revision {
                return Err("Revision conflict".into());
            }
            if self.fail.get() {
                return Err("injected write failure".into());
            }
            self.paths.borrow_mut().push(path.into());
            self.writes.set(self.writes.get() + 1);
            *self.bytes.borrow_mut() = bytes.to_vec();
            let result = self.file();
            if let Some(after) = self.after.borrow_mut().take() {
                *self.bytes.borrow_mut() = after;
            }
            Ok(result)
        }
    }
    fn fixture() -> (tempfile::TempDir, Source, SettingsRequest) {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().canonicalize().unwrap();
        let home = base.join("origin");
        std::fs::create_dir(&home).unwrap();
        let source = Source {
            id: "a".into(),
            label: "Same label".into(),
            current: base.join("mirror/a/current"),
            origin: Origin {
                home: Some(home.to_string_lossy().into()),
                ..Origin::default()
            },
            mounts: vec![],
            last_collected_at: None,
            allow_settings_write: true,
        };
        let request = SettingsRequest {
            source_id: source.id.clone(),
            provider: "claude".into(),
            scope: "user".into(),
            project_path: None,
        };
        (dir, source, request)
    }
    fn edit(request: &SettingsRequest, state: &SettingsState, content: &str) -> ApplyRequest {
        ApplyRequest {
            selection: request.clone(),
            revision: state.revision.clone().unwrap(),
            content: content.into(),
        }
    }
    #[test]
    fn specs_resolve_for_both_transports_and_reject_undeclared_paths() {
        let (_dir, source, mut request) = fixture();
        for (provider, expected) in [
            ("claude", ".claude/settings.json"),
            ("antigravity", ".gemini/antigravity-cli/settings.json"),
            ("codex", ".codex/config.toml"),
            ("opencode", ".config/opencode/opencode.json"),
        ] {
            request.provider = provider.into();
            let local = resolve_path(&source, &request, scope(&request).unwrap()).unwrap();
            let mut remote = source.clone();
            remote.origin.ssh = Some("example".into());
            assert_eq!(
                local,
                resolve_path(&remote, &request, scope(&request).unwrap()).unwrap()
            );
            assert!(local.ends_with(expected));
            assert!(reject_generic_settings_path(&local).is_err());
        }
        assert!(specs().iter().any(|s| s.settings.is_none()));
        request.scope = "../../escape".into();
        assert!(scope(&request).is_err());
    }
    #[test]
    fn default_capability_and_managed_scopes_never_write() {
        let (dir, mut source, mut request) = fixture();
        let default: Source = serde_json::from_value(json!({"id":"a","label":"A"})).unwrap();
        assert!(!default.allow_settings_write);
        let transport = Memory::new("{}");
        source.allow_settings_write = false;
        let read = read_with(
            &source,
            &request,
            &transport,
            &dir.path().canonicalize().unwrap(),
        )
        .unwrap();
        assert!(read.online && !read.write_enabled);
        assert!(apply_with(
            &source,
            &edit(&request, &read, "{}"),
            &transport,
            &dir.path().canonicalize().unwrap()
        )
        .is_err());
        source.allow_settings_write = true;
        for id in ["managed_macos", "managed_linux"] {
            request.scope = id.into();
            let read = read_with(
                &source,
                &request,
                &transport,
                &dir.path().canonicalize().unwrap(),
            )
            .unwrap();
            assert!(!read.write_enabled);
            assert!(apply_with(
                &source,
                &edit(&request, &read, "{}"),
                &transport,
                &dir.path().canonicalize().unwrap()
            )
            .is_err());
        }
        assert_eq!(transport.writes.get(), 0);
    }
    #[test]
    fn offline_snapshot_is_sanitized_read_only_and_never_queues() {
        let (dir, source, request) = fixture();
        let transport =
            Memory::new(r#"{"apiKey":"secret-value","env":{"WEIRD":"mcp-secret"},"model":"a"}"#);
        let live = read_with(
            &source,
            &request,
            &transport,
            &dir.path().canonicalize().unwrap(),
        )
        .unwrap();
        transport.online.set(false);
        let offline = read_with(
            &source,
            &request,
            &transport,
            &dir.path().canonicalize().unwrap(),
        )
        .unwrap();
        assert!(!offline.online && !offline.live && !offline.write_enabled);
        assert!(offline.revision.is_none());
        assert_eq!(offline.snapshot_at, live.snapshot_at);
        assert_eq!(offline.content, live.content);
        assert!(apply_with(
            &source,
            &edit(&request, &live, r#"{"model":"b"}"#),
            &transport,
            &dir.path().canonicalize().unwrap()
        )
        .is_err());
        transport.online.set(true);
        assert_eq!(
            read_with(
                &source,
                &request,
                &transport,
                &dir.path().canonicalize().unwrap()
            )
            .unwrap()
            .revision,
            live.revision
        );
        assert_eq!(transport.writes.get(), 0);
        let snapshot = std::fs::read_to_string(
            dir.path()
                .canonicalize()
                .unwrap()
                .join(format!("{}.json", key(&source, &request))),
        )
        .unwrap();
        assert!(!snapshot.contains("secret-value") && !snapshot.contains("mcp-secret"));
    }
    #[test]
    fn apply_preserves_unknown_fields_and_secrets_and_rereads_origin() {
        let (dir, source, request) = fixture();
        let transport = Memory::new(
            r#"{"unknown":{"extension":42},"api_key":"private","mcpServers":{"one":{"env":{"CUSTOM":"hidden"},"args":["--token","value"]}},"model":"a"}"#,
        );
        let read = read_with(
            &source,
            &request,
            &transport,
            &dir.path().canonicalize().unwrap(),
        )
        .unwrap();
        let mut proposed = parse(read.content.as_ref().unwrap(), "json").unwrap();
        proposed["model"] = json!("b");
        let updated = apply_with(
            &source,
            &edit(&request, &read, &proposed.to_string()),
            &transport,
            &dir.path().canonicalize().unwrap(),
        )
        .unwrap();
        let actual: Value = serde_json::from_slice(&transport.bytes.borrow()).unwrap();
        assert_eq!(actual["api_key"], "private");
        assert_eq!(actual["mcpServers"]["one"]["env"]["CUSTOM"], "hidden");
        assert_eq!(actual["unknown"]["extension"], 42);
        assert_eq!(actual["model"], "b");
        assert_eq!(transport.reads.get(), 3);
        assert_eq!(transport.writes.get(), 1);
        assert_ne!(updated.revision, read.revision);
        assert!(transport.paths.borrow().iter().all(|p| p
            .ends_with("origin/.claude/settings.json")
            && !p.starts_with(&source.current)));
        *transport.after.borrow_mut() =
            Some(br#"{"model":"authoritative","token":"new-secret"}"#.to_vec());
        let final_state = apply_with(
            &source,
            &edit(&request, &updated, r#"{"model":"c"}"#),
            &transport,
            &dir.path().canonicalize().unwrap(),
        )
        .unwrap();
        assert!(final_state
            .content
            .as_ref()
            .unwrap()
            .contains("authoritative"));
        assert!(!final_state.content.as_ref().unwrap().contains("new-secret"));
        let snapshot: SettingsState = serde_json::from_slice(
            &std::fs::read(
                dir.path()
                    .canonicalize()
                    .unwrap()
                    .join(format!("{}.json", key(&source, &request))),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(snapshot.content, final_state.content);
    }
    #[test]
    fn invalid_configs_conflicts_and_failed_writes_preserve_origin() {
        let (dir, source, mut request) = fixture();
        for (provider, original, invalid) in [
            ("claude", "{}", "{"),
            ("codex", "model = 'a'", "model = ["),
            ("opencode", "{}", "{'not_json': true}"),
        ] {
            request.provider = provider.into();
            let transport = Memory::new(original);
            let read = read_with(
                &source,
                &request,
                &transport,
                &dir.path().canonicalize().unwrap(),
            )
            .unwrap();
            assert!(apply_with(
                &source,
                &edit(&request, &read, invalid),
                &transport,
                &dir.path().canonicalize().unwrap()
            )
            .is_err());
            assert_eq!(*transport.bytes.borrow(), original.as_bytes());
        }
        request.provider = "claude".into();
        let transport = Memory::new(r#"{"model":"a"}"#);
        let read = read_with(
            &source,
            &request,
            &transport,
            &dir.path().canonicalize().unwrap(),
        )
        .unwrap();
        let change = edit(&request, &read, r#"{"model":"b"}"#);
        transport.fail.set(true);
        assert!(apply_with(
            &source,
            &change,
            &transport,
            &dir.path().canonicalize().unwrap()
        )
        .is_err());
        assert_eq!(transport.file().revision, read.revision.unwrap());
        transport.fail.set(false);
        transport.race.set(true);
        assert!(apply_with(
            &source,
            &change,
            &transport,
            &dir.path().canonicalize().unwrap()
        )
        .unwrap_err()
        .contains("conflict"));
        assert_eq!(*transport.bytes.borrow(), br#"{"external":true}"#);
        assert!(apply_with(
            &source,
            &change,
            &transport,
            &dir.path().canonicalize().unwrap()
        )
        .unwrap_err()
        .contains("conflict"));
        assert_eq!(transport.writes.get(), 0);
    }
    #[test]
    fn project_mapping_is_explicit_unambiguous_and_traversal_safe() {
        let (dir, mut source, mut request) = fixture();
        let origin = dir.path().canonicalize().unwrap().join("workspace");
        source.mounts.push(Mount {
            path: origin.to_string_lossy().into(),
            mirror_path: "projects".into(),
            kind: "directory".into(),
            providers: vec![],
            role: Some("project_root".into()),
            available: None,
        });
        request.scope = "project".into();
        request.project_path = Some(
            source
                .current
                .join("projects/demo")
                .to_string_lossy()
                .into(),
        );
        assert_eq!(
            resolve_path(&source, &request, scope(&request).unwrap()).unwrap(),
            origin.join("demo/.claude/settings.json")
        );
        let mut conflicting = source.mounts[0].clone();
        conflicting.path = "/different".into();
        source.origin.project_roots.push(conflicting);
        assert!(
            !read_with(
                &source,
                &request,
                &Memory::new("{}"),
                &dir.path().canonicalize().unwrap()
            )
            .unwrap()
            .write_enabled
        );
        source.origin.project_roots.clear();
        for project in [
            source.current.join("unmapped"),
            source.current.join("projects/../outside"),
            PathBuf::from("/outside"),
        ] {
            assert!(project_origin(&source, &project.to_string_lossy()).is_err());
        }
    }
    #[test]
    fn equal_providers_on_two_sources_remain_isolated() {
        let (dir, a, ra) = fixture();
        let mut b = a.clone();
        b.id = "b".into();
        b.origin.home = Some("/other/home".into());
        b.allow_settings_write = false;
        let mut rb = ra.clone();
        rb.source_id = b.id.clone();
        let ta = Memory::new(r#"{"model":"a"}"#);
        let tb = Memory::new(r#"{"model":"b"}"#);
        read_with(&a, &ra, &ta, &dir.path().canonicalize().unwrap()).unwrap();
        read_with(&b, &rb, &tb, &dir.path().canonicalize().unwrap()).unwrap();
        ta.online.set(false);
        tb.online.set(false);
        assert!(read_with(&a, &ra, &ta, &dir.path().canonicalize().unwrap())
            .unwrap()
            .content
            .unwrap()
            .contains("\"a\""));
        assert!(read_with(&b, &rb, &tb, &dir.path().canonicalize().unwrap())
            .unwrap()
            .content
            .unwrap()
            .contains("\"b\""));
        assert_ne!(key(&a, &ra), key(&b, &rb));
        assert!(read_with(&a, &rb, &ta, &dir.path().canonicalize().unwrap()).is_err());
    }
    #[test]
    fn jsonc_and_toml_snapshots_drop_secret_comments_and_preserve_values() {
        assert_eq!(
            parse("{ // comment\n \"x\": [1,2,], }", "jsonc").unwrap()["x"],
            json!([1, 2])
        );
        assert!(parse("{unquoted: 'value'}", "jsonc").is_err());
        assert!(parse("{,}", "jsonc").is_err());
        assert!(parse(r#"{"empty":[,]}"#, "jsonc").is_err());
        assert!(validate(&json!({"hooks": []}), "opencode").is_ok());
        assert!(parse("{} /* unterminated", "jsonc").is_err());
        let (dir, source, mut request) = fixture();
        request.provider = "codex".into();
        let transport = Memory::new("# secret-comment\nmodel = 'a'\nunknown = 9\n[mcp_servers.foo.env]\nCUSTOM = 'private-env'\n");
        let read = read_with(
            &source,
            &request,
            &transport,
            &dir.path().canonicalize().unwrap(),
        )
        .unwrap();
        let content = read.content.clone().unwrap();
        assert!(!content.contains("secret-comment") && !content.contains("private-env"));
        apply_with(
            &source,
            &edit(&request, &read, &content.replace("\"a\"", "\"b\"")),
            &transport,
            &dir.path().canonicalize().unwrap(),
        )
        .unwrap();
        let actual = String::from_utf8(transport.bytes.borrow().clone()).unwrap();
        assert!(actual.contains("private-env") && actual.contains("unknown = 9"));
        assert!(validate(&json!({"model":42}), "claude").is_err());
    }
    #[test]
    fn real_apply_writes_origin_and_snapshot_but_never_mirror() {
        let (dir, mut source, request) = fixture();
        let snapshots = dir.path().canonicalize().unwrap().join("snapshots");
        let transport = SourceTransport {
            ssh: None,
            home: source.origin.home.clone().unwrap(),
        };
        let read = read_with(&source, &request, &transport, &snapshots).unwrap();
        assert!(read.write_enabled, "{:?}", read.reason);
        let change = edit(
            &request,
            &read,
            r#"{"model":"test","apiKey":"private-key"}"#,
        );
        source.allow_settings_write = false;
        assert!(apply_with(&source, &change, &transport, &snapshots).is_err());
        let origin = Path::new(source.origin.home.as_ref().unwrap()).join(".claude/settings.json");
        assert!(!origin.exists());
        source.allow_settings_write = true;
        let next = apply_with(&source, &change, &transport, &snapshots).unwrap();
        assert!(next.live && next.online && next.write_enabled);
        assert!(!next.content.unwrap().contains("private-key"));
        assert!(std::fs::read_to_string(&origin)
            .unwrap()
            .contains("private-key"));
        assert!(!source.current.exists());
        let cached =
            std::fs::read_to_string(snapshots.join(format!("{}.json", key(&source, &request))))
                .unwrap();
        assert!(!cached.contains("private-key"));
        source.origin.home = Some(source.current.to_string_lossy().into());
        assert!(resolve_path(&source, &request, scope(&request).unwrap()).is_err());
    }
}
