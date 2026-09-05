//! Connection setup: foreign keys, WAL + busy_timeout for concurrent GUI/CLI access
//! to the same DB file (`docs/requirements.md` §10.16 implementation notes).

use std::path::Path;
use std::time::Duration;

use rusqlite::{Connection, Result};

use super::schema;

/// Opens an in-memory database with the schema applied. Useful for tests and for
/// scenarios that don't need persistence.
pub fn open_in_memory() -> Result<Connection> {
    let conn = Connection::open_in_memory()?;
    configure(&conn)?;
    schema::apply(&conn)?;
    Ok(conn)
}

/// Opens (creating if needed) a file-backed database at `path`, applying the schema
/// only if it doesn't already exist (`app_setting` is used as the "schema present"
/// marker table).
pub fn open<P: AsRef<Path>>(path: P) -> Result<Connection> {
    let conn = Connection::open(path)?;
    configure(&conn)?;
    if !schema_present(&conn)? {
        schema::apply(&conn)?;
    }
    Ok(conn)
}

fn configure(conn: &Connection) -> Result<()> {
    conn.pragma_update(None, "foreign_keys", "ON")?;
    // WAL is meaningless for :memory: databases; SQLite silently keeps them in
    // "memory" journal mode, so it's harmless to request it unconditionally here.
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.busy_timeout(Duration::from_secs(5))?;
    Ok(())
}

fn schema_present(conn: &Connection) -> Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'app_setting'",
        [],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_memory_db_has_all_expected_tables() {
        let conn = open_in_memory().unwrap();
        let mut stmt = conn
            .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
            .unwrap();
        let names: Vec<String> = stmt
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_>>()
            .unwrap();

        for expected in [
            "app_setting",
            "removable_media",
            "scan_run",
            "scanned_file",
            "reference_set",
            "reference_file",
            "check_run",
            "check_run_source",
            "integrity_check_result",
            "duplicate_group",
            "duplicate_group_member",
        ] {
            assert!(
                names.iter().any(|n| n == expected),
                "missing table {expected}"
            );
        }
    }

    #[test]
    fn foreign_keys_are_enforced() {
        let conn = open_in_memory().unwrap();
        let fk_on: i64 = conn
            .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
            .unwrap();
        assert_eq!(fk_on, 1);
    }

    #[test]
    fn reopening_a_file_backed_db_does_not_recreate_schema() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("filechecker.sqlite3");

        {
            let conn = open(&path).unwrap();
            conn.execute(
                "INSERT INTO app_setting (key, value) VALUES ('probe', '1')",
                [],
            )
            .unwrap();
        }

        // Re-opening must not fail with "table already exists", and the previously
        // written row must still be there.
        let conn = open(&path).unwrap();
        let value: String = conn
            .query_row(
                "SELECT value FROM app_setting WHERE key = 'probe'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(value, "1");
    }
}
