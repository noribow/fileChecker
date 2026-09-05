//! FK/CHECK constraint coverage for the §10.12 schema. Each test asserts that a row
//! violating a specific constraint is rejected, and (where useful) that the
//! corresponding valid row is accepted — so a constraint that's accidentally loosened
//! or a valid case that's accidentally over-restricted both get caught.

use filechecker_core::db::{open_in_memory, Connection};

fn db() -> Connection {
    open_in_memory().unwrap()
}

fn now() -> i64 {
    1_700_000_000_000
}

#[test]
fn scan_run_rejects_folder_type_with_removable_media_id_set() {
    let conn = db();
    let err = conn
        .execute(
            "INSERT INTO scan_run (target_type, folder_path, removable_media_id, started_at)
             VALUES ('folder', '/data/photos', 1, ?1)",
            [now()],
        )
        .unwrap_err();
    assert!(err.to_string().contains("CHECK") || err.to_string().contains("constraint"));
}

#[test]
fn scan_run_rejects_removable_media_type_without_media_id() {
    let conn = db();
    let err = conn
        .execute(
            "INSERT INTO scan_run (target_type, folder_path, removable_media_id, started_at)
             VALUES ('removable_media', NULL, NULL, ?1)",
            [now()],
        )
        .unwrap_err();
    assert!(err.to_string().contains("CHECK") || err.to_string().contains("constraint"));
}

#[test]
fn scan_run_accepts_valid_folder_row() {
    let conn = db();
    conn.execute(
        "INSERT INTO scan_run (target_type, folder_path, started_at) VALUES ('folder', '/data/photos', ?1)",
        [now()],
    )
    .unwrap();
}

#[test]
fn scan_run_rejects_unknown_target_type() {
    let conn = db();
    let err = conn
        .execute(
            "INSERT INTO scan_run (target_type, folder_path, started_at) VALUES ('cloud', '/x', ?1)",
            [now()],
        )
        .unwrap_err();
    assert!(err.to_string().contains("CHECK") || err.to_string().contains("constraint"));
}

#[test]
fn scanned_file_rejects_unknown_scan_run_id() {
    let conn = db();
    let err = conn
        .execute(
            "INSERT INTO scanned_file (scan_run_id, path, size, status, scanned_at)
             VALUES (999, 'a.jpg', 100, 'ok', ?1)",
            [now()],
        )
        .unwrap_err();
    assert!(err.to_string().to_lowercase().contains("foreign key"));
}

#[test]
fn scanned_file_rejects_negative_size() {
    let conn = db();
    conn.execute(
        "INSERT INTO scan_run (target_type, folder_path, started_at) VALUES ('folder', '/x', ?1)",
        [now()],
    )
    .unwrap();
    let scan_run_id = conn.last_insert_rowid();

    let err = conn
        .execute(
            "INSERT INTO scanned_file (scan_run_id, path, size, status, scanned_at)
             VALUES (?1, 'a.jpg', -1, 'ok', ?2)",
            rusqlite::params![scan_run_id, now()],
        )
        .unwrap_err();
    assert!(err.to_string().contains("CHECK") || err.to_string().contains("constraint"));
}

#[test]
fn integrity_check_result_rejects_both_ids_null() {
    let conn = db();
    conn.execute(
        "INSERT INTO reference_set (name, source_format, created_at) VALUES ('master', 'json', ?1)",
        [now()],
    )
    .unwrap();
    let reference_set_id = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO check_run (check_type, reference_set_id, started_at) VALUES ('integrity', ?1, ?2)",
        rusqlite::params![reference_set_id, now()],
    )
    .unwrap();
    let check_run_id = conn.last_insert_rowid();

    let err = conn
        .execute(
            "INSERT INTO integrity_check_result (check_run_id, reference_file_id, scanned_file_id, result_status)
             VALUES (?1, NULL, NULL, 'ok')",
            [check_run_id],
        )
        .unwrap_err();
    assert!(err.to_string().contains("CHECK") || err.to_string().contains("constraint"));
}

#[test]
fn integrity_check_result_rejects_unknown_status() {
    let conn = db();
    conn.execute(
        "INSERT INTO reference_set (name, source_format, created_at) VALUES ('master', 'json', ?1)",
        [now()],
    )
    .unwrap();
    let reference_set_id = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO check_run (check_type, reference_set_id, started_at) VALUES ('integrity', ?1, ?2)",
        rusqlite::params![reference_set_id, now()],
    )
    .unwrap();
    let check_run_id = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO reference_file (reference_set_id, path, size) VALUES (?1, 'a.jpg', 10)",
        [reference_set_id],
    )
    .unwrap();
    let reference_file_id = conn.last_insert_rowid();

    let err = conn
        .execute(
            "INSERT INTO integrity_check_result (check_run_id, reference_file_id, result_status)
             VALUES (?1, ?2, 'weird')",
            rusqlite::params![check_run_id, reference_file_id],
        )
        .unwrap_err();
    assert!(err.to_string().contains("CHECK") || err.to_string().contains("constraint"));
}

#[test]
fn check_run_rejects_integrity_without_reference_set() {
    let conn = db();
    let err = conn
        .execute(
            "INSERT INTO check_run (check_type, reference_set_id, started_at) VALUES ('integrity', NULL, ?1)",
            [now()],
        )
        .unwrap_err();
    assert!(err.to_string().contains("CHECK") || err.to_string().contains("constraint"));
}

#[test]
fn check_run_rejects_duplicate_with_reference_set() {
    let conn = db();
    conn.execute(
        "INSERT INTO reference_set (name, source_format, created_at) VALUES ('master', 'json', ?1)",
        [now()],
    )
    .unwrap();
    let reference_set_id = conn.last_insert_rowid();

    let err = conn
        .execute(
            "INSERT INTO check_run (check_type, reference_set_id, started_at) VALUES ('duplicate', ?1, ?2)",
            rusqlite::params![reference_set_id, now()],
        )
        .unwrap_err();
    assert!(err.to_string().contains("CHECK") || err.to_string().contains("constraint"));
}

#[test]
fn reference_set_supersedes_is_unique_no_branching_history() {
    let conn = db();
    conn.execute(
        "INSERT INTO reference_set (name, source_format, created_at) VALUES ('master', 'json', ?1)",
        [now()],
    )
    .unwrap();
    let v1 = conn.last_insert_rowid();

    conn.execute(
        "INSERT INTO reference_set (name, source_format, supersedes_reference_set_id, created_at)
         VALUES ('master', 'json', ?1, ?2)",
        rusqlite::params![v1, now()],
    )
    .unwrap();

    // A second reference_set claiming to supersede the same v1 must be rejected —
    // §10.12 requires a linear (non-branching) version history.
    let err = conn
        .execute(
            "INSERT INTO reference_set (name, source_format, supersedes_reference_set_id, created_at)
             VALUES ('master-fork', 'json', ?1, ?2)",
            rusqlite::params![v1, now()],
        )
        .unwrap_err();
    assert!(err.to_string().to_lowercase().contains("unique"));
}

#[test]
fn duplicate_group_rejects_duplicate_sha256_within_same_check_run() {
    let conn = db();
    conn.execute(
        "INSERT INTO check_run (check_type, started_at) VALUES ('duplicate', ?1)",
        [now()],
    )
    .unwrap();
    let check_run_id = conn.last_insert_rowid();
    let sha = vec![0xABu8; 32];

    conn.execute(
        "INSERT INTO duplicate_group (check_run_id, sha256, size) VALUES (?1, ?2, 1000)",
        rusqlite::params![check_run_id, sha],
    )
    .unwrap();

    let err = conn
        .execute(
            "INSERT INTO duplicate_group (check_run_id, sha256, size) VALUES (?1, ?2, 2000)",
            rusqlite::params![check_run_id, sha],
        )
        .unwrap_err();
    assert!(err.to_string().to_lowercase().contains("unique"));
}

#[test]
fn scanned_file_cascades_on_scan_run_delete() {
    let conn = db();
    conn.execute(
        "INSERT INTO scan_run (target_type, folder_path, started_at) VALUES ('folder', '/x', ?1)",
        [now()],
    )
    .unwrap();
    let scan_run_id = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO scanned_file (scan_run_id, path, size, status, scanned_at)
         VALUES (?1, 'a.jpg', 10, 'ok', ?2)",
        rusqlite::params![scan_run_id, now()],
    )
    .unwrap();

    conn.execute("DELETE FROM scan_run WHERE id = ?1", [scan_run_id])
        .unwrap();

    let remaining: i64 = conn
        .query_row("SELECT count(*) FROM scanned_file", [], |row| row.get(0))
        .unwrap();
    assert_eq!(
        remaining, 0,
        "scanned_file rows should cascade-delete with their scan_run"
    );
}
