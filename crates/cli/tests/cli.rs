//! End-to-end CLI tests driving the compiled `filechecker` binary as a subprocess
//! (`docs/requirements.md` §10.16): exit-code table coverage, `--exit-zero-on-diff`,
//! and fixed-string "snapshot" assertions on `text`/`json`/`csv` output. The
//! implementation plan's test-strategy table names `insta` for this; these use plain
//! `assert_eq!` against literal expected strings instead — functionally the same
//! (compare full output to a known-good baseline) without the snapshot-review tooling,
//! which doesn't fit a non-interactive environment.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_filechecker")
}

/// Runs the binary with stdin explicitly set to null — deterministically non-TTY
/// regardless of how `cargo test` itself was invoked, which several exit-code paths
/// (§10.16's "requires interactive input but stdin isn't a TTY", code 4) depend on.
fn run(db: &Path, args: &[&str]) -> Output {
    Command::new(bin())
        .arg("--db")
        .arg(db)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("failed to run the filechecker binary")
}

fn write(path: &Path, contents: &str) {
    std::fs::write(path, contents).unwrap();
}

fn stdout(out: &Output) -> String {
    String::from_utf8(out.stdout.clone()).unwrap()
}

fn stderr(out: &Output) -> String {
    String::from_utf8(out.stderr.clone()).unwrap()
}

struct Fixture {
    _dir: tempfile::TempDir,
    db: PathBuf,
    photos: PathBuf,
}

fn setup() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("fc.db");
    let photos = dir.path().join("photos");
    std::fs::create_dir(&photos).unwrap();
    write(&photos.join("a.jpg"), "hello");
    write(&photos.join("b.jpg"), "world!!");
    Fixture {
        _dir: dir,
        db,
        photos,
    }
}

#[test]
fn exit_code_table_is_covered() {
    let f = setup();
    let photos = f.photos.to_str().unwrap();

    let out = run(&f.db, &["scan", "folder", photos]);
    assert!(out.status.success(), "{}", stderr(&out));

    let out = run(
        &f.db,
        &[
            "reference",
            "generate",
            "--from-scan",
            "1",
            "--name",
            "master",
        ],
    );
    assert!(out.status.success(), "{}", stderr(&out));

    // 0: fully unchanged.
    let out = run(
        &f.db,
        &[
            "check",
            "integrity",
            "--reference-set",
            "1",
            "--scan-run",
            "1",
        ],
    );
    assert_eq!(out.status.code(), Some(0));

    // 1: a diff (extra file), no unverifiable files.
    write(&f.photos.join("extra.jpg"), "new");
    let out = run(
        &f.db,
        &[
            "check",
            "integrity",
            "--reference-set",
            "1",
            "--folder",
            photos,
        ],
    );
    assert_eq!(out.status.code(), Some(1));

    // --exit-zero-on-diff downgrades a pure diff (1) to 0.
    let out = run(
        &f.db,
        &[
            "check",
            "integrity",
            "--reference-set",
            "1",
            "--folder",
            photos,
            "--exit-zero-on-diff",
        ],
    );
    assert_eq!(out.status.code(), Some(0));

    // 3: execution failure (unknown reference_set).
    let out = run(
        &f.db,
        &[
            "check",
            "integrity",
            "--reference-set",
            "999",
            "--scan-run",
            "1",
        ],
    );
    assert_eq!(out.status.code(), Some(3));
    assert!(stderr(&out).contains("reference_set"));

    // 3: execution failure (unknown scan_run).
    let out = run(
        &f.db,
        &[
            "check",
            "integrity",
            "--reference-set",
            "1",
            "--scan-run",
            "999",
        ],
    );
    assert_eq!(out.status.code(), Some(3));

    // 64: usage error (missing required argument).
    let out = run(&f.db, &["check", "integrity"]);
    assert_eq!(out.status.code(), Some(64));

    // 64: usage error (mutually exclusive --folder/--scan-run both given).
    let out = run(
        &f.db,
        &[
            "check",
            "integrity",
            "--reference-set",
            "1",
            "--folder",
            photos,
            "--scan-run",
            "1",
        ],
    );
    assert_eq!(out.status.code(), Some(64));
}

#[cfg(unix)]
#[test]
fn unreadable_matched_file_yields_exit_code_2() {
    use std::os::unix::fs::PermissionsExt;

    let f = setup();
    let locked = f.photos.join("a.jpg");

    run(&f.db, &["scan", "folder", f.photos.to_str().unwrap()]);
    run(
        &f.db,
        &[
            "reference",
            "generate",
            "--from-scan",
            "1",
            "--name",
            "master",
        ],
    );

    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::File::open(&locked).is_ok() {
        eprintln!(
            "skipping: running with privileges that bypass permission bits (e.g. root); \
             cannot exercise a real read failure in this environment"
        );
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o644)).unwrap();
        return;
    }

    let out = run(
        &f.db,
        &[
            "check",
            "integrity",
            "--reference-set",
            "1",
            "--folder",
            f.photos.to_str().unwrap(),
        ],
    );
    assert_eq!(out.status.code(), Some(2));

    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o644)).unwrap();
}

#[test]
fn duplicate_check_exit_codes_and_text_summary() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("fc.db");
    let a = dir.path().join("a");
    let b = dir.path().join("b");
    std::fs::create_dir(&a).unwrap();
    std::fs::create_dir(&b).unwrap();
    write(&a.join("x.jpg"), "same content across folders");
    write(&b.join("y.jpg"), "same content across folders");

    let out = run(
        &db,
        &[
            "check",
            "duplicate",
            "--folder",
            a.to_str().unwrap(),
            "--folder",
            b.to_str().unwrap(),
        ],
    );
    assert_eq!(out.status.code(), Some(1));
    let text = stdout(&out);
    assert!(text.contains("グループ数           1"));
    assert!(text.contains("重複ファイル数        2"));

    let c = dir.path().join("c");
    std::fs::create_dir(&c).unwrap();
    write(&c.join("unique.jpg"), "nothing else matches this content");
    let out = run(
        &db,
        &["check", "duplicate", "--folder", c.to_str().unwrap()],
    );
    assert_eq!(out.status.code(), Some(0));

    // 64: neither --folder nor --scan-run given.
    let out = run(&db, &["check", "duplicate"]);
    assert_eq!(out.status.code(), Some(64));
}

#[test]
fn json_and_csv_output_match_expected_snapshots() {
    let f = setup();
    run(&f.db, &["scan", "folder", f.photos.to_str().unwrap()]);
    run(
        &f.db,
        &[
            "reference",
            "generate",
            "--from-scan",
            "1",
            "--name",
            "master",
        ],
    );
    write(&f.photos.join("b.jpg"), "tampered bytes!!");
    write(&f.photos.join("extra.jpg"), "wasn't here before");
    std::fs::remove_file(f.photos.join("a.jpg")).unwrap();

    let out = run(
        &f.db,
        &[
            "check",
            "integrity",
            "--reference-set",
            "1",
            "--folder",
            f.photos.to_str().unwrap(),
            "--format",
            "csv",
        ],
    );
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        stdout(&out),
        "status,path,size,detail\n\
         missing,a.jpg,5,\n\
         extra,extra.jpg,18,\n\
         corrupted,b.jpg,16,SHA256不一致\n"
    );

    let out = run(
        &f.db,
        &[
            "check",
            "show",
            "1",
            "--format",
            "json",
            "--status",
            "corrupted",
        ],
    );
    assert_eq!(out.status.code(), Some(0));
    let json = stdout(&out);
    let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid json");
    assert_eq!(parsed["check_run_id"], 1);
    assert_eq!(parsed["reference_set"]["name"], "master");
    assert_eq!(parsed["reference_set"]["version"], 1);
    assert_eq!(parsed["summary"]["missing"], 1);
    assert_eq!(parsed["summary"]["extra"], 1);
    assert_eq!(parsed["summary"]["corrupted"], 1);
    // --status corrupted narrowed the detail list to just the one corrupted row, even
    // though the summary above still reports the full unfiltered counts.
    let results = parsed["results"].as_array().unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["status"], "corrupted");
}

/// P13's own scope for "大量件数でのエクスポート性能" (implementation-plan.md): a
/// moderate-scale sanity check that csv/json/html export is linear rather than
/// accidentally quadratic (e.g. repeated full-string reallocation per row), not a real
/// benchmark — true TB-scale/hundreds-of-thousands-of-files load testing is P14's job
/// (`docs/implementation-plan.md`'s own dependency graph doesn't gate P13 on it).
#[test]
fn report_export_handles_several_thousand_rows_without_pathological_slowdown() {
    let f = setup();
    let dir = f.db.with_file_name("bulk");
    std::fs::create_dir(&dir).unwrap();
    const N: usize = 4000;
    for i in 0..N {
        write(&dir.join(format!("f{i}.dat")), &format!("v1-{i}"));
    }

    let out = run(&f.db, &["scan", "folder", dir.to_str().unwrap()]);
    assert!(out.status.success(), "{}", stderr(&out));
    let out = run(
        &f.db,
        &[
            "reference",
            "generate",
            "--from-scan",
            "1",
            "--name",
            "master",
        ],
    );
    assert!(out.status.success(), "{}", stderr(&out));

    // Change every file's content so the comparison scan produces N `corrupted` rows
    // (all included in the default export filter, unlike `ok`).
    for i in 0..N {
        write(&dir.join(format!("f{i}.dat")), &format!("v2-{i}-changed"));
    }
    let out = run(&f.db, &["scan", "folder", dir.to_str().unwrap()]);
    assert!(out.status.success(), "{}", stderr(&out));
    let out = run(
        &f.db,
        &[
            "check",
            "integrity",
            "--reference-set",
            "1",
            "--scan-run",
            "2",
        ],
    );
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));

    for format in ["csv", "json", "html"] {
        let out_path = f.db.with_file_name(format!("bulk_export.{format}"));
        let start = std::time::Instant::now();
        let out = run(
            &f.db,
            &[
                "report",
                "export",
                "1",
                "--format",
                format,
                "--output",
                out_path.to_str().unwrap(),
            ],
        );
        assert!(out.status.success(), "{format}: {}", stderr(&out));
        let elapsed = start.elapsed();
        assert!(
            elapsed.as_secs() < 10,
            "{format} export of {N} rows took too long: {elapsed:?}"
        );
        let contents = std::fs::read_to_string(&out_path).unwrap();
        let row_count = match format {
            "csv" => contents.lines().count() - 1, // header
            "json" => {
                let parsed: serde_json::Value = serde_json::from_str(&contents).unwrap();
                parsed["results"].as_array().unwrap().len()
            }
            // Header row uses <tr><th>, data rows use <tr><td> — count only the latter.
            "html" => contents.matches("<tr><td>").count(),
            _ => unreachable!(),
        };
        assert_eq!(row_count, N, "{format} exported the wrong row count");
    }
}

#[test]
fn report_export_rejects_text_format() {
    let f = setup();
    run(&f.db, &["scan", "folder", f.photos.to_str().unwrap()]);
    run(
        &f.db,
        &[
            "reference",
            "generate",
            "--from-scan",
            "1",
            "--name",
            "master",
        ],
    );
    run(
        &f.db,
        &[
            "check",
            "integrity",
            "--reference-set",
            "1",
            "--scan-run",
            "1",
        ],
    );

    let out_path = f.db.with_file_name("out.txt");
    let out = run(
        &f.db,
        &[
            "report",
            "export",
            "1",
            "--format",
            "text",
            "--output",
            out_path.to_str().unwrap(),
        ],
    );
    assert_eq!(out.status.code(), Some(64));
    assert!(!out_path.exists());
}

#[test]
fn report_export_supports_html_and_check_show_rejects_it() {
    let f = setup();
    run(&f.db, &["scan", "folder", f.photos.to_str().unwrap()]);
    run(
        &f.db,
        &[
            "reference",
            "generate",
            "--from-scan",
            "1",
            "--name",
            "master",
        ],
    );
    run(
        &f.db,
        &[
            "check",
            "integrity",
            "--reference-set",
            "1",
            "--scan-run",
            "1",
        ],
    );

    // §10.16: html is only ever valid for `report export`, never for `check show`
    // (which can print straight to stdout).
    let out = run(&f.db, &["check", "show", "1", "--format", "html"]);
    assert_eq!(out.status.code(), Some(64));

    let out_path = f.db.with_file_name("out.html");
    let out = run(
        &f.db,
        &[
            "report",
            "export",
            "1",
            "--format",
            "html",
            "--output",
            out_path.to_str().unwrap(),
        ],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    let html = std::fs::read_to_string(&out_path).unwrap();
    assert!(html.starts_with("<!doctype html>"));
    assert!(html.contains("check_run: 1"));
    assert!(html.contains("master"));
}

#[test]
fn log_dir_writes_no_scan_run_log_file_for_a_clean_scan() {
    // `fs::metadata` (what scan_folder's info-gathering pass actually calls) only needs
    // search permission on the parent directory, never read permission on the target
    // file itself — so unlike a content read, a plain chmod 000 can't be used here to
    // force a genuine scan-time error portably. This test sticks to the side that's
    // reliable everywhere: a clean scan writes no log file at all.
    let f = setup();
    let log_dir = f.db.with_file_name("logs");
    let out = run(
        &f.db,
        &[
            "--log-dir",
            log_dir.to_str().unwrap(),
            "scan",
            "folder",
            f.photos.to_str().unwrap(),
        ],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(!log_dir.join("scan_1.log").exists());
}

#[cfg(unix)]
#[test]
fn log_dir_writes_a_check_run_error_log_for_a_comparison_phase_read_failure() {
    use std::os::unix::fs::PermissionsExt;

    let f = setup();
    let locked = f.photos.join("a.jpg");

    run(&f.db, &["scan", "folder", f.photos.to_str().unwrap()]);
    run(
        &f.db,
        &[
            "reference",
            "generate",
            "--from-scan",
            "1",
            "--name",
            "master",
        ],
    );

    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::File::open(&locked).is_ok() {
        eprintln!(
            "skipping: running with privileges that bypass permission bits (e.g. root); \
             cannot exercise a real read failure in this environment"
        );
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o644)).unwrap();
        return;
    }

    let log_dir = f.db.with_file_name("logs");
    let out = run(
        &f.db,
        &[
            "--log-dir",
            log_dir.to_str().unwrap(),
            "check",
            "integrity",
            "--reference-set",
            "1",
            "--folder",
            f.photos.to_str().unwrap(),
        ],
    );
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    let log_contents = std::fs::read_to_string(log_dir.join("check_1.log")).unwrap();
    // The comparison-phase read failure's detail is the raw io::Error Display (e.g.
    // "Permission denied (os error 13)"), not scan_folder's own classify_error_message
    // wording — different code path, so check for the OS-level phrase instead.
    assert!(log_contents.contains("\tERROR\ta.jpg\t"));
    assert!(log_contents.to_lowercase().contains("permission denied"));

    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o644)).unwrap();
}

#[test]
fn config_get_and_set_round_trip() {
    let f = setup();
    let out = run(&f.db, &["config", "get"]);
    assert_eq!(stdout(&out), "");

    let out = run(&f.db, &["config", "set", "archive_max_depth", "3"]);
    assert!(out.status.success());
    assert_eq!(stdout(&out), "archive_max_depth=3\n");

    let out = run(&f.db, &["config", "get", "archive_max_depth"]);
    assert_eq!(stdout(&out), "archive_max_depth=3\n");

    let out = run(&f.db, &["config", "get", "unknown_key"]);
    assert_eq!(stdout(&out), "unknown_key は設定されていません\n");
}

#[test]
fn media_list_starts_empty() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("fc.db");
    let out = run(&db, &["media", "list"]);
    assert!(out.status.success());
    assert_eq!(stdout(&out), "(no known removable media)\n");
}

#[test]
fn scan_media_requires_a_selector() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("fc.db");
    let out = run(&db, &["scan", "media"]);
    assert_eq!(out.status.code(), Some(64));
}

#[test]
fn scan_media_by_unknown_id_fails() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("fc.db");
    let out = run(&db, &["scan", "media", "--media-id", "999"]);
    assert_eq!(out.status.code(), Some(3));
}

#[test]
fn scan_media_falls_back_to_exit_code_4_without_a_tty() {
    // In this sandboxed CI-like environment there's no removable media for the
    // platform backend to auto-identify, so §10.21's manual-label fallback kicks in —
    // and with stdin forced non-TTY (see `run`), that fallback must fail outright
    // rather than block waiting for input that will never come.
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("fc.db");
    let mount = dir.path().join("not_removable_media");
    std::fs::create_dir(&mount).unwrap();
    let out = run(&db, &["scan", "media", "--mount", mount.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(4));
}

const SOFTWARELIST_SAMPLE: &str = r#"<?xml version="1.0"?>
<softwarelist name="example">
  <software name="game1">
    <part name="cart" interface="cart">
      <dataarea name="rom" size="65536">
        <rom name="game1.bin" size="65536" crc="12345678" sha1="da39a3ee5e6b4b0d3255bfef95601890afd80709" status="good"/>
        <rom name="broken.bin" size="512" status="nodump"/>
      </dataarea>
    </part>
  </software>
</softwarelist>
"#;

#[test]
fn reference_import_mame_softwarelist_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("fc.db");
    let xml = dir.path().join("soft.xml");
    write(&xml, SOFTWARELIST_SAMPLE);

    let out = run(
        &db,
        &[
            "reference",
            "import",
            "--file",
            xml.to_str().unwrap(),
            "--format",
            "mame-softwarelist",
            "--name",
            "test softlist",
        ],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out), "reference_set: 1  imported: 1  excluded: 1\n");

    let out = run(&db, &["reference", "list"]);
    assert!(stdout(&out).contains("test softlist"));
}

#[test]
fn reference_import_rejects_unknown_format() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("fc.db");
    let xml = dir.path().join("soft.xml");
    write(&xml, SOFTWARELIST_SAMPLE);

    let out = run(
        &db,
        &[
            "reference",
            "import",
            "--file",
            xml.to_str().unwrap(),
            "--format",
            "bogus",
            "--name",
            "x",
        ],
    );
    assert_eq!(out.status.code(), Some(64));
}

#[test]
fn reference_import_machinelist_requires_merge_mode() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("fc.db");
    let xml = dir.path().join("soft.xml");
    write(&xml, SOFTWARELIST_SAMPLE);

    let out = run(
        &db,
        &[
            "reference",
            "import",
            "--file",
            xml.to_str().unwrap(),
            "--format",
            "mame-machinelist",
            "--name",
            "x",
        ],
    );
    assert_eq!(out.status.code(), Some(64));
}

#[test]
fn reference_import_missing_file_fails() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("fc.db");
    let out = run(
        &db,
        &[
            "reference",
            "import",
            "--file",
            dir.path().join("does_not_exist.xml").to_str().unwrap(),
            "--format",
            "mame-softwarelist",
            "--name",
            "x",
        ],
    );
    assert_eq!(out.status.code(), Some(3));
}

// ---- archive password handling (§10.7/§10.9/§10.10, P10) --------------------------------
//
// Registered-password *management* (adding passwords, setting a master password) is
// GUI-only (§10.16) — there's no CLI subcommand that could set one up, so a full
// round-trip through an actual TTY master-password prompt isn't something this
// subprocess harness can automate (the same limitation `scan_media_by_mount`'s TTY
// fallback already has, per its own test coverage below). What *is* testable here is
// everything CLI-specific around that prompt: the default (reject) behavior, the
// `--no-archive-password` override, and failing with exit code 4 when a master
// password would be needed but stdin isn't a TTY.

fn build_encrypted_zip(path: &Path, password: &str) {
    use std::io::Write;
    let mut writer = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
    let options = zip::write::SimpleFileOptions::default()
        .with_aes_encryption(zip::AesMode::Aes256, password);
    writer.start_file("secret.txt", options).unwrap();
    writer.write_all(b"top secret contents").unwrap();
    writer.finish().unwrap();
}

#[test]
fn password_protected_archive_entry_is_a_hash_error_by_default() {
    let f = setup();
    build_encrypted_zip(&f.photos.join("locked.zip"), "correct horse battery staple");

    let out = run(&f.db, &["scan", "folder", f.photos.to_str().unwrap()]);
    assert!(out.status.success(), "{}", stderr(&out));

    // `reference generate` hashes every ok scanned file, archive-nested entries
    // included — the encrypted entry can't be decrypted under the default (reject)
    // policy, so it counts as a hash error (§10.11), not a crash or a silent skip.
    let out = run(
        &f.db,
        &[
            "reference",
            "generate",
            "--from-scan",
            "1",
            "--name",
            "master",
        ],
    );
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(stdout(&out).contains("errors: 1"), "{}", stdout(&out));
}

#[test]
fn no_archive_password_flag_overrides_try_registered_mode() {
    let f = setup();
    build_encrypted_zip(&f.photos.join("locked.zip"), "correct horse battery staple");

    let out = run(
        &f.db,
        &["config", "set", "archive_password_mode", "try_registered"],
    );
    assert!(out.status.success(), "{}", stderr(&out));

    // Even with try_registered configured and no --password-store given (which would
    // otherwise be a configuration error), --no-archive-password short-circuits to
    // mode 1 before either is ever consulted — no prompt, no error about the missing
    // store path, just the same reject-and-error-count-1 behavior as the default.
    let out = run(
        &f.db,
        &[
            "--no-archive-password",
            "scan",
            "folder",
            f.photos.to_str().unwrap(),
        ],
    );
    assert!(out.status.success(), "{}", stderr(&out));

    let out = run(
        &f.db,
        &[
            "--no-archive-password",
            "reference",
            "generate",
            "--from-scan",
            "1",
            "--name",
            "master",
        ],
    );
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
}

#[test]
fn try_registered_mode_without_a_password_store_path_is_a_configuration_error() {
    let f = setup();
    let out = run(
        &f.db,
        &["config", "set", "archive_password_mode", "try_registered"],
    );
    assert!(out.status.success(), "{}", stderr(&out));

    let out = run(&f.db, &["scan", "folder", f.photos.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(3), "{}", stderr(&out));
}

#[test]
fn try_registered_mode_without_a_tty_fails_with_interactive_required() {
    let f = setup();
    let out = run(
        &f.db,
        &["config", "set", "archive_password_mode", "try_registered"],
    );
    assert!(out.status.success(), "{}", stderr(&out));

    let store_path = f.db.parent().unwrap().join("passwords.json");
    let out = run(
        &f.db,
        &[
            "--password-store",
            store_path.to_str().unwrap(),
            "scan",
            "folder",
            f.photos.to_str().unwrap(),
        ],
    );
    assert_eq!(out.status.code(), Some(4), "{}", stderr(&out));
}

// ---- reconstruct plan / run / status (§10.16/§10.20, P11) --------------------------------

#[test]
fn reconstruct_plan_and_run_rebuild_the_destination_from_a_library_folder() {
    let f = setup();
    let out = run(&f.db, &["scan", "folder", f.photos.to_str().unwrap()]);
    assert!(out.status.success(), "{}", stderr(&out));
    let out = run(
        &f.db,
        &[
            "reference",
            "generate",
            "--from-scan",
            "1",
            "--name",
            "master",
        ],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    let out = run(
        &f.db,
        &[
            "check",
            "integrity",
            "--reference-set",
            "1",
            "--scan-run",
            "1",
        ],
    );
    assert!(out.status.success(), "{}", stderr(&out));

    let dest = f.db.parent().unwrap().join("dest");
    std::fs::create_dir(&dest).unwrap();

    let out = run(
        &f.db,
        &[
            "reconstruct",
            "plan",
            "--check-run",
            "1",
            "--destination",
            dest.to_str().unwrap(),
        ],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("解決済み: 2"));
    // Planning alone must not create a reconstruction_run or write any files.
    assert!(!dest.join("a.jpg").exists());

    let out = run(
        &f.db,
        &[
            "reconstruct",
            "run",
            "--check-run",
            "1",
            "--destination",
            dest.to_str().unwrap(),
        ],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(std::fs::read(dest.join("a.jpg")).unwrap(), b"hello");
    assert_eq!(std::fs::read(dest.join("b.jpg")).unwrap(), b"world!!");

    let out = run(&f.db, &["reconstruct", "status", "1"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("written: 2"));
    assert!(stdout(&out).contains("pending: 0"));

    // Resuming an already-completed run is a harmless no-op.
    let out = run(&f.db, &["reconstruct", "run", "1"]);
    assert!(out.status.success(), "{}", stderr(&out));
}

#[test]
fn reconstruct_run_requires_either_a_run_id_or_both_check_run_and_destination() {
    let f = setup();
    // Same mutual-exclusivity-style validation as `scan media`'s selector check
    // (§10.16): a bad argument combination is a usage error (64), not a runtime one.
    let out = run(&f.db, &["reconstruct", "run"]);
    assert_eq!(out.status.code(), Some(64), "{}", stderr(&out));

    let out = run(&f.db, &["reconstruct", "run", "--check-run", "1"]);
    assert_eq!(out.status.code(), Some(64), "{}", stderr(&out));
}

#[test]
fn reconstruct_plan_reports_unresolved_reference_files_without_failing() {
    let f = setup();
    run(&f.db, &["scan", "folder", f.photos.to_str().unwrap()]);
    run(
        &f.db,
        &[
            "reference",
            "generate",
            "--from-scan",
            "1",
            "--name",
            "master",
        ],
    );

    // The check_run to reconstruct from bundles only an *empty* folder (scan_run 2) —
    // not scan_run 1 itself, since running an integrity check against it would
    // persist its files' hashes onto scanned_file regardless of what happens to the
    // files on disk afterward. With nothing anywhere matching either reference file's
    // hash, both must come back `missing` rather than the command failing outright.
    let empty_source = f.db.parent().unwrap().join("empty");
    std::fs::create_dir(&empty_source).unwrap();
    let out = run(&f.db, &["scan", "folder", empty_source.to_str().unwrap()]);
    assert!(out.status.success(), "{}", stderr(&out));
    let out = run(
        &f.db,
        &[
            "check",
            "integrity",
            "--reference-set",
            "1",
            "--scan-run",
            "2",
        ],
    );
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out)); // both reference files missing

    let dest = f.db.parent().unwrap().join("dest2");
    std::fs::create_dir(&dest).unwrap();
    let out = run(
        &f.db,
        &[
            "reconstruct",
            "plan",
            "--check-run",
            "1",
            "--destination",
            dest.to_str().unwrap(),
        ],
    );
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert!(stdout(&out).contains("未解決: 2"));
}

#[test]
fn reconstruct_plan_rejects_a_duplicate_type_check_run() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("fc.db");
    let a = dir.path().join("a");
    let b = dir.path().join("b");
    std::fs::create_dir(&a).unwrap();
    std::fs::create_dir(&b).unwrap();
    write(&a.join("x.jpg"), "same content across folders");
    write(&b.join("y.jpg"), "same content across folders");
    run(
        &db,
        &[
            "check",
            "duplicate",
            "--folder",
            a.to_str().unwrap(),
            "--folder",
            b.to_str().unwrap(),
        ],
    );

    let dest = dir.path().join("dest");
    std::fs::create_dir(&dest).unwrap();
    let out = run(
        &db,
        &[
            "reconstruct",
            "plan",
            "--check-run",
            "1",
            "--destination",
            dest.to_str().unwrap(),
        ],
    );
    assert_eq!(out.status.code(), Some(3), "{}", stderr(&out));
}
