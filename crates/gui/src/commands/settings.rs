//! 設定画面: 全般（§10.6のアーカイブ深度・サイズ上限・パスワード方針）、登録パスワード
//! 管理（§10.9）、マスターパスワード関連モーダル群（§10.10）。

use filechecker_core::archive::{ArchiveConfig, ArchiveFormat};
use filechecker_core::db::repo;
use filechecker_core::secrets::{RegisteredPassword, SecretsError, UnlockedStore};
use serde::Serialize;
use tauri::State;

use super::helpers::stringify;
use crate::state::AppState;

// ---- 全般設定 --------------------------------------------------------------------------

#[derive(Serialize)]
pub struct GeneralSettings {
    pub archive_max_depth: i64,
    pub archive_entry_size_limit_bytes: u64,
    /// "reject" | "try_registered"（§10.7のモード1/モード2）。
    pub archive_password_mode: String,
}

#[tauri::command]
pub fn settings_get_general(state: State<AppState>) -> Result<GeneralSettings, String> {
    let conn = state.conn.lock().expect("db mutex poisoned");
    let archive = ArchiveConfig::from_settings(&conn).map_err(stringify)?;
    let archive_password_mode = repo::get_app_setting(&conn, "archive_password_mode")
        .map_err(stringify)?
        .unwrap_or_else(|| "reject".to_string());
    Ok(GeneralSettings {
        archive_max_depth: archive.max_depth,
        archive_entry_size_limit_bytes: archive.entry_size_limit,
        archive_password_mode,
    })
}

#[tauri::command]
pub fn settings_set_general(
    state: State<AppState>,
    archive_max_depth: i64,
    archive_entry_size_limit_bytes: u64,
    archive_password_mode: String,
) -> Result<(), String> {
    if archive_password_mode != "reject" && archive_password_mode != "try_registered" {
        return Err("archive_password_mode は reject|try_registered のいずれかです".to_string());
    }
    let conn = state.conn.lock().expect("db mutex poisoned");
    repo::set_app_setting(&conn, "archive_max_depth", &archive_max_depth.to_string())
        .map_err(stringify)?;
    repo::set_app_setting(
        &conn,
        "archive_entry_size_limit_bytes",
        &archive_entry_size_limit_bytes.to_string(),
    )
    .map_err(stringify)?;
    repo::set_app_setting(&conn, "archive_password_mode", &archive_password_mode)
        .map_err(stringify)?;
    Ok(())
}

// ---- 登録パスワード管理（§10.9）/ マスターパスワード（§10.10） ------------------------

#[derive(Serialize)]
pub struct PasswordStoreStatus {
    pub exists: bool,
    pub unlocked: bool,
}

#[tauri::command]
pub fn password_store_status(state: State<AppState>) -> Result<PasswordStoreStatus, String> {
    let unlocked = state
        .password_store
        .lock()
        .expect("password_store mutex poisoned")
        .is_some();
    Ok(PasswordStoreStatus {
        exists: state.password_store_path.exists(),
        unlocked,
    })
}

fn secrets_error_message(e: SecretsError) -> String {
    e.to_string()
}

/// マスターパスワードの初回設定モーダル（§10.10）: 設定ファイルが未作成の場合のみ。
#[tauri::command]
pub fn password_store_create(
    state: State<AppState>,
    master_password: String,
) -> Result<(), String> {
    let store = UnlockedStore::create(&state.password_store_path, &master_password)
        .map_err(secrets_error_message)?;
    *state
        .password_store
        .lock()
        .expect("password_store mutex poisoned") = Some(store);
    Ok(())
}

/// マスターパスワードの入力モーダル（§10.10、都度）: 既存の設定ファイルをロック解除する。
/// 解除した鍵はメモリ上にのみ保持され（`AppState::password_store`）、`password_store_lock`
/// を呼ぶかアプリを終了するまで保持される。
#[tauri::command]
pub fn password_store_unlock(
    state: State<AppState>,
    master_password: String,
) -> Result<(), String> {
    let store = UnlockedStore::unlock(&state.password_store_path, &master_password)
        .map_err(secrets_error_message)?;
    *state
        .password_store
        .lock()
        .expect("password_store mutex poisoned") = Some(store);
    Ok(())
}

/// メモリ上の鍵を破棄する（ファイル自体は残す）。
#[tauri::command]
pub fn password_store_lock(state: State<AppState>) -> Result<(), String> {
    *state
        .password_store
        .lock()
        .expect("password_store mutex poisoned") = None;
    Ok(())
}

fn require_unlocked<'a>(
    guard: &'a mut std::sync::MutexGuard<'_, Option<UnlockedStore>>,
) -> Result<&'a mut UnlockedStore, String> {
    guard.as_mut().ok_or_else(|| {
        "登録パスワード設定ファイルがロックされています。先にマスターパスワードを入力してください"
            .to_string()
    })
}

#[derive(Serialize)]
pub struct RegisteredPasswordDto {
    pub id: String,
    /// `None`は「全形式共通」（§10.9の一括設定）。
    pub format: Option<ArchiveFormat>,
    pub password: String,
}

impl From<&RegisteredPassword> for RegisteredPasswordDto {
    fn from(p: &RegisteredPassword) -> Self {
        RegisteredPasswordDto {
            id: p.id.clone(),
            format: p.format,
            password: p.password.clone(),
        }
    }
}

/// 登録パスワード管理画面の一覧（§10.9）。ロック中はエラーを返す——呼び出し側の設定
/// 画面は`password_store_status`で先に確認し、未解除ならマスターパスワード入力モーダルを
/// 先に出す想定。
#[tauri::command]
pub fn password_list(state: State<AppState>) -> Result<Vec<RegisteredPasswordDto>, String> {
    let mut guard = state
        .password_store
        .lock()
        .expect("password_store mutex poisoned");
    let store = require_unlocked(&mut guard)?;
    Ok(store
        .list()
        .iter()
        .map(RegisteredPasswordDto::from)
        .collect())
}

#[tauri::command]
pub fn password_add(
    state: State<AppState>,
    format: Option<String>,
    password: String,
) -> Result<String, String> {
    let format = format
        .map(|f| ArchiveFormat::parse_str(&f).ok_or_else(|| format!("不明な形式: {f}")))
        .transpose()?;
    let mut guard = state
        .password_store
        .lock()
        .expect("password_store mutex poisoned");
    let store = require_unlocked(&mut guard)?;
    let id = store.add(format, password);
    store.save().map_err(secrets_error_message)?;
    Ok(id)
}

#[tauri::command]
pub fn password_remove(state: State<AppState>, id: String) -> Result<bool, String> {
    let mut guard = state
        .password_store
        .lock()
        .expect("password_store mutex poisoned");
    let store = require_unlocked(&mut guard)?;
    let removed = store.remove(&id);
    if removed {
        store.save().map_err(secrets_error_message)?;
    }
    Ok(removed)
}

/// マスターパスワード変更モーダル（現在→新規、§10.10）。「現在」を再入力させて
/// `UnlockedStore::unlock`で独立に検証してから切り替える（メモリ上に既に解除済みの
/// ステートがあっても、変更操作自体は毎回現在のパスワードを再確認する）。
#[tauri::command]
pub fn master_password_change(
    state: State<AppState>,
    current_master_password: String,
    new_master_password: String,
) -> Result<(), String> {
    UnlockedStore::unlock(&state.password_store_path, &current_master_password)
        .map_err(secrets_error_message)?;
    let mut guard = state
        .password_store
        .lock()
        .expect("password_store mutex poisoned");
    let store = require_unlocked(&mut guard)?;
    store
        .change_master_password(&new_master_password)
        .map_err(secrets_error_message)
}

/// リセットモーダル（§10.10）: 登録パスワード設定ファイルの破棄を伴う。呼び出し前に
/// フロントエンドで警告文言付きの確認を取ること。
#[tauri::command]
pub fn master_password_reset(state: State<AppState>) -> Result<(), String> {
    UnlockedStore::reset(&state.password_store_path).map_err(secrets_error_message)?;
    *state
        .password_store
        .lock()
        .expect("password_store mutex poisoned") = None;
    Ok(())
}
