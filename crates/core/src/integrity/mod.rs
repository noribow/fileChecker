//! Integrity check, regular folders only (`docs/requirements.md` §10.11, P5). Compares
//! a reference set (§3.4) against `scanned_file` rows from one or more prior `scan_run`
//! folders and records the 5-way `result_status` distinction: ok / corrupted / missing /
//! extra / error. Archive contents (§3.3) and removable media (P8) are out of scope
//! until P7/P8. Matching only ever compares SHA-256: P5's own generator (§10.1) always
//! fills that column in, and matching against the other three algorithms is an
//! external-format-import concern (P9) this module doesn't yet need to handle.

use std::collections::HashMap;
use std::io;
use std::path::Path;

use rayon::prelude::*;

use crate::db::repo::{ReferenceFileRow, ScannedFileForIntegrity};
use crate::db::{repo, Connection, FileStatus, Result, ResultStatus, RunStatus};
use crate::hash::hash_file_sha256;

/// Detail text recorded on a `corrupted` result (§10.14's result-list wireframe uses the
/// same wording).
const SHA256_MISMATCH_DETAIL: &str = "SHA256不一致";

/// Outcome of one `run_integrity_check` call — the same 5-way split as
/// `integrity_check_result.result_status` (§10.11).
#[derive(Debug, Clone, Copy)]
pub struct IntegrityCheckSummary {
    pub check_run_id: i64,
    pub ok_count: usize,
    pub corrupted_count: usize,
    pub missing_count: usize,
    pub extra_count: usize,
    pub error_count: usize,
}

/// Compares `reference_set_id` against the `scanned_file` rows of `scan_run_ids`,
/// recording one `integrity_check_result` row per reference entry and per scanned file
/// (§10.11's 5 statuses). Files already hashed by an earlier duplicate check or
/// integrity check (`scanned_file.sha256` already set) are compared without touching
/// the disk again.
pub fn run_integrity_check(
    conn: &mut Connection,
    reference_set_id: i64,
    scan_run_ids: &[i64],
    started_at: i64,
) -> Result<IntegrityCheckSummary> {
    let check_run_id = repo::insert_check_run_integrity(conn, reference_set_id, started_at)?;
    for &scan_run_id in scan_run_ids {
        repo::insert_check_run_source(conn, check_run_id, scan_run_id)?;
    }

    let mut reference_by_path: HashMap<String, ReferenceFileRow> =
        repo::list_reference_files(conn, reference_set_id)?
            .into_iter()
            .map(|r| (r.path.clone(), r))
            .collect();

    let mut extra: Vec<ScannedFileForIntegrity> = Vec::new();
    let mut matched_error: Vec<(ScannedFileForIntegrity, ReferenceFileRow)> = Vec::new();
    let mut matched_known: Vec<(ScannedFileForIntegrity, ReferenceFileRow)> = Vec::new();
    let mut matched_need_hash: Vec<(ScannedFileForIntegrity, ReferenceFileRow)> = Vec::new();

    for sf in repo::list_scanned_files_for_integrity(conn, scan_run_ids)? {
        match reference_by_path.remove(&sf.path) {
            None => extra.push(sf),
            Some(rf) => match sf.status {
                FileStatus::Ok if sf.sha256.is_some() => matched_known.push((sf, rf)),
                FileStatus::Ok => matched_need_hash.push((sf, rf)),
                FileStatus::Error | FileStatus::Skipped => matched_error.push((sf, rf)),
            },
        }
    }

    // Whatever's left unmatched in the reference set never showed up in the scan at all.
    let missing: Vec<ReferenceFileRow> = reference_by_path.into_values().collect();

    let hashed: Vec<(
        ScannedFileForIntegrity,
        ReferenceFileRow,
        io::Result<[u8; 32]>,
    )> = matched_need_hash
        .into_par_iter()
        .map(|(sf, rf)| {
            let full_path = Path::new(&sf.folder_path).join(&sf.path);
            let result = hash_file_sha256(&full_path);
            (sf, rf, result)
        })
        .collect();

    let mut summary = IntegrityCheckSummary {
        check_run_id,
        ok_count: 0,
        corrupted_count: 0,
        missing_count: missing.len(),
        extra_count: extra.len(),
        error_count: 0,
    };

    {
        let tx = conn.transaction()?;

        for rf in &missing {
            repo::insert_integrity_check_result(
                &tx,
                check_run_id,
                Some(rf.id),
                None,
                ResultStatus::Missing,
                None,
            )?;
        }

        for sf in &extra {
            repo::insert_integrity_check_result(
                &tx,
                check_run_id,
                None,
                Some(sf.id),
                ResultStatus::Extra,
                None,
            )?;
        }

        for (sf, rf) in &matched_error {
            repo::insert_integrity_check_result(
                &tx,
                check_run_id,
                Some(rf.id),
                Some(sf.id),
                ResultStatus::Error,
                sf.error_message.as_deref(),
            )?;
            summary.error_count += 1;
        }

        for (sf, rf) in &matched_known {
            let status = if sf.sha256.as_deref() == rf.sha256.as_deref() {
                ResultStatus::Ok
            } else {
                ResultStatus::Corrupted
            };
            record_hash_comparison(&tx, check_run_id, rf.id, sf.id, status, &mut summary)?;
        }

        for (sf, rf, result) in hashed {
            match result {
                Ok(sha256) => {
                    repo::update_scanned_file_sha256(&tx, sf.id, &sha256)?;
                    let status = if rf.sha256.as_deref() == Some(sha256.as_slice()) {
                        ResultStatus::Ok
                    } else {
                        ResultStatus::Corrupted
                    };
                    record_hash_comparison(&tx, check_run_id, rf.id, sf.id, status, &mut summary)?;
                }
                Err(err) => {
                    repo::mark_scanned_file_error(&tx, sf.id, &err.to_string())?;
                    repo::insert_integrity_check_result(
                        &tx,
                        check_run_id,
                        Some(rf.id),
                        Some(sf.id),
                        ResultStatus::Error,
                        Some(&err.to_string()),
                    )?;
                    summary.error_count += 1;
                }
            }
        }

        tx.commit()?;
    }

    repo::finish_check_run(conn, check_run_id, RunStatus::Completed, started_at)?;

    Ok(summary)
}

fn record_hash_comparison(
    conn: &Connection,
    check_run_id: i64,
    reference_file_id: i64,
    scanned_file_id: i64,
    status: ResultStatus,
    summary: &mut IntegrityCheckSummary,
) -> Result<()> {
    let detail = matches!(status, ResultStatus::Corrupted).then_some(SHA256_MISMATCH_DETAIL);
    repo::insert_integrity_check_result(
        conn,
        check_run_id,
        Some(reference_file_id),
        Some(scanned_file_id),
        status,
        detail,
    )?;
    match status {
        ResultStatus::Ok => summary.ok_count += 1,
        ResultStatus::Corrupted => summary.corrupted_count += 1,
        _ => unreachable!("record_hash_comparison only ever records ok/corrupted"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::open_in_memory;
    use crate::reference::generate_reference_set_from_scan_run;
    use crate::scan::scan_folder;

    fn now() -> i64 {
        1_700_000_000_000
    }

    /// Builds a reference set from `dir`'s current contents, so the caller can then
    /// mutate `dir` and re-scan to exercise the 5 statuses against that baseline.
    fn generate_baseline(conn: &mut Connection, dir: &Path) -> i64 {
        let scan_run_id = scan_folder(conn, dir, now()).unwrap().scan_run_id;
        generate_reference_set_from_scan_run(conn, scan_run_id, "master", None, now())
            .unwrap()
            .reference_set_id
    }

    #[test]
    fn distinguishes_ok_corrupted_missing_and_extra() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("unchanged.jpg"), b"stays the same").unwrap();
        std::fs::write(dir.path().join("will_corrupt.jpg"), b"original bytes").unwrap();
        std::fs::write(dir.path().join("will_vanish.jpg"), b"gone later").unwrap();

        let mut conn = open_in_memory().unwrap();
        let reference_set_id = generate_baseline(&mut conn, dir.path());

        // T2: corrupt one file, delete another, add a new one not in the reference set.
        std::fs::write(dir.path().join("will_corrupt.jpg"), b"tampered bytes!!").unwrap();
        std::fs::remove_file(dir.path().join("will_vanish.jpg")).unwrap();
        std::fs::write(dir.path().join("new_extra.jpg"), b"wasn't here before").unwrap();
        let scan_run_id = scan_folder(&mut conn, dir.path(), now())
            .unwrap()
            .scan_run_id;

        let summary =
            run_integrity_check(&mut conn, reference_set_id, &[scan_run_id], now()).unwrap();

        assert_eq!(summary.ok_count, 1);
        assert_eq!(summary.corrupted_count, 1);
        assert_eq!(summary.missing_count, 1);
        assert_eq!(summary.extra_count, 1);
        assert_eq!(summary.error_count, 0);

        let results = repo::list_integrity_results(&conn, summary.check_run_id, None).unwrap();
        let by_path: HashMap<Option<String>, String> = results
            .into_iter()
            .map(|r| (r.scanned_file_path, r.result_status.as_str().to_string()))
            .collect();
        assert_eq!(by_path.get(&Some("unchanged.jpg".into())).unwrap(), "ok");
        assert_eq!(
            by_path.get(&Some("will_corrupt.jpg".into())).unwrap(),
            "corrupted"
        );
        assert_eq!(by_path.get(&Some("new_extra.jpg".into())).unwrap(), "extra");
        // The missing row has no scanned_file (None key); just confirm one exists.
        assert!(by_path.values().any(|s| s == "missing"));
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_matched_file_is_recorded_as_error_not_corrupted_or_missing() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let locked_path = dir.path().join("locked.jpg");
        std::fs::write(&locked_path, b"secret bytes").unwrap();

        let mut conn = open_in_memory().unwrap();
        let reference_set_id = generate_baseline(&mut conn, dir.path());

        std::fs::set_permissions(&locked_path, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::File::open(&locked_path).is_ok() {
            eprintln!(
                "skipping: running with privileges that bypass permission bits (e.g. root); \
                 cannot exercise a real read failure in this environment"
            );
            std::fs::set_permissions(&locked_path, std::fs::Permissions::from_mode(0o644)).unwrap();
            return;
        }

        let scan_run_id = scan_folder(&mut conn, dir.path(), now())
            .unwrap()
            .scan_run_id;
        let summary =
            run_integrity_check(&mut conn, reference_set_id, &[scan_run_id], now()).unwrap();

        assert_eq!(summary.error_count, 1);
        assert_eq!(summary.ok_count, 0);
        assert_eq!(summary.corrupted_count, 0);
        assert_eq!(summary.missing_count, 0);

        std::fs::set_permissions(&locked_path, std::fs::Permissions::from_mode(0o644)).unwrap();
    }

    #[test]
    fn reuses_already_persisted_sha256_without_rehashing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.jpg"), b"hello").unwrap();

        let mut conn = open_in_memory().unwrap();
        let reference_set_id = generate_baseline(&mut conn, dir.path());

        let scan_run_id = scan_folder(&mut conn, dir.path(), now())
            .unwrap()
            .scan_run_id;
        // Simulate a prior duplicate check having already computed and persisted the
        // SHA-256 for this file, with a value guaranteed to differ if it were
        // recomputed from disk (which still holds the original "hello" bytes).
        let fake_sha256 = vec![0xAAu8; 32];
        conn.execute(
            "UPDATE scanned_file SET sha256 = ?1 WHERE path = 'a.jpg'",
            [&fake_sha256],
        )
        .unwrap();

        let summary =
            run_integrity_check(&mut conn, reference_set_id, &[scan_run_id], now()).unwrap();

        // The reused (fake) hash doesn't match the reference's real one, proving the
        // comparison used the persisted value rather than reading the file itself.
        assert_eq!(summary.corrupted_count, 1);
        assert_eq!(summary.ok_count, 0);
    }
}
