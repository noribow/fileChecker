//! Minimal CRUD over the §10.12 schema, enough to drive P2's integration tests and to
//! give later phases (scanning, integrity/duplicate check) a typed API instead of raw
//! SQL scattered through business logic. Query helpers are added as later phases need
//! them rather than speculatively now.

use rusqlite::{params, params_from_iter, Connection, OptionalExtension, Result};

use super::models::{FileStatus, HashMode, ResultStatus, RunStatus, TargetType};

// ---- scan_run --------------------------------------------------------------------

pub fn insert_scan_run_folder(
    conn: &Connection,
    folder_path: &str,
    hash_mode: HashMode,
    started_at: i64,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO scan_run (target_type, folder_path, removable_media_id, hash_mode, status, started_at)
         VALUES ('folder', ?1, NULL, ?2, 'running', ?3)",
        params![folder_path, hash_mode.as_str(), started_at],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn insert_scan_run_removable_media(
    conn: &Connection,
    removable_media_id: i64,
    hash_mode: HashMode,
    started_at: i64,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO scan_run (target_type, folder_path, removable_media_id, hash_mode, status, started_at)
         VALUES ('removable_media', NULL, ?1, ?2, 'running', ?3)",
        params![removable_media_id, hash_mode.as_str(), started_at],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn finish_scan_run(
    conn: &Connection,
    scan_run_id: i64,
    status: RunStatus,
    completed_at: i64,
    error_message: Option<&str>,
) -> Result<()> {
    conn.execute(
        "UPDATE scan_run SET status = ?1, completed_at = ?2, error_message = ?3 WHERE id = ?4",
        params![status.as_str(), completed_at, error_message, scan_run_id],
    )?;
    Ok(())
}

pub struct ScanRunRow {
    pub id: i64,
    pub target_type: TargetType,
    pub folder_path: Option<String>,
    pub removable_media_id: Option<i64>,
    pub status: String,
}

pub fn get_scan_run(conn: &Connection, id: i64) -> Result<Option<ScanRunRow>> {
    conn.query_row(
        "SELECT id, target_type, folder_path, removable_media_id, status FROM scan_run WHERE id = ?1",
        params![id],
        |row| {
            let target_type: String = row.get(1)?;
            Ok(ScanRunRow {
                id: row.get(0)?,
                target_type: if target_type == "folder" {
                    TargetType::Folder
                } else {
                    TargetType::RemovableMedia
                },
                folder_path: row.get(2)?,
                removable_media_id: row.get(3)?,
                status: row.get(4)?,
            })
        },
    )
    .optional()
}

// ---- scanned_file -----------------------------------------------------------------

/// Fields needed to record one scanned entry (regular file or archive-nested entry,
/// §3.3/§10.6). Hash fields are `None` when not yet computed (lazy path, §10.2) or
/// when the file couldn't be read (`status = error`).
pub struct NewScannedFile<'a> {
    pub scan_run_id: i64,
    pub path: &'a str,
    pub parent_archive_file_id: Option<i64>,
    pub archive_format: Option<&'a str>,
    pub archive_depth: i64,
    pub size: i64,
    pub mtime: Option<i64>,
    pub crc32: Option<u32>,
    pub md5: Option<&'a [u8]>,
    pub sha1: Option<&'a [u8]>,
    pub sha256: Option<&'a [u8]>,
    pub status: FileStatus,
    pub error_message: Option<&'a str>,
    pub scanned_at: i64,
}

pub fn insert_scanned_file(conn: &Connection, f: &NewScannedFile<'_>) -> Result<i64> {
    conn.execute(
        "INSERT INTO scanned_file
            (scan_run_id, path, parent_archive_file_id, archive_format, archive_depth,
             size, mtime, crc32, md5, sha1, sha256, status, error_message, scanned_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        params![
            f.scan_run_id,
            f.path,
            f.parent_archive_file_id,
            f.archive_format,
            f.archive_depth,
            f.size,
            f.mtime,
            f.crc32,
            f.md5,
            f.sha1,
            f.sha256,
            f.status.as_str(),
            f.error_message,
            f.scanned_at,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

pub struct ScannedFileForDuplicate {
    pub id: i64,
    pub folder_path: String,
    pub path: String,
    pub size: i64,
}

/// Regular (non-archived, successfully scanned) files from one or more `scan_run`s,
/// joined with `scan_run.folder_path` so callers can resolve a full path on disk.
/// Used by the duplicate-check comparison phase (§10.3) to gather everything eligible
/// for grouping, possibly across several previously-scanned folders at once (§3.2).
/// Archive-nested entries (`parent_archive_file_id` set) and removable-media scan runs
/// (no `folder_path` yet, P8) are excluded — out of scope until P7/P8.
pub fn list_ok_scanned_files_for_scan_runs(
    conn: &Connection,
    scan_run_ids: &[i64],
) -> Result<Vec<ScannedFileForDuplicate>> {
    if scan_run_ids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = scan_run_ids
        .iter()
        .map(|_| "?")
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT sf.id, sr.folder_path, sf.path, sf.size
         FROM scanned_file sf
         JOIN scan_run sr ON sr.id = sf.scan_run_id
         WHERE sf.scan_run_id IN ({placeholders})
           AND sf.status = 'ok'
           AND sf.parent_archive_file_id IS NULL
           AND sr.folder_path IS NOT NULL"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(params_from_iter(scan_run_ids.iter()), |row| {
            Ok(ScannedFileForDuplicate {
                id: row.get(0)?,
                folder_path: row.get(1)?,
                path: row.get(2)?,
                size: row.get(3)?,
            })
        })?
        .collect::<Result<Vec<_>>>()?;
    Ok(rows)
}

pub struct ScannedFileForIntegrity {
    pub id: i64,
    pub folder_path: String,
    pub path: String,
    pub size: i64,
    pub sha256: Option<Vec<u8>>,
    pub status: FileStatus,
    pub error_message: Option<String>,
}

/// All non-archived files (any `status`) from one or more `scan_run`s, for the
/// integrity-check comparison phase (§10.11). Unlike
/// `list_ok_scanned_files_for_scan_runs`, scan-time errors are included rather than
/// filtered out: a file that matches a reference-set path but couldn't be read must
/// still surface as `result_status = 'error'`, not silently vanish into `missing`.
pub fn list_scanned_files_for_integrity(
    conn: &Connection,
    scan_run_ids: &[i64],
) -> Result<Vec<ScannedFileForIntegrity>> {
    if scan_run_ids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = scan_run_ids
        .iter()
        .map(|_| "?")
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT sf.id, sr.folder_path, sf.path, sf.size, sf.sha256, sf.status, sf.error_message
         FROM scanned_file sf
         JOIN scan_run sr ON sr.id = sf.scan_run_id
         WHERE sf.scan_run_id IN ({placeholders})
           AND sf.parent_archive_file_id IS NULL
           AND sr.folder_path IS NOT NULL"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(params_from_iter(scan_run_ids.iter()), |row| {
            let status: String = row.get(5)?;
            Ok(ScannedFileForIntegrity {
                id: row.get(0)?,
                folder_path: row.get(1)?,
                path: row.get(2)?,
                size: row.get(3)?,
                sha256: row.get(4)?,
                status: FileStatus::parse_str(&status).expect("valid scanned_file.status"),
                error_message: row.get(6)?,
            })
        })?
        .collect::<Result<Vec<_>>>()?;
    Ok(rows)
}

/// Records a hash value computed for a `scanned_file` during the comparison phase's
/// staged filter (§10.2/§10.3): the regular-folder lazy path only fills these in when a
/// file actually reaches that stage (most files never need SHA-256).
pub fn update_scanned_file_crc32(conn: &Connection, id: i64, crc32: u32) -> Result<()> {
    conn.execute(
        "UPDATE scanned_file SET crc32 = ?1 WHERE id = ?2",
        params![crc32, id],
    )?;
    Ok(())
}

pub fn update_scanned_file_sha256(conn: &Connection, id: i64, sha256: &[u8]) -> Result<()> {
    conn.execute(
        "UPDATE scanned_file SET sha256 = ?1 WHERE id = ?2",
        params![sha256, id],
    )?;
    Ok(())
}

/// Marks a `scanned_file` as errored after a hash-computation failure discovered during
/// the comparison phase (as opposed to the metadata-collection failures P3's scan phase
/// already records) — §10.11 requires this be recorded explicitly, never silently
/// dropped from results.
pub fn mark_scanned_file_error(conn: &Connection, id: i64, error_message: &str) -> Result<()> {
    conn.execute(
        "UPDATE scanned_file SET status = 'error', error_message = ?1 WHERE id = ?2",
        params![error_message, id],
    )?;
    Ok(())
}

// ---- reference_set / reference_file ------------------------------------------------

pub fn insert_reference_set(
    conn: &Connection,
    name: &str,
    source_format: &str,
    source_path: Option<&str>,
    generated_from_scan_run_id: Option<i64>,
    supersedes_reference_set_id: Option<i64>,
    created_at: i64,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO reference_set
            (name, source_format, source_path, generated_from_scan_run_id, supersedes_reference_set_id, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            name,
            source_format,
            source_path,
            generated_from_scan_run_id,
            supersedes_reference_set_id,
            created_at
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

pub struct NewReferenceFile<'a> {
    pub reference_set_id: i64,
    pub path: &'a str,
    pub size: i64,
    pub crc32: Option<u32>,
    pub md5: Option<&'a [u8]>,
    pub sha1: Option<&'a [u8]>,
    pub sha256: Option<&'a [u8]>,
}

pub fn insert_reference_file(conn: &Connection, f: &NewReferenceFile<'_>) -> Result<i64> {
    conn.execute(
        "INSERT INTO reference_file (reference_set_id, path, size, crc32, md5, sha1, sha256)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            f.reference_set_id,
            f.path,
            f.size,
            f.crc32,
            f.md5,
            f.sha1,
            f.sha256
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

pub struct ReferenceFileRow {
    pub id: i64,
    pub path: String,
    pub size: i64,
    pub sha256: Option<Vec<u8>>,
}

/// All entries of one `reference_set`, for the integrity-check comparison phase to
/// index by path (§10.11). Only `sha256` is read: P5's own generator (native JSON
/// format, §10.1) always fills it in, and matching against the other algorithms is an
/// external-format-import concern out of scope until P9.
pub fn list_reference_files(
    conn: &Connection,
    reference_set_id: i64,
) -> Result<Vec<ReferenceFileRow>> {
    let mut stmt = conn
        .prepare("SELECT id, path, size, sha256 FROM reference_file WHERE reference_set_id = ?1")?;
    let rows = stmt
        .query_map(params![reference_set_id], |row| {
            Ok(ReferenceFileRow {
                id: row.get(0)?,
                path: row.get(1)?,
                size: row.get(2)?,
                sha256: row.get(3)?,
            })
        })?
        .collect::<Result<Vec<_>>>()?;
    Ok(rows)
}

// ---- check_run / check_run_source --------------------------------------------------

pub fn insert_check_run_integrity(
    conn: &Connection,
    reference_set_id: i64,
    started_at: i64,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO check_run (check_type, reference_set_id, status, started_at)
         VALUES ('integrity', ?1, 'running', ?2)",
        params![reference_set_id, started_at],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn insert_check_run_duplicate(conn: &Connection, started_at: i64) -> Result<i64> {
    conn.execute(
        "INSERT INTO check_run (check_type, reference_set_id, status, started_at)
         VALUES ('duplicate', NULL, 'running', ?1)",
        params![started_at],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn finish_check_run(
    conn: &Connection,
    check_run_id: i64,
    status: RunStatus,
    completed_at: i64,
) -> Result<()> {
    conn.execute(
        "UPDATE check_run SET status = ?1, completed_at = ?2 WHERE id = ?3",
        params![status.as_str(), completed_at, check_run_id],
    )?;
    Ok(())
}

pub fn insert_check_run_source(
    conn: &Connection,
    check_run_id: i64,
    scan_run_id: i64,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO check_run_source (check_run_id, scan_run_id) VALUES (?1, ?2)",
        params![check_run_id, scan_run_id],
    )?;
    Ok(conn.last_insert_rowid())
}

// ---- integrity_check_result ---------------------------------------------------------

pub fn insert_integrity_check_result(
    conn: &Connection,
    check_run_id: i64,
    reference_file_id: Option<i64>,
    scanned_file_id: Option<i64>,
    result_status: ResultStatus,
    detail: Option<&str>,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO integrity_check_result
            (check_run_id, reference_file_id, scanned_file_id, result_status, detail)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            check_run_id,
            reference_file_id,
            scanned_file_id,
            result_status.as_str(),
            detail
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

pub struct IntegrityResultRow {
    pub id: i64,
    pub result_status: ResultStatus,
    pub scanned_file_id: Option<i64>,
    pub scanned_file_path: Option<String>,
    pub detail: Option<String>,
}

/// Results for one `check_run`, optionally filtered to a single status
/// (mirrors the GUI/CLI `--status` filter, §10.14/§10.16).
pub fn list_integrity_results(
    conn: &Connection,
    check_run_id: i64,
    status_filter: Option<ResultStatus>,
) -> Result<Vec<IntegrityResultRow>> {
    let sql = "SELECT r.id, r.result_status, r.scanned_file_id, sf.path, r.detail
               FROM integrity_check_result r
               LEFT JOIN scanned_file sf ON sf.id = r.scanned_file_id
               WHERE r.check_run_id = ?1 AND (?2 IS NULL OR r.result_status = ?2)
               ORDER BY r.id";
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt
        .query_map(
            params![check_run_id, status_filter.map(ResultStatus::as_str)],
            |row| {
                let status: String = row.get(1)?;
                Ok(IntegrityResultRow {
                    id: row.get(0)?,
                    result_status: ResultStatus::parse_str(&status).expect("valid result_status"),
                    scanned_file_id: row.get(2)?,
                    scanned_file_path: row.get(3)?,
                    detail: row.get(4)?,
                })
            },
        )?
        .collect::<Result<Vec<_>>>()?;
    Ok(rows)
}

// ---- duplicate_group / duplicate_group_member ---------------------------------------

pub fn insert_duplicate_group(
    conn: &Connection,
    check_run_id: i64,
    sha256: &[u8],
    size: i64,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO duplicate_group (check_run_id, sha256, size, member_count) VALUES (?1, ?2, ?3, 0)",
        params![check_run_id, sha256, size],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn add_duplicate_group_member(
    conn: &Connection,
    duplicate_group_id: i64,
    scanned_file_id: i64,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO duplicate_group_member (duplicate_group_id, scanned_file_id) VALUES (?1, ?2)",
        params![duplicate_group_id, scanned_file_id],
    )?;
    conn.execute(
        "UPDATE duplicate_group SET member_count = member_count + 1 WHERE id = ?1",
        params![duplicate_group_id],
    )?;
    Ok(conn.last_insert_rowid())
}
