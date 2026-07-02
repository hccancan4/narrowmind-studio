//! Tool framework: trait, registry, project-scoped execution context, sandbox helpers.
//!
//! The agent loop calls [`ToolRegistry::dispatch`] for every `tool_use` block emitted by the
//! provider. Each [`Tool`] implementation owns its JSON Schema, argument deserialisation, and
//! the actual work — the registry only routes and the loop only forwards the result back to
//! the provider as a `tool_result` block.

pub mod build_dataset;
pub mod chunks;
pub mod context;
pub mod corpora;
pub mod eval;
pub mod fs;
pub mod import_sft_from_hf;
pub mod inference;
pub mod ingest;
pub mod projects;
pub mod rag;
pub mod registry;
pub mod run_command;
pub mod sandbox;
pub mod synth_gen;
pub mod training;
pub(crate) mod util;

pub use context::{
    new_selected_project, ProjectScope, SelectedProject, ToolContext, ToolEvent, ToolEventSink,
};
pub use registry::{Tool, ToolDef, ToolError, ToolRegistry, ToolResult};
