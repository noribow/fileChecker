//! ホーム画面（§10.14）: 直近のcheck_run一覧、お手本セット数、登録済みリムーバブルメディア数。

use filechecker_core::db::{repo, CheckType};
use serde::Serialize;
use tauri::State;

use super::helpers::stringify;
use crate::state::AppState;

#[derive(Serialize)]
pub struct CheckRunSummary {
    pub id: i64,
    pub check_type: CheckType,
    pub status: filechecker_core::db::RunStatus,
    pub started_at: i64,
    pub completed_at: Option<i64>,
    /// 短い1行サマリ（整合性: "OK 12340 / 破損 3 / 欠落 1 / 余剰 5 / エラー 52"、
    /// 重複: "グループ 5件"）。結果一覧そのものは各画面で取得する。
    pub summary_text: String,
}

#[derive(Serialize)]
pub struct HomeSummary {
    pub recent_check_runs: Vec<CheckRunSummary>,
    pub reference_set_count: i64,
    pub removable_media_count: i64,
}

#[tauri::command]
pub fn home_summary(state: State<AppState>) -> Result<HomeSummary, String> {
    let conn = state.conn.lock().expect("db mutex poisoned");

    let runs = repo::list_check_runs(&conn, None, Some(10)).map_err(stringify)?;
    let mut recent_check_runs = Vec::with_capacity(runs.len());
    for r in runs {
        let summary_text = match r.check_type {
            CheckType::Integrity => {
                let rows = repo::list_integrity_results(&conn, r.id, None).map_err(stringify)?;
                let mut ok = 0;
                let mut corrupted = 0;
                let mut missing = 0;
                let mut extra = 0;
                let mut error = 0;
                for row in &rows {
                    match row.result_status {
                        filechecker_core::db::ResultStatus::Ok => ok += 1,
                        filechecker_core::db::ResultStatus::Corrupted => corrupted += 1,
                        filechecker_core::db::ResultStatus::Missing => missing += 1,
                        filechecker_core::db::ResultStatus::Extra => extra += 1,
                        filechecker_core::db::ResultStatus::Error => error += 1,
                    }
                }
                format!(
                    "OK {ok} / 破損 {corrupted} / 欠落 {missing} / 余剰 {extra} / エラー {error}"
                )
            }
            CheckType::Duplicate => {
                let groups = repo::list_duplicate_groups(&conn, r.id).map_err(stringify)?;
                format!("重複グループ {}件", groups.len())
            }
        };
        recent_check_runs.push(CheckRunSummary {
            id: r.id,
            check_type: r.check_type,
            status: r.status,
            started_at: r.started_at,
            completed_at: r.completed_at,
            summary_text,
        });
    }

    let reference_set_count = repo::list_reference_sets(&conn).map_err(stringify)?.len() as i64;
    let removable_media_count = repo::list_removable_media(&conn).map_err(stringify)?.len() as i64;

    Ok(HomeSummary {
        recent_check_runs,
        reference_set_count,
        removable_media_count,
    })
}
