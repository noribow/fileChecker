//! Subcommand handlers (`docs/requirements.md` §10.16). Each function does its own
//! validation of user-supplied IDs/paths and returns a `CliError` (mapped to exit code
//! 3, "実行失敗") for anything that keeps the command from completing at all; the
//! `Ok(i32)` it returns on success is the process's final exit code, computed per
//! §10.16's table by the caller-specific logic below.

use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};

use filechecker_core::archive::PasswordPolicy;
use filechecker_core::db::{repo, CheckType, Connection};
use filechecker_core::import::{self, MameFormat, MergeMode};
use filechecker_core::{duplicate, errorlog, integrity, media, reconstruct, reference, scan};

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

pub fn scan_folder(
    conn: &mut Connection,
    path: &Path,
    quiet: bool,
    policy: &PasswordPolicy,
    log_dir: Option<&Path>,
) -> CmdResult {
    if !path.is_dir() {
        return Err(CliError::failure(format!(
            "対象フォルダが存在しません: {}",
            path.display()
        )));
    }
    progress(quiet, &format!("scanning {} ...", path.display()));
    let now = now_millis();
    let summary = scan::scan_folder_with_password_policy(conn, path, now, policy)?;
    write_scan_run_log(conn, log_dir, summary.scan_run_id, now);
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

/// Best-effort (§10.17's log file is the *secondary* record): writes the scan_run's
/// text error log when `--log-dir` was given, silently doing nothing otherwise.
fn write_scan_run_log(conn: &Connection, log_dir: Option<&Path>, scan_run_id: i64, now: i64) {
    if let Some(dir) = log_dir {
        let _ = errorlog::write_scan_run_log(conn, dir, scan_run_id, now);
    }
}

/// Same as `write_scan_run_log`, for a `check_run` (§10.17/§10.22).
fn write_check_run_log(
    conn: &Connection,
    log_dir: Option<&Path>,
    check_run_id: i64,
    is_integrity: bool,
    now: i64,
) {
    if let Some(dir) = log_dir {
        let _ = errorlog::write_check_run_log(conn, dir, check_run_id, is_integrity, now);
    }
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
#[allow(clippy::too_many_arguments)]
pub fn scan_media(
    conn: &mut Connection,
    media_id: Option<i64>,
    mount: Option<PathBuf>,
    quiet: bool,
    policy: &PasswordPolicy,
    log_dir: Option<&Path>,
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
        (Some(id), None) => scan_media_by_id(conn, id, quiet, policy, log_dir),
        (None, Some(mount_path)) => scan_media_by_mount(conn, &mount_path, quiet, policy, log_dir),
    }
}

fn scan_media_by_id(
    conn: &mut Connection,
    media_id: i64,
    quiet: bool,
    policy: &PasswordPolicy,
    log_dir: Option<&Path>,
) -> CmdResult {
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
        policy,
        log_dir,
    )
}

fn scan_media_by_mount(
    conn: &mut Connection,
    mount_path: &Path,
    quiet: bool,
    policy: &PasswordPolicy,
    log_dir: Option<&Path>,
) -> CmdResult {
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
            policy,
            log_dir,
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
        policy,
        log_dir,
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
    policy: &PasswordPolicy,
    log_dir: Option<&Path>,
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
    let summary =
        scan::scan_removable_media_with_password_policy(conn, media_id, mount_path, now, policy)?;
    write_scan_run_log(conn, log_dir, summary.scan_run_id, now);
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
    policy: &PasswordPolicy,
    log_dir: Option<&Path>,
) -> CmdResult {
    require_scan_run(conn, from_scan)?;
    if let Some(id) = supersede {
        require_reference_set(conn, id)?;
    }
    let now = now_millis();
    let summary = reference::generate_reference_set_from_scan_run_with_password_policy(
        conn, from_scan, name, supersede, now, policy,
    )?;
    // Generation can newly mark scanned_file rows errored (hash failures since the
    // original scan, §10.11) beyond whatever `scan folder` itself already logged —
    // append those to that same scan_run's log rather than starting a new file.
    write_scan_run_log(conn, log_dir, from_scan, now);
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
    policy: &PasswordPolicy,
    log_dir: Option<&Path>,
) -> CmdResult {
    reject_html_for_stdout(format, "check integrity")?;
    let reference_set = require_reference_set(conn, reference_set_id)?;
    let scan_run_ids = resolve_scan_run_ids(conn, folder, scan_run_ids, quiet, policy)?;

    progress(
        quiet,
        &format!(
            "comparing against reference set \"{}\" ...",
            reference_set.name
        ),
    );
    let now = now_millis();
    let summary = integrity::run_integrity_check_with_password_policy(
        conn,
        reference_set_id,
        &scan_run_ids,
        now,
        policy,
    )?;
    write_check_run_log(conn, log_dir, summary.check_run_id, true, now);
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
    policy: &PasswordPolicy,
    log_dir: Option<&Path>,
) -> CmdResult {
    reject_html_for_stdout(format, "check duplicate")?;
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
        let now = now_millis();
        let summary = scan::scan_folder_with_password_policy(conn, &folder, now, policy)?;
        write_scan_run_log(conn, log_dir, summary.scan_run_id, now);
        ids.push(summary.scan_run_id);
    }
    for id in scan_run_ids {
        require_scan_run(conn, id)?;
        ids.push(id);
    }

    progress(quiet, "comparing (size -> CRC32 -> SHA-256) ...");
    let now = now_millis();
    let summary = duplicate::run_duplicate_check_with_password_policy(conn, &ids, now, policy)?;
    write_check_run_log(conn, log_dir, summary.check_run_id, false, now);

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

/// `check show` itself (unlike `report export`, which reuses this same rendering via
/// `check_show` below) rejects `html` — §10.16 reserves html for `report export`.
pub fn check_show_cli(
    conn: &Connection,
    check_run_id: i64,
    format: Format,
    output_file: Option<PathBuf>,
    status: Vec<String>,
) -> CmdResult {
    reject_html_for_stdout(format, "check show")?;
    check_show(conn, check_run_id, format, output_file, status)
}

/// §10.16: `html` is only ever valid for `report export`, never for a command that can
/// print straight to stdout (`check integrity`/`check duplicate`/`check show`) — html
/// output "is large" and is meant for a file, not a terminal or a `| jq` pipeline.
fn reject_html_for_stdout(format: Format, command_name: &str) -> Result<(), CliError> {
    if format == Format::Html {
        return Err(CliError {
            message: format!(
                "{command_name} は text|json|csv のみ対応します（htmlはreport exportを使用してください）"
            ),
            exit_code: exit::USAGE_ERROR,
        });
    }
    Ok(())
}

fn check_show(
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
                "report export は csv|json|html のみ対応します（textはcheck showを使用してください）"
                    .to_string(),
            exit_code: exit::USAGE_ERROR,
        });
    }
    check_show(conn, check_run_id, format, Some(output_file), Vec::new())
}

// ---- reconstruct plan / run / status ---------------------------------------------------

/// `reconstruct plan --check-run <ID> --destination <PATH>` (§10.16/§10.20): scans the
/// destination fresh, computes the fulfillment plan, and reports it — no
/// `reconstruction_run` is created (planning is read-only/repeatable; `reconstruct
/// run` is what commits to executing it).
pub fn reconstruct_plan(
    conn: &mut Connection,
    check_run_id: i64,
    destination: &Path,
    quiet: bool,
    policy: &PasswordPolicy,
) -> CmdResult {
    let plan = compute_plan_for(conn, check_run_id, destination, quiet, policy)?;
    print_plan(conn, &plan)?;
    Ok(if plan.missing.is_empty() {
        exit::SUCCESS
    } else {
        exit::DIFF
    })
}

/// `reconstruct run (<RECONSTRUCTION_RUN_ID> | --check-run <ID> --destination <PATH>)`
/// (§10.16/§10.20): either resumes an existing reconstruction_run's still-outstanding
/// items, or plans-and-creates a new one first. Runs passes until either everything is
/// resolved or every removable medium still needed has been offered a chance to
/// connect (prompting on a TTY; reporting and stopping otherwise, §10.16's
/// TTY-required pattern already used for the master-password/`--mount` flows).
pub fn reconstruct_run(
    conn: &mut Connection,
    reconstruction_run: Option<i64>,
    check_run: Option<i64>,
    destination: Option<PathBuf>,
    quiet: bool,
    policy: &PasswordPolicy,
) -> CmdResult {
    let run_id = match (reconstruction_run, check_run, destination) {
        (Some(id), None, None) => id,
        (None, Some(check_run_id), Some(destination)) => {
            let plan = compute_plan_for(conn, check_run_id, &destination, quiet, policy)?;
            print_plan(conn, &plan)?;
            reconstruct::create_run(
                conn,
                plan.check_run_id,
                &destination.to_string_lossy(),
                &plan.resolved,
                now_millis(),
            )?
        }
        _ => {
            return Err(CliError {
                message: "RECONSTRUCTION_RUN_ID を単独で指定するか、--check-run と --destination を両方指定してください"
                    .to_string(),
                exit_code: exit::USAGE_ERROR,
            })
        }
    };

    let mut total_written = 0usize;
    let mut total_error = 0usize;
    loop {
        let run = repo::get_reconstruction_run(conn, run_id)?.ok_or_else(|| {
            CliError::failure(format!("reconstruction_run が見つかりません: {run_id}"))
        })?;
        let destination_path = PathBuf::from(&run.destination_folder_path);
        let connected_media = detect_connected_media_for_run(conn, run_id)?;

        let summary = reconstruct::run_pass(
            conn,
            run_id,
            &destination_path,
            policy,
            &connected_media,
            now_millis(),
        )?;
        total_written += summary.written_count;
        total_error += summary.error_count;
        progress(
            quiet,
            &format!(
                "written {} (total {}), error {} (total {})",
                summary.written_count, total_written, summary.error_count, total_error
            ),
        );

        if summary.still_needed_removable_media.is_empty() {
            break;
        }
        let names = removable_media_names(conn, &summary.still_needed_removable_media)?;
        if !std::io::stdin().is_terminal() {
            println!(
                "未接続のリムーバブルメディアが必要です（標準入力がTTYでないため入れ替えを待てません）: {}",
                names.join(", ")
            );
            break;
        }
        eprintln!("次のメディアに入れ替えてください: {}", names.join(", "));
        eprintln!("入れ替えが完了したらEnterキーを押してください:");
        let mut line = String::new();
        std::io::stdin()
            .read_line(&mut line)
            .map_err(|e| CliError::failure(e.to_string()))?;
    }

    let counts = repo::count_reconstruction_items(conn, run_id)?;
    println!(
        "reconstruction_run: {run_id}  written: {}  pending: {}  error: {}",
        counts.written, counts.pending, counts.error
    );

    Ok(if counts.error > 0 {
        exit::UNVERIFIABLE
    } else if counts.pending > 0 {
        exit::DIFF
    } else {
        exit::SUCCESS
    })
}

pub fn reconstruct_status(conn: &Connection, reconstruction_run_id: i64) -> CmdResult {
    let run = repo::get_reconstruction_run(conn, reconstruction_run_id)?.ok_or_else(|| {
        CliError::failure(format!(
            "reconstruction_run が見つかりません: {reconstruction_run_id}"
        ))
    })?;
    let counts = repo::count_reconstruction_items(conn, reconstruction_run_id)?;
    println!(
        "reconstruction_run: {}  status: {}  destination: {}",
        run.id,
        run.status.as_str(),
        run.destination_folder_path
    );
    println!(
        "  written: {}  pending: {}  error: {}",
        counts.written, counts.pending, counts.error
    );
    Ok(exit::SUCCESS)
}

fn compute_plan_for(
    conn: &mut Connection,
    check_run_id: i64,
    destination: &Path,
    quiet: bool,
    policy: &PasswordPolicy,
) -> Result<reconstruct::Plan, CliError> {
    let check_run = require_integrity_check_run(conn, check_run_id)?;
    let reference_set_id = check_run
        .reference_set_id
        .expect("an integrity check_run always has a reference_set_id");
    let mut scan_run_ids = repo::list_check_run_source_scan_run_ids(conn, check_run_id)?;

    if !destination.is_dir() {
        return Err(CliError::failure(format!(
            "再構成先フォルダが存在しません: {}",
            destination.display()
        )));
    }
    progress(
        quiet,
        &format!("scanning destination {} ...", destination.display()),
    );
    let destination_summary =
        scan::scan_folder_with_password_policy(conn, destination, now_millis(), policy)?;
    scan_run_ids.push(destination_summary.scan_run_id);

    progress(quiet, "computing fulfillment plan ...");
    let plan = reconstruct::compute_plan(
        conn,
        reference_set_id,
        &scan_run_ids,
        destination_summary.scan_run_id,
        policy,
        now_millis(),
    )?;
    Ok(plan)
}

fn print_plan(conn: &Connection, plan: &reconstruct::Plan) -> Result<(), CliError> {
    println!(
        "充当計画 (check_run: {})  解決済み: {}  未解決: {}",
        plan.check_run_id,
        plan.resolved.len(),
        plan.missing.len()
    );
    let required_media = plan.required_removable_media();
    if required_media.is_empty() {
        println!("  必要なリムーバブルメディア: なし");
    } else {
        println!("  必要なリムーバブルメディア:");
        for name in removable_media_names(conn, &required_media)? {
            println!("    - {name}");
        }
    }
    if !plan.missing.is_empty() {
        println!("  未解決の参照ファイル:");
        for m in &plan.missing {
            println!("    - {}", m.path);
        }
    }
    Ok(())
}

fn removable_media_names(conn: &Connection, media_ids: &[i64]) -> Result<Vec<String>, CliError> {
    media_ids
        .iter()
        .map(|&id| {
            let m = repo::get_removable_media(conn, id)?.ok_or_else(|| {
                CliError::failure(format!("removable_media が見つかりません: {id}"))
            })?;
            Ok(format!(
                "{} ({}={})",
                m.display_name.as_deref().unwrap_or("(no name)"),
                m.identifier_type,
                m.identifier_value
            ))
        })
        .collect()
}

/// Which of a reconstruction run's still-needed removable media (any item not yet
/// `written`, regardless of source) are connected right now, mapped to their current
/// mount path (§10.4 — there's no persisted mount path to reuse, it can change between
/// connections).
fn detect_connected_media_for_run(
    conn: &Connection,
    reconstruction_run_id: i64,
) -> Result<std::collections::HashMap<i64, PathBuf>, CliError> {
    let items = repo::list_reconstruction_items(conn, reconstruction_run_id, None)?;
    let mut media_ids: Vec<i64> = items
        .iter()
        .filter(|i| i.status != filechecker_core::db::ReconstructionItemStatus::Written)
        .filter_map(|i| i.source_removable_media_id)
        .collect();
    media_ids.sort_unstable();
    media_ids.dedup();
    if media_ids.is_empty() {
        return Ok(std::collections::HashMap::new());
    }

    let connected = media::platform_identifier()
        .list_connected()
        .map_err(|e| CliError::failure(format!("接続中メディアの一覧取得に失敗しました: {e}")))?;
    let mut result = std::collections::HashMap::new();
    for media_id in media_ids {
        let known = repo::get_removable_media(conn, media_id)?.ok_or_else(|| {
            CliError::failure(format!("removable_media が見つかりません: {media_id}"))
        })?;
        if let Some(detected) = connected.iter().find(|d| {
            d.identifier_type == known.identifier_type
                && d.identifier_value == known.identifier_value
        }) {
            result.insert(media_id, detected.mount_path.clone());
        }
    }
    Ok(result)
}

fn require_integrity_check_run(
    conn: &Connection,
    check_run_id: i64,
) -> Result<repo::CheckRunRow, CliError> {
    let check_run = repo::get_check_run(conn, check_run_id)?
        .ok_or_else(|| CliError::failure(format!("check_run が見つかりません: {check_run_id}")))?;
    if check_run.check_type != CheckType::Integrity {
        return Err(CliError::failure(format!(
            "check_run {check_run_id} は整合性チェックではありません（再構成には整合性チェックのcheck_runが必要です）"
        )));
    }
    Ok(check_run)
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
    policy: &PasswordPolicy,
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
            let summary =
                scan::scan_folder_with_password_policy(conn, &folder, now_millis(), policy)?;
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
