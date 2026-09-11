//! Consistent capture of live SQLite databases.
//!
//! Never snapshot a live `.db` with a plain file copy: the owning application
//! may be checkpointing concurrently, and `-wal`/`-shm` siblings make a naive
//! copy torn by construction. Instead this module copies through SQLite's
//! online backup API (a consistent committed-image copy) and then validates
//! the staging copy before it may be published.
//!
//! Lifecycle per database:
//! ```text
//! open live read-only (short busy timeout)
//!   → backup API into a fresh staging file
//!   → open staging read-only + PRAGMA quick_check
//!   → on success the staged file joins the candidate snapshot
//!   → on failure the candidate keeps the previous snapshot's copy (if any)
//!     and the sync aborts only when no previous good copy exists
//! ```

use rusqlite::{Connection, OpenFlags};
use std::path::Path;
use std::time::Duration;

/// How long backup steps wait on a locked source database.
const SOURCE_BUSY_TIMEOUT: Duration = Duration::from_secs(5);

fn open_live_read_only(path: &Path) -> Result<Connection, String> {
    if !path.is_file() {
        return Err(format!("SQLite source {} is not a file", path.display()));
    }
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| format!("Failed to open live SQLite {}: {e}", path.display()))?;
    conn.busy_timeout(SOURCE_BUSY_TIMEOUT)
        .map_err(|e| format!("Failed to set busy timeout: {e}"))?;
    Ok(conn)
}

/// Validate a staged database copy: open read-only and run `quick_check`.
pub fn quick_check_db(path: &Path) -> Result<(), String> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| format!("Failed to open staged SQLite {}: {e}", path.display()))?;
    let verdict: String = conn
        .query_row("PRAGMA quick_check", [], |row| row.get(0))
        .map_err(|e| format!("quick_check failed on {}: {e}", path.display()))?;
    if verdict == "ok" {
        Ok(())
    } else {
        Err(format!(
            "quick_check on {} reported corruption: {verdict}",
            path.display()
        ))
    }
}

/// Copy `live_db` into `staging_db` through the online backup API and validate
/// the result. The staging parent directory must exist. Any pre-existing
/// staging file is replaced; live `-wal`/`-shm` siblings are never copied.
pub fn backup_sqlite_into(live_db: &Path, staging_db: &Path) -> Result<(), String> {
    if live_db == staging_db {
        return Err("Refusing SQLite backup onto itself".to_string());
    }
    let src = open_live_read_only(live_db)?;
    // Fresh destination: a stale partial from a crashed run must not be
    // mistaken for a valid copy.
    let _ = std::fs::remove_file(staging_db);
    let mut dst = Connection::open(staging_db).map_err(|e| {
        format!(
            "Failed to create staging SQLite {}: {e}",
            staging_db.display()
        )
    })?;
    {
        let backup = rusqlite::backup::Backup::new(&src, &mut dst).map_err(|e| {
            format!(
                "Failed to start SQLite backup of {}: {e}",
                live_db.display()
            )
        })?;
        backup
            .run_to_completion(64, Duration::from_millis(5), None)
            .map_err(|e| format!("SQLite backup of {} failed: {e}", live_db.display()))?;
    }
    drop(dst);
    drop(src);
    quick_check_db(staging_db)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn make_live_db(dir: &Path) -> PathBuf {
        let path = dir.join("live.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE sessions(id TEXT PRIMARY KEY, title TEXT);
             INSERT INTO sessions VALUES ('s1', 'hello');",
        )
        .unwrap();
        drop(conn);
        path
    }

    #[test]
    fn backup_produces_validated_copy() {
        let dir = tempfile::tempdir().unwrap();
        let live = make_live_db(dir.path());
        let staging = dir.path().join("staging.db");
        backup_sqlite_into(&live, &staging).unwrap();
        // Same rows, independent file.
        let conn = Connection::open_with_flags(&staging, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let title: String = conn
            .query_row("SELECT title FROM sessions WHERE id = 's1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(title, "hello");
    }

    #[test]
    fn missing_source_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let err =
            backup_sqlite_into(&dir.path().join("no.db"), &dir.path().join("out.db")).unwrap_err();
        assert!(err.contains("not a file"), "unexpected: {err}");
    }

    #[test]
    fn corrupt_copy_fails_quick_check() {
        let dir = tempfile::tempdir().unwrap();
        let bad = dir.path().join("bad.db");
        std::fs::write(&bad, b"definitely not a sqlite file at all........").unwrap();
        assert!(quick_check_db(&bad).is_err());
    }
}
