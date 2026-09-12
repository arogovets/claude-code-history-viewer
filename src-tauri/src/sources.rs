//! Local source identity and provider discovery context. No network or persistence engine.
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Source {
    pub id: String,
    pub label: String,
    #[serde(skip)]
    pub current: PathBuf,
}

tokio::task_local! { static ASYNC_HOME: PathBuf; }
thread_local! { static SYNC_HOME: RefCell<Option<PathBuf>> = const { RefCell::new(None) }; }

pub fn root() -> Option<PathBuf> {
    std::env::var_os("CCHV_MIRROR_ROOT")
        .map(PathBuf::from)
        .or_else(|| {
            #[cfg(test)]
            let home = std::env::var_os("CCHV_TEST_HOME").map(PathBuf::from);
            #[cfg(not(test))]
            let home = crate::utils::home_dir();
            home.map(|home| home.join(".claude-history-viewer/mirrors"))
        })
}

pub fn list() -> Result<Vec<Source>, String> {
    let root = root().ok_or("Could not resolve mirror root")?;
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut sources = Vec::new();
    for entry in std::fs::read_dir(&root).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        if !entry.file_type().map_err(|e| e.to_string())?.is_dir() {
            continue;
        }
        let metadata = entry.path().join("source.json");
        if !metadata.is_file() {
            continue;
        }
        let mut source: Source =
            serde_json::from_slice(&std::fs::read(metadata).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        if source.id != entry.file_name().to_string_lossy()
            || !source
                .id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
        {
            return Err("Invalid mirror source identity".into());
        }
        source.current = entry.path().join("current");
        if source.current.is_symlink() {
            return Err("Mirror current must not be a symlink".into());
        }
        if source.current.is_dir() {
            sources.push(source);
        }
    }
    sources.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(sources)
}

pub fn for_path(path: &Path) -> Option<Source> {
    let canonical = path.canonicalize().ok()?;
    list().ok()?.into_iter().find(|source| {
        source
            .current
            .canonicalize()
            .is_ok_and(|root| canonical.starts_with(root))
    })
}

pub fn home_dir() -> Option<PathBuf> {
    SYNC_HOME
        .with(|home| home.borrow().clone())
        .or_else(|| ASYNC_HOME.try_with(Clone::clone).ok())
        .or_else(|| {
            #[cfg(test)]
            {
                crate::utils::home_dir()
            }
            #[cfg(not(test))]
            {
                list().ok()?.first().map(|s| s.current.clone())
            }
        })
}

pub async fn scope<F: std::future::Future>(home: PathBuf, future: F) -> F::Output {
    ASYNC_HOME.scope(home, future).await
}

pub fn sync_scope<T>(home: PathBuf, action: impl FnOnce() -> T) -> T {
    struct Restore(Option<PathBuf>);
    impl Drop for Restore {
        fn drop(&mut self) {
            SYNC_HOME.with(|home| *home.borrow_mut() = self.0.take());
        }
    }
    let _restore = Restore(SYNC_HOME.with(|slot| slot.replace(Some(home))));
    action()
}

pub fn env_var(key: &str) -> Result<String, std::env::VarError> {
    #[cfg(test)]
    {
        if SYNC_HOME.with(|home| home.borrow().is_some()) || ASYNC_HOME.try_with(|_| ()).is_ok() {
            Err(std::env::VarError::NotPresent)
        } else {
            std::env::var(key)
        }
    }
    #[cfg(not(test))]
    {
        let _ = key;
        Err(std::env::VarError::NotPresent)
    }
}

pub fn config_dir() -> Option<PathBuf> {
    home_dir().map(|home| {
        home.join(if cfg!(target_os = "macos") {
            "Library/Application Support"
        } else {
            ".config"
        })
    })
}
pub fn data_dir() -> Option<PathBuf> {
    home_dir().map(|home| {
        home.join(if cfg!(target_os = "macos") {
            "Library/Application Support"
        } else {
            ".local/share"
        })
    })
}
pub fn data_local_dir() -> Option<PathBuf> {
    data_dir()
}

/// Files keep their real mirror paths; database provider handles carry an
/// explicit source id so equal native IDs on two machines never collide.
pub fn qualify(source: &Source, value: &str) -> String {
    if value.contains("://") {
        format!("source:{}|{value}", source.id)
    } else {
        value.to_owned()
    }
}

fn validate_handle_path(source: &Source, inner: &str) -> Result<(), String> {
    let (scheme, body) = inner.split_once("://").ok_or("Invalid provider handle")?;
    let embedded_path = match scheme {
        "aider" | "crush" => Some(body.split('#').next().unwrap_or(body)),
        "cline" => Some(body.split(':').next().unwrap_or(body)),
        "vscode" => Some(body),
        "cursor" if Path::new(body).is_absolute() => Some(body),
        "codex" | "gemini" | "grok" | "kimi" | "dsh" | "opencode" | "openinterpreter"
        | "openhands" | "goose" | "amazonq" | "kiro" | "llm" | "zed" | "trae" | "vibe" | "qwen"
        | "cursor" | "forgecode" | "forgecode-db" | "copilot-cli" | "copilot-desktop" => None,
        "copilot" => {
            use base64::Engine;
            let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(body)
                .map_err(|e| e.to_string())?;
            let value: serde_json::Value =
                serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
            for reference in value
                .get("sources")
                .and_then(serde_json::Value::as_array)
                .ok_or("Invalid Copilot references")?
            {
                if reference.get("kind").and_then(serde_json::Value::as_str) == Some("vs_code") {
                    validate_handle_path(
                        source,
                        reference
                            .get("path")
                            .and_then(serde_json::Value::as_str)
                            .ok_or("Invalid Copilot path")?,
                    )?;
                }
            }
            None
        }
        _ => return Err("Unknown provider handle".into()),
    };
    if let Some(path) = embedded_path {
        let actual_source =
            for_path(Path::new(path)).ok_or("Provider path escapes collected mirrors")?;
        if actual_source.id != source.id {
            return Err("Provider path belongs to another source".into());
        }
    }
    Ok(())
}

pub fn resolve(value: &str) -> Result<(Source, String), String> {
    if let Some(rest) = value.strip_prefix("source:") {
        let (id, inner) = rest.split_once('|').ok_or("Invalid source handle")?;
        let source = list()?
            .into_iter()
            .find(|s| s.id == id)
            .ok_or("Unknown source")?;
        if inner.split(['/', '\\']).any(|part| part == "..") {
            return Err("Invalid source handle".into());
        }
        validate_handle_path(&source, inner)?;
        Ok((source, inner.to_owned()))
    } else {
        let source =
            for_path(Path::new(value)).ok_or("History path is outside collected mirrors")?;
        Ok((source, value.to_owned()))
    }
}

pub fn require_history_path(value: &str) -> Result<(), String> {
    #[cfg(test)]
    {
        let _ = value;
        Ok(())
    }
    #[cfg(not(test))]
    {
        if let (Some(home), Ok(path)) = (crate::utils::home_dir(), Path::new(value).canonicalize())
        {
            if home
                .join(".claude-history-viewer/archives")
                .canonicalize()
                .is_ok_and(|archive| path.starts_with(archive))
            {
                return Ok(());
            }
        }
        resolve(value).map(|_| ())
    }
}

pub fn require_mutable_history(value: &str) -> Result<(), String> {
    #[cfg(test)]
    {
        let _ = value;
        Ok(())
    }
    #[cfg(not(test))]
    {
        let _ = value;
        Err("Collected history is read-only; use CCHV metadata or Archive Manager".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn concurrent_source_scopes_do_not_leak() {
        let (a, b) = tokio::join!(
            scope(PathBuf::from("/mirror/a"), async {
                tokio::task::yield_now().await;
                home_dir()
            }),
            scope(PathBuf::from("/mirror/b"), async {
                tokio::task::yield_now().await;
                home_dir()
            }),
        );
        assert_eq!(a, Some(PathBuf::from("/mirror/a")));
        assert_eq!(b, Some(PathBuf::from("/mirror/b")));
    }

    #[test]
    fn synchronous_scope_restores_after_unwind() {
        sync_scope(PathBuf::from("/outer"), || {
            let _ =
                std::panic::catch_unwind(|| sync_scope(PathBuf::from("/inner"), || panic!("test")));
            assert_eq!(home_dir(), Some(PathBuf::from("/outer")));
        });
    }

    #[test]
    fn provider_handles_have_distinct_source_identity() {
        let a = Source {
            id: "a".into(),
            label: "Same label".into(),
            current: "/mirror/a".into(),
        };
        let b = Source {
            id: "b".into(),
            ..a.clone()
        };
        assert_ne!(qualify(&a, "codex://same"), qualify(&b, "codex://same"));
        assert_eq!(
            qualify(&a, "/mirror/a/session.jsonl"),
            "/mirror/a/session.jsonl"
        );
    }

    #[test]
    #[serial_test::serial]
    fn mirrors_resolve_and_escape_is_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let previous = std::env::var_os("CCHV_MIRROR_ROOT");
        std::env::set_var("CCHV_MIRROR_ROOT", temp.path());
        struct Restore(Option<std::ffi::OsString>);
        impl Drop for Restore {
            fn drop(&mut self) {
                match &self.0 {
                    Some(v) => std::env::set_var("CCHV_MIRROR_ROOT", v),
                    None => std::env::remove_var("CCHV_MIRROR_ROOT"),
                }
            }
        }
        let _restore = Restore(previous);
        let current = temp.path().join("laptop/current");
        std::fs::create_dir_all(&current).unwrap();
        std::fs::write(
            temp.path().join("laptop/source.json"),
            r#"{"id":"laptop","label":"Laptop"}"#,
        )
        .unwrap();
        let session = current.join("session.jsonl");
        std::fs::write(&session, "{}\n").unwrap();
        assert_eq!(resolve(session.to_str().unwrap()).unwrap().0.id, "laptop");
        assert!(resolve(temp.path().to_str().unwrap()).is_err());
        assert!(resolve("source:laptop|codex://../outside").is_err());
        assert!(resolve("source:unknown|codex://same").is_err());
        #[cfg(unix)]
        {
            let outside = tempfile::NamedTempFile::new().unwrap();
            let link = current.join("link.jsonl");
            std::os::unix::fs::symlink(outside.path(), &link).unwrap();
            assert!(resolve(link.to_str().unwrap()).is_err());
        }
    }
}
