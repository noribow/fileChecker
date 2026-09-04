//! Shared core logic for File Checker.
//!
//! This crate will host hashing, scanning, persistence, integrity-check and
//! duplicate-check logic shared by the CLI and the Tauri GUI (see
//! `docs/requirements.md` §5 / §10.13). It is being built out incrementally
//! per `docs/implementation-plan.md`; modules are added phase by phase.

pub mod hash;

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
