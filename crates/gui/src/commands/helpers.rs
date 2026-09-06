//! Shared plumbing for command handlers: the millisecond timestamp every scan_run/
//! check_run needs (§10.12), hex encoding for hash bytes sent to the frontend (JSON has
//! no byte-string type, and a raw `Vec<u8>` would serialize as an ugly number array),
//! and the GUI's archive-password-policy resolution (see `crate::state` for why this
//! differs from the CLI's one-shot-per-process version).

use std::time::{SystemTime, UNIX_EPOCH};

use filechecker_core::archive::PasswordPolicy;
use filechecker_core::db::{repo, Connection};

use crate::state::AppState;

pub fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is before the unix epoch")
        .as_millis() as i64
}

pub fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn stringify<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

/// Resolves the current archive password policy (§10.7) from the `archive_password_mode`
/// app_setting and runs `op` with it: `Reject` unless the setting is `try_registered`
/// *and* the password store is currently unlocked in memory, in which case `op` runs
/// with `PasswordPolicy::TryRegistered`. If the setting is `try_registered` but the
/// store isn't unlocked yet, this errors out (asking the frontend to unlock it first via
/// `password_store_unlock`) rather than silently falling back to `Reject` and letting
/// otherwise-decryptable archives quietly show up as errors.
pub fn with_password_policy<T>(
    state: &AppState,
    conn: &mut Connection,
    op: impl FnOnce(&mut Connection, &PasswordPolicy) -> filechecker_core::db::Result<T>,
) -> Result<T, String> {
    let mode = repo::get_app_setting(conn, "archive_password_mode").map_err(stringify)?;
    if mode.as_deref() == Some("try_registered") {
        let guard = state
            .password_store
            .lock()
            .expect("password_store mutex poisoned");
        return match guard.as_ref() {
            Some(store) => {
                let policy = PasswordPolicy::TryRegistered(store);
                op(conn, &policy).map_err(stringify)
            }
            None => Err(
                "設定でパスワード保護アーカイブの復号が有効になっていますが、登録パスワード\
                 設定ファイルがロックされています。設定画面でマスターパスワードを入力してから\
                 やり直してください。"
                    .to_string(),
            ),
        };
    }
    op(conn, &PasswordPolicy::Reject).map_err(stringify)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_encode_is_lowercase_and_zero_padded() {
        assert_eq!(hex_encode(&[0x00, 0x0f, 0xab, 0xff]), "000fabff");
        assert_eq!(hex_encode(&[]), "");
    }

    #[test]
    fn with_password_policy_rejects_by_default_when_no_setting_is_configured() {
        let mut conn = filechecker_core::db::open_in_memory().unwrap();
        let state = AppState {
            conn: std::sync::Mutex::new(filechecker_core::db::open_in_memory().unwrap()),
            password_store: std::sync::Mutex::new(None),
            password_store_path: std::path::PathBuf::from("/nonexistent"),
        };
        let saw_reject = with_password_policy(&state, &mut conn, |_conn, policy| {
            Ok(matches!(policy, PasswordPolicy::Reject))
        })
        .unwrap();
        assert!(saw_reject);
    }

    #[test]
    fn with_password_policy_errors_when_try_registered_but_store_is_locked() {
        let mut conn = filechecker_core::db::open_in_memory().unwrap();
        repo::set_app_setting(&conn, "archive_password_mode", "try_registered").unwrap();
        let state = AppState {
            conn: std::sync::Mutex::new(filechecker_core::db::open_in_memory().unwrap()),
            password_store: std::sync::Mutex::new(None),
            password_store_path: std::path::PathBuf::from("/nonexistent"),
        };
        let result = with_password_policy(&state, &mut conn, |_conn, _policy| {
            Ok::<(), filechecker_core::db::Error>(())
        });
        assert!(result.is_err());
    }
}
