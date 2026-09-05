//! リムーバブルメディア管理画面、および重複チェック対象設定の「リムーバブルメディアの
//! 追加」（§10.14）。

use std::path::PathBuf;

use filechecker_core::db::repo;
use filechecker_core::media::{self, DetectedMedia};
use filechecker_core::scan::ScanSummary;
use tauri::State;

use super::helpers::{now_millis, stringify, with_password_policy};
use crate::state::AppState;

/// 既知メディア一覧（`removable_media`、§10.14の「リムーバブルメディア管理」画面）。
#[tauri::command]
pub fn media_list(state: State<AppState>) -> Result<Vec<repo::RemovableMediaRow>, String> {
    let conn = state.conn.lock().expect("db mutex poisoned");
    repo::list_removable_media(&conn).map_err(stringify)
}

/// 現在接続中のリムーバブルメディア一覧（§10.14の対象設定「接続中メディア一覧から選択」）。
/// 既知メディアかどうか（未スキャンか、保存済みスキャン結果があるか）の判定はフロント
/// エンド側で`media_list`の結果と`identifier_type`/`identifier_value`を突き合わせる。
#[tauri::command]
pub fn media_connected() -> Result<Vec<DetectedMedia>, String> {
    media::platform_identifier()
        .list_connected()
        .map_err(|e| format!("接続中メディアの一覧取得に失敗しました: {e}"))
}

/// 特定のメディアIDの「今すぐスキャン」（該当メディアが接続中の場合のみGUI側でボタンを
/// 活性化する想定。接続確認自体はここで行う）。
#[tauri::command]
pub fn media_scan_by_id(state: State<AppState>, media_id: i64) -> Result<ScanSummary, String> {
    let mut conn = state.conn.lock().expect("db mutex poisoned");
    let known = repo::get_removable_media(&conn, media_id)
        .map_err(stringify)?
        .ok_or_else(|| format!("removable_media が見つかりません: {media_id}"))?;
    let connected = media::platform_identifier()
        .list_connected()
        .map_err(|e| format!("接続中メディアの一覧取得に失敗しました: {e}"))?;
    let detected = connected
        .iter()
        .find(|d| {
            d.identifier_type == known.identifier_type
                && d.identifier_value == known.identifier_value
        })
        .ok_or_else(|| format!("メディア {media_id} は現在接続されていません"))?
        .clone();
    run_scan_media(
        &state,
        &mut conn,
        &known.platform,
        &known.identifier_type,
        &known.identifier_value,
        detected.display_name,
        &detected.mount_path,
    )
}

/// 対象設定画面で未知のマウントパスを選んだ場合のスキャン。自動識別に失敗した場合は
/// §10.21の通りダイアログでラベル入力を受け付ける——GUIはネイティブダイアログを出せるので
/// CLIのTTYフォールバック（コード4）は不要で、フロントエンドが`manual_label`を渡す。
#[tauri::command]
pub fn media_scan_by_mount(
    state: State<AppState>,
    mount_path: String,
    manual_label: Option<String>,
) -> Result<ScanSummary, String> {
    let mount = PathBuf::from(&mount_path);
    if !mount.is_dir() {
        return Err(format!("マウントパスが存在しません: {mount_path}"));
    }
    let mut conn = state.conn.lock().expect("db mutex poisoned");
    let connected = media::platform_identifier()
        .list_connected()
        .map_err(|e| format!("接続中メディアの一覧取得に失敗しました: {e}"))?;
    if let Some(detected) = connected.iter().find(|d| d.mount_path == mount) {
        return run_scan_media(
            &state,
            &mut conn,
            media::current_platform(),
            &detected.identifier_type,
            &detected.identifier_value,
            detected.display_name.clone(),
            &mount,
        );
    }

    let label = manual_label
        .filter(|l| !l.trim().is_empty())
        .ok_or_else(|| {
            "識別子を自動取得できませんでした。このメディアのラベルを入力してください".to_string()
        })?;
    run_scan_media(
        &state,
        &mut conn,
        media::current_platform(),
        "user_defined",
        label.trim(),
        None,
        &mount,
    )
}

#[allow(clippy::too_many_arguments)]
fn run_scan_media(
    state: &AppState,
    conn: &mut filechecker_core::db::Connection,
    platform: &str,
    identifier_type: &str,
    identifier_value: &str,
    display_name: Option<String>,
    mount_path: &std::path::Path,
) -> Result<ScanSummary, String> {
    let now = now_millis();
    let media_id = repo::find_or_create_removable_media(
        conn,
        platform,
        identifier_type,
        identifier_value,
        display_name.as_deref(),
        now,
    )
    .map_err(stringify)?;
    with_password_policy(state, conn, |conn, policy| {
        filechecker_core::scan::scan_removable_media_with_password_policy(
            conn, media_id, mount_path, now, policy,
        )
    })
}
