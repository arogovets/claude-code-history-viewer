//! Source model: stable identity for preservation units.
//!
//! A `Source` is one physical preservation unit — not merely a provider. One
//! logical provider may own several sources (e.g. Copilot CLI vs VS Code
//! storage, Cursor global vs workspace databases, per-editor Cline roots).
//!
//! Identity (v1) is a canonical, fully specified string hashed with SHA-256:
//! ```text
//! cchv-source-v1
//! machine=<machine id, local uuid | wsl:<distro> | remote uuid>
//! kind=<local|custom|remote|wsl>
//! provider=<provider id>
//! role=<storage role, e.g. primary|cli|vscode|global>
//! origin=<canonical origin: absolute path | distro|UNC | remote root>
//! ```
//! The directory id is `{provider}-{role}-{16 hex chars}`: human-scannable,
//! filesafe, and stable across restarts, rebuilds, and endpoint changes.
//! Endpoints stay mutable connection metadata and never enter the id.
//!
//! Compatibility with f94c008-era directories (`local-{provider}`,
//! `custom-{provider}-<defaulthash>`, `remote-<endpoint>-{provider}[-<hash>]`,
//! which used the implementation-defined `DefaultHasher`): those directories
//! are adopted, never recreated. [`claim_canonical_source`] matches them by
//! stored fields (provider/kind/origin/endpoint), renames the directory to
//! the canonical id, and records an alias so old references keep resolving.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::storage::hash::sha256_hex_str;

/// Identity scheme version embedded in the hashed canonical string.
pub const SOURCE_IDENTITY_VERSION: u32 = 1;

/// Default storage role for single-source providers.
pub const ROLE_PRIMARY: &str = "primary";

/// Where the bytes originally live.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SourceKind {
    Local,
    Custom,
    Remote,
    Wsl,
}

impl SourceKind {
    #[allow(clippy::inherent_to_string)]
    #[must_use]
    pub fn to_string(&self) -> String {
        match self {
            Self::Local => "local".to_string(),
            Self::Custom => "custom".to_string(),
            Self::Remote => "remote".to_string(),
            Self::Wsl => "wsl".to_string(),
        }
    }
}

fn default_role() -> String {
    ROLE_PRIMARY.to_string()
}

/// Keep an id-prefix segment filesafe: lowercase alphanumerics and dashes.
fn sanitize_id_segment(segment: &str) -> String {
    let cleaned: String = segment
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let trimmed = cleaned.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "x".to_string()
    } else {
        trimmed
    }
}

/// Canonical identity string. Every field is explicit; concatenation is
/// newline-delimited with a version header so origins containing newlines or
/// separators cannot create ambiguous identities.
#[must_use]
pub fn canonical_identity(
    machine_id: &str,
    kind: &SourceKind,
    provider: &str,
    role: &str,
    origin: &str,
) -> String {
    format!(
        "cchv-source-v{SOURCE_IDENTITY_VERSION}\nmachine={}\nkind={}\nprovider={provider}\nrole={role}\norigin={origin}",
        machine_id.trim(),
        kind.to_string(),
        provider = provider.trim(),
        role = role.trim(),
    )
}

/// Directory id for a canonical identity. Deterministic across processes,
/// toolchains, and restarts (SHA-256, never `DefaultHasher`).
#[must_use]
pub fn canonical_source_id(
    machine_id: &str,
    kind: &SourceKind,
    provider: &str,
    role: &str,
    origin: &str,
) -> String {
    let hash = sha256_hex_str(&canonical_identity(
        machine_id, kind, provider, role, origin,
    ));
    format!(
        "{}-{}-{}",
        sanitize_id_segment(provider),
        sanitize_id_segment(role),
        &hash[..16]
    )
}

/// Lexically normalize an absolute local path into a canonical origin string
/// (no trailing separator, no `.` segments). Does not touch the filesystem.
#[must_use]
pub fn canonical_path_origin(path: &Path) -> String {
    let mut out = String::new();
    for component in path.components() {
        use std::path::Component as C;
        match component {
            C::Prefix(prefix) => out.push_str(&prefix.as_os_str().to_string_lossy()),
            C::RootDir => out.push('/'),
            C::CurDir => {}
            C::ParentDir => {
                // Lexical pop; a leading `..` that escapes is kept literally
                // so distinct inputs never collapse to one identity.
                if out.ends_with('/') && out.len() > 1 {
                    out.pop();
                }
                match out.rfind('/') {
                    Some(0) => out.truncate(1),
                    Some(idx) => out.truncate(idx),
                    None if out.is_empty() => out.push_str(".."),
                    None => out.push_str("/.."),
                }
            }
            C::Normal(part) => {
                if !out.ends_with('/') && !out.is_empty() {
                    out.push('/');
                }
                out.push_str(&part.to_string_lossy());
            }
        }
    }
    if out.len() > 1 {
        while out.ends_with('/') {
            out.pop();
        }
    }
    if out.is_empty() {
        out.push('/');
    }
    out
}

/// A syncable preservation unit. The `id` is deterministic so restarts and
/// parallel workers agree without a central registry write.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Source {
    pub id: String,
    pub kind: SourceKind,
    pub provider: String,
    /// Storage role within the provider (`primary`, `cli`, `vscode`, ...).
    #[serde(default = "default_role")]
    pub role: String,
    /// Owning machine: local install uuid, `wsl:<distro>`, or remote uuid
    /// (`""` = not yet learned, pre-machine-id era).
    #[serde(default)]
    pub machine_id: String,
    /// Canonical origin the id was derived from (absolute path, remote root,
    /// or `distro|UNC`).
    #[serde(default)]
    pub origin: String,
    /// Absolute original root for local/custom/wsl sources.
    /// `None` for remote sources, where bytes arrive over the network.
    pub original_root: Option<String>,
    /// Remote endpoint for remote sources (mutable connection metadata).
    pub endpoint: Option<String>,
    /// Human label (host name, custom label, ...).
    pub label: Option<String>,
    /// Snapshot-relative database paths needing consistent backup capture.
    /// Empty means pure file-tree capture.
    #[serde(default)]
    pub sqlite_dbs: Vec<String>,
    /// Snapshot-relative subtree prefixes to capture. Empty means the whole
    /// root (used to bound large roots like a VS Code `User` dir).
    #[serde(default)]
    pub includes: Vec<String>,
    /// Maximum capture depth below the root (`None` = unbounded).
    #[serde(default)]
    pub max_depth: Option<usize>,
}

fn normalize_endpoint(endpoint: &str) -> String {
    endpoint.trim().trim_end_matches('/').to_string()
}

impl Source {
    fn canonical(
        kind: SourceKind,
        provider: &str,
        role: &str,
        machine_id: &str,
        origin: &str,
    ) -> Self {
        let role = if role.trim().is_empty() {
            ROLE_PRIMARY
        } else {
            role.trim()
        };
        Self {
            id: canonical_source_id(machine_id, &kind, provider, role, origin),
            kind,
            provider: provider.to_string(),
            role: role.to_string(),
            machine_id: machine_id.trim().to_string(),
            origin: origin.to_string(),
            original_root: None,
            endpoint: None,
            label: None,
            sqlite_dbs: Vec::new(),
            includes: Vec::new(),
            max_depth: None,
        }
    }

    /// Canonical local source (e.g. `~/.claude`).
    #[must_use]
    pub fn local(provider: &str, machine_id: &str, original_root: &Path) -> Self {
        let origin = canonical_path_origin(original_root);
        let mut source = Self::canonical(
            SourceKind::Local,
            provider,
            ROLE_PRIMARY,
            machine_id,
            &origin,
        );
        source.original_root = Some(original_root.to_string_lossy().to_string());
        source
    }

    /// User-configured extra directory for a provider.
    #[must_use]
    pub fn custom(
        provider: &str,
        machine_id: &str,
        original_root: &Path,
        label: Option<&str>,
    ) -> Self {
        let origin = canonical_path_origin(original_root);
        let mut source = Self::canonical(
            SourceKind::Custom,
            provider,
            ROLE_PRIMARY,
            machine_id,
            &origin,
        );
        source.original_root = Some(original_root.to_string_lossy().to_string());
        source.label = label.map(str::to_string);
        source
    }

    /// Canonical remote source: identity binds the remote machine + root, and
    /// the endpoint stays mutable metadata.
    #[must_use]
    pub fn remote_canonical(
        provider: &str,
        role: &str,
        remote_machine_id: &str,
        remote_root: &str,
        endpoint: &str,
        label: Option<&str>,
    ) -> Self {
        let origin = remote_root.trim().trim_end_matches('/').to_string();
        let mut source = Self::canonical(
            SourceKind::Remote,
            provider,
            role,
            remote_machine_id,
            &origin,
        );
        source.endpoint = Some(normalize_endpoint(endpoint));
        source.label = label.map(str::to_string);
        source
    }

    /// Legacy-compatible remote source for hosts whose machine id is not yet
    /// known (old servers without `/api/machine-id`). The origin falls back
    /// to the normalized endpoint; once the machine id is learned the lineage
    /// is adopted into the canonical id via aliases.
    #[must_use]
    pub fn remote(provider: &str, endpoint: &str, label: Option<&str>) -> Self {
        let origin = normalize_endpoint(endpoint).to_lowercase();
        let mut source = Self::canonical(SourceKind::Remote, provider, ROLE_PRIMARY, "", &origin);
        source.endpoint = Some(normalize_endpoint(endpoint));
        source.label = label.map(str::to_string);
        source
    }

    /// Legacy-compatible remote source for one provider root. Prefer
    /// [`Source::remote_canonical`] once the machine id is known.
    #[must_use]
    pub fn remote_with_root(
        provider: &str,
        endpoint: &str,
        remote_root: &str,
        label: Option<&str>,
    ) -> Self {
        if remote_root.trim().is_empty() {
            return Self::remote(provider, endpoint, label);
        }
        let origin = format!(
            "{}|{}",
            normalize_endpoint(endpoint).to_lowercase(),
            remote_root.trim().trim_end_matches('/')
        );
        let mut source = Self::canonical(SourceKind::Remote, provider, ROLE_PRIMARY, "", &origin);
        source.endpoint = Some(normalize_endpoint(endpoint));
        source.label = label.map(str::to_string);
        source
    }

    /// WSL source: observed from this machine, owned by the distro.
    #[must_use]
    pub fn wsl(
        provider: &str,
        role: &str,
        distro: &str,
        unc_root: &Path,
        label: Option<&str>,
    ) -> Self {
        let machine = crate::storage::machine::wsl_machine_id(distro);
        let unc = unc_root.to_string_lossy().trim().to_string();
        let origin = format!("{}|{unc}", distro.trim());
        let mut source = Self::canonical(SourceKind::Wsl, provider, role, &machine, &origin);
        source.original_root = Some(unc);
        source.label = label.map(str::to_string);
        source
    }

    /// Filesystem root owning this source's snapshots.
    #[must_use]
    pub fn storage_dir(&self, data_root: &Path) -> PathBuf {
        data_root.join("sources").join(&self.id)
    }
}

// ---------------------------------------------------------------------------
// Alias table + lineage adoption.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct AliasTable {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    aliases: std::collections::HashMap<String, String>,
}

fn aliases_path(data_root: &Path) -> PathBuf {
    data_root.join("aliases.json")
}

fn read_aliases(data_root: &Path) -> AliasTable {
    std::fs::read(aliases_path(data_root))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn write_aliases(data_root: &Path, table: &AliasTable) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(table)
        .map_err(|e| format!("Failed to encode alias table: {e}"))?;
    let path = aliases_path(data_root);
    let tmp = path.with_extension(format!(
        "json.{}.tmp",
        &uuid::Uuid::new_v4().to_string()[..8]
    ));
    std::fs::write(&tmp, &bytes).map_err(|e| format!("Failed to write alias staging file: {e}"))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("Failed to publish alias table: {e}"))?;
    Ok(())
}

/// Resolve an old source id to its current canonical id through the alias
/// table. Unknown ids pass through unchanged. Cycle-safe.
#[must_use]
pub fn resolve_source_id(data_root: &Path, id: &str) -> String {
    let table = read_aliases(data_root);
    let mut current = id.to_string();
    for _ in 0..8 {
        match table.aliases.get(&current) {
            Some(next) if next != &current => current = next.clone(),
            _ => break,
        }
    }
    current
}

/// Record `old_id → new_id` (no-op when equal). Never removes history.
pub fn record_alias(data_root: &Path, old_id: &str, new_id: &str) -> Result<(), String> {
    if old_id == new_id {
        return Ok(());
    }
    let mut table = read_aliases(data_root);
    // Keep the first (oldest) mapping target stable: re-pointing an existing
    // alias would orphan whatever adopted it.
    table
        .aliases
        .entry(old_id.to_string())
        .or_insert_with(|| new_id.to_string());
    table.version = 1;
    write_aliases(data_root, &table)
}

/// Rename a source directory to a new id and record the alias. Content is
/// preserved verbatim; only the directory name changes. If the target already
/// exists, only the alias is recorded and both lineages stay readable.
pub fn adopt_source_dir(data_root: &Path, old_id: &str, new_id: &str) -> Result<(), String> {
    if old_id == new_id {
        return Ok(());
    }
    let old_dir = data_root.join("sources").join(old_id);
    let new_dir = data_root.join("sources").join(new_id);
    if !old_dir.is_dir() {
        return record_alias(data_root, old_id, new_id);
    }
    if new_dir.exists() {
        // Both lineages exist: keep both readable, alias old references to the
        // canonical one for future writes.
        return record_alias(data_root, old_id, new_id);
    }
    std::fs::rename(&old_dir, &new_dir).map_err(|e| {
        format!(
            "Failed to adopt source directory {}: {e}",
            old_dir.display()
        )
    })?;
    record_alias(data_root, old_id, new_id)
}

/// Whether a stored source record belongs to the same lineage as `desired`.
/// Old f94c008 records (no machine/origin/role) match by their stored
/// provider/kind/endpoint/original-root fields against the desired origin.
fn lineage_matches(stored: &Source, desired: &Source, data_root: &Path) -> bool {
    if stored.provider != desired.provider || stored.kind != desired.kind {
        return false;
    }
    // Machine: hard mismatch only when both sides know and disagree.
    if !stored.machine_id.is_empty()
        && !desired.machine_id.is_empty()
        && stored.machine_id != desired.machine_id
    {
        return false;
    }
    match desired.kind {
        SourceKind::Local | SourceKind::Custom | SourceKind::Wsl => {
            let desired_root = desired.original_root.as_deref().unwrap_or(&desired.origin);
            let stored_root = stored.original_root.as_deref().unwrap_or("");
            if stored_root.is_empty() {
                return false;
            }
            canonical_path_origin(Path::new(stored_root))
                == canonical_path_origin(Path::new(desired_root))
        }
        SourceKind::Remote => {
            let endpoints_match = match (stored.endpoint.as_deref(), desired.endpoint.as_deref()) {
                (Some(a), Some(b)) => normalize_endpoint(a) == normalize_endpoint(b),
                _ => false,
            };
            if !endpoints_match {
                return false;
            }
            // Distinguish multiple roots per endpoint via the latest synced
            // root recorded in the manifest.
            let stored_root = latest_manifest_root(data_root, &stored.id);
            match (stored_root, desired_origin_root(desired)) {
                (Some(have), Some(want)) => roots_equal(&have, &want),
                // Snapshot-less or legacy dirs with no recorded root: adoptable
                // only when unambiguous (handled by the caller counting).
                _ => true,
            }
        }
    }
}

/// Separable remote root for lineage matching, if the desired origin is one.
/// Legacy endpoint-derived origins (`remote()`/`remote_with_root()` forms)
/// carry no separable root and match by endpoint alone.
fn desired_origin_root(desired: &Source) -> Option<String> {
    // Legacy endpoint-derived origins carry no separable root: they contain
    // separators, or (for the plain-endpoint form) match the normalized
    // endpoint while the machine is still unknown.
    let legacy = desired.origin.is_empty()
        || desired.origin.contains('|')
        || desired.origin.contains("://")
        || (desired.machine_id.is_empty()
            && desired.origin == normalize_endpoint(&desired.origin).to_lowercase());
    if legacy {
        None
    } else {
        Some(desired.origin.clone())
    }
}

fn roots_equal(a: &str, b: &str) -> bool {
    a.trim().trim_end_matches('/') == b.trim().trim_end_matches('/')
}

fn latest_manifest_root(data_root: &Path, source_id: &str) -> Option<String> {
    let snapshots = crate::storage::snapshot::list_snapshots_in_root(data_root, source_id);
    snapshots
        .into_iter()
        .next()
        .and_then(|snap| snap.manifest.original_root)
}

/// Ensure the canonical source directory exists and adopt any older lineage
/// directory into it. Returns the effective source (canonical id, refreshed
/// mutable metadata). Never deletes history.
pub fn claim_canonical_source(desired: &Source) -> Result<Source, String> {
    let root = crate::storage::snapshot::data_root()?;
    // Fast path: canonical dir already registered.
    let canonical_dir = root.join("sources").join(&desired.id);
    if let Ok(bytes) = std::fs::read(canonical_dir.join("source.json")) {
        if let Ok(mut stored) = serde_json::from_slice::<Source>(&bytes) {
            if stored.id == desired.id {
                // Refresh mutable metadata (labels, endpoints) without
                // touching identity or history.
                let mut dirty = false;
                if desired.label.is_some() && stored.label != desired.label {
                    stored.label.clone_from(&desired.label);
                    dirty = true;
                }
                if desired.endpoint.is_some() && stored.endpoint != desired.endpoint {
                    stored.endpoint.clone_from(&desired.endpoint);
                    dirty = true;
                }
                if dirty {
                    let _ = write_source_json(&canonical_dir, &stored);
                }
                return Ok(stored);
            }
        }
    }
    // Adoption path: find older lineage dirs by stored fields.
    let sources_dir = root.join("sources");
    let mut candidates: Vec<(String, Source)> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&sources_dir) {
        for entry in entries.flatten() {
            let dir = entry.path();
            if !dir.is_dir() {
                continue;
            }
            let Some(dir_id) = dir.file_name().and_then(|n| n.to_str()).map(str::to_string) else {
                continue;
            };
            if resolve_source_id(&root, &dir_id) == desired.id {
                continue;
            }
            let Ok(bytes) = std::fs::read(dir.join("source.json")) else {
                continue;
            };
            let Ok(stored) = serde_json::from_slice::<Source>(&bytes) else {
                continue;
            };
            if lineage_matches(&stored, desired, &root) {
                candidates.push((dir_id, stored));
            }
        }
    }
    // Prefer candidates that already hold snapshots; snapshot-less duplicates
    // are only adopted when unambiguous.
    candidates.sort_by_key(|(id, _)| {
        std::cmp::Reverse(crate::storage::snapshot::list_snapshots_in_root(&root, id).len())
    });
    let chosen = candidates.into_iter().next().map(|(id, _)| id);
    if let Some(old_id) = chosen {
        let has_history =
            !crate::storage::snapshot::list_snapshots_in_root(&root, &old_id).is_empty();
        let ambiguous_snapshotless = !has_history && canonical_dir.exists();
        if ambiguous_snapshotless {
            record_alias(&root, &old_id, &desired.id)?;
        } else {
            adopt_source_dir(&root, &old_id, &desired.id)?;
        }
    }
    let mut effective = desired.clone();
    effective.id = resolve_source_id(&root, &effective.id);
    write_source_json(&root.join("sources").join(&effective.id), &effective)?;
    Ok(effective)
}

fn write_source_json(dir: &Path, source: &Source) -> Result<(), String> {
    std::fs::create_dir_all(dir)
        .map_err(|e| format!("Failed to create source dir {}: {e}", dir.display()))?;
    let bytes =
        serde_json::to_vec_pretty(source).map_err(|e| format!("Failed to encode source: {e}"))?;
    let path = dir.join("source.json");
    let tmp = dir.join(format!(
        ".source.{}.tmp",
        &uuid::Uuid::new_v4().to_string()[..8]
    ));
    std::fs::write(&tmp, &bytes)
        .map_err(|e| format!("Failed to write source staging file: {e}"))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("Failed to publish source record: {e}"))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Remote endpoint → machine map (endpoints are mutable connection metadata).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct RemoteMachineTable {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    endpoints: std::collections::HashMap<String, String>,
}

fn remote_machines_path(data_root: &Path) -> PathBuf {
    data_root.join("remote-machines.json")
}

/// Remember which machine id was learned for an endpoint.
pub fn record_remote_machine(
    data_root: &Path,
    endpoint: &str,
    machine_id: &str,
) -> Result<(), String> {
    let path = remote_machines_path(data_root);
    let mut table: RemoteMachineTable = std::fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    table.version = 1;
    table
        .endpoints
        .insert(normalize_endpoint(endpoint), machine_id.trim().to_string());
    let bytes = serde_json::to_vec_pretty(&table)
        .map_err(|e| format!("Failed to encode machine map: {e}"))?;
    let tmp = path.with_extension(format!(
        "json.{}.tmp",
        &uuid::Uuid::new_v4().to_string()[..8]
    ));
    std::fs::write(&tmp, &bytes).map_err(|e| format!("Failed to write machine map: {e}"))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("Failed to publish machine map: {e}"))?;
    Ok(())
}

/// Look up a previously learned machine id for an endpoint.
#[must_use]
pub fn remote_machine_for_endpoint(data_root: &Path, endpoint: &str) -> Option<String> {
    let table: RemoteMachineTable = std::fs::read(remote_machines_path(data_root))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())?;
    table.endpoints.get(&normalize_endpoint(endpoint)).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[serial_test::serial]
    fn canonical_ids_are_stable_and_specific() {
        let a = canonical_source_id(
            "m1",
            &SourceKind::Local,
            "claude",
            "primary",
            "/home/u/.claude",
        );
        let b = canonical_source_id(
            "m1",
            &SourceKind::Local,
            "claude",
            "primary",
            "/home/u/.claude",
        );
        assert_eq!(a, b);
        assert!(a.starts_with("claude-primary-"));
        // Different machine, kind, role, or origin all fork the id.
        assert_ne!(
            a,
            canonical_source_id(
                "m2",
                &SourceKind::Local,
                "claude",
                "primary",
                "/home/u/.claude"
            )
        );
        assert_ne!(
            a,
            canonical_source_id(
                "m1",
                &SourceKind::Custom,
                "claude",
                "primary",
                "/home/u/.claude"
            )
        );
        assert_ne!(
            a,
            canonical_source_id(
                "m1",
                &SourceKind::Local,
                "claude",
                "vscode",
                "/home/u/.claude"
            )
        );
        assert_ne!(
            a,
            canonical_source_id(
                "m1",
                &SourceKind::Local,
                "claude",
                "primary",
                "/home/u/.other"
            )
        );
        // Newline/case games in the origin cannot collide or break filesafety.
        let tricky = canonical_source_id(
            "m1",
            &SourceKind::Local,
            "claude",
            "primary",
            "a\nmachine=x",
        );
        assert!(!tricky.contains(':') && !tricky.contains('/') && !tricky.contains('\n'));
    }

    #[test]
    #[serial_test::serial]
    fn canonical_path_origin_normalizes() {
        assert_eq!(canonical_path_origin(Path::new("/a/b/")), "/a/b");
        assert_eq!(canonical_path_origin(Path::new("/a/./b")), "/a/b");
        assert_eq!(canonical_path_origin(Path::new("/")), "/");
    }

    #[test]
    #[serial_test::serial]
    fn legacy_local_dir_is_adopted_not_duplicated() {
        let _sandbox = crate::test_utils::SandboxHome::new();
        let root = crate::storage::snapshot::data_root().unwrap();
        // Simulate an f94c008-era directory with an old-style id and record.
        let legacy = Source {
            id: "local-claude".to_string(),
            kind: SourceKind::Local,
            provider: "claude".to_string(),
            role: "primary".to_string(),
            machine_id: String::new(),
            origin: String::new(),
            original_root: Some("/home/u/.claude".to_string()),
            endpoint: None,
            label: None,
            sqlite_dbs: Vec::new(),
            includes: Vec::new(),
            max_depth: None,
        };
        write_source_json(&root.join("sources").join("local-claude"), &legacy).unwrap();
        std::fs::create_dir_all(root.join("sources").join("local-claude").join("snapshots"))
            .unwrap();

        let desired = Source::local("claude", "machine-1", Path::new("/home/u/.claude"));
        assert_ne!(desired.id, "local-claude");
        let effective = claim_canonical_source(&desired).unwrap();
        assert_eq!(effective.id, desired.id);
        // Old directory renamed, history preserved, alias recorded.
        assert!(!root.join("sources").join("local-claude").exists());
        assert!(root.join("sources").join(&desired.id).is_dir());
        assert_eq!(resolve_source_id(&root, "local-claude"), desired.id);
    }

    #[test]
    #[serial_test::serial]
    fn claim_is_idempotent_and_refreshes_metadata() {
        let _sandbox = crate::test_utils::SandboxHome::new();
        let first = Source::custom("codex", "m1", Path::new("/data/r"), Some("label-a"));
        let claimed = claim_canonical_source(&first).unwrap();
        assert_eq!(claimed.id, first.id);
        let second = Source::custom("codex", "m1", Path::new("/data/r"), Some("label-b"));
        let relabeled = claim_canonical_source(&second).unwrap();
        assert_eq!(relabeled.id, first.id);
        assert_eq!(relabeled.label.as_deref(), Some("label-b"));
    }
}
