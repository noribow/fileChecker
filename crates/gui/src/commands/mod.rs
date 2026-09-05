//! Tauri command handlers, one module per §10.14 screen group. Each command is a thin
//! wrapper over `filechecker_core` (§10.13 — no check/scan logic lives in the GUI
//! layer, same rule the CLI already follows) plus request validation and DTO shaping
//! for Tauri's JSON-based IPC (hex-encoding hash bytes, etc.).

pub mod check;
pub mod helpers;
pub mod history;
pub mod home;
pub mod media;
pub mod reconstruct;
pub mod reference;
pub mod settings;
