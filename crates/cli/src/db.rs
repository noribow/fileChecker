//! Small CLI-only helpers around the shared core's DB layer: opening the results DB
//! with a CLI-appropriate error message, and the millisecond-epoch timestamp every
//! `scan_run`/`check_run` needs (§10.12 stores all timestamps as unix milliseconds).

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use filechecker_core::db::Connection;

pub fn open_db(path: &Path) -> Result<Connection, String> {
    filechecker_core::db::open(path)
        .map_err(|e| format!("DBを開けません ({}): {e}", path.display()))
}

pub fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is before the unix epoch")
        .as_millis() as i64
}
