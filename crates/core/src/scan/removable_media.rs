//! Removable-media scanning, eager hash mode (`docs/requirements.md` §10.8/§3.2).
//! Unlike the regular-folder path (§10.3's lazy metadata-only info-gathering), every
//! file's CRC32 and SHA-256 are computed in this single connected pass: the medium
//! might not be reconnectable later, so the comparison phase's usual "come back and
//! hash only the candidates that survived the size/CRC32 filter" approach can't be
//! relied on here (§10.2's staged filter still runs at comparison time — it just never
//! needs to touch the medium again, since the values are already in `scanned_file`).

use std::fs;
use std::path::Path;
use std::time::UNIX_EPOCH;

use rayon::prelude::*;

use crate::archive::{ArchiveConfig, ArchiveFormat, PasswordPolicy};
use crate::db::{repo, Connection, FileStatus, HashMode, Result, RunStatus};
use crate::hash::{hash_file, HashAlgorithm};
use crate::retry::{is_retryable_fs_error, retry_io};

use super::{archive_walk, classify_error_message, walk_files, ScanSummary};

/// Scans `mount_path` (the currently-connected mount point of `removable_media_id`,
/// already identified — see `media`), recording one `scanned_file` row per file found
/// with CRC32/SHA-256 already computed. Archive structure (§3.3/§10.6) is enumerated
/// the same way as for regular folders; nested entries still get hashed lazily at
/// comparison time (a known limitation — see `docs/progress-log.md`'s P8 entry — since
/// eagerly hashing every archive-nested entry too would be a much larger read during
/// the connected window). Archive password handling (§10.7) defaults to
/// `PasswordPolicy::Reject` — use `scan_removable_media_with_password_policy` for mode 2.
pub fn scan_removable_media(
    conn: &mut Connection,
    removable_media_id: i64,
    mount_path: &Path,
    started_at: i64,
) -> Result<ScanSummary> {
    scan_removable_media_with_password_policy(
        conn,
        removable_media_id,
        mount_path,
        started_at,
        &PasswordPolicy::Reject,
    )
}

/// Same as `scan_removable_media`, with an explicit archive password policy (§10.7).
pub fn scan_removable_media_with_password_policy(
    conn: &mut Connection,
    removable_media_id: i64,
    mount_path: &Path,
    started_at: i64,
    policy: &PasswordPolicy,
) -> Result<ScanSummary> {
    let scan_run_id = repo::insert_scan_run_removable_media(
        conn,
        removable_media_id,
        HashMode::Eager,
        started_at,
    )?;

    let (file_paths, walk_errors) = walk_files(mount_path);

    // I/O-bound (metadata + full read + hash per file): parallelize across files (§4).
    let metas: Vec<EagerFileMeta> = file_paths
        .par_iter()
        .map(|path| collect_eager_file_meta(mount_path, path))
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
            let format = ArchiveFormat::detect(Path::new(&meta.relative_path));
            let scanned_file_id = repo::insert_scanned_file(
                &tx,
                &repo::NewScannedFile {
                    scan_run_id,
                    path: &meta.relative_path,
                    parent_archive_file_id: None,
                    archive_format: format.map(ArchiveFormat::as_str),
                    archive_depth: 0,
                    size: meta.size,
                    mtime: meta.mtime,
                    crc32: meta.crc32,
                    md5: None,
                    sha1: None,
                    sha256: meta.sha256.as_ref().map(|s| s.as_slice()),
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
                    &mount_path.join(&meta.relative_path),
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

struct EagerFileMeta {
    relative_path: String,
    size: i64,
    mtime: Option<i64>,
    crc32: Option<u32>,
    sha256: Option<[u8; 32]>,
    status: FileStatus,
    error_message: Option<String>,
}

fn collect_eager_file_meta(root: &Path, path: &Path) -> EagerFileMeta {
    let relative_path = path
        .strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/");

    let metadata = match retry_io(|| fs::metadata(path), is_retryable_fs_error) {
        Ok(md) => md,
        Err(err) => {
            return EagerFileMeta {
                relative_path,
                size: 0,
                mtime: None,
                crc32: None,
                sha256: None,
                status: FileStatus::Error,
                error_message: Some(classify_error_message(&err)),
            }
        }
    };
    let mtime = metadata
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64);

    match hash_file(path, &[HashAlgorithm::Crc32, HashAlgorithm::Sha256]) {
        Ok(values) => EagerFileMeta {
            relative_path,
            size: metadata.len() as i64,
            mtime,
            crc32: values.crc32,
            sha256: values.sha256,
            status: FileStatus::Ok,
            error_message: None,
        },
        Err(err) => EagerFileMeta {
            relative_path,
            size: metadata.len() as i64,
            mtime,
            crc32: None,
            sha256: None,
            status: FileStatus::Error,
            error_message: Some(classify_error_message(&err)),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::open_in_memory;

    fn now() -> i64 {
        1_700_000_000_000
    }

    #[test]
    fn scans_files_with_hashes_already_computed_under_eager_mode() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.jpg"), b"hello").unwrap();

        let mut conn = open_in_memory().unwrap();
        let media_id = repo::find_or_create_removable_media(
            &conn,
            "linux",
            "device_serial",
            "TEST123",
            None,
            now(),
        )
        .unwrap();

        let summary = scan_removable_media(&mut conn, media_id, dir.path(), now()).unwrap();
        assert_eq!(summary.scanned_ok, 1);
        assert_eq!(summary.scanned_error, 0);

        let scan_run = repo::get_scan_run(&conn, summary.scan_run_id)
            .unwrap()
            .unwrap();
        assert_eq!(scan_run.removable_media_id, Some(media_id));
        let hash_mode: String = conn
            .query_row(
                "SELECT hash_mode FROM scan_run WHERE id = ?1",
                [summary.scan_run_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(hash_mode, "eager");

        let (crc32, sha256, status): (Option<u32>, Option<Vec<u8>>, String) = conn
            .query_row(
                "SELECT crc32, sha256, status FROM scanned_file WHERE path = 'a.jpg'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(status, "ok");
        assert!(crc32.is_some());
        let expected_sha256 = crate::hash::hash_file_sha256(&dir.path().join("a.jpg")).unwrap();
        assert_eq!(sha256.unwrap(), expected_sha256);
    }
}
