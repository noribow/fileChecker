//! Shared core logic for File Checker.
//!
//! This crate will host hashing, scanning, persistence, integrity-check and
//! duplicate-check logic shared by the CLI and the Tauri GUI (see
//! `docs/requirements.md` §5 / §10.13). It is being built out incrementally
//! per `docs/implementation-plan.md`; modules are added phase by phase.

pub mod archive;
pub mod db;
pub mod duplicate;
pub mod hash;
pub mod import;
pub mod integrity;
pub mod media;
pub mod reconstruct;
pub mod reference;
pub mod retry;
pub mod scan;
pub mod secrets;

/// Returns the crate version, mainly to give P0 something concrete to test.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_not_empty() {
        assert!(!version().is_empty());
    }
}
