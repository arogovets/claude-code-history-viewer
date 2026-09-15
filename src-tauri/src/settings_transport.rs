//! Machine access only; no provider-specific paths or configuration formats.
use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Debug)]
pub struct OriginFile {
    pub revision: String,
    pub bytes: Option<Vec<u8>>,
}

pub trait SettingsTransport {
    fn probe(&self) -> Result<(), String>;
    fn read(&self, path: &Path) -> Result<OriginFile, String>;
    fn compare_replace(
        &self,
        path: &Path,
        revision: &str,
        bytes: &[u8],
    ) -> Result<OriginFile, String>;
}

pub struct SourceTransport {
    pub ssh: Option<String>,
    pub home: String,
}

impl SourceTransport {
    fn request(&self, mut request: Value) -> Result<Value, String> {
        // Local canonical-path checks belong to the machine transport, not providers.
        if self.ssh.is_none() {
            if let (Some(path), Some(root)) = (request["path"].as_str(), crate::sources::root()) {
                let mut existing = Path::new(path);
                while !existing.exists() {
                    existing = existing.parent().ok_or("Invalid origin path")?;
                }
                if let (Ok(actual), Ok(mirror)) = (existing.canonicalize(), root.canonicalize()) {
                    if actual.starts_with(mirror) {
                        return Err("Provider settings cannot target history mirrors".into());
                    }
                }
            }
        }
        request["home"] = json!(self.home);
        let worker = include_str!("settings_transport.py");
        let mut command = if let Some(host) = &self.ssh {
            if host.is_empty()
                || host.starts_with('-')
                || !host
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"@._-".contains(&c))
            {
                return Err("Invalid SSH source address".into());
            }
            let mut command = Command::new("ssh");
            command.args([
                "-oBatchMode=yes",
                "-oConnectTimeout=5",
                "-oServerAliveInterval=5",
                "-oServerAliveCountMax=2",
                "--",
                host,
            ]);
            command.arg(format!("python3 -c '{}'", worker.replace('\'', "'\\''")));
            command
        } else {
            let mut command = Command::new("python3");
            command.args(["-c", worker]);
            command
        };
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| "Source transport unavailable (Python 3 required)")?;
        let mut stdin = child.stdin.take().ok_or("Transport input unavailable")?;
        let payload = serde_json::to_vec(&request).map_err(|_| "Invalid transport request")?;
        let writer = std::thread::spawn(move || stdin.write_all(&payload));
        let mut stdout = child.stdout.take().ok_or("Transport output unavailable")?;
        let reader = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            stdout
                .by_ref()
                .take(3 * 1024 * 1024)
                .read_to_end(&mut bytes)
                .map(|_| bytes)
        });
        let start = Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if start.elapsed() < Duration::from_secs(20) => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err("Source transport timed out; refresh before retrying".into());
                }
            }
        }
        writer
            .join()
            .map_err(|_| "Transport input failed")?
            .map_err(|_| "Source transport unavailable")?;
        let output = reader
            .join()
            .map_err(|_| "Transport output failed")?
            .map_err(|_| "Transport output failed")?;
        let reply: Value =
            serde_json::from_slice(&output).map_err(|_| "Source transport unavailable")?;
        if let Some(code) = reply.get("error").and_then(Value::as_str) {
            return Err(match code {
                "revision_conflict" => {
                    "Revision conflict: origin changed; refresh and review before applying"
                }
                "unsafe_path" => "Origin path is unsafe or contains a symlink",
                _ => "Origin configuration could not be accessed",
            }
            .into());
        }
        reply
            .get("ok")
            .cloned()
            .ok_or("Invalid transport response".into())
    }

    fn file(&self, request: Value) -> Result<OriginFile, String> {
        let reply = self.request(request)?;
        Ok(OriginFile {
            revision: reply["revision"].as_str().ok_or("Missing revision")?.into(),
            bytes: reply["bytes"]
                .as_str()
                .map(|s| {
                    STANDARD
                        .decode(s)
                        .map_err(|_| "Invalid transport bytes".to_string())
                })
                .transpose()?,
        })
    }
}

impl SettingsTransport for SourceTransport {
    fn probe(&self) -> Result<(), String> {
        self.request(json!({"operation": "probe"})).map(|_| ())
    }
    fn read(&self, path: &Path) -> Result<OriginFile, String> {
        self.file(json!({"operation": "read", "path": path}))
    }
    fn compare_replace(
        &self,
        path: &Path,
        revision: &str,
        bytes: &[u8],
    ) -> Result<OriginFile, String> {
        self.file(json!({"operation": "write", "path": path, "revision": revision, "bytes": STANDARD.encode(bytes)}))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    #[test]
    #[serial_test::serial]
    fn local_and_ssh_share_atomic_conflict_and_failure_semantics() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let bin = root.join("bin");
        std::fs::create_dir(&bin).unwrap();
        let ssh = bin.join("ssh");
        // Exercise the SSH argument/quoting path and execute its worker as a remote shell would.
        std::fs::write(
            &ssh,
            "#!/bin/sh\nfor last; do :; done\nexec /bin/sh -c \"$last\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o700)).unwrap();
        let old = std::env::var_os("PATH").unwrap();
        struct Restore(std::ffi::OsString);
        impl Drop for Restore {
            fn drop(&mut self) {
                std::env::set_var("PATH", &self.0);
            }
        }
        let _restore = Restore(old.clone());
        std::env::set_var(
            "PATH",
            format!("{}:{}", bin.display(), old.to_string_lossy()),
        );
        for remote in [false, true] {
            let home = root.join(if remote {
                "ssh origin's home"
            } else {
                "local origin's home"
            });
            std::fs::create_dir(&home).unwrap();
            let transport = SourceTransport {
                ssh: remote.then(|| "fixture-host".into()),
                home: home.to_string_lossy().into(),
            };
            transport.probe().unwrap();
            let path = home.join(".claude/settings.json");
            let empty = transport.read(&path).unwrap();
            assert_eq!(empty.revision, "missing");
            transport
                .compare_replace(&path, &empty.revision, br#"{"model":"a"}"#)
                .unwrap();
            let initial = transport.read(&path).unwrap();
            assert_eq!(initial.bytes.unwrap(), br#"{"model":"a"}"#);
            std::fs::write(&path, br#"{"external":true}"#).unwrap();
            assert!(transport
                .compare_replace(&path, &initial.revision, b"{}")
                .unwrap_err()
                .contains("conflict"));
            assert_eq!(std::fs::read(&path).unwrap(), br#"{"external":true}"#);
            let current = transport.read(&path).unwrap();
            let lock = home.join(".claude/.settings.json.cchv.lock");
            std::fs::remove_file(&lock).unwrap();
            std::fs::create_dir(&lock).unwrap();
            assert!(transport
                .compare_replace(&path, &current.revision, b"{}")
                .is_err());
            assert_eq!(std::fs::read(&path).unwrap(), current.bytes.unwrap());
            std::fs::remove_dir(&lock).unwrap();
            let outside = home.join("outside.json");
            std::fs::write(&outside, "{}").unwrap();
            std::fs::remove_file(&path).unwrap();
            std::os::unix::fs::symlink(&outside, &path).unwrap();
            assert!(transport.read(&path).is_err());
            assert!(transport.compare_replace(&path, "missing", b"{}").is_err());
            assert!(transport.read(&home.join("../outside")).is_err());
            std::fs::remove_file(&path).unwrap();
            std::fs::remove_file(&lock).unwrap();
            std::fs::remove_dir(home.join(".claude")).unwrap();
            std::os::unix::fs::symlink(&root, home.join(".claude")).unwrap();
            assert!(transport.read(&path).is_err());
        }
    }
}
