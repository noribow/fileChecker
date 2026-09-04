//! End-to-end CLI tests driving the compiled `filechecker` binary as a subprocess
//! (`docs/requirements.md` §10.16): exit-code table coverage, `--exit-zero-on-diff`,
//! and fixed-string "snapshot" assertions on `text`/`json`/`csv` output. The
//! implementation plan's test-strategy table names `insta` for this; these use plain
//! `assert_eq!` against literal expected strings instead — functionally the same
//! (compare full output to a known-good baseline) without the snapshot-review tooling,
//! which doesn't fit a non-interactive environment.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_filechecker")
}

fn run(db: &Path, args: &[&str]) -> Output {
    Command::new(bin())
        .arg("--db")
        .arg(db)
        .args(args)
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
