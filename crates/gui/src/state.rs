//! Shared Tauri app state: the results DB connection and the (optionally unlocked)
//! registered-password store (`docs/requirements.md` §10.9/§10.10).
//!
//! Unlike the CLI — which resolves its archive password policy once per process
//! invocation from `--password-store` (see `crates/cli/src/password_policy.rs`) — the
//! GUI is a long-lived process with dedicated "登録パスワード管理"/"マスターパスワード"
//! screens (§10.14), so the unlocked store lives here for as long as the user leaves it
//! unlocked (never written back to disk, per §10.10) rather than being re-derived for
//! every single command.

use std::path::PathBuf;
use std::sync::Mutex;

use filechecker_core::db::Connection;
use filechecker_core::secrets::UnlockedStore;

pub struct AppState {
    pub conn: Mutex<Connection>,
    pub password_store: Mutex<Option<UnlockedStore>>,
    pub password_store_path: PathBuf,
}
