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

/// Whether `path` starts with the SQLite file magic.
#[must_use]
pub fn is_sqlite_file(path: &Path) -> bool {
    std::fs::File::open(path)
        .and_then(|mut file| {
            use std::io::Read as _;
            let mut magic = [0u8; 16];
            file.read_exact(&mut magic).map(|()| magic)
        })
        .is_ok_and(|magic| magic == *b"SQLite format 3\0")
}

/// Merge history forward: copy rows present in `prev_db` but missing from
/// `staged_db`, generically over every ordinary table with a single-column
/// primary key, restricted to columns existing in both databases (schema-drift
/// safe). This is what makes cumulative snapshots hold for SQLite-backed
/// providers: file-level carry-over alone cannot resurrect rows a newer
/// database image deleted.
///
/// Returns the number of resurrected rows. On any error the caller must fall
/// back to the previous database wholesale (never publish a half-merged db).
pub fn merge_missing_rows(prev_db: &Path, staged_db: &Path) -> Result<usize, String> {
    if !prev_db.is_file() {
        return Err(format!(
            "Previous SQLite {} is not a file",
            prev_db.display()
        ));
    }
    let staged = Connection::open(staged_db)
        .map_err(|e| format!("Failed to open staged SQLite {}: {e}", staged_db.display()))?;
    staged
        .busy_timeout(Duration::from_secs(5))
        .map_err(|e| format!("Failed to set busy timeout: {e}"))?;

    let mut tables: Vec<String> = Vec::new();
    {
        let mut stmt = staged
            .prepare(
                "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
            )
            .map_err(|e| format!("Failed to list staged tables: {e}"))?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|e| format!("Failed to read staged tables: {e}"))?;
        for name in rows {
            tables.push(name.map_err(|e| format!("Failed to read staged tables: {e}"))?);
        }
    }
    if tables.is_empty() {
        return Ok(0);
    }

    let prev_escaped = prev_db.to_string_lossy().replace('\'', "''");
    staged
        .execute_batch(&format!("ATTACH DATABASE '{prev_escaped}' AS prev"))
        .map_err(|e| format!("Failed to attach previous SQLite: {e}"))?;

    let tx = staged
        .unchecked_transaction()
        .map_err(|e| format!("Failed to begin merge transaction: {e}"))?;
    let mut resurrected = 0usize;
    for table in &tables {
        match merge_table(&tx, table) {
            Ok(count) => resurrected += count,
            Err(e) => {
                log::warn!("SQLite history merge skipped table {table}: {e}");
            }
        }
    }
    tx.commit()
        .map_err(|e| format!("Failed to commit SQLite history merge: {e}"))?;
    Ok(resurrected)
}

/// Copy rows of one table whose primary key is absent from the staged copy.
/// Tables without exactly one single-column primary key, or whose key column
/// is missing on either side, are skipped (never guessed).
fn merge_table(tx: &rusqlite::Transaction<'_>, table: &str) -> Result<usize, String> {
    let quoted = format!("\"{}\"", table.replace('"', "\"\""));
    // NOTE: the schema-qualified PRAGMA form is `PRAGMA schema.table_info(t)`;
    // `PRAGMA table_info(schema.t)` does not resolve attached databases.
    let columns_of = |schema: &str| -> Result<Vec<(String, bool)>, String> {
        let qualified = format!("{schema}.table_info({quoted})");
        let mut stmt = tx
            .prepare(&format!("PRAGMA {qualified}"))
            .map_err(|e| format!("PRAGMA table_info failed for {qualified}: {e}"))?;
        let rows = stmt
            .query_map([], |row| {
                let name: String = row.get("name")?;
                let pk_order: i64 = row.get("pk")?;
                Ok((name, pk_order > 0))
            })
            .map_err(|e| format!("Failed to read table_info for {qualified}: {e}"))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("Failed to collect table_info for {qualified}: {e}"))
    };
    let prev_cols = columns_of("prev");
    let new_cols = columns_of("main");
    let (prev_cols, new_cols) = match (prev_cols, new_cols) {
        (Ok(prev), Ok(new)) => (prev, new),
        // Table missing on one side (schema drift): nothing to merge.
        _ => return Ok(0),
    };
    let prev_pk: Vec<&str> = prev_cols
        .iter()
        .filter(|(_, pk)| *pk)
        .map(|(name, _)| name.as_str())
        .collect();
    let new_pk: Vec<&str> = new_cols
        .iter()
        .filter(|(_, pk)| *pk)
        .map(|(name, _)| name.as_str())
        .collect();
    if prev_pk.len() != 1 || new_pk != prev_pk {
        return Ok(0);
    }
    let pk = prev_pk[0];
    let common: Vec<&str> = prev_cols
        .iter()
        .map(|(name, _)| name.as_str())
        .filter(|name| new_cols.iter().any(|(other, _)| other == name))
        .collect();
    if !common.contains(&pk) || common.is_empty() {
        return Ok(0);
    }
    let cols = common
        .iter()
        .map(|name| format!("\"{}\"", name.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(", ");
    let pk_quoted = format!("\"{}\"", pk.replace('"', "\"\""));
    let sql = format!(
        "INSERT INTO main.{quoted} ({cols}) SELECT {cols} FROM prev.{quoted} \
         WHERE {pk_quoted} NOT IN (SELECT {pk_quoted} FROM main.{quoted})"
    );
    let changed = tx
        .execute(&sql, [])
        .map_err(|e| format!("History merge into {quoted} failed: {e}"))?;
    Ok(changed)
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

    fn make_db(path: &Path, rows: &[&str]) {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            "CREATE TABLE sessions(id TEXT PRIMARY KEY, title TEXT);
             CREATE TABLE no_pk(body TEXT);",
        )
        .unwrap();
        for row in rows {
            conn.execute(
                "INSERT INTO sessions(id, title) VALUES (?1, ?2)",
                rusqlite::params![row, format!("t-{row}")],
            )
            .unwrap();
        }
        conn.execute("INSERT INTO no_pk(body) VALUES ('x')", [])
            .unwrap();
    }

    #[test]
    fn merge_resurrects_deleted_rows_and_skips_keyless_tables() {
        let dir = tempfile::tempdir().unwrap();
        let prev = dir.path().join("prev.db");
        let staged = dir.path().join("staged.db");
        make_db(&prev, &["a", "b"]);
        make_db(&staged, &["b", "c"]);
        let resurrected = merge_missing_rows(&prev, &staged).unwrap();
        assert_eq!(resurrected, 1);
        let conn = Connection::open(&staged).unwrap();
        let ids: Vec<String> = conn
            .prepare("SELECT id FROM sessions ORDER BY id")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .flatten()
            .collect();
        assert_eq!(ids, vec!["a", "b", "c"]);
        quick_check_db(&staged).unwrap();
    }

    #[test]
    fn merge_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let prev = dir.path().join("prev.db");
        let staged = dir.path().join("staged.db");
        make_db(&prev, &["a"]);
        make_db(&staged, &["a", "b"]);
        assert_eq!(merge_missing_rows(&prev, &staged).unwrap(), 0);
    }

    #[test]
    fn corrupt_copy_fails_quick_check() {
        let dir = tempfile::tempdir().unwrap();
        let bad = dir.path().join("bad.db");
        std::fs::write(&bad, b"definitely not a sqlite file at all........").unwrap();
        assert!(quick_check_db(&bad).is_err());
    }
}
