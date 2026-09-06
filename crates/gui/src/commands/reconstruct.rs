//! 再構成実行（充当計画画面・実行中画面・完了報告画面、§10.14/§10.20）。

use std::collections::HashMap;
use std::path::PathBuf;

use filechecker_core::db::{repo, CheckType};
use filechecker_core::reconstruct::{self, Plan};
use filechecker_core::{media, scan};
use serde::Serialize;
use tauri::State;

use super::helpers::{now_millis, stringify, with_password_policy};
use crate::state::AppState;

/// 再構成先の指定（§10.14「新規スキャン or 既存scan_runの再利用」）。
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DestinationSelector {
    pub path: Option<String>,
    pub existing_scan_run_id: Option<i64>,
}

fn resolve_destination(
    state: &AppState,
    conn: &mut filechecker_core::db::Connection,
    destination: &DestinationSelector,
) -> Result<(PathBuf, i64), String> {
    match (&destination.path, destination.existing_scan_run_id) {
        (Some(path), None) => {
            let dest = PathBuf::from(path);
            if !dest.is_dir() {
                return Err(format!("再構成先フォルダが存在しません: {path}"));
            }
            let summary = with_password_policy(state, conn, |conn, policy| {
                scan::scan_folder_with_password_policy(conn, &dest, now_millis(), policy)
            })?;
            Ok((dest, summary.scan_run_id))
        }
        (None, Some(scan_run_id)) => {
            let run = repo::get_scan_run(conn, scan_run_id)
                .map_err(stringify)?
                .ok_or_else(|| format!("scan_run が見つかりません: {scan_run_id}"))?;
            let folder = run.folder_path.ok_or_else(|| {
                "指定されたscan_runはフォルダのスキャンではありません".to_string()
            })?;
            Ok((PathBuf::from(folder), scan_run_id))
        }
        _ => Err("path または existing_scan_run_id のいずれか一方を指定してください".to_string()),
    }
}

fn require_integrity_check_run(
    conn: &filechecker_core::db::Connection,
    check_run_id: i64,
) -> Result<repo::CheckRunRow, String> {
    let run = repo::get_check_run(conn, check_run_id)
        .map_err(stringify)?
        .ok_or_else(|| format!("check_run が見つかりません: {check_run_id}"))?;
    if run.check_type != CheckType::Integrity {
        return Err(format!(
            "check_run {check_run_id} は整合性チェックではありません（再構成には整合性チェックのcheck_runが必要です）"
        ));
    }
    Ok(run)
}

fn compute_plan_for(
    state: &AppState,
    conn: &mut filechecker_core::db::Connection,
    check_run_id: i64,
    destination: &DestinationSelector,
) -> Result<Plan, String> {
    let check_run = require_integrity_check_run(conn, check_run_id)?;
    let reference_set_id = check_run
        .reference_set_id
        .expect("an integrity check_run always has a reference_set_id");
    let mut scan_run_ids =
        repo::list_check_run_source_scan_run_ids(conn, check_run_id).map_err(stringify)?;
    let (_, destination_scan_run_id) = resolve_destination(state, conn, destination)?;
    scan_run_ids.push(destination_scan_run_id);

    with_password_policy(state, conn, |conn, policy| {
        reconstruct::compute_plan(
            conn,
            reference_set_id,
            &scan_run_ids,
            destination_scan_run_id,
            policy,
            now_millis(),
        )
    })
}

/// 充当計画画面（§10.14）: 計画のみ算出する（新規`check_run`は作られるが
/// `reconstruction_run`はまだ作らない——「不足分を追加スキャン」で何度でも再算出できる）。
#[tauri::command]
pub fn reconstruct_plan(
    state: State<AppState>,
    check_run_id: i64,
    destination: DestinationSelector,
) -> Result<Plan, String> {
    let mut conn = state.conn.lock().expect("db mutex poisoned");
    compute_plan_for(&state, &mut conn, check_run_id, &destination)
}

/// 「この計画で再構成を実行」ボタン（§10.14）: 計画を再算出したうえで`reconstruction_run`
/// を作成する。以降の実際の読み書きは`reconstruct_run_pass`が担う。
#[tauri::command]
pub fn reconstruct_start(
    state: State<AppState>,
    check_run_id: i64,
    destination: DestinationSelector,
) -> Result<i64, String> {
    let mut conn = state.conn.lock().expect("db mutex poisoned");
    let plan = compute_plan_for(&state, &mut conn, check_run_id, &destination)?;
    let (destination_path, _) = resolve_destination(&state, &mut conn, &destination)?;
    reconstruct::create_run(
        &mut conn,
        plan.check_run_id,
        &destination_path.to_string_lossy(),
        &plan.resolved,
        now_millis(),
    )
    .map_err(stringify)
}

#[derive(Serialize)]
pub struct PassResult {
    pub written_count: usize,
    pub error_count: usize,
    /// まだ接続されていないリムーバブルメディア（表示名付き）。実行中画面（§10.14）の
    /// 「次のメディアの接続を促すダイアログ」に使う。
    pub still_needed_removable_media: Vec<MediaLabel>,
}

#[derive(Serialize)]
pub struct MediaLabel {
    pub id: i64,
    pub label: String,
}

fn media_label(m: &repo::RemovableMediaRow) -> String {
    format!(
        "{} ({}={})",
        m.display_name.as_deref().unwrap_or("(no name)"),
        m.identifier_type,
        m.identifier_value
    )
}

/// 実行中（メディア入れ替え）画面(§10.14)が1回の「実行」または「入れ替え完了」操作ごとに
/// 呼ぶ1パス分の処理。現在接続中のリムーバブルメディアはこのコマンド自身が検出するので、
/// フロントエンドは接続先を意識せず、単に結果（書き込み件数・エラー件数・まだ必要な
/// メディア一覧）を再描画すればよい。
#[tauri::command]
pub fn reconstruct_run_pass(
    state: State<AppState>,
    reconstruction_run_id: i64,
) -> Result<PassResult, String> {
    let mut conn = state.conn.lock().expect("db mutex poisoned");
    let run = repo::get_reconstruction_run(&conn, reconstruction_run_id)
        .map_err(stringify)?
        .ok_or_else(|| format!("reconstruction_run が見つかりません: {reconstruction_run_id}"))?;
    let destination_path = PathBuf::from(&run.destination_folder_path);

    let items =
        repo::list_reconstruction_items(&conn, reconstruction_run_id, None).map_err(stringify)?;
    let mut media_ids: Vec<i64> = items
        .iter()
        .filter(|i| i.status != filechecker_core::db::ReconstructionItemStatus::Written)
        .filter_map(|i| i.source_removable_media_id)
        .collect();
    media_ids.sort_unstable();
    media_ids.dedup();

    let mut connected_media = HashMap::new();
    if !media_ids.is_empty() {
        let connected = media::platform_identifier()
            .list_connected()
            .map_err(|e| format!("接続中メディアの一覧取得に失敗しました: {e}"))?;
        for media_id in &media_ids {
            let known = repo::get_removable_media(&conn, *media_id)
                .map_err(stringify)?
                .ok_or_else(|| format!("removable_media が見つかりません: {media_id}"))?;
            if let Some(detected) = connected.iter().find(|d| {
                d.identifier_type == known.identifier_type
                    && d.identifier_value == known.identifier_value
            }) {
                connected_media.insert(*media_id, detected.mount_path.clone());
            }
        }
    }

    let summary = with_password_policy(&state, &mut conn, |conn, policy| {
        reconstruct::run_pass(
            conn,
            reconstruction_run_id,
            &destination_path,
            policy,
            &connected_media,
            now_millis(),
        )
    })?;

    let mut still_needed = Vec::new();
    for id in &summary.still_needed_removable_media {
        let m = repo::get_removable_media(&conn, *id)
            .map_err(stringify)?
            .ok_or_else(|| format!("removable_media が見つかりません: {id}"))?;
        still_needed.push(MediaLabel {
            id: *id,
            label: media_label(&m),
        });
    }

    Ok(PassResult {
        written_count: summary.written_count,
        error_count: summary.error_count,
        still_needed_removable_media: still_needed,
    })
}

#[derive(Serialize)]
pub struct ReconstructionStatus {
    pub id: i64,
    pub status: filechecker_core::db::RunStatus,
    pub destination_folder_path: String,
    pub written: i64,
    pub pending: i64,
    pub error: i64,
}

/// 完了報告画面（§10.14）: 書き出し件数・未解決件数のサマリ。CSV/JSON/HTML出力は既存の
/// `report_export`（整合性チェックのcheck_run経由）を再利用する想定——再構成自体が
/// 記録するのは`reconstruction_item`だが、その元になった`integrity_check_result`
/// （§10.20の充当計画）は通常の整合性チェック結果としてすでに`check show`/`report
/// export`で参照できる。
#[tauri::command]
pub fn reconstruct_status(
    state: State<AppState>,
    reconstruction_run_id: i64,
) -> Result<ReconstructionStatus, String> {
    let conn = state.conn.lock().expect("db mutex poisoned");
    let run = repo::get_reconstruction_run(&conn, reconstruction_run_id)
        .map_err(stringify)?
        .ok_or_else(|| format!("reconstruction_run が見つかりません: {reconstruction_run_id}"))?;
    let counts =
        repo::count_reconstruction_items(&conn, reconstruction_run_id).map_err(stringify)?;
    Ok(ReconstructionStatus {
        id: run.id,
        status: run.status,
        destination_folder_path: run.destination_folder_path,
        written: counts.written,
        pending: counts.pending,
        error: counts.error,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use filechecker_core::db::HashMode;

    fn test_state() -> AppState {
        AppState {
            conn: std::sync::Mutex::new(filechecker_core::db::open_in_memory().unwrap()),
            password_store: std::sync::Mutex::new(None),
            password_store_path: PathBuf::from("/nonexistent/passwords.json"),
        }
    }

    #[test]
    fn resolve_destination_rejects_both_and_neither() {
        let state = test_state();
        let mut conn = filechecker_core::db::open_in_memory().unwrap();
        assert!(resolve_destination(
            &state,
            &mut conn,
            &DestinationSelector {
                path: None,
                existing_scan_run_id: None
            }
        )
        .is_err());
        assert!(resolve_destination(
            &state,
            &mut conn,
            &DestinationSelector {
                path: Some("/tmp".to_string()),
                existing_scan_run_id: Some(1)
            }
        )
        .is_err());
    }

    #[test]
    fn resolve_destination_rejects_a_nonexistent_folder_path() {
        let state = test_state();
        let mut conn = filechecker_core::db::open_in_memory().unwrap();
        let err = resolve_destination(
            &state,
            &mut conn,
            &DestinationSelector {
                path: Some("/no/such/directory/at/all".to_string()),
                existing_scan_run_id: None,
            },
        )
        .unwrap_err();
        assert!(err.contains("存在しません"));
    }

    #[test]
    fn resolve_destination_reuses_an_existing_folder_scan_run_without_rescanning() {
        let state = test_state();
        let mut conn = filechecker_core::db::open_in_memory().unwrap();
        let scan_run_id =
            repo::insert_scan_run_folder(&conn, "/some/folder", HashMode::Lazy, 1000).unwrap();

        let (path, resolved_id) = resolve_destination(
            &state,
            &mut conn,
            &DestinationSelector {
                path: None,
                existing_scan_run_id: Some(scan_run_id),
            },
        )
        .unwrap();
        assert_eq!(path, PathBuf::from("/some/folder"));
        assert_eq!(resolved_id, scan_run_id);
    }

    #[test]
    fn resolve_destination_rejects_a_removable_media_scan_run() {
        let state = test_state();
        let mut conn = filechecker_core::db::open_in_memory().unwrap();
        let media_id =
            repo::find_or_create_removable_media(&conn, "linux", "serial", "abc", None, 1000)
                .unwrap();
        let scan_run_id =
            repo::insert_scan_run_removable_media(&conn, media_id, HashMode::Eager, 1000).unwrap();

        let err = resolve_destination(
            &state,
            &mut conn,
            &DestinationSelector {
                path: None,
                existing_scan_run_id: Some(scan_run_id),
            },
        )
        .unwrap_err();
        assert!(err.contains("フォルダのスキャンではありません"));
    }
}
