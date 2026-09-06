//! Resolves the CLI's archive-password policy (§10.7) once at startup, before
//! dispatching to whichever subcommand was requested — every subcommand that might
//! touch archive content (`scan folder`/`scan media`/`check integrity`/
//! `check duplicate`/`reference generate`) uses the same resolved policy.
//!
//! Registered-password *management* (adding/removing passwords, setting/changing/
//! resetting the master password) is GUI-only (§10.16 — it's inherently interactive
//! and doesn't fit a non-TTY/CI environment); the CLI only ever *consumes* an existing
//! store, via `--password-store`.

use std::io::IsTerminal;
use std::path::Path;

use filechecker_core::archive::PasswordPolicy;
use filechecker_core::db::{repo, Connection};
use filechecker_core::secrets::UnlockedStore;

use crate::exit;

/// Owns the unlocked store (if any) so its borrow can outlive the `PasswordPolicy` that
/// points at it for the rest of the process's run.
pub struct ResolvedPolicy {
    store: Option<UnlockedStore>,
}

impl ResolvedPolicy {
    pub fn as_policy(&self) -> PasswordPolicy<'_> {
        match &self.store {
            Some(store) => PasswordPolicy::TryRegistered(store),
            None => PasswordPolicy::Reject,
        }
    }
}

/// Reads the `archive_password_mode` app_setting (§10.7: `try_registered` or anything
/// else/unset, meaning mode 1/"error") and, only when it's `try_registered` and
/// `--no-archive-password` wasn't passed, unlocks the password store at
/// `password_store_path` — prompting for the master password on a TTY, failing with
/// exit code 4 (§10.16) if stdin isn't one. Returns `(message, exit_code)` on failure,
/// matching how `main` already reports other startup failures.
pub fn resolve(
    conn: &Connection,
    no_archive_password: bool,
    password_store_path: Option<&Path>,
) -> Result<ResolvedPolicy, (String, i32)> {
    if no_archive_password {
        return Ok(ResolvedPolicy { store: None });
    }
    let mode = repo::get_app_setting(conn, "archive_password_mode")
        .map_err(|e| (e.to_string(), exit::FAILURE))?;
    if mode.as_deref() != Some("try_registered") {
        return Ok(ResolvedPolicy { store: None });
    }
    let Some(path) = password_store_path else {
        return Err((
            "app_setting archive_password_mode=try_registered ですが、--password-store で登録パス\
             ワード設定ファイルの場所を指定してください"
                .to_string(),
            exit::FAILURE,
        ));
    };
    if !std::io::stdin().is_terminal() {
        return Err((
            "登録済みパスワードでの復号にはマスターパスワードの入力が必要ですが、標準入力がTTYでは\
             ありません（--no-archive-passwordで復号を試みずエラー扱いにできます）"
                .to_string(),
            exit::INTERACTIVE_REQUIRED,
        ));
    }
    eprintln!("マスターパスワードを入力してください:");
    let mut input = String::new();
    std::io::stdin()
        .read_line(&mut input)
        .map_err(|e| (e.to_string(), exit::FAILURE))?;
    let master_password = input.trim_end_matches(['\n', '\r']);

    let store =
        UnlockedStore::unlock(path, master_password).map_err(|e| (e.to_string(), exit::FAILURE))?;
    Ok(ResolvedPolicy { store: Some(store) })
}
