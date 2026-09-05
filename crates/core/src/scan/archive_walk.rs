//! Recursive archive expansion during the scan phase (§10.6/§10.15). Once a plain
//! file's `scanned_file` row exists, if it looks like an archive (§10.5) this opens it
//! and recursively records its entries as child `scanned_file` rows, up to the
//! configured depth limit. This only reads archive headers/central directories (via
//! `archive::list_entries`) plus, for entries that are themselves archives to recurse
//! into, their compressed bytes — never an ordinary leaf entry's content. Actual
//! leaf-entry content hashing stays deferred to the comparison phase (duplicate/
//! integrity check), matching the regular-file lazy path (§10.2/§10.3).

use std::fs::File;
use std::io::{self, Cursor};
use std::path::Path;

use rusqlite::Transaction;

use crate::archive::{self, ArchiveConfig, ArchiveFormat, PasswordPolicy};
use crate::db::{repo, FileStatus, Result};
use crate::retry::{is_retryable_fs_error, retry_io};

enum Source<'a> {
    File(&'a Path),
    Bytes(&'a [u8]),
}

impl Source<'_> {
    fn list_entries(
        &self,
        format: ArchiveFormat,
        policy: &PasswordPolicy,
    ) -> io::Result<Vec<archive::ArchiveEntryMeta>> {
        match self {
            Source::File(path) => {
                let file = retry_io(|| File::open(path), is_retryable_fs_error)?;
                archive::list_entries(format, file, policy)
            }
            Source::Bytes(bytes) => archive::list_entries(format, Cursor::new(*bytes), policy),
        }
    }

    fn read_entry(
        &self,
        format: ArchiveFormat,
        name: &str,
        declared_size: u64,
        policy: &PasswordPolicy,
    ) -> io::Result<Vec<u8>> {
        match self {
            Source::File(path) => {
                let file = retry_io(|| File::open(path), is_retryable_fs_error)?;
                archive::read_entry_bytes(format, file, name, declared_size, policy)
            }
            Source::Bytes(bytes) => {
                archive::read_entry_bytes(format, Cursor::new(*bytes), name, declared_size, policy)
            }
        }
    }
}

/// If `top_level_path` looks like an archive (§10.5's zip/7z, by extension), recursively
/// records its entries as child `scanned_file` rows of `scanned_file_id`. An archive
/// that fails to even open is marked `status = 'error'` on its own row (§10.15), with
/// no children created; individual oversized entries (§10.6) get a row but are never
/// expanded further even if they're themselves archives.
#[allow(clippy::too_many_arguments)]
pub fn expand_if_archive(
    tx: &Transaction,
    scan_run_id: i64,
    scanned_file_id: i64,
    compound_path: &str,
    top_level_path: &Path,
    config: &ArchiveConfig,
    scanned_at: i64,
    policy: &PasswordPolicy,
) -> Result<()> {
    let Some(format) = ArchiveFormat::detect(top_level_path) else {
        return Ok(());
    };
    if config.max_depth < 1 {
        return Ok(());
    }
    expand(
        tx,
        scan_run_id,
        scanned_file_id,
        compound_path,
        format,
        &Source::File(top_level_path),
        1,
        config,
        scanned_at,
        policy,
    )
}

#[allow(clippy::too_many_arguments)]
fn expand(
    tx: &Transaction,
    scan_run_id: i64,
    parent_id: i64,
    parent_compound_path: &str,
    format: ArchiveFormat,
    source: &Source,
    depth: i64,
    config: &ArchiveConfig,
    scanned_at: i64,
    policy: &PasswordPolicy,
) -> Result<()> {
    let entries = match source.list_entries(format, policy) {
        Ok(entries) => entries,
        Err(err) => {
            // §10.15: the archive itself couldn't be opened/parsed — record that on its
            // own row and stop; its expected entries are handled at compare time by
            // matching on path prefix, not by any scanned_file row existing here.
            repo::mark_scanned_file_error(tx, parent_id, &err.to_string())?;
            return Ok(());
        }
    };

    for entry in entries {
        let compound_path = format!("{parent_compound_path}/{}", entry.name);
        // §10.5: archive_format identifies the entry's format regardless of whether we
        // actually recurse into it — even a depth/size-limited or failed-to-open
        // archive is still an archive, just not one we expand further (§10.6/§10.15).
        let child_format = ArchiveFormat::detect(Path::new(&entry.name));
        let within_size_limit = entry.size <= config.entry_size_limit;
        let will_recurse = child_format.is_some() && within_size_limit && depth < config.max_depth;

        let child_id = repo::insert_scanned_file(
            tx,
            &repo::NewScannedFile {
                scan_run_id,
                path: &compound_path,
                parent_archive_file_id: Some(parent_id),
                archive_format: child_format.map(ArchiveFormat::as_str),
                archive_depth: depth,
                size: entry.size as i64,
                mtime: None,
                crc32: None,
                md5: None,
                sha1: None,
                sha256: None,
                status: FileStatus::Ok,
                error_message: None,
                scanned_at,
            },
        )?;

        if will_recurse {
            let child_format = child_format.expect("checked by will_recurse above");
            match source.read_entry(format, &entry.name, entry.size, policy) {
                Ok(bytes) => {
                    expand(
                        tx,
                        scan_run_id,
                        child_id,
                        &compound_path,
                        child_format,
                        &Source::Bytes(&bytes),
                        depth + 1,
                        config,
                        scanned_at,
                        policy,
                    )?;
                }
                Err(err) => {
                    repo::mark_scanned_file_error(tx, child_id, &err.to_string())?;
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::scan_folder;
    use crate::db::open_in_memory;
    use std::io::Write;

    fn now() -> i64 {
        1_700_000_000_000
    }

    fn build_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for (name, data) in entries {
            writer
                .start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(data).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    #[derive(Debug)]
    struct Row {
        path: String,
        parent_archive_file_id: Option<i64>,
        archive_format: Option<String>,
        archive_depth: i64,
        status: String,
    }

    fn all_rows(conn: &rusqlite::Connection, scan_run_id: i64) -> Vec<Row> {
        let mut stmt = conn
            .prepare(
                "SELECT id, path, parent_archive_file_id, archive_format, archive_depth, status
                 FROM scanned_file WHERE scan_run_id = ?1",
            )
            .unwrap();
        stmt.query_map([scan_run_id], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                Row {
                    path: row.get(1)?,
                    parent_archive_file_id: row.get(2)?,
                    archive_format: row.get(3)?,
                    archive_depth: row.get(4)?,
                    status: row.get(5)?,
                },
            ))
        })
        .unwrap()
        .map(|r| r.unwrap().1)
        .collect()
    }

    #[test]
    fn expands_a_normal_zip_into_child_rows() {
        let dir = tempfile::tempdir().unwrap();
        let zip_bytes = build_zip(&[("a.txt", b"hello"), ("b.txt", b"world!!")]);
        std::fs::write(dir.path().join("data.zip"), &zip_bytes).unwrap();

        let mut conn = open_in_memory().unwrap();
        let summary = scan_folder(&mut conn, dir.path(), now()).unwrap();
        let rows = all_rows(&conn, summary.scan_run_id);

        let archive_row = rows.iter().find(|r| r.path == "data.zip").unwrap();
        assert_eq!(archive_row.archive_format.as_deref(), Some("zip"));
        assert_eq!(archive_row.archive_depth, 0);
        assert_eq!(archive_row.status, "ok");
        let archive_id = {
            let mut stmt = conn
                .prepare("SELECT id FROM scanned_file WHERE path = 'data.zip'")
                .unwrap();
            stmt.query_row([], |r| r.get::<_, i64>(0)).unwrap()
        };

        let a = rows.iter().find(|r| r.path == "data.zip/a.txt").unwrap();
        assert_eq!(a.parent_archive_file_id, Some(archive_id));
        assert_eq!(a.archive_depth, 1);
        assert!(a.archive_format.is_none());

        let b = rows.iter().find(|r| r.path == "data.zip/b.txt").unwrap();
        assert_eq!(b.archive_depth, 1);
        assert_eq!(rows.len(), 3);
    }

    #[test]
    fn nesting_stops_at_the_configured_depth_limit() {
        let dir = tempfile::tempdir().unwrap();
        let level3 = build_zip(&[("leaf.txt", b"deepest content")]);
        let level2 = build_zip(&[("level3.zip", &level3)]);
        let level1 = build_zip(&[("level2.zip", &level2)]);
        let level0 = build_zip(&[("level1.zip", &level1)]);
        std::fs::write(dir.path().join("outer.zip"), &level0).unwrap();

        let mut conn = open_in_memory().unwrap();
        let summary = scan_folder(&mut conn, dir.path(), now()).unwrap();
        let rows = all_rows(&conn, summary.scan_run_id);

        // Default archive_max_depth is 3, so level1/level2/level3 (depths 1-3) all get
        // their own scanned_file row, but level3.zip's *contents* (leaf.txt, which
        // would be depth 4) are never discovered.
        let path_depths: std::collections::HashMap<&str, i64> = rows
            .iter()
            .map(|r| (r.path.as_str(), r.archive_depth))
            .collect();
        assert_eq!(path_depths["outer.zip"], 0);
        assert_eq!(path_depths["outer.zip/level1.zip"], 1);
        assert_eq!(path_depths["outer.zip/level1.zip/level2.zip"], 2);
        assert_eq!(path_depths["outer.zip/level1.zip/level2.zip/level3.zip"], 3);
        assert!(!path_depths.contains_key("outer.zip/level1.zip/level2.zip/level3.zip/leaf.txt"));

        // level3.zip is still identified as a zip even though it wasn't expanded.
        let level3_row = rows
            .iter()
            .find(|r| r.path == "outer.zip/level1.zip/level2.zip/level3.zip")
            .unwrap();
        assert_eq!(level3_row.archive_format.as_deref(), Some("zip"));
    }

    #[test]
    fn a_corrupted_archive_is_recorded_as_error_with_no_children() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("broken.zip"), b"not a real zip file").unwrap();

        let mut conn = open_in_memory().unwrap();
        let summary = scan_folder(&mut conn, dir.path(), now()).unwrap();
        let rows = all_rows(&conn, summary.scan_run_id);

        assert_eq!(rows.len(), 1);
        let broken = &rows[0];
        assert_eq!(broken.path, "broken.zip");
        assert_eq!(broken.status, "error");
        assert_eq!(broken.archive_format.as_deref(), Some("zip"));
    }

    #[test]
    fn an_entry_over_the_configured_size_limit_is_recorded_but_not_expanded() {
        let dir = tempfile::tempdir().unwrap();
        let inner = build_zip(&[("secret.txt", b"should never be discovered")]);
        let outer = build_zip(&[("inner.zip", &inner), ("small.txt", b"ok")]);
        std::fs::write(dir.path().join("outer.zip"), &outer).unwrap();

        let mut conn = open_in_memory().unwrap();
        // Cap smaller than inner.zip's declared size but larger than small.txt's.
        crate::db::repo::set_app_setting(&conn, "archive_entry_size_limit_bytes", "5").unwrap();

        let summary = scan_folder(&mut conn, dir.path(), now()).unwrap();
        let rows = all_rows(&conn, summary.scan_run_id);

        let inner_row = rows
            .iter()
            .find(|r| r.path == "outer.zip/inner.zip")
            .unwrap();
        assert_eq!(inner_row.archive_format.as_deref(), Some("zip"));
        assert!(!rows
            .iter()
            .any(|r| r.path == "outer.zip/inner.zip/secret.txt"));

        let small = rows
            .iter()
            .find(|r| r.path == "outer.zip/small.txt")
            .unwrap();
        assert_eq!(small.status, "ok");
    }
}
