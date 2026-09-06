//! スキャン履歴画面（§10.14）: 全scan_run（フォルダ・リムーバブルメディア双方）の横断一覧。
//! 新規フォルダスキャン単体の実行もここに置く（結果一覧・お手本セット作成画面いずれからも
//! 呼ばれる共通操作のため）。

use std::path::PathBuf;

use filechecker_core::db::repo;
use filechecker_core::scan::ScanSummary;
use tauri::State;

use super::helpers::{now_millis, stringify, with_password_policy};
use crate::state::AppState;

/// フォルダの新規スキャン（§10.3の情報取得フェーズ）。整合性チェック実行設定・重複
/// チェック対象設定・お手本セット作成のいずれの画面からも、対象フォルダ選択後にこれを
/// 呼んで`scan_run_id`を得てから後続の操作に渡す。
#[tauri::command]
pub fn scan_folder(state: State<AppState>, folder_path: String) -> Result<ScanSummary, String> {
    let path = PathBuf::from(&folder_path);
    if !path.is_dir() {
        return Err(format!("対象フォルダが存在しません: {folder_path}"));
    }
    let mut conn = state.conn.lock().expect("db mutex poisoned");
    with_password_policy(&state, &mut conn, |conn, policy| {
        filechecker_core::scan::scan_folder_with_password_policy(conn, &path, now_millis(), policy)
    })
}

#[tauri::command]
pub fn scan_history_list(
    state: State<AppState>,
    limit: Option<i64>,
) -> Result<Vec<repo::ScanRunSummaryRow>, String> {
    let conn = state.conn.lock().expect("db mutex poisoned");
    repo::list_scan_runs(&conn, limit).map_err(stringify)
}
