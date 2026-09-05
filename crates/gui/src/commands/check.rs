//! 整合性チェック・重複チェックの実行設定/結果一覧画面、および両方に共通する
//! スキャン履歴一覧・レポート出力（§10.14）。

use std::io::Write as _;
use std::path::PathBuf;

use filechecker_core::db::{repo, CheckType, Connection, ResultStatus};
use filechecker_core::{duplicate, integrity};
use serde::Serialize;
use tauri::State;

use super::helpers::{hex_encode, now_millis, stringify, with_password_policy};
use crate::state::AppState;

// ---- integrity ---------------------------------------------------------------------

#[derive(Serialize)]
pub struct IntegrityCounts {
    pub ok: usize,
    pub corrupted: usize,
    pub missing: usize,
    pub extra: usize,
    pub error: usize,
}

impl From<integrity::IntegrityCheckSummary> for IntegrityCounts {
    fn from(s: integrity::IntegrityCheckSummary) -> Self {
        IntegrityCounts {
            ok: s.ok_count,
            corrupted: s.corrupted_count,
            missing: s.missing_count,
            extra: s.extra_count,
            error: s.error_count,
        }
    }
}

#[derive(Serialize)]
pub struct IntegrityRunResult {
    pub check_run_id: i64,
    pub reference_set_name: String,
    pub reference_set_version: u32,
    pub counts: IntegrityCounts,
}

/// 整合性チェック実行設定 → 実行（§10.14）。`folder_path`（新規スキャン）と
/// `scan_run_ids`（スキャン履歴からの再利用）はどちらか一方のみ指定する。
#[tauri::command]
pub fn integrity_run(
    state: State<AppState>,
    reference_set_id: i64,
    folder_path: Option<String>,
    scan_run_ids: Vec<i64>,
) -> Result<IntegrityRunResult, String> {
    let mut conn = state.conn.lock().expect("db mutex poisoned");
    let reference_set = repo::get_reference_set(&conn, reference_set_id)
        .map_err(stringify)?
        .ok_or_else(|| format!("お手本セットが見つかりません: {reference_set_id}"))?;

    let scan_run_ids = resolve_scan_run_ids(&state, &mut conn, folder_path, scan_run_ids)?;

    let summary = with_password_policy(&state, &mut conn, |conn, policy| {
        integrity::run_integrity_check_with_password_policy(
            conn,
            reference_set_id,
            &scan_run_ids,
            now_millis(),
            policy,
        )
    })?;
    let reference_set_version =
        repo::reference_set_version(&conn, reference_set_id).map_err(stringify)?;

    Ok(IntegrityRunResult {
        check_run_id: summary.check_run_id,
        reference_set_name: reference_set.name,
        reference_set_version,
        counts: summary.into(),
    })
}

#[derive(Serialize)]
pub struct IntegrityResultDto {
    pub id: i64,
    pub result_status: ResultStatus,
    pub path: String,
    pub size: Option<i64>,
    pub detail: Option<String>,
    /// パスに`/`を含む場合（アーカイブ内エントリ、§10.14の`親アーカイブ名 > 内部相対パス`
    /// 表示）の折り返しなしのネスト段数。フロントエンドの📦表示に使う。
    pub archive_depth: usize,
}

/// 整合性チェック結果一覧（§10.14）。`status_filter`未指定時はCLIと同じ既定（ok以外全件）。
#[tauri::command]
pub fn integrity_results(
    state: State<AppState>,
    check_run_id: i64,
    status_filter: Option<Vec<String>>,
) -> Result<Vec<IntegrityResultDto>, String> {
    let conn = state.conn.lock().expect("db mutex poisoned");
    let rows = repo::list_integrity_results(&conn, check_run_id, None).map_err(stringify)?;
    let selected: Vec<ResultStatus> = match status_filter {
        Some(names) => names
            .iter()
            .map(|s| ResultStatus::parse_str(s).ok_or_else(|| format!("不明なstatus: {s}")))
            .collect::<Result<_, _>>()?,
        None => vec![
            ResultStatus::Corrupted,
            ResultStatus::Missing,
            ResultStatus::Extra,
            ResultStatus::Error,
        ],
    };
    Ok(rows
        .into_iter()
        .filter(|r| selected.contains(&r.result_status))
        .map(|r| {
            let archive_depth = r.path.matches('/').count();
            IntegrityResultDto {
                id: r.id,
                result_status: r.result_status,
                path: r.path,
                size: r.size,
                detail: r.detail,
                archive_depth,
            }
        })
        .collect())
}

/// 整合性チェック結果一覧の件数バッジ（§10.14）。ステータスフィルタの影響を受けず
/// 常に全件を反映する。
#[tauri::command]
pub fn integrity_counts(
    state: State<AppState>,
    check_run_id: i64,
) -> Result<IntegrityCounts, String> {
    let conn = state.conn.lock().expect("db mutex poisoned");
    let rows = repo::list_integrity_results(&conn, check_run_id, None).map_err(stringify)?;
    let mut counts = IntegrityCounts {
        ok: 0,
        corrupted: 0,
        missing: 0,
        extra: 0,
        error: 0,
    };
    for r in &rows {
        match r.result_status {
            ResultStatus::Ok => counts.ok += 1,
            ResultStatus::Corrupted => counts.corrupted += 1,
            ResultStatus::Missing => counts.missing += 1,
            ResultStatus::Extra => counts.extra += 1,
            ResultStatus::Error => counts.error += 1,
        }
    }
    Ok(counts)
}

// ---- duplicate ------------------------------------------------------------------------

#[derive(Serialize)]
pub struct DuplicateGroupDto {
    pub id: i64,
    pub sha256_hex: String,
    pub size: i64,
    pub member_count: i64,
    pub members: Vec<DuplicateMemberDto>,
}

#[derive(Serialize)]
pub struct DuplicateMemberDto {
    pub scanned_file_id: i64,
    pub path: String,
    pub scan_run_id: i64,
}

#[derive(Serialize)]
pub struct DuplicateRunResult {
    pub check_run_id: i64,
    pub group_count: usize,
    pub duplicate_file_count: usize,
    pub reclaimable_bytes: i64,
    pub error_count: usize,
}

/// 重複チェック対象設定 → 実行（§10.14）。`folder_paths`は新規スキャン、`scan_run_ids`は
/// スキャン履歴・リムーバブルメディアの保存済みスキャン結果の再利用で、両方を混在できる。
#[tauri::command]
pub fn duplicate_run(
    state: State<AppState>,
    folder_paths: Vec<String>,
    scan_run_ids: Vec<i64>,
) -> Result<DuplicateRunResult, String> {
    if folder_paths.is_empty() && scan_run_ids.is_empty() {
        return Err("対象フォルダまたはscan_runを1つ以上指定してください".to_string());
    }
    let mut conn = state.conn.lock().expect("db mutex poisoned");

    let mut ids = Vec::new();
    for folder in &folder_paths {
        let path = PathBuf::from(folder);
        if !path.is_dir() {
            return Err(format!("対象フォルダが存在しません: {folder}"));
        }
        let summary = with_password_policy(&state, &mut conn, |conn, policy| {
            filechecker_core::scan::scan_folder_with_password_policy(
                conn,
                &path,
                now_millis(),
                policy,
            )
        })?;
        ids.push(summary.scan_run_id);
    }
    for id in scan_run_ids {
        repo::get_scan_run(&conn, id)
            .map_err(stringify)?
            .ok_or_else(|| format!("scan_run が見つかりません: {id}"))?;
        ids.push(id);
    }

    let summary = with_password_policy(&state, &mut conn, |conn, policy| {
        duplicate::run_duplicate_check_with_password_policy(conn, &ids, now_millis(), policy)
    })?;

    let groups = repo::list_duplicate_groups(&conn, summary.check_run_id).map_err(stringify)?;
    let reclaimable_bytes: i64 = groups
        .iter()
        .map(|g| g.size * g.member_count.saturating_sub(1))
        .sum();

    Ok(DuplicateRunResult {
        check_run_id: summary.check_run_id,
        group_count: summary.group_count,
        duplicate_file_count: summary.duplicate_file_count,
        reclaimable_bytes,
        error_count: summary.error_count,
    })
}

/// 重複チェック結果一覧（§10.14）。グループ展開でメンバーのフルパス・所属scan_runを表示。
#[tauri::command]
pub fn duplicate_groups(
    state: State<AppState>,
    check_run_id: i64,
) -> Result<Vec<DuplicateGroupDto>, String> {
    let conn = state.conn.lock().expect("db mutex poisoned");
    let groups = repo::list_duplicate_groups(&conn, check_run_id).map_err(stringify)?;
    groups
        .into_iter()
        .map(|g| {
            let members = repo::list_duplicate_group_members(&conn, g.id)
                .map_err(stringify)?
                .into_iter()
                .map(|m| DuplicateMemberDto {
                    scanned_file_id: m.scanned_file_id,
                    path: m.path,
                    scan_run_id: m.scan_run_id,
                })
                .collect();
            Ok(DuplicateGroupDto {
                id: g.id,
                sha256_hex: hex_encode(&g.sha256),
                size: g.size,
                member_count: g.member_count,
                members,
            })
        })
        .collect()
}

// ---- check list / export -------------------------------------------------------------

/// 「実行中」画面(§10.14)から遷移する結果画面が使う、過去`check_run`の横断一覧
/// （ホーム画面の直近一覧より詳細な絞り込みができるよう、typeとlimitを受け取る）。
#[tauri::command]
pub fn check_list(
    state: State<AppState>,
    check_type: Option<String>,
    limit: Option<i64>,
) -> Result<Vec<repo::CheckRunRow>, String> {
    let conn = state.conn.lock().expect("db mutex poisoned");
    let check_type = match check_type.as_deref() {
        None => None,
        Some("integrity") => Some(CheckType::Integrity),
        Some("duplicate") => Some(CheckType::Duplicate),
        Some(other) => return Err(format!("不明なcheck_type: {other}")),
    };
    repo::list_check_runs(&conn, check_type, limit).map_err(stringify)
}

/// 結果一覧のCSV/JSON出力（§10.14）。HTML出力はP13で追加予定（CLIの`report export`と
/// 同じ制限、`docs/progress-log.md`のP6エントリを参照）。
#[tauri::command]
pub fn report_export(
    state: State<AppState>,
    check_run_id: i64,
    format: String,
    output_path: String,
) -> Result<(), String> {
    if format != "csv" && format != "json" {
        return Err("format は csv|json のみ対応します".to_string());
    }
    let conn = state.conn.lock().expect("db mutex poisoned");
    let run = repo::get_check_run(&conn, check_run_id)
        .map_err(stringify)?
        .ok_or_else(|| format!("check_run が見つかりません: {check_run_id}"))?;

    let text = match run.check_type {
        CheckType::Integrity => {
            let rows =
                repo::list_integrity_results(&conn, check_run_id, None).map_err(stringify)?;
            render_integrity_export(&format, &rows)
        }
        CheckType::Duplicate => {
            let groups = repo::list_duplicate_groups(&conn, check_run_id).map_err(stringify)?;
            let mut with_members = Vec::new();
            for g in groups {
                let members = repo::list_duplicate_group_members(&conn, g.id).map_err(stringify)?;
                with_members.push((g, members));
            }
            render_duplicate_export(&format, &with_members)
        }
    };

    let mut file = std::fs::File::create(&output_path)
        .map_err(|e| format!("出力ファイルを作成できません ({output_path}): {e}"))?;
    file.write_all(text.as_bytes())
        .map_err(|e| format!("出力ファイルへの書き込みに失敗しました: {e}"))
}

fn csv_field(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

fn render_integrity_export(format: &str, rows: &[repo::IntegrityResultRow]) -> String {
    if format == "json" {
        return serde_json::to_string_pretty(rows).unwrap_or_default();
    }
    let mut out = String::from("path,size,status,detail\n");
    for r in rows {
        out.push_str(&csv_field(&r.path));
        out.push(',');
        out.push_str(&r.size.map(|s| s.to_string()).unwrap_or_default());
        out.push(',');
        out.push_str(r.result_status.as_str());
        out.push(',');
        out.push_str(&csv_field(r.detail.as_deref().unwrap_or("")));
        out.push('\n');
    }
    out
}

fn render_duplicate_export(
    format: &str,
    groups: &[(repo::DuplicateGroupRow, Vec<repo::DuplicateGroupMemberRow>)],
) -> String {
    if format == "json" {
        #[derive(Serialize)]
        struct GroupOut<'a> {
            sha256: String,
            size: i64,
            member_count: i64,
            members: &'a [repo::DuplicateGroupMemberRow],
        }
        let out: Vec<GroupOut> = groups
            .iter()
            .map(|(g, members)| GroupOut {
                sha256: hex_encode(&g.sha256),
                size: g.size,
                member_count: g.member_count,
                members,
            })
            .collect();
        return serde_json::to_string_pretty(&out).unwrap_or_default();
    }
    let mut out = String::from("group_sha256,size,path,scan_run_id\n");
    for (g, members) in groups {
        let sha = hex_encode(&g.sha256);
        for m in members {
            out.push_str(&sha);
            out.push(',');
            out.push_str(&g.size.to_string());
            out.push(',');
            out.push_str(&csv_field(&m.path));
            out.push(',');
            out.push_str(&m.scan_run_id.to_string());
            out.push('\n');
        }
    }
    out
}

/// `check integrity`の`--folder`(新規スキャン)/`--scan-run`(再利用)相当。
pub(super) fn resolve_scan_run_ids(
    state: &AppState,
    conn: &mut Connection,
    folder_path: Option<String>,
    scan_run_ids: Vec<i64>,
) -> Result<Vec<i64>, String> {
    match (folder_path, scan_run_ids.is_empty()) {
        (Some(_), false) => Err("folder_path と scan_run_ids は同時に指定できません".to_string()),
        (None, true) => {
            Err("folder_path または scan_run_ids のいずれかを指定してください".to_string())
        }
        (Some(folder), true) => {
            let path = PathBuf::from(&folder);
            if !path.is_dir() {
                return Err(format!("対象フォルダが存在しません: {folder}"));
            }
            let summary = with_password_policy(state, conn, |conn, policy| {
                filechecker_core::scan::scan_folder_with_password_policy(
                    conn,
                    &path,
                    now_millis(),
                    policy,
                )
            })?;
            Ok(vec![summary.scan_run_id])
        }
        (None, false) => {
            for id in &scan_run_ids {
                repo::get_scan_run(conn, *id)
                    .map_err(stringify)?
                    .ok_or_else(|| format!("scan_run が見つかりません: {id}"))?;
            }
            Ok(scan_run_ids)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use filechecker_core::db::ResultStatus;

    fn test_state() -> AppState {
        AppState {
            conn: std::sync::Mutex::new(filechecker_core::db::open_in_memory().unwrap()),
            password_store: std::sync::Mutex::new(None),
            password_store_path: PathBuf::from("/nonexistent/passwords.json"),
        }
    }

    fn row(path: &str, status: ResultStatus, detail: Option<&str>) -> repo::IntegrityResultRow {
        repo::IntegrityResultRow {
            id: 1,
            result_status: status,
            scanned_file_id: Some(1),
            scanned_file_path: Some(path.to_string()),
            detail: detail.map(|s| s.to_string()),
            path: path.to_string(),
            size: Some(42),
        }
    }

    #[test]
    fn integrity_export_csv_quotes_commas_and_reports_missing_size_as_empty() {
        let rows = vec![
            row("a,b.jpg", ResultStatus::Corrupted, Some("SHA256不一致")),
            repo::IntegrityResultRow {
                size: None,
                ..row("c.jpg", ResultStatus::Missing, None)
            },
        ];
        let csv = render_integrity_export("csv", &rows);
        let mut lines = csv.lines();
        assert_eq!(lines.next().unwrap(), "path,size,status,detail");
        assert_eq!(
            lines.next().unwrap(),
            "\"a,b.jpg\",42,corrupted,SHA256不一致"
        );
        assert_eq!(lines.next().unwrap(), "c.jpg,,missing,");
    }

    #[test]
    fn integrity_export_json_round_trips_through_serde() {
        let rows = vec![row("a.jpg", ResultStatus::Ok, None)];
        let json = render_integrity_export("json", &rows);
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed[0]["path"], "a.jpg");
        assert_eq!(parsed[0]["result_status"], "ok");
    }

    #[test]
    fn duplicate_export_csv_has_one_row_per_member_sharing_the_group_hash() {
        let group = repo::DuplicateGroupRow {
            id: 1,
            sha256: vec![0xab, 0xcd],
            size: 100,
            member_count: 2,
        };
        let members = vec![
            repo::DuplicateGroupMemberRow {
                scanned_file_id: 1,
                path: "a.jpg".to_string(),
                scan_run_id: 10,
            },
            repo::DuplicateGroupMemberRow {
                scanned_file_id: 2,
                path: "b.jpg".to_string(),
                scan_run_id: 20,
            },
        ];
        let csv = render_duplicate_export("csv", &[(group, members)]);
        let lines: Vec<&str> = csv.lines().collect();
        assert_eq!(lines[0], "group_sha256,size,path,scan_run_id");
        assert_eq!(lines[1], "abcd,100,a.jpg,10");
        assert_eq!(lines[2], "abcd,100,b.jpg,20");
    }

    #[test]
    fn resolve_scan_run_ids_rejects_both_or_neither() {
        let state = test_state();
        let mut conn = filechecker_core::db::open_in_memory().unwrap();
        assert!(resolve_scan_run_ids(&state, &mut conn, None, vec![]).is_err());
        assert!(
            resolve_scan_run_ids(&state, &mut conn, Some("/tmp".to_string()), vec![1]).is_err()
        );
    }

    #[test]
    fn resolve_scan_run_ids_rejects_a_scan_run_that_does_not_exist() {
        let state = test_state();
        let mut conn = filechecker_core::db::open_in_memory().unwrap();
        let err = resolve_scan_run_ids(&state, &mut conn, None, vec![999]).unwrap_err();
        assert!(err.contains("999"));
    }
}
