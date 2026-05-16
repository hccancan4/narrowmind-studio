//! `NarrowMind Studio` orchestrator.
//!
//! Owns Python worker lifecycle, project state, and IPC for the Tauri shell.
//! Phase 0 ships only the `hello` round-trip; richer surface lands in later phases.

pub mod error;
pub mod hello;
pub mod project;
pub mod secrets;
pub mod tools;
pub mod worker;

pub use error::WorkerError;
pub use hello::{hello_round_trip, HelloResult};
pub use project::{
    validate_name, ProjectConfig, ProjectError, ProjectStatus, ProjectStore, ProjectTier,
    ProviderConfig, SCHEMA_VERSION,
};
pub use secrets::{SecretError, SecretStore};
pub use tools::{ProjectScope, Tool, ToolContext, ToolDef, ToolError, ToolEvent, ToolRegistry, ToolResult};
pub use worker::{PythonRunner, WorkerCommand};

/// Crate version string, surfaced through Tauri so the UI can report build info.
#[must_use]
pub const fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
