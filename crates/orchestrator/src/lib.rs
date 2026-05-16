//! `NarrowMind Studio` orchestrator.
//!
//! Owns Python worker lifecycle, project state, and IPC for the Tauri shell.
//! Phase 0 ships only the `hello` round-trip; richer surface lands in later phases.

pub mod error;
pub mod hello;
pub mod worker;

pub use error::WorkerError;
pub use hello::{hello_round_trip, HelloResult};
pub use worker::{PythonRunner, WorkerCommand};

/// Crate version string, surfaced through Tauri so the UI can report build info.
#[must_use]
pub const fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
