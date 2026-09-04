//! Duplicate check (`docs/requirements.md` §10.1/§10.2/§10.3). Regular files and
//! archive-nested entries alike (§3.3) — this consumes every `scanned_file` row left by
//! `scan_folder`/`scan::archive_walk`. Removable media's eager hash mode (§10.8) is out
//! of scope until P8.
//!
//! Runs the comparison phase's staged filter — size -> CRC32 (whole file) -> SHA-256
//! (whole file), §10.2 — over one or more prior `scan_run`s (§3.2 allows checking
//! several folders at once) and records the resulting groups as `duplicate_group`/
//! `duplicate_group_member` rows under a new `check_run`.

use std::collections::HashMap;

use rayon::prelude::*;

use crate::archive;
use crate::db::{repo, Connection, Result, RunStatus};
use crate::hash::HashAlgorithm;

/// Outcome of one `run_duplicate_check` call.
#[derive(Debug, Clone, Copy)]
pub struct DuplicateCheckSummary {
    pub check_run_id: i64,
    pub group_count: usize,
    pub duplicate_file_count: usize,
    /// Files that reached this comparison phase with `scanned_file.status = 'ok'` but
    /// then failed to hash (e.g. removed or became unreadable since the scan, or —
    /// for archive-nested entries — the containing archive became unreadable or the
    /// entry's actual size violated its declared size, §10.6). Excluded from grouping
    /// but never silently dropped, per §10.11.
    pub error_count: usize,
}

#[derive(Clone, Copy)]
struct Candidate {
    scanned_file_id: i64,
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

    let rows = repo::list_ok_scanned_files_for_scan_runs(conn, scan_run_ids)?;
    let candidates: Vec<Candidate> = rows
        .iter()
        .map(|f| Candidate {
            scanned_file_id: f.id,
            size: f.size,
        })
        .collect();
    // Kept alive for the whole run: `archive::resolve_hops` walks parent pointers
    // through this map (no extra DB round trips) to reach any archive-nested entry's
    // containing file, whether or not that ancestor is itself a duplicate candidate.
    let by_id: HashMap<i64, repo::ScannedFileForDuplicate> =
        rows.into_iter().map(|f| (f.id, f)).collect();

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
        for (c, result) in hash_group_crc32(group, &by_id) {
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
        for (c, result) in hash_group_sha256(group, &by_id) {
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
/// resolving each one's location (plain file or archive-nested entry, §3.3) through
/// `by_id` and pairing it back up with its result.
fn hash_group_crc32(
    group: Vec<Candidate>,
    by_id: &HashMap<i64, repo::ScannedFileForDuplicate>,
) -> Vec<(Candidate, std::io::Result<u32>)> {
    group
        .into_par_iter()
        .map(|c| {
            let (root, hops) = archive::resolve_hops(c.scanned_file_id, by_id);
            let result = archive::hash_entry(&root, &hops, &[HashAlgorithm::Crc32])
                .map(|v| v.crc32.expect("crc32 was requested"));
            (c, result)
        })
        .collect()
}

fn hash_group_sha256(
    group: Vec<Candidate>,
    by_id: &HashMap<i64, repo::ScannedFileForDuplicate>,
) -> Vec<(Candidate, std::io::Result<[u8; 32]>)> {
    group
        .into_par_iter()
        .map(|c| {
            let (root, hops) = archive::resolve_hops(c.scanned_file_id, by_id);
            let result = archive::hash_entry(&root, &hops, &[HashAlgorithm::Sha256])
                .map(|v| v.sha256.expect("sha256 was requested"));
            (c, result)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::open_in_memory;
    use crate::scan::scan_folder;
    #[cfg(unix)]
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

    #[test]
    fn groups_a_plain_file_with_an_identical_archive_nested_entry() {
        let dir = tempfile::tempdir().unwrap();
        let content: &[u8] = b"duplicated across a plain file and inside a zip";
        std::fs::write(dir.path().join("plain.bin"), content).unwrap();

        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        writer
            .start_file("inner.bin", zip::write::SimpleFileOptions::default())
            .unwrap();
        std::io::Write::write_all(&mut writer, content).unwrap();
        let zip_bytes = writer.finish().unwrap().into_inner();
        std::fs::write(dir.path().join("archive.zip"), &zip_bytes).unwrap();

        let mut conn = open_in_memory().unwrap();
        let run_id = scan_folder(&mut conn, dir.path(), now())
            .unwrap()
            .scan_run_id;

        let summary = run_duplicate_check(&mut conn, &[run_id], now()).unwrap();

        assert_eq!(summary.group_count, 1);
        assert_eq!(summary.duplicate_file_count, 2);
        assert_eq!(summary.error_count, 0);

        let members: Vec<String> = conn
            .prepare(
                "SELECT sf.path FROM duplicate_group_member m
                 JOIN scanned_file sf ON sf.id = m.scanned_file_id
                 ORDER BY sf.path",
            )
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(members, vec!["archive.zip/inner.bin", "plain.bin"]);
    }
}
