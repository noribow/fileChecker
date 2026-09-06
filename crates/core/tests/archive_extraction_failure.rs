//! Scripted §10.15 scenario: when an archive itself fails to open, the reference-set
//! entries expected inside it must come out as `error` (couldn't verify), never as
//! `missing` (confirmed absent) — the distinction §10.11 exists to protect. This test
//! drives the repo layer by hand to prove the schema *can* represent and distinguish
//! the two cases via a plain query; the actual matching algorithm that produces these
//! rows automatically during a real integrity check is P5's job.

use filechecker_core::db::{open_in_memory, repo, FileStatus, HashMode, ResultStatus};

fn now() -> i64 {
    1_700_000_000_000
}

#[test]
fn broken_archive_entries_are_error_not_missing() {
    let conn = open_in_memory().unwrap();

    // --- information-gathering phase (§10.3) ---------------------------------------
    let scan_run_id =
        repo::insert_scan_run_folder(&conn, "/data/photos", HashMode::Lazy, now()).unwrap();

    // A working archive with two entries inside it.
    let working_zip = repo::insert_scanned_file(
        &conn,
        &repo::NewScannedFile {
            scan_run_id,
            path: "working.zip",
            parent_archive_file_id: None,
            archive_format: Some("zip"),
            archive_depth: 0,
            size: 4096,
            mtime: None,
            crc32: None,
            md5: None,
            sha1: None,
            sha256: None,
            status: FileStatus::Ok,
            error_message: None,
            scanned_at: now(),
        },
    )
    .unwrap();

    let a_jpg_hash = [0xAAu8; 32];
    let a_jpg = repo::insert_scanned_file(
        &conn,
        &repo::NewScannedFile {
            scan_run_id,
            path: "working.zip/a.jpg",
            parent_archive_file_id: Some(working_zip),
            archive_format: None,
            archive_depth: 1,
            size: 100,
            mtime: None,
            crc32: None,
            md5: None,
            sha1: None,
            sha256: Some(&a_jpg_hash),
            status: FileStatus::Ok,
            error_message: None,
            scanned_at: now(),
        },
    )
    .unwrap();

    // A broken archive: extraction itself failed, so it has no child scanned_file rows
    // at all (§10.15 — this is the crux of the scenario).
    let broken_zip = repo::insert_scanned_file(
        &conn,
        &repo::NewScannedFile {
            scan_run_id,
            path: "broken.zip",
            parent_archive_file_id: None,
            archive_format: Some("zip"),
            archive_depth: 0,
            size: 2048,
            mtime: None,
            crc32: None,
            md5: None,
            sha1: None,
            sha256: None,
            status: FileStatus::Error,
            error_message: Some("展開失敗: 宣言サイズ超過"),
            scanned_at: now(),
        },
    )
    .unwrap();

    // --- reference set (§3.4) -------------------------------------------------------
    let reference_set_id =
        repo::insert_reference_set(&conn, "master", "json", None, None, None, now()).unwrap();

    let ref_a = repo::insert_reference_file(
        &conn,
        &repo::NewReferenceFile {
            reference_set_id,
            path: "working.zip/a.jpg",
            size: 100,
            crc32: None,
            md5: None,
            sha1: None,
            sha256: Some(&a_jpg_hash),
        },
    )
    .unwrap();

    let ref_c = repo::insert_reference_file(
        &conn,
        &repo::NewReferenceFile {
            reference_set_id,
            path: "broken.zip/c.jpg",
            size: 200,
            crc32: None,
            md5: None,
            sha1: None,
            sha256: None,
        },
    )
    .unwrap();

    let ref_never_scanned = repo::insert_reference_file(
        &conn,
        &repo::NewReferenceFile {
            reference_set_id,
            path: "photos/never_scanned.jpg",
            size: 50,
            crc32: None,
            md5: None,
            sha1: None,
            sha256: None,
        },
    )
    .unwrap();

    // --- comparison phase (§10.3): check_run + integrity_check_result --------------
    let check_run_id = repo::insert_check_run_integrity(&conn, reference_set_id, now()).unwrap();
    repo::insert_check_run_source(&conn, check_run_id, scan_run_id).unwrap();

    // ok: a.jpg matched cleanly against the working archive's entry.
    repo::insert_integrity_check_result(
        &conn,
        check_run_id,
        Some(ref_a),
        Some(a_jpg),
        ResultStatus::Ok,
        None,
    )
    .unwrap();

    // error (not missing!): c.jpg's path falls under broken.zip, whose extraction
    // failed. scanned_file_id points at the *archive's own* row (§10.15), and detail
    // carries the archive's error message.
    repo::insert_integrity_check_result(
        &conn,
        check_run_id,
        Some(ref_c),
        Some(broken_zip),
        ResultStatus::Error,
        Some("展開失敗: 宣言サイズ超過"),
    )
    .unwrap();

    // missing: genuinely absent from any scan_run in this check_run.
    repo::insert_integrity_check_result(
        &conn,
        check_run_id,
        Some(ref_never_scanned),
        None,
        ResultStatus::Missing,
        None,
    )
    .unwrap();

    repo::finish_check_run(
        &conn,
        check_run_id,
        filechecker_core::db::RunStatus::Completed,
        now(),
    )
    .unwrap();

    // --- assertions: the three cases are distinguishable via a plain query ---------
    let all = repo::list_integrity_results(&conn, check_run_id, None).unwrap();
    assert_eq!(all.len(), 3);

    let ok_rows =
        repo::list_integrity_results(&conn, check_run_id, Some(ResultStatus::Ok)).unwrap();
    assert_eq!(ok_rows.len(), 1);
    assert_eq!(
        ok_rows[0].scanned_file_path.as_deref(),
        Some("working.zip/a.jpg")
    );

    let error_rows =
        repo::list_integrity_results(&conn, check_run_id, Some(ResultStatus::Error)).unwrap();
    assert_eq!(error_rows.len(), 1);
    // Crucially: the error row resolves to the *archive's* path, not a null/absent
    // scanned_file — this is what distinguishes "couldn't verify" from "confirmed gone".
    assert_eq!(
        error_rows[0].scanned_file_path.as_deref(),
        Some("broken.zip")
    );
    assert_eq!(
        error_rows[0].detail.as_deref(),
        Some("展開失敗: 宣言サイズ超過")
    );

    let missing_rows =
        repo::list_integrity_results(&conn, check_run_id, Some(ResultStatus::Missing)).unwrap();
    assert_eq!(missing_rows.len(), 1);
    assert!(missing_rows[0].scanned_file_path.is_none());

    // Sanity check: no row was misclassified as 'missing' when it should be 'error'.
    assert!(all
        .iter()
        .all(|r| !(r.result_status == ResultStatus::Missing && r.scanned_file_path.is_some())));
}
