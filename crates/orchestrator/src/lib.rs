//! `NarrowMind Studio` orchestrator.
//!
//! Owns Python worker lifecycle, project state, and IPC for the Tauri shell.
//! Phase 0 ships only the `hello` round-trip; richer surface lands in later phases.

pub mod agent_bridge;
pub mod app_paths;
pub mod error;
pub mod hello;
pub mod inference;
pub mod models;
pub mod project;
pub mod retry;
pub mod secrets;
pub mod tools;
pub mod training;
pub mod worker;
pub mod worker_pool;

pub use agent_bridge::{default_registry, OrchestratorDispatcher};
pub use app_paths::{app_data_root, hf_cache_dir, hf_cache_env, AppPathError};
pub use error::WorkerError;
pub use hello::{hello_round_trip, HelloResult};
pub use inference::{
    InferenceError, InferenceManager, InferenceStatus, ModelSpec,
    DEFAULT_PORT as INFERENCE_DEFAULT_PORT, IDLE_TTL as INFERENCE_IDLE_TTL,
};
pub use models::{BaseModel, ChatTemplateFormat, VramProfile};
pub use project::{
    validate_name, ProjectConfig, ProjectError, ProjectStatus, ProjectStore, ProjectTier,
    ProviderConfig, RagConfig, RetrievalMode, SynthConfig, TrainingConfig, SCHEMA_VERSION,
};
pub use secrets::{SecretError, SecretStore};
pub use tools::{
    new_selected_project, ProjectScope, SelectedProject, Tool, ToolContext, ToolDef, ToolError,
    ToolEvent, ToolRegistry, ToolResult,
};
pub use retry::{with_backoff, BackoffPolicy, Retry};
pub use training::{TrainingEvent, TrainingManager, TrainingStatus};
pub use worker::{call_worker_streaming, PythonRunner, WorkerCommand};
pub use worker_pool::{PooledWorkerStatus, WorkerPool};

/// Crate version string, surfaced through Tauri so the UI can report build info.
#[must_use]
pub const fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
