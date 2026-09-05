//! Reconstruction feature integration tests (`docs/requirements.md` §10.20): the
//! source-priority rule, partial reconstruction, unconditional overwrite (§10.24/7.4),
//! archive-container reassembly, and removable-media gating.

use std::collections::HashMap;
use std::path::Path;

use filechecker_core::archive::PasswordPolicy;
use filechecker_core::db::{open_in_memory, repo, Connection};
use filechecker_core::reconstruct;

fn now() -> i64 {
    1_700_000_000_000
}

fn write(path: &Path, contents: &[u8]) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, contents).unwrap();
}

/// Scans `dir` as a plain folder and returns its `scan_run_id`.
fn scan_folder(conn: &mut Connection, dir: &Path) -> i64 {
    filechecker_core::scan::scan_folder(conn, dir, now())
        .unwrap()
        .scan_run_id
}

/// Builds a reference set from `scan_run_id`'s contents.
fn generate_reference_set(conn: &mut Connection, scan_run_id: i64, name: &str) -> i64 {
    filechecker_core::reference::generate_reference_set_from_scan_run(
        conn,
        scan_run_id,
        name,
        None,
        now(),
    )
    .unwrap()
    .reference_set_id
}

#[test]
fn resolves_from_the_only_available_source_and_writes_it_to_the_destination() {
    let mut conn = open_in_memory().unwrap();
    let library = tempfile::tempdir().unwrap();
    write(&library.path().join("a.rom"), b"rom a content");
    let library_scan = scan_folder(&mut conn, library.path());
    let reference_set_id = generate_reference_set(&mut conn, library_scan, "master");

    let destination = tempfile::tempdir().unwrap();
    let destination_scan = scan_folder(&mut conn, destination.path());

    let plan = reconstruct::compute_plan(
        &mut conn,
        reference_set_id,
        &[library_scan, destination_scan],
        destination_scan,
        &PasswordPolicy::Reject,
        now(),
    )
    .unwrap();
    assert_eq!(plan.resolved.len(), 1);
    assert!(plan.missing.is_empty());

    let run_id = reconstruct::create_run(
        &mut conn,
        plan.check_run_id,
        &destination.path().to_string_lossy(),
        &plan.resolved,
        now(),
    )
    .unwrap();

    let summary = reconstruct::run_pass(
        &mut conn,
        run_id,
        destination.path(),
        &PasswordPolicy::Reject,
        &HashMap::new(),
        now(),
    )
    .unwrap();
    assert_eq!(summary.written_count, 1);
    assert_eq!(summary.error_count, 0);
    assert!(summary.still_needed_removable_media.is_empty());

    assert_eq!(
        std::fs::read(destination.path().join("a.rom")).unwrap(),
        b"rom a content"
    );

    let run = repo::get_reconstruction_run(&conn, run_id)
        .unwrap()
        .unwrap();
    assert_eq!(run.status, filechecker_core::db::RunStatus::Completed);
}

#[test]
fn a_reference_file_with_no_available_source_is_missing_but_does_not_block_the_rest() {
    let mut conn = open_in_memory().unwrap();
    let library = tempfile::tempdir().unwrap();
    write(&library.path().join("a.rom"), b"rom a content");
    write(&library.path().join("b.rom"), b"rom b content");
    let library_scan = scan_folder(&mut conn, library.path());
    let reference_set_id = generate_reference_set(&mut conn, library_scan, "master");

    // Only a.rom actually exists anywhere reconstruction can look (b.rom is nowhere
    // to be found — pretend the library folder lost it since the reference was made).
    std::fs::remove_file(library.path().join("b.rom")).unwrap();
    let library_scan_again = scan_folder(&mut conn, library.path());

    let destination = tempfile::tempdir().unwrap();
    let destination_scan = scan_folder(&mut conn, destination.path());

    let plan = reconstruct::compute_plan(
        &mut conn,
        reference_set_id,
        &[library_scan_again, destination_scan],
        destination_scan,
        &PasswordPolicy::Reject,
        now(),
    )
    .unwrap();
    assert_eq!(plan.resolved.len(), 1);
    assert_eq!(plan.missing.len(), 1);
    assert_eq!(plan.missing[0].path, "b.rom");

    let run_id = reconstruct::create_run(
        &mut conn,
        plan.check_run_id,
        &destination.path().to_string_lossy(),
        &plan.resolved,
        now(),
    )
    .unwrap();
    let summary = reconstruct::run_pass(
        &mut conn,
        run_id,
        destination.path(),
        &PasswordPolicy::Reject,
        &HashMap::new(),
        now(),
    )
    .unwrap();
    assert_eq!(summary.written_count, 1);
    assert!(destination.path().join("a.rom").exists());
    assert!(!destination.path().join("b.rom").exists());
}

#[test]
fn destination_local_copy_outranks_a_library_copy() {
    let mut conn = open_in_memory().unwrap();
    let library = tempfile::tempdir().unwrap();
    write(&library.path().join("a.rom"), b"shared content");
    let library_scan = scan_folder(&mut conn, library.path());
    let reference_set_id = generate_reference_set(&mut conn, library_scan, "master");

    // Destination already has its own (byte-identical) copy at the same path.
    let destination = tempfile::tempdir().unwrap();
    write(&destination.path().join("a.rom"), b"shared content");
    let destination_scan = scan_folder(&mut conn, destination.path());

    let plan = reconstruct::compute_plan(
        &mut conn,
        reference_set_id,
        &[library_scan, destination_scan],
        destination_scan,
        &PasswordPolicy::Reject,
        now(),
    )
    .unwrap();
    assert_eq!(plan.resolved.len(), 1);
    assert_eq!(plan.resolved[0].scan_run_id, destination_scan);
}

#[test]
fn a_same_path_different_content_file_is_overwritten_unconditionally() {
    let mut conn = open_in_memory().unwrap();
    let library = tempfile::tempdir().unwrap();
    write(&library.path().join("a.rom"), b"correct content");
    let library_scan = scan_folder(&mut conn, library.path());
    let reference_set_id = generate_reference_set(&mut conn, library_scan, "master");

    // Destination has a *different* file sitting at the exact target path (§10.24/7.4).
    let destination = tempfile::tempdir().unwrap();
    write(&destination.path().join("a.rom"), b"stale, wrong content");
    let destination_scan = scan_folder(&mut conn, destination.path());

    let plan = reconstruct::compute_plan(
        &mut conn,
        reference_set_id,
        &[library_scan, destination_scan],
        destination_scan,
        &PasswordPolicy::Reject,
        now(),
    )
    .unwrap();
    // The destination's own file doesn't match the reference's hash, so the library
    // copy is the (only) resolved source here.
    assert_eq!(plan.resolved.len(), 1);
    assert_eq!(plan.resolved[0].scan_run_id, library_scan);

    let run_id = reconstruct::create_run(
        &mut conn,
        plan.check_run_id,
        &destination.path().to_string_lossy(),
        &plan.resolved,
        now(),
    )
    .unwrap();
    reconstruct::run_pass(
        &mut conn,
        run_id,
        destination.path(),
        &PasswordPolicy::Reject,
        &HashMap::new(),
        now(),
    )
    .unwrap();

    assert_eq!(
        std::fs::read(destination.path().join("a.rom")).unwrap(),
        b"correct content"
    );
}

#[test]
fn archive_nested_reference_entries_are_reassembled_into_a_fresh_container() {
    let mut conn = open_in_memory().unwrap();

    // Build the reference set from an *original* zip (so its reference paths are
    // archive-nested: "game.zip/a.bin", "game.zip/b.bin").
    let original_dir = tempfile::tempdir().unwrap();
    let original_zip = {
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        writer
            .start_file("a.bin", zip::write::SimpleFileOptions::default())
            .unwrap();
        std::io::Write::write_all(&mut writer, b"entry a content").unwrap();
        writer
            .start_file("b.bin", zip::write::SimpleFileOptions::default())
            .unwrap();
        std::io::Write::write_all(&mut writer, b"entry b content").unwrap();
        writer.finish().unwrap().into_inner()
    };
    write(&original_dir.path().join("game.zip"), &original_zip);
    let original_scan = scan_folder(&mut conn, original_dir.path());
    let reference_set_id = generate_reference_set(&mut conn, original_scan, "master");

    // The two entries now live as loose files scattered across a library folder — no
    // "game.zip" exists there at all, only the individual ROMs by content.
    let library = tempfile::tempdir().unwrap();
    write(&library.path().join("loose_a.bin"), b"entry a content");
    write(
        &library.path().join("subdir/loose_b.bin"),
        b"entry b content",
    );
    let library_scan = scan_folder(&mut conn, library.path());

    let destination = tempfile::tempdir().unwrap();
    let destination_scan = scan_folder(&mut conn, destination.path());

    let plan = reconstruct::compute_plan(
        &mut conn,
        reference_set_id,
        &[library_scan, destination_scan],
        destination_scan,
        &PasswordPolicy::Reject,
        now(),
    )
    .unwrap();
    assert_eq!(plan.resolved.len(), 2);
    assert!(plan.missing.is_empty());

    let run_id = reconstruct::create_run(
        &mut conn,
        plan.check_run_id,
        &destination.path().to_string_lossy(),
        &plan.resolved,
        now(),
    )
    .unwrap();
    let summary = reconstruct::run_pass(
        &mut conn,
        run_id,
        destination.path(),
        &PasswordPolicy::Reject,
        &HashMap::new(),
        now(),
    )
    .unwrap();
    assert_eq!(summary.written_count, 2);

    // "game.zip" itself was assembled fresh at the destination (never copied as a
    // loose file — no reference entry named exactly "game.zip" existed to copy).
    let rebuilt = destination.path().join("game.zip");
    assert!(rebuilt.exists());
    let mut archive = zip::ZipArchive::new(std::fs::File::open(&rebuilt).unwrap()).unwrap();
    assert_eq!(archive.len(), 2);
    let mut got_a = Vec::new();
    std::io::Read::read_to_end(&mut archive.by_name("a.bin").unwrap(), &mut got_a).unwrap();
    assert_eq!(got_a, b"entry a content");
    let mut got_b = Vec::new();
    std::io::Read::read_to_end(&mut archive.by_name("b.bin").unwrap(), &mut got_b).unwrap();
    assert_eq!(got_b, b"entry b content");

    // It's TorrentZip-shaped: the EOCD comment self-verifies the central directory.
    let comment = String::from_utf8(archive.comment().to_vec()).unwrap();
    assert!(comment.starts_with("TORRENTZIPPED-"));
}

#[test]
fn a_removable_media_source_is_left_pending_until_that_medium_is_connected() {
    let mut conn = open_in_memory().unwrap();

    let original_dir = tempfile::tempdir().unwrap();
    write(&original_dir.path().join("a.rom"), b"rom a content");
    let original_scan = scan_folder(&mut conn, original_dir.path());
    let reference_set_id = generate_reference_set(&mut conn, original_scan, "master");

    // The only available copy lives on a removable medium.
    let media_mount = tempfile::tempdir().unwrap();
    write(&media_mount.path().join("a.rom"), b"rom a content");
    let media_id = repo::find_or_create_removable_media(
        &conn,
        "linux",
        "device_serial",
        "USB123",
        None,
        now(),
    )
    .unwrap();
    let media_scan = filechecker_core::scan::scan_removable_media(
        &mut conn,
        media_id,
        media_mount.path(),
        now(),
    )
    .unwrap()
    .scan_run_id;

    let destination = tempfile::tempdir().unwrap();
    let destination_scan = scan_folder(&mut conn, destination.path());

    let plan = reconstruct::compute_plan(
        &mut conn,
        reference_set_id,
        &[media_scan, destination_scan],
        destination_scan,
        &PasswordPolicy::Reject,
        now(),
    )
    .unwrap();
    assert_eq!(plan.resolved.len(), 1);
    assert_eq!(plan.resolved[0].removable_media_id, Some(media_id));

    let run_id = reconstruct::create_run(
        &mut conn,
        plan.check_run_id,
        &destination.path().to_string_lossy(),
        &plan.resolved,
        now(),
    )
    .unwrap();

    // Pass 1: the medium isn't connected (empty map) — nothing gets written, and the
    // medium is reported as needed.
    let first = reconstruct::run_pass(
        &mut conn,
        run_id,
        destination.path(),
        &PasswordPolicy::Reject,
        &HashMap::new(),
        now(),
    )
    .unwrap();
    assert_eq!(first.written_count, 0);
    assert_eq!(first.still_needed_removable_media, vec![media_id]);
    assert!(!destination.path().join("a.rom").exists());

    // Pass 2: the medium is now connected (its live mount path supplied).
    let mut connected = HashMap::new();
    connected.insert(media_id, media_mount.path().to_path_buf());
    let second = reconstruct::run_pass(
        &mut conn,
        run_id,
        destination.path(),
        &PasswordPolicy::Reject,
        &connected,
        now(),
    )
    .unwrap();
    assert_eq!(second.written_count, 1);
    assert!(second.still_needed_removable_media.is_empty());
    assert_eq!(
        std::fs::read(destination.path().join("a.rom")).unwrap(),
        b"rom a content"
    );
}
