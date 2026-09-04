//! SQLite persistence layer (`docs/requirements.md` §10.12).

mod connection;
mod models;
pub mod repo;
pub mod schema;

pub use connection::{open, open_in_memory};
pub use models::{CheckType, FileStatus, HashMode, ResultStatus, RunStatus, TargetType};
pub use rusqlite::{Connection, Error, Result};
