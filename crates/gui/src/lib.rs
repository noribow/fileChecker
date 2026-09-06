//! File Checker GUI (Tauri, `docs/requirements.md` §10.14, P12). All check/scan logic
//! lives in `filechecker_core` (§10.13); this crate is Tauri command handlers plus a
//! static frontend (`dist/`) that calls them.

mod commands;
mod state;

use std::fs;

use filechecker_core::db;
use tauri::Manager;

use state::AppState;

/// Results DB and registered-password store both live under the OS-standard app data
/// directory (§10.9's "アプリ内で完結する簡易な管理" — no separate location to configure).
fn app_state(app: &tauri::App) -> AppState {
    let app_data_dir = app
        .path()
        .app_data_dir()
        .expect("app data dir should be resolvable on every supported OS");
    fs::create_dir_all(&app_data_dir).expect("failed to create app data directory");

    let db_path = app_data_dir.join("filechecker.sqlite3");
    let conn = db::open(&db_path).expect("failed to open results database");
    let password_store_path = app_data_dir.join("passwords.json");

    AppState {
        conn: std::sync::Mutex::new(conn),
        password_store: std::sync::Mutex::new(None),
        password_store_path,
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let state = app_state(app);
            app.manage(state);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::home::home_summary,
            commands::reference::reference_list,
            commands::reference::reference_generate_from_folder,
            commands::reference::reference_generate_from_scan,
            commands::reference::reference_import_mame,
            commands::check::integrity_run,
            commands::check::integrity_results,
            commands::check::integrity_counts,
            commands::check::duplicate_run,
            commands::check::duplicate_groups,
            commands::check::check_list,
            commands::check::report_export,
            commands::history::scan_folder,
            commands::history::scan_history_list,
            commands::media::media_list,
            commands::media::media_connected,
            commands::media::media_scan_by_id,
            commands::media::media_scan_by_mount,
            commands::settings::settings_get_general,
            commands::settings::settings_set_general,
            commands::settings::password_store_status,
            commands::settings::password_store_create,
            commands::settings::password_store_unlock,
            commands::settings::password_store_lock,
            commands::settings::password_list,
            commands::settings::password_add,
            commands::settings::password_remove,
            commands::settings::master_password_change,
            commands::settings::master_password_reset,
            commands::reconstruct::reconstruct_plan,
            commands::reconstruct::reconstruct_start,
            commands::reconstruct::reconstruct_run_pass,
            commands::reconstruct::reconstruct_status,
        ])
        .run(tauri::generate_context!())
        .expect("error while running the File Checker GUI");
}
