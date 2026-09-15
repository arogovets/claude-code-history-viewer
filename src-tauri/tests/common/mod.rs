//! Disposable registered mirror for integration tests (the library is compiled
//! without cfg(test), so this exercises production source routing).
// Each integration binary uses a different subset of this shared fixture.
#![allow(dead_code)]
use std::path::{Path, PathBuf};

pub struct MirrorFixture {
    _temp: tempfile::TempDir,
    current: PathBuf,
    previous_root: Option<std::ffi::OsString>,
}

impl MirrorFixture {
    pub fn new() -> Self {
        let temp = tempfile::tempdir().expect("temporary mirror root");
        let source = temp.path().join("test");
        let current = source.join("current");
        std::fs::create_dir_all(&current).unwrap();
        std::fs::write(
            source.join("source.json"),
            r#"{"id":"test","label":"Test"}"#,
        )
        .unwrap();
        let previous_root = std::env::var_os("CCHV_MIRROR_ROOT");
        std::env::set_var("CCHV_MIRROR_ROOT", temp.path());
        Self {
            _temp: temp,
            current,
            previous_root,
        }
    }

    pub fn path(&self) -> &Path {
        &self.current
    }
}

impl Drop for MirrorFixture {
    fn drop(&mut self) {
        match &self.previous_root {
            Some(value) => std::env::set_var("CCHV_MIRROR_ROOT", value),
            None => std::env::remove_var("CCHV_MIRROR_ROOT"),
        }
    }
}
