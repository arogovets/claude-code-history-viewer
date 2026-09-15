//! Canonical provider collection contract, shared verbatim with the Python collector.
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::OnceLock;

#[derive(Debug, Deserialize, Serialize)]
pub struct HomePath {
    pub path: String,
    pub kind: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct CollectionSpec {
    pub provider: super::ProviderId,
    pub home_paths: Vec<HomePath>,
    pub project_paths: Vec<String>,
    pub settings: Option<SettingsSpec>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct SettingsSpec {
    pub scopes: Vec<SettingsScope>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct SettingsScope {
    pub id: String,
    pub label: String,
    pub path: String,
    pub base: String,
    pub format: String,
    pub editable: bool,
}

#[derive(Deserialize)]
struct Manifest {
    providers: Vec<CollectionSpec>,
}

pub fn specs() -> &'static [CollectionSpec] {
    static MANIFEST: OnceLock<Manifest> = OnceLock::new();
    &MANIFEST
        .get_or_init(|| {
            serde_json::from_str(include_str!("../../../collector/provider-specs.json"))
                .expect("valid provider collection manifest")
        })
        .providers
}

/// Resolve only beneath the active local mirror, independent of host OS/transport.
pub fn home_paths(provider: super::ProviderId) -> Vec<PathBuf> {
    let Some(home) = crate::sources::home_dir() else {
        return Vec::new();
    };
    specs()
        .iter()
        .filter(|s| s.provider == provider)
        .flat_map(|s| &s.home_paths)
        .map(|p| home.join(&p.path))
        .collect()
}

pub fn existing_home(provider: super::ProviderId) -> Option<PathBuf> {
    home_paths(provider)
        .into_iter()
        .find(|p| p.is_dir() && !p.is_symlink())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn every_enum_variant_has_exactly_one_nonempty_specification() {
        // Read enum declarations rather than a second manually maintained list.
        // An added variant therefore fails this test even if all dispatch matches were updated.
        let source = include_str!("mod.rs");
        let declaration = source
            .split("pub enum ProviderId {")
            .nth(1)
            .unwrap()
            .split("\n}")
            .next()
            .unwrap();
        let variants = declaration
            .lines()
            .map(str::trim)
            .filter(|line| {
                line.ends_with(',') && line.chars().next().is_some_and(char::is_uppercase)
            })
            .count();
        let mut seen = std::collections::HashSet::new();
        for spec in specs() {
            assert!(
                seen.insert(spec.provider.as_str()),
                "duplicate specification"
            );
            assert!(!spec.home_paths.is_empty() || !spec.project_paths.is_empty());
            for p in &spec.home_paths {
                assert!(crate::sources::relative_mount(&p.path).is_ok());
                assert!(["file", "directory"].contains(&p.kind.as_str()));
            }
        }
        assert_eq!(
            variants,
            seen.len(),
            "new ProviderId needs a collection specification"
        );
    }
}
