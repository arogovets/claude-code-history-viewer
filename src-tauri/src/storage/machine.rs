//! Stable per-installation machine identity.
//!
//! Remote source identity is `machine_id + provider + role + origin` so that
//! changing an endpoint (IP rotation, new Tailscale hostname) does not fork a
//! new archive lineage. The endpoint stays mutable connection metadata.
//!
//! The id is persisted at `~/.claude-history-viewer/machine-id`. Tests run
//! under [`crate::test_utils::SandboxHome`], which isolates the home directory
//! and therefore the machine id.

/// Whether `id` is shaped like a persisted machine id (uuid or a
/// `wsl:<distro>` pseudo-identity used for WSL sources).
#[must_use]
pub fn is_valid_machine_id(id: &str) -> bool {
    let id = id.trim();
    if id.is_empty() || id.len() > 128 {
        return false;
    }
    if let Some(distro) = id.strip_prefix("wsl:") {
        return !distro.trim().is_empty()
            && distro
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    }
    uuid::Uuid::parse_str(id).is_ok()
}

/// Load the local machine id, creating and persisting it on first use.
pub fn local_machine_id() -> Result<String, String> {
    let home =
        crate::utils::home_dir().ok_or_else(|| "Could not determine home directory".to_string())?;
    let dir = home.join(".claude-history-viewer");
    let path = dir.join("machine-id");
    if let Ok(raw) = std::fs::read_to_string(&path) {
        let id = raw.trim().to_string();
        if is_valid_machine_id(&id) {
            return Ok(id);
        }
        // Fall through and replace a corrupt value rather than forking
        // identity silently on every restart.
        log::warn!("Replacing invalid machine-id file {}", path.display());
    }
    let id = uuid::Uuid::new_v4().to_string();
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("Failed to create state directory {}: {e}", dir.display()))?;
    std::fs::write(&path, format!("{id}\n"))
        .map_err(|e| format!("Failed to persist machine-id {}: {e}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(id)
}

/// Pseudo-identity for a WSL distro observed from this machine.
#[must_use]
pub fn wsl_machine_id(distro: &str) -> String {
    format!("wsl:{}", distro.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[serial_test::serial]
    fn machine_id_is_stable_and_valid() {
        let _sandbox = crate::test_utils::SandboxHome::new();
        let first = local_machine_id().unwrap();
        let second = local_machine_id().unwrap();
        assert_eq!(first, second);
        assert!(is_valid_machine_id(&first));
    }

    #[test]
    #[serial_test::serial]
    fn machine_id_is_isolated_per_home() {
        let first_home = crate::test_utils::SandboxHome::new();
        let first = local_machine_id().unwrap();
        drop(first_home);
        let _second_home = crate::test_utils::SandboxHome::new();
        let second = local_machine_id().unwrap();
        assert_ne!(first, second);
    }

    #[test]
    fn wsl_machine_id_shape() {
        assert!(is_valid_machine_id(&wsl_machine_id("Ubuntu")));
        assert!(!is_valid_machine_id("wsl:"));
        assert!(!is_valid_machine_id(""));
    }
}
