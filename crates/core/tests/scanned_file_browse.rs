//! GUI「スキャン状況」4ペイン画面（requirements.md §10.14拡張）が使う、
//! `scanned_file`のページング付き閲覧クエリ（`list_top_level_scanned_files_page`/
//! `list_archive_children_page`）の検証。大規模スキャン（§4）でも一覧全体をフロント
//! エンドへ一括転送しないよう、`offset`/`limit`で分割取得できること、および各行が
//! 自分の属する一覧の総件数（`total_count`）を運んでくること（フロントエンドの
//! 仮想スクロールが最初の1ページで全体件数を知るための仕組み）を確認する。

use filechecker_core::db::{open_in_memory, repo, FileStatus, HashMode};

fn now() -> i64 {
    1_700_000_000_000
}

fn insert_top_level(conn: &rusqlite::Connection, scan_run_id: i64, path: &str, size: i64) -> i64 {
    repo::insert_scanned_file(
        conn,
        &repo::NewScannedFile {
            scan_run_id,
            path,
            parent_archive_file_id: None,
            archive_format: None,
            archive_depth: 0,
            size,
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
    .unwrap()
}

fn insert_archive(conn: &rusqlite::Connection, scan_run_id: i64, path: &str) -> i64 {
    repo::insert_scanned_file(
        conn,
        &repo::NewScannedFile {
            scan_run_id,
            path,
            parent_archive_file_id: None,
            archive_format: Some("zip"),
            archive_depth: 0,
            size: 1024,
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
    .unwrap()
}

fn insert_child(
    conn: &rusqlite::Connection,
    scan_run_id: i64,
    parent_archive_file_id: i64,
    path: &str,
) -> i64 {
    repo::insert_scanned_file(
        conn,
        &repo::NewScannedFile {
            scan_run_id,
            path,
            parent_archive_file_id: Some(parent_archive_file_id),
            archive_format: None,
            archive_depth: 1,
            size: 10,
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
    .unwrap()
}

#[test]
fn top_level_listing_excludes_archive_children_and_paginates() {
    let conn = open_in_memory().unwrap();
    let scan_run_id = repo::insert_scan_run_folder(&conn, "/data", HashMode::Lazy, now()).unwrap();

    insert_top_level(&conn, scan_run_id, "a.txt", 5);
    insert_top_level(&conn, scan_run_id, "b.txt", 7);
    let archive_id = insert_archive(&conn, scan_run_id, "c.zip");
    // Archive contents must never appear in the top-level listing.
    insert_child(&conn, scan_run_id, archive_id, "c.zip/inner.txt");

    let page1 = repo::list_top_level_scanned_files_page(&conn, scan_run_id, 0, 2).unwrap();
    assert_eq!(page1.len(), 2);
    assert_eq!(page1[0].path, "a.txt");
    assert_eq!(page1[1].path, "b.txt");
    // Every row on every page carries the same total, letting the frontend learn the
    // full extent of the list from page 1 alone.
    assert!(page1.iter().all(|r| r.total_count == 3));

    let page2 = repo::list_top_level_scanned_files_page(&conn, scan_run_id, 2, 2).unwrap();
    assert_eq!(page2.len(), 1);
    assert_eq!(page2[0].path, "c.zip");
    assert_eq!(page2[0].archive_format.as_deref(), Some("zip"));
    assert_eq!(page2[0].total_count, 3);

    let beyond_end = repo::list_top_level_scanned_files_page(&conn, scan_run_id, 10, 2).unwrap();
    assert!(beyond_end.is_empty());
}

#[test]
fn archive_children_listing_is_scoped_to_its_parent_and_paginates() {
    let conn = open_in_memory().unwrap();
    let scan_run_id = repo::insert_scan_run_folder(&conn, "/data", HashMode::Lazy, now()).unwrap();

    let archive_a = insert_archive(&conn, scan_run_id, "a.zip");
    insert_child(&conn, scan_run_id, archive_a, "a.zip/1.txt");
    insert_child(&conn, scan_run_id, archive_a, "a.zip/2.txt");

    let archive_b = insert_archive(&conn, scan_run_id, "b.zip");
    insert_child(&conn, scan_run_id, archive_b, "b.zip/only.txt");

    let a_children = repo::list_archive_children_page(&conn, archive_a, 0, 100).unwrap();
    assert_eq!(a_children.len(), 2);
    assert!(a_children.iter().all(|r| r.total_count == 2));
    assert_eq!(a_children[0].path, "a.zip/1.txt");

    // b.zip's single child must not be affected by a.zip's listing/paging.
    let b_children = repo::list_archive_children_page(&conn, archive_b, 0, 100).unwrap();
    assert_eq!(b_children.len(), 1);
    assert_eq!(b_children[0].path, "b.zip/only.txt");
    assert_eq!(b_children[0].total_count, 1);

    let a_children_page2 = repo::list_archive_children_page(&conn, archive_a, 1, 1).unwrap();
    assert_eq!(a_children_page2.len(), 1);
    assert_eq!(a_children_page2[0].path, "a.zip/2.txt");
}

#[test]
fn a_plain_file_has_no_children() {
    let conn = open_in_memory().unwrap();
    let scan_run_id = repo::insert_scan_run_folder(&conn, "/data", HashMode::Lazy, now()).unwrap();
    let plain_id = insert_top_level(&conn, scan_run_id, "a.txt", 5);

    let children = repo::list_archive_children_page(&conn, plain_id, 0, 100).unwrap();
    assert!(children.is_empty());
}
