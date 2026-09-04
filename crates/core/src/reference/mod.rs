//! Reference-set generation from an existing `scan_run` (`docs/requirements.md`
//! §3.4/§8/§10.1/§10.12, P5). "自前形式（JSON）" names the native, DB-backed format —
//! generated sets always use SHA-256 (§10.1's standard algorithm) — as opposed to
//! adapters that import external CSV/XML definition files with other algorithms; that
//! import path is out of scope until P9. Per the CLI's `reference generate
//! --from-scan <SCAN_RUN_ID>` (§10.16), generation reads an already-completed
//! `scan_run`'s metadata (P3) rather than scanning a folder itself.

use std::path::Path;

use rayon::prelude::*;

use crate::db::{repo, Connection, Result};
use crate::hash::hash_file_sha256;

/// Outcome of one `generate_reference_set_from_scan_run` call.
#[derive(Debug, Clone, Copy)]
pub struct GenerateReferenceSetSummary {
    pub reference_set_id: i64,
    pub file_count: usize,
    /// Files that were `status = 'ok'` at scan time but failed to hash here (e.g.
    /// removed or became unreadable since the scan). Excluded from the generated
    /// reference set and marked errored on `scanned_file`, per §10.11.
    pub error_count: usize,
}

/// Builds a new `reference_set` (native JSON format, SHA-256) from every ok, non-
/// archived file recorded by `scan_run_id`. `supersedes_reference_set_id` links this as
/// a new version of an existing named set (§10.12's linear version history) — passing
/// `None` creates a fresh, unrelated set.
pub fn generate_reference_set_from_scan_run(
    conn: &mut Connection,
    scan_run_id: i64,
    name: &str,
    supersedes_reference_set_id: Option<i64>,
    created_at: i64,
) -> Result<GenerateReferenceSetSummary> {
    let candidates = repo::list_ok_scanned_files_for_scan_runs(conn, &[scan_run_id])?;

    let hashed: Vec<_> = candidates
        .into_par_iter()
        .map(|f| {
            let full_path = Path::new(&f.folder_path).join(&f.path);
            let result = hash_file_sha256(&full_path);
            (f, result)
        })
        .collect();

    let reference_set_id = repo::insert_reference_set(
        conn,
        name,
        "json",
        None,
        Some(scan_run_id),
        supersedes_reference_set_id,
        created_at,
    )?;

    let mut file_count = 0usize;
    let mut error_count = 0usize;
    {
        let tx = conn.transaction()?;
        for (f, result) in hashed {
            match result {
                Ok(sha256) => {
                    repo::insert_reference_file(
                        &tx,
                        &repo::NewReferenceFile {
                            reference_set_id,
                            path: &f.path,
                            size: f.size,
                            crc32: None,
                            md5: None,
                            sha1: None,
                            sha256: Some(&sha256),
                        },
                    )?;
                    file_count += 1;
                }
                Err(err) => {
                    repo::mark_scanned_file_error(&tx, f.id, &err.to_string())?;
                    error_count += 1;
                }
            }
        }
        tx.commit()?;
    }

    Ok(GenerateReferenceSetSummary {
        reference_set_id,
        file_count,
        error_count,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::open_in_memory;
    use crate::scan::scan_folder;

    fn now() -> i64 {
        1_700_000_000_000
    }

    #[test]
    fn generates_reference_files_with_sha256_from_a_scan_run() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.jpg"), b"hello").unwrap();
        std::fs::write(dir.path().join("b.jpg"), b"world!!").unwrap();

        let mut conn = open_in_memory().unwrap();
        let scan_run_id = scan_folder(&mut conn, dir.path(), now())
            .unwrap()
            .scan_run_id;

        let summary =
            generate_reference_set_from_scan_run(&mut conn, scan_run_id, "master v1", None, now())
                .unwrap();

        assert_eq!(summary.file_count, 2);
        assert_eq!(summary.error_count, 0);

        let files = repo::list_reference_files(&conn, summary.reference_set_id).unwrap();
        let mut by_path: std::collections::HashMap<_, _> =
            files.into_iter().map(|f| (f.path.clone(), f)).collect();

        let expected_a = crate::hash::hash_file_sha256(&dir.path().join("a.jpg")).unwrap();
        let a = by_path.remove("a.jpg").unwrap();
        assert_eq!(a.size, 5);
        assert_eq!(a.sha256.unwrap(), expected_a);
    }

    #[test]
    fn links_a_new_version_via_supersedes() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.jpg"), b"v1").unwrap();

        let mut conn = open_in_memory().unwrap();
        let run1 = scan_folder(&mut conn, dir.path(), now())
            .unwrap()
            .scan_run_id;
        let v1 = generate_reference_set_from_scan_run(&mut conn, run1, "master", None, now())
            .unwrap()
            .reference_set_id;

        std::fs::write(dir.path().join("a.jpg"), b"v2 changed").unwrap();
        let run2 = scan_folder(&mut conn, dir.path(), now())
            .unwrap()
            .scan_run_id;
        let v2 = generate_reference_set_from_scan_run(&mut conn, run2, "master", Some(v1), now())
            .unwrap()
            .reference_set_id;

        let supersedes: Option<i64> = conn
            .query_row(
                "SELECT supersedes_reference_set_id FROM reference_set WHERE id = ?1",
                [v2],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(supersedes, Some(v1));
    }
}
