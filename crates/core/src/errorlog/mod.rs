//! Per-run text error log file (`docs/requirements.md` §10.17/§10.22): a plain-text
//! diagnostic companion to the concise one-line summaries already persisted in the DB
//! (`scanned_file.error_message` / `integrity_check_result.detail`). §10.17 asks for
//! "タイムスタンプ・レベル・対象ファイルパス・詳細メッセージ" recorded per file-level
//! error, in a file placed in the app's settings folder, kept forever (§10.22: no
//! rotation, no auto-deletion).
//!
//! Rather than threading a live log writer through every scan/hash call site (which
//! would ripple `Option<&ErrorLog>` through scan_folder, archive_walk, duplicate,
//! integrity, reference generation — each already juggling a `PasswordPolicy` param —
//! for no informational gain), this module reconstructs each run's log *after the
//! fact* from what the DB already recorded. That's a deliberate, documented scope
//! choice: this codebase's "detailed diagnostic info" already lives entirely inside the
//! one-line `error_message`/`detail` string (the raw OS error is embedded via `{err}`
//! Display at the point of failure, and the retry count is a fixed constant, not
//! per-instance data worth duplicating) — so a live writer would only ever produce the
//! same text, one call frame earlier. If a future error path ever records something
//! richer than that, this module's data source should move to wherever that gets
//! captured.
//!
//! A run with zero errors gets no log file at all — an empty diagnostic file for a
//! clean run has no reader and would just multiply file count for §4's
//! hundreds-of-thousands-of-files scale.
//!
//! Each write reflects the *complete* current error set for that run (queried fresh
//! from the DB each time), so a repeat call for the same run overwrites its file rather
//! than appending to it — appending would duplicate every previously written line.
//! §10.22's "no rotation, kept forever" governs not deleting/rotating these files
//! across different runs over time, not how a single run's own file is written.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::db::repo::{self, ErrorFileRow};
use crate::db::{Connection, ResultStatus};

/// Writes `{log_dir}/scan_{scan_run_id}.log` from every `scanned_file` row of
/// `scan_run_id` that ended up `status = 'error'`. Returns `Ok(None)` (no file written)
/// when there were no errors. `timestamp_ms` stamps every line — the DB doesn't record
/// a separate per-file error timestamp, only the run's own, so that's what's honestly
/// available here rather than inventing false per-line precision.
pub fn write_scan_run_log(
    conn: &Connection,
    log_dir: &Path,
    scan_run_id: i64,
    timestamp_ms: i64,
) -> crate::db::Result<Option<PathBuf>> {
    let rows = repo::list_error_scanned_files_for_scan_runs(conn, &[scan_run_id])?;
    Ok(write_log_file(
        log_dir,
        &format!("scan_{scan_run_id}.log"),
        timestamp_ms,
        rows.iter().map(|r| (r.path.as_str(), error_detail(r))),
    ))
}

/// Writes `{log_dir}/check_{check_run_id}.log` for one `check_run`'s file-level errors:
/// for an integrity check, its own `error`-status `integrity_check_result` rows; for a
/// duplicate check, the `scanned_file` errors of whichever `scan_run`s it bundled
/// (`check_run_source`) — duplicate check doesn't persist its own error rows per
/// `check_run` (see `docs/progress-log.md`'s P6 entry), but every comparison-phase hash
/// failure it hits does get written back to `scanned_file` (`mark_scanned_file_error`),
/// so the same source-scan-run join used for reconstruction (P11) recovers them here.
pub fn write_check_run_log(
    conn: &Connection,
    log_dir: &Path,
    check_run_id: i64,
    is_integrity: bool,
    timestamp_ms: i64,
) -> crate::db::Result<Option<PathBuf>> {
    let file_name = format!("check_{check_run_id}.log");
    if is_integrity {
        let rows = repo::list_integrity_results(conn, check_run_id, Some(ResultStatus::Error))?;
        Ok(write_log_file(
            log_dir,
            &file_name,
            timestamp_ms,
            rows.iter().map(|r| {
                (
                    r.path.as_str(),
                    r.detail.as_deref().unwrap_or("").to_string(),
                )
            }),
        ))
    } else {
        let scan_run_ids = repo::list_check_run_source_scan_run_ids(conn, check_run_id)?;
        let rows = repo::list_error_scanned_files_for_scan_runs(conn, &scan_run_ids)?;
        Ok(write_log_file(
            log_dir,
            &file_name,
            timestamp_ms,
            rows.iter().map(|r| (r.path.as_str(), error_detail(r))),
        ))
    }
}

fn error_detail(row: &ErrorFileRow) -> String {
    row.error_message.clone().unwrap_or_default()
}

/// Appends (creating `log_dir` and the file if needed) one `{timestamp_ms}\tERROR\t
/// {path}\t{detail}` line per entry. Best-effort: per §10.17 this is the *secondary*
/// diagnostic record (the DB row is primary), so an I/O failure writing it is logged to
/// stderr rather than failing the run that's otherwise already completed successfully.
fn write_log_file<'a>(
    log_dir: &Path,
    file_name: &str,
    timestamp_ms: i64,
    entries: impl Iterator<Item = (&'a str, String)> + Clone,
) -> Option<PathBuf> {
    entries.clone().next()?;
    match write_log_file_inner(log_dir, file_name, timestamp_ms, entries) {
        Ok(path) => Some(path),
        Err(err) => {
            eprintln!("警告: エラーログファイルの書き込みに失敗しました: {err}");
            None
        }
    }
}

fn write_log_file_inner<'a>(
    log_dir: &Path,
    file_name: &str,
    timestamp_ms: i64,
    entries: impl Iterator<Item = (&'a str, String)>,
) -> io::Result<PathBuf> {
    fs::create_dir_all(log_dir)?;
    let path = log_dir.join(file_name);
    let mut text = String::new();
    for (target_path, detail) in entries {
        text.push_str(&format!("{timestamp_ms}\tERROR\t{target_path}\t{detail}\n"));
    }
    // Each call re-derives the *complete* current error set for this run straight from
    // the DB (not just what's new since the last call), so the file is (re)written from
    // scratch rather than appended to — appending would duplicate every previously
    // written line on a second call for the same run (e.g. `scan folder` followed by
    // `reference generate` against that same scan_run). §10.22's "no rotation, kept
    // forever" is about not deleting/rotating the file across runs, not about append-
    // only writes within one run's own log.
    fs::write(&path, text)?;
    Ok(path)
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
    fn no_log_file_is_written_when_a_scan_run_has_no_errors() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"ok").unwrap();
        let mut conn = open_in_memory().unwrap();
        let scan_run_id = scan_folder(&mut conn, dir.path(), now())
            .unwrap()
            .scan_run_id;

        let log_dir = tempfile::tempdir().unwrap();
        let result = write_scan_run_log(&conn, log_dir.path(), scan_run_id, now()).unwrap();
        assert!(result.is_none());
        assert!(fs::read_dir(log_dir.path()).unwrap().next().is_none());
    }

    #[test]
    fn writes_one_line_per_scan_time_error_with_path_and_detail() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("ok.txt"), b"fine").unwrap();
        let mut conn = open_in_memory().unwrap();
        let scan_run_id = scan_folder(&mut conn, dir.path(), now())
            .unwrap()
            .scan_run_id;
        conn.execute(
            "INSERT INTO scanned_file (scan_run_id, path, size, status, error_message, scanned_at)
             VALUES (?1, 'broken.txt', 0, 'error', 'アクセス不可', ?2)",
            rusqlite::params![scan_run_id, now()],
        )
        .unwrap();

        let log_dir = tempfile::tempdir().unwrap();
        let path = write_scan_run_log(&conn, log_dir.path(), scan_run_id, now())
            .unwrap()
            .expect("errors exist, a log file must be written");
        let contents = fs::read_to_string(&path).unwrap();
        assert_eq!(
            contents,
            format!("{}\tERROR\tbroken.txt\tアクセス不可\n", now())
        );
    }

    #[test]
    fn writing_a_second_time_reflects_the_current_error_set_rather_than_duplicating_lines() {
        // Each call re-derives the complete current error set straight from the DB, so
        // calling it twice for the same still-unchanged scan_run must not double the
        // line count (a naive append would duplicate every line already written).
        let dir = tempfile::tempdir().unwrap();
        let mut conn = open_in_memory().unwrap();
        let scan_run_id = scan_folder(&mut conn, dir.path(), now())
            .unwrap()
            .scan_run_id;
        conn.execute(
            "INSERT INTO scanned_file (scan_run_id, path, size, status, error_message, scanned_at)
             VALUES (?1, 'broken.txt', 0, 'error', 'アクセス不可', ?2)",
            rusqlite::params![scan_run_id, now()],
        )
        .unwrap();

        let log_dir = tempfile::tempdir().unwrap();
        write_scan_run_log(&conn, log_dir.path(), scan_run_id, now()).unwrap();
        let path = write_scan_run_log(&conn, log_dir.path(), scan_run_id, now() + 1000)
            .unwrap()
            .unwrap();
        let contents = fs::read_to_string(&path).unwrap();
        assert_eq!(contents.lines().count(), 1);
        // The second call's timestamp wins, since the file was rewritten wholesale.
        assert!(contents.starts_with(&format!("{}\t", now() + 1000)));
    }
}
