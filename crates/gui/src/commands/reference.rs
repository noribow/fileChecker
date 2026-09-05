//! お手本セット一覧・お手本セット作成画面（§10.14）。

use std::path::PathBuf;

use filechecker_core::db::repo;
use filechecker_core::import::{self, ImportOptions, MameFormat, MergeMode};
use filechecker_core::reference;
use serde::Serialize;
use tauri::State;

use super::helpers::{now_millis, stringify, with_password_policy};
use crate::state::AppState;

#[derive(Serialize)]
pub struct ReferenceSetSummary {
    pub id: i64,
    pub name: String,
    pub source_format: String,
    pub generated_from_scan_run_id: Option<i64>,
    pub supersedes_reference_set_id: Option<i64>,
    pub created_at: i64,
    pub version: u32,
}

/// `reference list`のGUI版（§10.14「お手本セット一覧」）。バージョン履歴のグルーピング
/// （`name`単位、`supersedes_reference_set_id`の連鎖）はフロントエンド側で行う——各行が
/// 自分の`version`番号を持っているので、`name`でグループ化して`version`降順に並べれば
/// 「最新版を先頭に、過去版は折りたたみ」の表示になる。
#[tauri::command]
pub fn reference_list(state: State<AppState>) -> Result<Vec<ReferenceSetSummary>, String> {
    let conn = state.conn.lock().expect("db mutex poisoned");
    let sets = repo::list_reference_sets(&conn).map_err(stringify)?;
    sets.into_iter()
        .map(|s| {
            let version = repo::reference_set_version(&conn, s.id).map_err(stringify)?;
            Ok(ReferenceSetSummary {
                id: s.id,
                name: s.name,
                source_format: s.source_format,
                generated_from_scan_run_id: s.generated_from_scan_run_id,
                supersedes_reference_set_id: s.supersedes_reference_set_id,
                created_at: s.created_at,
                version,
            })
        })
        .collect()
}

/// お手本セット作成画面、タブ「フォルダをスキャンして生成」（§10.14）: 対象フォルダを
/// 新規スキャンしてから生成する。既存`scan_run`の再利用は`reference_generate_from_scan`。
#[tauri::command]
pub fn reference_generate_from_folder(
    state: State<AppState>,
    folder_path: String,
    name: String,
    supersede: Option<i64>,
) -> Result<reference::GenerateReferenceSetSummary, String> {
    let path = PathBuf::from(&folder_path);
    if !path.is_dir() {
        return Err(format!("対象フォルダが存在しません: {folder_path}"));
    }
    let mut conn = state.conn.lock().expect("db mutex poisoned");
    let scan_summary = with_password_policy(&state, &mut conn, |conn, policy| {
        filechecker_core::scan::scan_folder_with_password_policy(conn, &path, now_millis(), policy)
    })?;
    reference_generate_from_scan_inner(&state, &mut conn, scan_summary.scan_run_id, name, supersede)
}

/// お手本セット作成画面、「スキャン履歴」からの再利用、および結果一覧の
/// 「再スキャンして新バージョン作成」（新しいscan_runは呼び出し前に別途作成済み）。
#[tauri::command]
pub fn reference_generate_from_scan(
    state: State<AppState>,
    scan_run_id: i64,
    name: String,
    supersede: Option<i64>,
) -> Result<reference::GenerateReferenceSetSummary, String> {
    let mut conn = state.conn.lock().expect("db mutex poisoned");
    reference_generate_from_scan_inner(&state, &mut conn, scan_run_id, name, supersede)
}

fn reference_generate_from_scan_inner(
    state: &AppState,
    conn: &mut filechecker_core::db::Connection,
    scan_run_id: i64,
    name: String,
    supersede: Option<i64>,
) -> Result<reference::GenerateReferenceSetSummary, String> {
    repo::get_scan_run(conn, scan_run_id)
        .map_err(stringify)?
        .ok_or_else(|| format!("scan_run が見つかりません: {scan_run_id}"))?;
    if let Some(id) = supersede {
        repo::get_reference_set(conn, id)
            .map_err(stringify)?
            .ok_or_else(|| format!("お手本セットが見つかりません: {id}"))?;
    }
    with_password_policy(state, conn, |conn, policy| {
        reference::generate_reference_set_from_scan_run_with_password_policy(
            conn,
            scan_run_id,
            &name,
            supersede,
            now_millis(),
            policy,
        )
    })
}

/// お手本セット作成画面、タブ「外部ファイルをインポート」（§10.14/§10.18）。現状MAME形式
/// （`mame-softwarelist`/`mame-machinelist`）のみ対応。
#[tauri::command]
pub fn reference_import_mame(
    state: State<AppState>,
    file_path: String,
    format: String,
    name: String,
    merge_mode: Option<String>,
    include_baddump: bool,
) -> Result<import::ImportSummary, String> {
    let mame_format = MameFormat::parse_str(&format)
        .ok_or_else(|| format!("不明な形式: {format} (mame-softwarelist|mame-machinelist)"))?;
    let merge_mode = match mame_format {
        MameFormat::MachineList => match merge_mode.as_deref() {
            Some("merged") => MergeMode::Merged,
            Some("split") => MergeMode::Split,
            Some(other) => return Err(format!("不明なmerge_mode: {other} (merged|split)")),
            None => {
                return Err(
                    "mame-machinelistの取り込みにはmerge_mode（merged|split）の指定が必要です"
                        .to_string(),
                )
            }
        },
        MameFormat::SoftwareList => MergeMode::Split,
    };
    let path = PathBuf::from(&file_path);
    if !path.is_file() {
        return Err(format!("入力ファイルが存在しません: {file_path}"));
    }
    let options = ImportOptions {
        include_baddump,
        merge_mode,
    };
    let mut conn = state.conn.lock().expect("db mutex poisoned");
    import::import_mame_reference_set(&mut conn, mame_format, &path, &name, &options, now_millis())
        .map_err(stringify)
}
