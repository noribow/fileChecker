//! Subcommand handlers (`docs/requirements.md` §10.16). Each function does its own
//! validation of user-supplied IDs/paths and returns a `CliError` (mapped to exit code
//! 3, "実行失敗") for anything that keeps the command from completing at all; the
//! `Ok(i32)` it returns on success is the process's final exit code, computed per
//! §10.16's table by the caller-specific logic below.

use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};

use filechecker_core::db::{repo, CheckType, Connection};
use filechecker_core::import::{self, MameFormat, MergeMode};
use filechecker_core::{duplicate, integrity, media, reference, scan};

use crate::db::now_millis;
use crate::exit;
use crate::output::{self, Format};

pub struct CliError {
    pub message: String,
    pub exit_code: i32,
}

impl CliError {
    fn failure(msg: impl Into<String>) -> Self {
        CliError {
            message: msg.into(),
            exit_code: exit::FAILURE,
        }
    }
}

impl From<filechecker_core::db::Error> for CliError {
    fn from(e: filechecker_core::db::Error) -> Self {
        CliError::failure(e.to_string())
    }
}

type CmdResult = Result<i32, CliError>;

fn progress(quiet: bool, line: &str) {
    if !quiet {
        eprintln!("{line}");
    }
}

fn write_output(text: &str, output_file: Option<&Path>) -> Result<(), CliError> {
    match output_file {
        Some(path) => std::fs::write(path, text).map_err(|e| {
            CliError::failure(format!(
                "出力ファイルに書き込めません ({}): {e}",
                path.display()
            ))
        }),
        None => {
            print!("{text}");
            std::io::stdout().flush().ok();
            Ok(())
        }
    }
}

// ---- scan folder ----------------------------------------------------------------------

pub fn scan_folder(conn: &mut Connection, path: &Path, quiet: bool) -> CmdResult {
    if !path.is_dir() {
        return Err(CliError::failure(format!(
            "対象フォルダが存在しません: {}",
            path.display()
        )));
    }
    progress(quiet, &format!("scanning {} ...", path.display()));
    let summary = scan::scan_folder(conn, path, now_millis())?;
    progress(
        quiet,
        &format!(
            "done: scan_run {}, ok {}, error {}, walk_errors {}",
            summary.scan_run_id, summary.scanned_ok, summary.scanned_error, summary.walk_errors
        ),
    );
    println!(
        "scan_run: {}  ok: {}  error: {}  walk_errors: {}",
        summary.scan_run_id, summary.scanned_ok, summary.scanned_error, summary.walk_errors
    );
    Ok(exit::SUCCESS)
}

// ---- scan media / media list -----------------------------------------------------------

pub fn media_list(conn: &Connection) -> CmdResult {
    let media = repo::list_removable_media(conn)?;
    if media.is_empty() {
        println!("(no known removable media)");
        return Ok(exit::SUCCESS);
    }
    for m in &media {
        println!(
            "{:>5}  {:<20} {}={}  last_seen={}",
            m.id,
            m.display_name.as_deref().unwrap_or("(no name)"),
            m.identifier_type,
            m.identifier_value,
            m.last_seen_at
        );
    }
    Ok(exit::SUCCESS)
}

/// `scan media (--media-id <ID> | --mount <PATH>)` (§10.16): scans a currently-
/// connected removable medium under the eager hash mode (§10.8). Exactly one of the
/// two selectors must be given — `--media-id` reuses an already-known medium (it must
/// currently be connected, found via re-running platform identification), `--mount`
/// identifies whatever is mounted there (auto-detecting via the platform backend,
/// falling back to a TTY-prompted manual label per §10.21 if that fails).
pub fn scan_media(
    conn: &mut Connection,
    media_id: Option<i64>,
    mount: Option<PathBuf>,
    quiet: bool,
) -> CmdResult {
    match (media_id, mount) {
        (Some(_), Some(_)) => Err(CliError {
            message: "--media-id と --mount は同時に指定できません".to_string(),
            exit_code: exit::USAGE_ERROR,
        }),
        (None, None) => Err(CliError {
            message: "--media-id または --mount のいずれかを指定してください".to_string(),
            exit_code: exit::USAGE_ERROR,
        }),
        (Some(id), None) => scan_media_by_id(conn, id, quiet),
        (None, Some(mount_path)) => scan_media_by_mount(conn, &mount_path, quiet),
    }
}

fn scan_media_by_id(conn: &mut Connection, media_id: i64, quiet: bool) -> CmdResult {
    let known = repo::get_removable_media(conn, media_id)?.ok_or_else(|| {
        CliError::failure(format!("removable_media が見つかりません: {media_id}"))
    })?;
    let connected = media::platform_identifier()
        .list_connected()
        .map_err(|e| CliError::failure(format!("接続中メディアの一覧取得に失敗しました: {e}")))?;
    let detected = connected
        .iter()
        .find(|d| {
            d.identifier_type == known.identifier_type
                && d.identifier_value == known.identifier_value
        })
        .ok_or_else(|| {
            CliError::failure(format!("メディア {media_id} は現在接続されていません"))
        })?;
    run_scan_media(
        conn,
        &known.platform,
        &known.identifier_type,
        &known.identifier_value,
        detected.display_name.clone(),
        &detected.mount_path,
        quiet,
    )
}

fn scan_media_by_mount(conn: &mut Connection, mount_path: &Path, quiet: bool) -> CmdResult {
    if !mount_path.is_dir() {
        return Err(CliError::failure(format!(
            "マウントパスが存在しません: {}",
            mount_path.display()
        )));
    }
    let connected = media::platform_identifier()
        .list_connected()
        .map_err(|e| CliError::failure(format!("接続中メディアの一覧取得に失敗しました: {e}")))?;
    if let Some(detected) = connected.iter().find(|d| d.mount_path == mount_path) {
        return run_scan_media(
            conn,
            media::current_platform(),
            &detected.identifier_type,
            &detected.identifier_value,
            detected.display_name.clone(),
            mount_path,
            quiet,
        );
    }

    // §10.21 fallback: couldn't auto-identify this medium. GUI prompts in a dialog;
    // the CLI equivalent is a TTY prompt, failing outright (exit code 4) when there's
    // no TTY to prompt on (e.g. CI).
    if !std::io::stdin().is_terminal() {
        return Err(CliError {
            message:
                "識別子を自動取得できませんでした（標準入力がTTYでないためラベル入力できません）"
                    .to_string(),
            exit_code: exit::INTERACTIVE_REQUIRED,
        });
    }
    eprintln!("識別子を自動取得できませんでした。このメディアのラベルを入力してください:");
    let mut label = String::new();
    std::io::stdin()
        .read_line(&mut label)
        .map_err(|e| CliError::failure(e.to_string()))?;
    let label = label.trim();
    if label.is_empty() {
        return Err(CliError::failure("ラベルが入力されませんでした"));
    }
    run_scan_media(
        conn,
        media::current_platform(),
        "user_defined",
        label,
        None,
        mount_path,
        quiet,
    )
}

#[allow(clippy::too_many_arguments)]
fn run_scan_media(
    conn: &mut Connection,
    platform: &str,
    identifier_type: &str,
    identifier_value: &str,
    display_name: Option<String>,
    mount_path: &Path,
    quiet: bool,
) -> CmdResult {
    let now = now_millis();
    let media_id = repo::find_or_create_removable_media(
        conn,
        platform,
        identifier_type,
        identifier_value,
        display_name.as_deref(),
        now,
    )?;
    progress(
        quiet,
        &format!(
            "scanning media {} ({identifier_type}={identifier_value}) ...",
            mount_path.display()
        ),
    );
    let summary = scan::scan_removable_media(conn, media_id, mount_path, now)?;
    println!(
        "removable_media: {media_id}  scan_run: {}  ok: {}  error: {}  walk_errors: {}",
        summary.scan_run_id, summary.scanned_ok, summary.scanned_error, summary.walk_errors
    );
    Ok(exit::SUCCESS)
}

// ---- reference generate / list ---------------------------------------------------------

pub fn reference_generate(
    conn: &mut Connection,
    from_scan: i64,
    name: &str,
    supersede: Option<i64>,
) -> CmdResult {
    require_scan_run(conn, from_scan)?;
    if let Some(id) = supersede {
        require_reference_set(conn, id)?;
    }
    let summary = reference::generate_reference_set_from_scan_run(
        conn,
        from_scan,
        name,
        supersede,
        now_millis(),
    )?;
    println!(
        "reference_set: {}  files: {}  errors: {}",
        summary.reference_set_id, summary.file_count, summary.error_count
    );
    Ok(if summary.error_count > 0 {
        exit::UNVERIFIABLE
    } else {
        exit::SUCCESS
    })
}

pub fn reference_list(conn: &Connection) -> CmdResult {
    let sets = repo::list_reference_sets(conn)?;
    if sets.is_empty() {
        println!("(no reference sets)");
        return Ok(exit::SUCCESS);
    }
    for s in &sets {
        let version = repo::reference_set_version(conn, s.id)?;
        let supersedes = s
            .supersedes_reference_set_id
            .map(|id| format!(", supersedes {id}"))
            .unwrap_or_default();
        println!(
            "{:>5}  \"{}\" v{}  format={}{}",
            s.id, s.name, version, s.source_format, supersedes
        );
    }
    Ok(exit::SUCCESS)
}

/// `reference import --file <FILE> --format <ID> --name <NAME> [--merge-mode ...]
/// [--include-baddump]` (§10.16/§10.18). Only MAME's two surveyed formats exist today;
/// `--merge-mode` is required for `mame-machinelist` (§10.18 point 3 — the physical
/// archive layout can't be auto-detected) and unused for `mame-softwarelist` (that
/// format has no merge/romof concept at all).
pub fn reference_import(
    conn: &mut Connection,
    file: &Path,
    format: &str,
    name: &str,
    merge_mode: Option<&str>,
    include_baddump: bool,
) -> CmdResult {
    let mame_format = MameFormat::parse_str(format).ok_or_else(|| CliError {
        message: format!("不明な--format: {format} (mame-softwarelist|mame-machinelistのいずれか)"),
        exit_code: exit::USAGE_ERROR,
    })?;

    let merge_mode =
        match mame_format {
            MameFormat::MachineList => match merge_mode {
                Some("merged") => MergeMode::Merged,
                Some("split") => MergeMode::Split,
                Some(other) => {
                    return Err(CliError {
                        message: format!("不明な--merge-mode: {other} (merged|splitのいずれか)"),
                        exit_code: exit::USAGE_ERROR,
                    })
                }
                None => return Err(CliError {
                    message:
                        "mame-machinelistの取り込みには--merge-mode（merged|split）の指定が必要です"
                            .to_string(),
                    exit_code: exit::USAGE_ERROR,
                }),
            },
            // softwarelist has no merge/romof concept; the value is never consulted.
            MameFormat::SoftwareList => MergeMode::Split,
        };

    if !file.is_file() {
        return Err(CliError::failure(format!(
            "入力ファイルが存在しません: {}",
            file.display()
        )));
    }

    let options = import::ImportOptions {
        include_baddump,
        merge_mode,
    };
    let summary =
        import::import_mame_reference_set(conn, mame_format, file, name, &options, now_millis())
            .map_err(|e| CliError::failure(e.to_string()))?;

    println!(
        "reference_set: {}  imported: {}  excluded: {}",
        summary.reference_set_id, summary.imported_count, summary.excluded_count
    );
    Ok(exit::SUCCESS)
}

// ---- check integrity / duplicate --------------------------------------------------------

#[allow(clippy::too_many_arguments)]
pub fn check_integrity(
    conn: &mut Connection,
    reference_set_id: i64,
    folder: Option<PathBuf>,
    scan_run_ids: Vec<i64>,
    format: Format,
    output_file: Option<PathBuf>,
    status: Vec<String>,
    exit_zero_on_diff: bool,
    quiet: bool,
) -> CmdResult {
    let reference_set = require_reference_set(conn, reference_set_id)?;
    let scan_run_ids = resolve_scan_run_ids(conn, folder, scan_run_ids, quiet)?;

    progress(
        quiet,
        &format!(
            "comparing against reference set \"{}\" ...",
            reference_set.name
        ),
    );
    let summary =
        integrity::run_integrity_check(conn, reference_set_id, &scan_run_ids, now_millis())?;
    let reference_set_version = repo::reference_set_version(conn, reference_set_id)?;

    let all_rows = repo::list_integrity_results(conn, summary.check_run_id, None)?;
    let selected = if status.is_empty() {
        output::default_status_filter()
    } else {
        status
    };
    let detail_rows = output::filter_by_status(&all_rows, &selected);
    let counts = output::IntegrityCounts {
        ok: summary.ok_count,
        corrupted: summary.corrupted_count,
        missing: summary.missing_count,
        extra: summary.extra_count,
        error: summary.error_count,
    };
    let text = output::render_integrity(
        format,
        summary.check_run_id,
        &reference_set.name,
        reference_set_version,
        &counts,
        &detail_rows,
    );
    write_output(&text, output_file.as_deref())?;

    Ok(exit::integrity_exit_code(
        summary.corrupted_count,
        summary.missing_count,
        summary.extra_count,
        summary.error_count,
        exit_zero_on_diff,
    ))
}

#[allow(clippy::too_many_arguments)]
pub fn check_duplicate(
    conn: &mut Connection,
    folders: Vec<PathBuf>,
    scan_run_ids: Vec<i64>,
    format: Format,
    output_file: Option<PathBuf>,
    exit_zero_on_diff: bool,
    quiet: bool,
) -> CmdResult {
    if folders.is_empty() && scan_run_ids.is_empty() {
        return Err(CliError {
            message: "--folder または --scan-run を1つ以上指定してください".to_string(),
            exit_code: exit::USAGE_ERROR,
        });
    }

    let mut ids = Vec::new();
    for folder in folders {
        if !folder.is_dir() {
            return Err(CliError::failure(format!(
                "対象フォルダが存在しません: {}",
                folder.display()
            )));
        }
        progress(quiet, &format!("scanning {} ...", folder.display()));
        let summary = scan::scan_folder(conn, &folder, now_millis())?;
        ids.push(summary.scan_run_id);
    }
    for id in scan_run_ids {
        require_scan_run(conn, id)?;
        ids.push(id);
    }

    progress(quiet, "comparing (size -> CRC32 -> SHA-256) ...");
    let summary = duplicate::run_duplicate_check(conn, &ids, now_millis())?;

    let groups = repo::list_duplicate_groups(conn, summary.check_run_id)?;
    let groups_with_members: Result<Vec<_>, CliError> = groups
        .into_iter()
        .map(|g| {
            let members = repo::list_duplicate_group_members(conn, g.id)?;
            Ok((g, members))
        })
        .collect();
    let groups_with_members = groups_with_members?;

    let counts = output::DuplicateCounts {
        group_count: summary.group_count,
        duplicate_file_count: summary.duplicate_file_count,
        reclaimable_bytes: groups_with_members
            .iter()
            .map(|(g, _)| g.size * (g.member_count.saturating_sub(1)))
            .sum(),
        error_count: Some(summary.error_count),
    };
    let text =
        output::render_duplicate(format, summary.check_run_id, &counts, &groups_with_members);
    write_output(&text, output_file.as_deref())?;

    Ok(exit::duplicate_exit_code(
        summary.group_count,
        summary.error_count,
        exit_zero_on_diff,
    ))
}

// ---- check list / show ------------------------------------------------------------------

pub fn check_list(conn: &Connection, check_type: Option<String>, limit: Option<i64>) -> CmdResult {
    let check_type = match check_type.as_deref() {
        None => None,
        Some("integrity") => Some(CheckType::Integrity),
        Some("duplicate") => Some(CheckType::Duplicate),
        Some(other) => {
            return Err(CliError {
                message: format!("不明な--type: {other} (integrity|duplicate)"),
                exit_code: exit::USAGE_ERROR,
            })
        }
    };
    let runs = repo::list_check_runs(conn, check_type, limit)?;
    if runs.is_empty() {
        println!("(no check runs)");
        return Ok(exit::SUCCESS);
    }
    for r in &runs {
        println!(
            "{:>5}  {:<10} status={:<10} started_at={}",
            r.id,
            r.check_type.as_str(),
            r.status.as_str(),
            r.started_at
        );
    }
    Ok(exit::SUCCESS)
}

pub fn check_show(
    conn: &Connection,
    check_run_id: i64,
    format: Format,
    output_file: Option<PathBuf>,
    status: Vec<String>,
) -> CmdResult {
    let run = repo::get_check_run(conn, check_run_id)?
        .ok_or_else(|| CliError::failure(format!("check_run が見つかりません: {check_run_id}")))?;

    match run.check_type {
        CheckType::Integrity => {
            let reference_set_id = run
                .reference_set_id
                .expect("integrity check_run always has one");
            let reference_set = require_reference_set(conn, reference_set_id)?;
            let reference_set_version = repo::reference_set_version(conn, reference_set_id)?;
            let all_rows = repo::list_integrity_results(conn, check_run_id, None)?;
            let counts = output::IntegrityCounts::from_rows(&all_rows);
            let selected = if status.is_empty() {
                output::default_status_filter()
            } else {
                status
            };
            let detail_rows = output::filter_by_status(&all_rows, &selected);
            let text = output::render_integrity(
                format,
                check_run_id,
                &reference_set.name,
                reference_set_version,
                &counts,
                &detail_rows,
            );
            write_output(&text, output_file.as_deref())?;
        }
        CheckType::Duplicate => {
            let groups = repo::list_duplicate_groups(conn, check_run_id)?;
            let groups_with_members: Result<Vec<_>, CliError> = groups
                .into_iter()
                .map(|g| {
                    let members = repo::list_duplicate_group_members(conn, g.id)?;
                    Ok((g, members))
                })
                .collect();
            let groups_with_members = groups_with_members?;
            let counts = output::DuplicateCounts {
                group_count: groups_with_members.len(),
                duplicate_file_count: groups_with_members
                    .iter()
                    .map(|(g, _)| g.member_count as usize)
                    .sum(),
                reclaimable_bytes: groups_with_members
                    .iter()
                    .map(|(g, _)| g.size * g.member_count.saturating_sub(1))
                    .sum(),
                error_count: None,
            };
            let text =
                output::render_duplicate(format, check_run_id, &counts, &groups_with_members);
            write_output(&text, output_file.as_deref())?;
        }
    }

    // `check show` redisplays a past result rather than performing a fresh check, so it
    // always reports plain success/failure (0/3) — see docs/progress-log.md's P6 entry
    // for why the diff-based 0/1/2 codes are reserved for `check integrity`/`check
    // duplicate` themselves.
    Ok(exit::SUCCESS)
}

// ---- report export -----------------------------------------------------------------------

pub fn report_export(
    conn: &Connection,
    check_run_id: i64,
    format: Format,
    output_file: PathBuf,
) -> CmdResult {
    if format == Format::Text {
        return Err(CliError {
            message:
                "report export は csv|json のみ対応します（textはcheck showを使用してください）"
                    .to_string(),
            exit_code: exit::USAGE_ERROR,
        });
    }
    check_show(conn, check_run_id, format, Some(output_file), Vec::new())
}

// ---- config get / set ----------------------------------------------------------------------

pub fn config_get(conn: &Connection, key: Option<String>) -> CmdResult {
    match key {
        Some(key) => match repo::get_app_setting(conn, &key)? {
            Some(value) => println!("{key}={value}"),
            None => println!("{key} は設定されていません"),
        },
        None => {
            for (key, value) in repo::list_app_settings(conn)? {
                println!("{key}={value}");
            }
        }
    }
    Ok(exit::SUCCESS)
}

pub fn config_set(conn: &Connection, key: &str, value: &str) -> CmdResult {
    repo::set_app_setting(conn, key, value)?;
    println!("{key}={value}");
    Ok(exit::SUCCESS)
}

// ---- shared validation helpers ---------------------------------------------------------

fn require_scan_run(conn: &Connection, id: i64) -> Result<repo::ScanRunRow, CliError> {
    repo::get_scan_run(conn, id)?
        .ok_or_else(|| CliError::failure(format!("scan_run が見つかりません: {id}")))
}

fn require_reference_set(conn: &Connection, id: i64) -> Result<repo::ReferenceSetRow, CliError> {
    repo::get_reference_set(conn, id)?
        .ok_or_else(|| CliError::failure(format!("reference_set が見つかりません: {id}")))
}

/// `check integrity`'s `--folder` (fresh scan) vs. `--scan-run` (reuse saved scans, §6)
/// choice — exactly one form must be given.
fn resolve_scan_run_ids(
    conn: &mut Connection,
    folder: Option<PathBuf>,
    scan_run_ids: Vec<i64>,
    quiet: bool,
) -> Result<Vec<i64>, CliError> {
    match (folder, scan_run_ids.is_empty()) {
        (Some(_), false) => Err(CliError {
            message: "--folder と --scan-run は同時に指定できません".to_string(),
            exit_code: exit::USAGE_ERROR,
        }),
        (None, true) => Err(CliError {
            message: "--folder または --scan-run のいずれかを指定してください".to_string(),
            exit_code: exit::USAGE_ERROR,
        }),
        (Some(folder), true) => {
            if !folder.is_dir() {
                return Err(CliError::failure(format!(
                    "対象フォルダが存在しません: {}",
                    folder.display()
                )));
            }
            progress(quiet, &format!("scanning {} ...", folder.display()));
            let summary = scan::scan_folder(conn, &folder, now_millis())?;
            Ok(vec![summary.scan_run_id])
        }
        (None, false) => {
            for id in &scan_run_ids {
                require_scan_run(conn, *id)?;
            }
            Ok(scan_run_ids)
        }
    }
}
