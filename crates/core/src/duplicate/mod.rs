//! Duplicate check, regular folders only (`docs/requirements.md` §10.1/§10.2/§10.3,
//! P4). Archive contents (§3.3) and removable media's eager hash mode (§10.8) are out
//! of scope until P7/P8; this only consumes `scanned_file` rows left by `scan_folder`.
//!
//! Runs the comparison phase's staged filter — size -> CRC32 (whole file) -> SHA-256
//! (whole file), §10.2 — over one or more prior `scan_run`s (§3.2 allows checking
//! several folders at once) and records the resulting groups as `duplicate_group`/
//! `duplicate_group_member` rows under a new `check_run`.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

use rayon::prelude::*;

use crate::db::{repo, Connection, Result, RunStatus};
use crate::hash::{hash_file_crc32, hash_file_sha256};

/// Outcome of one `run_duplicate_check` call.
#[derive(Debug, Clone, Copy)]
pub struct DuplicateCheckSummary {
    pub check_run_id: i64,
    pub group_count: usize,
    pub duplicate_file_count: usize,
    /// Files that reached this comparison phase with `scanned_file.status = 'ok'` but
    /// then failed to hash (e.g. removed or became unreadable since the scan). Excluded
    /// from grouping but never silently dropped, per §10.11.
    pub error_count: usize,
}

struct Candidate {
    scanned_file_id: i64,
    full_path: PathBuf,
    size: i64,
}

/// Runs a duplicate check across the `scanned_file` rows of `scan_run_ids`. Files whose
/// scan already recorded `status = 'error'` are skipped entirely (already accounted for
/// as scan-time errors, §10.17); files that fail to hash during this comparison phase
/// are newly marked errored and excluded from grouping (§10.11).
pub fn run_duplicate_check(
    conn: &mut Connection,
    scan_run_ids: &[i64],
    started_at: i64,
) -> Result<DuplicateCheckSummary> {
    let check_run_id = repo::insert_check_run_duplicate(conn, started_at)?;
    for &scan_run_id in scan_run_ids {
        repo::insert_check_run_source(conn, check_run_id, scan_run_id)?;
    }

    let candidates: Vec<Candidate> = repo::list_ok_scanned_files_for_scan_runs(conn, scan_run_ids)?
        .into_iter()
        .map(|f| Candidate {
            scanned_file_id: f.id,
            full_path: Path::new(&f.folder_path).join(&f.path),
            size: f.size,
        })
        .collect();

    let mut error_count = 0usize;

    // Stage 1 (size): a size with only one file can never be a duplicate, so it's
    // dropped here without ever touching the disk.
    let mut by_size: HashMap<i64, Vec<Candidate>> = HashMap::new();
    for c in candidates {
        by_size.entry(c.size).or_default().push(c);
    }
    by_size.retain(|_, group| group.len() >= 2);

    // Stage 2 (CRC32, whole file): computed only for files that already share a size.
    let mut by_size_crc32: HashMap<(i64, u32), Vec<Candidate>> = HashMap::new();
    for (size, group) in by_size {
        for (c, result) in hash_group(group, hash_file_crc32) {
            match result {
                Ok(crc32) => {
                    repo::update_scanned_file_crc32(conn, c.scanned_file_id, crc32)?;
                    by_size_crc32.entry((size, crc32)).or_default().push(c);
                }
                Err(err) => {
                    repo::mark_scanned_file_error(conn, c.scanned_file_id, &err.to_string())?;
                    error_count += 1;
                }
            }
        }
    }
    by_size_crc32.retain(|_, group| group.len() >= 2);

    // Stage 3 (SHA-256, whole file): computed only for files that also share a CRC32.
    // Grouped by SHA-256 alone for the final result, matching `duplicate_group`'s
    // UNIQUE(check_run_id, sha256) — identical content always implies identical size.
    let mut by_sha256: HashMap<[u8; 32], Vec<Candidate>> = HashMap::new();
    for group in by_size_crc32.into_values() {
        for (c, result) in hash_group(group, hash_file_sha256) {
            match result {
                Ok(sha256) => {
                    repo::update_scanned_file_sha256(conn, c.scanned_file_id, &sha256)?;
                    by_sha256.entry(sha256).or_default().push(c);
                }
                Err(err) => {
                    repo::mark_scanned_file_error(conn, c.scanned_file_id, &err.to_string())?;
                    error_count += 1;
                }
            }
        }
    }
    by_sha256.retain(|_, group| group.len() >= 2);

    let mut group_count = 0usize;
    let mut duplicate_file_count = 0usize;
    {
        let tx = conn.transaction()?;
        for (sha256, members) in by_sha256 {
            let size = members[0].size;
            let group_id = repo::insert_duplicate_group(&tx, check_run_id, &sha256, size)?;
            for m in &members {
                repo::add_duplicate_group_member(&tx, group_id, m.scanned_file_id)?;
            }
            group_count += 1;
            duplicate_file_count += members.len();
        }
        tx.commit()?;
    }

    repo::finish_check_run(conn, check_run_id, RunStatus::Completed, started_at)?;

    Ok(DuplicateCheckSummary {
        check_run_id,
        group_count,
        duplicate_file_count,
        error_count,
    })
}

/// Hashes every candidate in `group` in parallel (§4's parallel-I/O requirement),
/// pairing each candidate back up with its result.
fn hash_group<T: Send>(
    group: Vec<Candidate>,
    hash_fn: impl Fn(&Path) -> io::Result<T> + Sync,
) -> Vec<(Candidate, io::Result<T>)> {
    group
        .into_par_iter()
        .map(|c| {
            let result = hash_fn(&c.full_path);
            (c, result)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::open_in_memory;
    use crate::scan::scan_folder;
    use std::fs::File;

    fn now() -> i64 {
        1_700_000_000_000
    }

    #[test]
    fn groups_identical_files_across_two_folders() {
        let dir_a = tempfile::tempdir().unwrap();
        let dir_b = tempfile::tempdir().unwrap();
        std::fs::write(dir_a.path().join("a.jpg"), b"same content").unwrap();
        std::fs::write(dir_b.path().join("b.jpg"), b"same content").unwrap();
        std::fs::write(dir_a.path().join("unique.jpg"), b"only in a").unwrap();

        let mut conn = open_in_memory().unwrap();
        let run_a = scan_folder(&mut conn, dir_a.path(), now())
            .unwrap()
            .scan_run_id;
        let run_b = scan_folder(&mut conn, dir_b.path(), now())
            .unwrap()
            .scan_run_id;

        let summary = run_duplicate_check(&mut conn, &[run_a, run_b], now()).unwrap();

        assert_eq!(summary.group_count, 1);
        assert_eq!(summary.duplicate_file_count, 2);
        assert_eq!(summary.error_count, 0);

        let member_count: i64 = conn
            .query_row(
                "SELECT member_count FROM duplicate_group WHERE check_run_id = ?1",
                [summary.check_run_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(member_count, 2);
    }

    #[test]
    fn same_size_different_content_is_not_grouped() {
        let dir = tempfile::tempdir().unwrap();
        // Same length, different bytes -> same size, different CRC32/SHA-256.
        std::fs::write(dir.path().join("x.bin"), b"AAAAAAAA").unwrap();
        std::fs::write(dir.path().join("y.bin"), b"BBBBBBBB").unwrap();

        let mut conn = open_in_memory().unwrap();
        let run_id = scan_folder(&mut conn, dir.path(), now())
            .unwrap()
            .scan_run_id;

        let summary = run_duplicate_check(&mut conn, &[run_id], now()).unwrap();

        assert_eq!(summary.group_count, 0);
        assert_eq!(summary.duplicate_file_count, 0);
        assert_eq!(summary.error_count, 0);
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_file_is_excluded_and_recorded_as_error() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let locked_path = dir.path().join("locked.bin");
        std::fs::write(dir.path().join("readable.bin"), b"duplicate content").unwrap();
        std::fs::write(&locked_path, b"duplicate content").unwrap();

        let mut conn = open_in_memory().unwrap();
        let run_id = scan_folder(&mut conn, dir.path(), now())
            .unwrap()
            .scan_run_id;

        std::fs::set_permissions(&locked_path, std::fs::Permissions::from_mode(0o000)).unwrap();
        if File::open(&locked_path).is_ok() {
            eprintln!(
                "skipping: running with privileges that bypass permission bits (e.g. root); \
                 cannot exercise a real read failure in this environment"
            );
            std::fs::set_permissions(&locked_path, std::fs::Permissions::from_mode(0o644)).unwrap();
            return;
        }

        let summary = run_duplicate_check(&mut conn, &[run_id], now()).unwrap();

        // The pair shared a size, so both entered the CRC32 stage; only the readable
        // one could be hashed, so no group can form and the other is a hash error.
        assert_eq!(summary.group_count, 0);
        assert_eq!(summary.duplicate_file_count, 0);
        assert_eq!(summary.error_count, 1);

        let status: String = conn
            .query_row(
                "SELECT status FROM scanned_file WHERE path = 'locked.bin'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(status, "error");

        std::fs::set_permissions(&locked_path, std::fs::Permissions::from_mode(0o644)).unwrap();
    }

    #[test]
    fn files_already_errored_at_scan_time_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.bin"), b"same content").unwrap();
        std::fs::write(dir.path().join("b.bin"), b"same content").unwrap();

        let mut conn = open_in_memory().unwrap();
        let run_id = scan_folder(&mut conn, dir.path(), now())
            .unwrap()
            .scan_run_id;

        // Simulate a scan-time error on one of the two identical files (as if it went
        // missing between metadata collection and this comparison run).
        conn.execute(
            "UPDATE scanned_file SET status = 'error', error_message = 'gone' WHERE path = 'b.bin'",
            [],
        )
        .unwrap();

        let summary = run_duplicate_check(&mut conn, &[run_id], now()).unwrap();

        // Only one candidate remains once the pre-errored file is excluded, so its
        // size-group has a single member and never reaches the hashing stages at all.
        assert_eq!(summary.group_count, 0);
        assert_eq!(summary.duplicate_file_count, 0);
        assert_eq!(summary.error_count, 0);
    }
}
