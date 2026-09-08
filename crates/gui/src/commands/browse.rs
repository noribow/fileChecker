//! 「スキャン状況」4ペイン画面（requirements.md §10.14拡張）: 整合性/重複チェックを
//! 実行しなくても、スキャンしただけのフォルダ・リムーバブルメディアの中身
//! （トップレベルのファイル一覧、および圧縮ファイルを選んだ場合はその内部一覧）を
//! そのまま閲覧できるようにするTauriコマンド群。大規模スキャン（§4: 数十万〜百万
//! ファイル）を想定し、全件を一度に返さずoffset/limitでページングする——フロント
//! エンドはこれを仮想スクロールで消費する（`docs/requirements.md`側の追記は
//! `docs/progress-log.md`のP15エントリ参照）。

use filechecker_core::db::repo;
use tauri::State;

use super::helpers::stringify;
use crate::state::AppState;

/// 指定`scan_run_id`のトップレベルエントリ（§10.5: どのアーカイブの内部にも
/// ネストしていないファイル。アーカイブ自身の行は含むが、その中身は含まない）を
/// 1ページ分返す。
#[tauri::command]
pub fn browse_top_level_files(
    state: State<AppState>,
    scan_run_id: i64,
    offset: i64,
    limit: i64,
) -> Result<Vec<repo::ScannedFileBrowseRow>, String> {
    let conn = state.conn.lock().expect("db mutex poisoned");
    repo::list_top_level_scanned_files_page(&conn, scan_run_id, offset, limit).map_err(stringify)
}

/// 指定`parent_archive_file_id`（`browse_top_level_files`が返したアーカイブ行の`id`）
/// の直下エントリを1ページ分返す。
#[tauri::command]
pub fn browse_archive_children(
    state: State<AppState>,
    parent_archive_file_id: i64,
    offset: i64,
    limit: i64,
) -> Result<Vec<repo::ScannedFileBrowseRow>, String> {
    let conn = state.conn.lock().expect("db mutex poisoned");
    repo::list_archive_children_page(&conn, parent_archive_file_id, offset, limit)
        .map_err(stringify)
}
