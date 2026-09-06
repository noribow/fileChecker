//! Regular-folder scanning (`docs/requirements.md` §10.3 information-gathering phase,
//! regular-folder path): recursively walks a folder, recording each file's path/size/
//! mtime as a `scanned_file` row. No *content* hashing happens here for regular files —
//! §10.3 explicitly keeps the regular-folder info-gathering phase to metadata
//! collection only, deferring CRC32/SHA-256 to the comparison phase's staged filter
//! (§10.2). Archive files (§3.3/§10.5/§10.6) are the one exception: their entries are
//! enumerated (not hashed) here too, since listing an archive's central directory is
//! metadata-level work — see `archive_walk`. Removable media's eager hashing (§10.8) is
//! `removable_media::scan_removable_media`, a separate entry point since its per-file
//! work (hash everything now) differs from this lazy metadata-only path.

mod archive_walk;
mod removable_media;

pub use removable_media::{scan_removable_media, scan_removable_media_with_password_policy};

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use rayon::prelude::*;
use walkdir::WalkDir;

use crate::archive::{ArchiveConfig, PasswordPolicy};
use crate::db::{repo, Connection, FileStatus, HashMode, Result, RunStatus};
use crate::retry::{is_retryable_fs_error, retry_io};

/// Outcome of one `scan_folder` call.
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct ScanSummary {
    pub scan_run_id: i64,
    pub scanned_ok: usize,
    pub scanned_error: usize,
    /// Directory entries walkdir couldn't even list (e.g. a subdirectory whose
    /// contents are inaccessible). These don't become `scanned_file` rows because we
    /// never learn their path/identity — §10.17's skip-and-continue still applies at
    /// the run level (the rest of the tree is still scanned).
    pub walk_errors: usize,
}

/// Recursively scans `root`, recording one `scanned_file` row per regular file found.
/// Errors reading an individual file's metadata are retried per §10.17 and recorded
/// with `status = 'error'`; they never abort the scan. Archive password handling
/// (§10.7) defaults to `PasswordPolicy::Reject` — use
/// `scan_folder_with_password_policy` to try registered passwords (mode 2) instead.
pub fn scan_folder(conn: &mut Connection, root: &Path, started_at: i64) -> Result<ScanSummary> {
    scan_folder_with_password_policy(conn, root, started_at, &PasswordPolicy::Reject)
}

/// Same as `scan_folder`, with an explicit archive password policy (§10.7) instead of
/// always rejecting password-protected archive entries.
pub fn scan_folder_with_password_policy(
    conn: &mut Connection,
    root: &Path,
    started_at: i64,
    policy: &PasswordPolicy,
) -> Result<ScanSummary> {
    let folder_path = root.to_string_lossy().into_owned();
    let scan_run_id = repo::insert_scan_run_folder(conn, &folder_path, HashMode::Lazy, started_at)?;

    let (file_paths, walk_errors) = walk_files(root);

    // Metadata retrieval is the I/O-bound step here; do it in parallel (§4 calls for
    // parallel I/O at scale). DB writes stay serialized in one transaction afterwards.
    let metas: Vec<FileMeta> = file_paths
        .par_iter()
        .map(|path| collect_file_meta(root, path))
        .collect();

    let archive_config = ArchiveConfig::from_settings(conn)?;

    let mut scanned_ok = 0usize;
    let mut scanned_error = 0usize;
    {
        let tx = conn.transaction()?;
        for meta in &metas {
            match meta.status {
                FileStatus::Ok => scanned_ok += 1,
                _ => scanned_error += 1,
            }
            // §10.5: archive_format identifies the format regardless of whether it
            // ends up expanded (depth 0 or unopenable) — see archive_walk's own note.
            let format = crate::archive::ArchiveFormat::detect(Path::new(&meta.relative_path));
            let scanned_file_id = repo::insert_scanned_file(
                &tx,
                &repo::NewScannedFile {
                    scan_run_id,
                    path: &meta.relative_path,
                    parent_archive_file_id: None,
                    archive_format: format.map(crate::archive::ArchiveFormat::as_str),
                    archive_depth: 0,
                    size: meta.size,
                    mtime: meta.mtime,
                    crc32: None,
                    md5: None,
                    sha1: None,
                    sha256: None,
                    status: meta.status,
                    error_message: meta.error_message.as_deref(),
                    scanned_at: started_at,
                },
            )?;
            if meta.status == FileStatus::Ok {
                archive_walk::expand_if_archive(
                    &tx,
                    scan_run_id,
                    scanned_file_id,
                    &meta.relative_path,
                    &root.join(&meta.relative_path),
                    &archive_config,
                    started_at,
                    policy,
                )?;
            }
        }
        tx.commit()?;
    }

    repo::finish_scan_run(conn, scan_run_id, RunStatus::Completed, started_at, None)?;

    Ok(ScanSummary {
        scan_run_id,
        scanned_ok,
        scanned_error,
        walk_errors,
    })
}

/// Walks `root` and returns the paths of regular files found, plus a count of
/// directory entries that couldn't be listed at all (permission denied on a
/// subdirectory, etc. — §10.17: skip and continue, don't abort the scan).
fn walk_files(root: &Path) -> (Vec<PathBuf>, usize) {
    let mut files = Vec::new();
    let mut walk_errors = 0usize;
    for entry in WalkDir::new(root).min_depth(1).into_iter() {
        match entry {
            Ok(e) if e.file_type().is_file() => files.push(e.into_path()),
            Ok(_) => {} // directories and symlinks are not scanned entries themselves
            Err(_) => walk_errors += 1,
        }
    }
    (files, walk_errors)
}

struct FileMeta {
    relative_path: String,
    size: i64,
    mtime: Option<i64>,
    status: FileStatus,
    error_message: Option<String>,
}

fn collect_file_meta(root: &Path, path: &Path) -> FileMeta {
    let relative_path = path
        .strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/");

    match retry_io(|| fs::metadata(path), is_retryable_fs_error) {
        Ok(md) => FileMeta {
            relative_path,
            size: md.len() as i64,
            mtime: md
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as i64),
            status: FileStatus::Ok,
            error_message: None,
        },
        Err(err) => FileMeta {
            relative_path,
            size: 0,
            mtime: None,
            status: FileStatus::Error,
            error_message: Some(classify_error_message(&err)),
        },
    }
}

/// Short one-line error summaries for `scanned_file.error_message` (§10.17: "簡潔な
/// 一行サマリ"). Full diagnostic detail belongs in the text log file, which is added
/// alongside the CLI/GUI layers that can surface it (P13).
fn classify_error_message(err: &io::Error) -> String {
    if err.kind() == io::ErrorKind::PermissionDenied {
        "アクセス不可".to_string()
    } else {
        format!("I/Oエラー(3回再試行後失敗): {err}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::open_in_memory;
    use std::fs;

    fn now() -> i64 {
        1_700_000_000_000
    }

    #[test]
    fn scans_nested_files_and_records_metadata() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"hello").unwrap();
        fs::create_dir(dir.path().join("sub")).unwrap();
        fs::write(dir.path().join("sub/b.txt"), b"world!!").unwrap();

        let mut conn = open_in_memory().unwrap();
        let summary = scan_folder(&mut conn, dir.path(), now()).unwrap();

        assert_eq!(summary.scanned_ok, 2);
        assert_eq!(summary.scanned_error, 0);
        assert_eq!(summary.walk_errors, 0);

        let scan_run = repo::get_scan_run(&conn, summary.scan_run_id)
            .unwrap()
            .unwrap();
        assert_eq!(scan_run.status, "completed");

        let mut stmt = conn
            .prepare(
                "SELECT path, size, status FROM scanned_file WHERE scan_run_id = ?1 ORDER BY path",
            )
            .unwrap();
        let rows: Vec<(String, i64, String)> = stmt
            .query_map([summary.scan_run_id], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();

        assert_eq!(
            rows,
            vec![
                ("a.txt".to_string(), 5, "ok".to_string()),
                ("sub/b.txt".to_string(), 7, "ok".to_string()),
            ]
        );
    }

    #[test]
    fn empty_folder_produces_a_completed_run_with_no_files() {
        let dir = tempfile::tempdir().unwrap();
        let mut conn = open_in_memory().unwrap();
        let summary = scan_folder(&mut conn, dir.path(), now()).unwrap();
        assert_eq!(summary.scanned_ok, 0);
        assert_eq!(summary.scanned_error, 0);
    }

    #[test]
    fn classify_error_message_distinguishes_permission_from_other_io_errors() {
        let permission = classify_error_message(&io::Error::from(io::ErrorKind::PermissionDenied));
        assert_eq!(permission, "アクセス不可");

        let other = classify_error_message(&io::Error::other("device busy"));
        assert!(other.starts_with("I/Oエラー(3回再試行後失敗)"));
    }

    /// Exercises the real skip-and-continue path with an actual OS-level permission
    /// error, where the environment allows it. Running as root (common in sandboxed
    /// dev/CI containers) makes permission bits unenforceable, so this test verifies
    /// its own precondition first and skips gracefully rather than failing when it
    /// can't be exercised — `classify_error_message_distinguishes_*` above covers the
    /// same error-message logic in an environment-independent way.
    #[cfg(unix)]
    #[test]
    fn inaccessible_file_is_recorded_as_error_without_aborting_the_scan() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("readable.txt"), b"ok").unwrap();
        let locked_path = dir.path().join("locked.txt");
        fs::write(&locked_path, b"secret").unwrap();
        fs::set_permissions(&locked_path, fs::Permissions::from_mode(0o000)).unwrap();

        if fs::metadata(&locked_path).is_ok() {
            eprintln!(
                "skipping: running with privileges that bypass permission bits (e.g. root); \
                 cannot exercise a real PermissionDenied error in this environment"
            );
            fs::set_permissions(&locked_path, fs::Permissions::from_mode(0o644)).unwrap();
            return;
        }

        let mut conn = open_in_memory().unwrap();
        let summary = scan_folder(&mut conn, dir.path(), now()).unwrap();

        // The whole scan must still complete despite one unreadable file.
        assert_eq!(summary.scanned_ok, 1);
        assert_eq!(summary.scanned_error, 1);

        let error_message: String = conn
            .query_row(
                "SELECT error_message FROM scanned_file WHERE path = 'locked.txt'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(error_message, "アクセス不可");

        fs::set_permissions(&locked_path, fs::Permissions::from_mode(0o644)).unwrap();
    }
}
