//! Source model: stable identity for local / custom / remote origins.

use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

/// Where the bytes originally live.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SourceKind {
    Local,
    Custom,
    Remote,
}

impl SourceKind {
    #[allow(clippy::inherent_to_string)]
    #[must_use]
    pub fn to_string(&self) -> String {
        match self {
            Self::Local => "local".to_string(),
            Self::Custom => "custom".to_string(),
            Self::Remote => "remote".to_string(),
        }
    }
}

/// A syncable origin. The `id` is deterministic so restarts and parallel
/// workers agree without a central registry write.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Source {
    pub id: String,
    pub kind: SourceKind,
    pub provider: String,
    /// Absolute original root for local/custom sources (e.g. `~/.claude`).
    /// `None` for remote sources, where bytes arrive over HTTP.
    pub original_root: Option<String>,
    /// Remote endpoint for remote sources (e.g. `http://host:3728`).
    pub endpoint: Option<String>,
    /// Human label (host name, custom label, ...).
    pub label: Option<String>,
}

fn short_hash(input: &str) -> String {
    let mut hasher = DefaultHasher::new();
    input.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn normalize_endpoint(endpoint: &str) -> String {
    endpoint.trim().trim_end_matches('/').to_lowercase()
}

fn sanitize_endpoint(endpoint: &str) -> String {
    let lower = normalize_endpoint(endpoint);
    let stripped = lower
        .strip_prefix("http://")
        .or_else(|| lower.strip_prefix("https://"))
        .unwrap_or(&lower);
    let host_port: String = stripped
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let host_port = host_port.trim_matches('-').to_string();
    // Hash the normalized endpoint so trailing-slash variants share an id.
    let hash = short_hash(&lower);
    if host_port.is_empty() {
        format!("host-{hash}")
    } else if host_port.len() > 48 {
        format!("{}-{}", &host_port[..40], &hash[..8])
    } else {
        format!("{host_port}-{}", &hash[..8])
    }
}

impl Source {
    /// Canonical local source for a provider (e.g. local `~/.claude`).
    #[must_use]
    pub fn local(provider: &str, original_root: &Path) -> Self {
        Self {
            id: format!("local-{provider}"),
            kind: SourceKind::Local,
            provider: provider.to_string(),
            original_root: Some(original_root.to_string_lossy().to_string()),
            endpoint: None,
            label: None,
        }
    }

    /// User-configured extra directory for a provider.
    #[must_use]
    pub fn custom(provider: &str, original_root: &Path, label: Option<&str>) -> Self {
        let root = original_root.to_string_lossy().to_string();
        Self {
            id: format!("custom-{provider}-{}", short_hash(&root)),
            kind: SourceKind::Custom,
            provider: provider.to_string(),
            original_root: Some(root),
            endpoint: None,
            label: label.map(str::to_string),
        }
    }

    /// Remote host source for one provider.
    #[must_use]
    pub fn remote(provider: &str, endpoint: &str, label: Option<&str>) -> Self {
        Self {
            id: format!("remote-{}-{provider}", sanitize_endpoint(endpoint)),
            kind: SourceKind::Remote,
            provider: provider.to_string(),
            original_root: None,
            endpoint: Some(endpoint.trim_end_matches('/').to_string()),
            label: label.map(str::to_string),
        }
    }

    /// Remote host source for one provider *root* (e.g. a custom directory on
    /// the remote machine). The id folds the remote root in so multiple roots
    /// per endpoint stay distinct; the root itself travels in snapshot
    /// manifests, keeping `Source` stable for existing `source.json` files.
    #[must_use]
    pub fn remote_with_root(
        provider: &str,
        endpoint: &str,
        remote_root: &str,
        label: Option<&str>,
    ) -> Self {
        let base = Self::remote(provider, endpoint, label);
        if remote_root.trim().is_empty() {
            return base;
        }
        Self {
            id: format!("{}-{}", base.id, short_hash(remote_root.trim())),
            ..base
        }
    }

    /// Filesystem root owning this source's snapshots.
    #[must_use]
    pub fn storage_dir(&self, data_root: &Path) -> PathBuf {
        data_root.join("sources").join(&self.id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_source_id_is_stable() {
        let a = Source::local("claude", Path::new("/home/u/.claude"));
        let b = Source::local("claude", Path::new("/home/u/.claude"));
        assert_eq!(a.id, "local-claude");
        assert_eq!(a, b);
    }

    #[test]
    fn custom_source_ids_differ_per_path() {
        let a = Source::custom("claude", Path::new("/data/a"), None);
        let b = Source::custom("claude", Path::new("/data/b"), None);
        assert_ne!(a.id, b.id);
        assert!(a.id.starts_with("custom-claude-"));
    }

    #[test]
    fn remote_source_id_is_stable_and_filesafe() {
        let a = Source::remote("claude", "http://100.93.94.80:3728/", None);
        let b = Source::remote("claude", "http://100.93.94.80:3728", None);
        assert_eq!(a.id, b.id);
        assert!(a.id.starts_with("remote-"));
        assert!(!a.id.contains(':') && !a.id.contains('/'));
    }
}
